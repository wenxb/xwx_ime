//! crate 级单测:布尔 DAG 可达性与 beam 的等价性(codex 审查第二批 P1)。
//!
//! 生产查询把「精确格上有没有覆盖整串的读法」从**跑一整次 beam** 改成
//! **布尔 DP**(0 次 LM 调用、O(边数))。这条改动只有在两者语义确实等价时
//! 才成立,所以这里在**随机小格**上穷举对拍:边的图是随机词库生成的,
//! 输入末音节随机截断(模拟「还在打字」),两种判定必须逐例一致。

use super::*;

/// 固定种子的线性同余发生器:测试要可复现,不引第三方依赖。
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

#[test]
fn dag_reachability_matches_beam_emptiness_on_random_lattices() {
    const SYLS: [&str; 12] = [
        "a", "ai", "an", "ba", "bei", "hao", "ni", "wo", "shi", "qu", "jing", "tian",
    ];
    let ctx = LmContext::empty();
    let mut rng = Lcg(0x5eed_1234_abcd);
    let mut cases = 0usize;
    let mut reachable_cases = 0usize;
    let mut unreachable_cases = 0usize;

    for round in 0..300 {
        let engine = ImeEngine::new(None).expect("内存引擎创建失败");
        // 随机小词库:1~3 个音节组成词,词频随机。
        let mut words_pinyin: Vec<String> = Vec::new();
        for w in 0..(1 + rng.below(6)) {
            let pinyin: Vec<&str> = (0..(1 + rng.below(3)))
                .map(|_| SYLS[rng.below(SYLS.len())])
                .collect();
            let key = pinyin.join(" ");
            engine
                .dict
                .upsert(&format!("词{w}号{round}"), &key, 1 + rng.below(1000) as u64)
                .expect("插入失败");
            words_pinyin.push(key);
        }
        // 随机输入,两种构造各一半——**必须两种都覆盖到**,否则这条对拍会
        // 偏向一边(第一版全是随机截断,300 例里只有 5 例可达,等于没验):
        // - 「拼接」:直接拼 1~3 个刚插进词库的词条 ⇒ 构造上必有覆盖整串的
        //   读法(那些词边就在格里),用来验「可达」一侧;
        // - 「截断」:拼接后把末尾随机截一刀 ⇒ 多半不可达,验另一侧。
        let mut input = String::new();
        for _ in 0..(1 + rng.below(3)) {
            if words_pinyin.is_empty() {
                input.push_str(SYLS[rng.below(SYLS.len())]);
            } else {
                input.push_str(&words_pinyin[rng.below(words_pinyin.len())].replace(' ', ""));
            }
        }
        if rng.below(2) == 0 && !input.is_empty() {
            let cut = 1 + rng.below(input.len());
            input.truncate(cut);
        }

        let lattice = engine.build_lattice(&input, None, None).expect("建格失败");
        let reachable = ImeEngine::lattice_reachable(&lattice, input.len());
        let beam = engine
            .decode_beam_with_lattice(&input, &ctx, BEAM_WIDTH, &lattice)
            .expect("beam 失败");
        assert_eq!(
            reachable,
            !beam.is_empty(),
            "可达性判定与 beam 不一致:输入 {input:?}(第 {round} 例)\n格 = {lattice:?}"
        );

        cases += 1;
        if reachable {
            reachable_cases += 1;
        } else {
            unreachable_cases += 1;
        }
    }

    // 两种结果都要覆盖到:全可达或全不可达时,这条对拍等于什么都没验。
    assert!(cases >= 300, "样本不足:{cases}");
    assert!(
        reachable_cases > 20 && unreachable_cases > 20,
        "随机样本偏向一边,对拍没有说服力:可达 {reachable_cases} / 不可达 {unreachable_cases}"
    );
}
