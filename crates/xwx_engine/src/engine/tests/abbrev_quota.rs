//! crate 级单测:abbrev_quota。

use super::*;

/// 保底名额(原 `engine::ABBREV_QUOTA`,第 8 步删通道后生产侧已无消费者,
/// 语义由 ky 腿的「简拼层填满 100」与桶序断言继承)。
const ABBREV_QUOTA: usize = 5;

/// 钉住桶内层序契约（**唯一出处**：[`crate::engine::rank::effective_layer`]）：
///
/// 1. `(u_min 桶, 有效层序)` 整体非降——`Exact` → `Fuzzy` → `Abbrev`；
/// 2. `Candidate::spelling`（原始来源）在桶内**允许且只允许一种交错**：
///    被收编的整串条目级简拼排在 `Fuzzy` 之前。
///
/// 旧判据直接比 `c.spelling`，隐含假设「两层在桶内不交错」；收编生效后该假设
/// 不再成立（实测 `renm` 桶 0 内 `人们`(收编档) → `仍`(Fuzzy) → `日俄`(Abbrev)），
/// 故按①+②重述。②不是①的推论，单独留着：它禁止的是「未被收编的简拼
/// 插到容错读法之前」这种真错序。
fn assert_bucket_layer_order(inp: &str, cands: &[Candidate], seg: &Segmenter, has_abbrev: bool) {
    let keys: Vec<(usize, SpellingKind)> = cands
        .iter()
        .map(|c| (sort_bucket(inp, c), sort_layer(inp, c, seg, has_abbrev)))
        .collect();
    assert!(
        keys.windows(2).all(|w| w[0] <= w[1]),
        "{inp} (u_min 桶, 有效层序) 应整体非降: {:?}",
        cands
            .iter()
            .map(|c| (
                &c.word,
                c.spelling,
                sort_bucket(inp, c),
                sort_layer(inp, c, seg, has_abbrev)
            ))
            .collect::<Vec<_>>()
    );
    let exc = entry_tier_exception(inp, seg, has_abbrev);
    for w in cands.windows(2) {
        if sort_bucket(inp, &w[0]) != sort_bucket(inp, &w[1]) {
            continue;
        }
        // 桶内**原始来源**出现 `Abbrev` 在 `Fuzzy` 之前时，只允许一种理由：
        // 那条 Abbrev 是被收编的整串条目级简拼（有效层序因此是 `Exact`，
        // 未收编的 Abbrev 排在 Fuzzy 之前则是真错序）。
        // 反方向（Fuzzy 后接 Abbrev）是正常层序，不在此列。
        if w[0].spelling == SpellingKind::Abbrev && w[1].spelling == SpellingKind::Fuzzy {
            assert!(
                exc && w[0].freq > 0 && w[0].covered >= inp.len(),
                "{inp} 桶内 Abbrev→Fuzzy 交错只允许「被收编的整串条目级简拼」: \
                 {:?}(Abbrev, freq={}, covered={}) 之后是 {:?}(Fuzzy)",
                w[0].word,
                w[0].freq,
                w[0].covered,
                w[1].word
            );
        }
    }
}

/// 简拼层保底配额(当年配额缺口长尾的修法):
/// 精确+兜底层装满 `MAX_CANDIDATES` 时,简拼层仍有 `ABBREV_QUOTA`
/// 条进候选列表,且目标词必须出现。真实词库 + 万象 gram,
/// 与 probe 对拍同一装配;缺文件时跳过(同 `real_wanxiang_routes_through_engine`)。
#[test]
fn abbrev_layer_survives_full_exact_layer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    let gram = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
    let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
    else {
        return; // 环境缺文件:跳过(与 real_wanxiang_routes_through_engine 同款)
    };
    if !gram_path.is_file() {
        return;
    }
    let mut engine =
        ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    for (input, want) in [
        ("zhongguor", "中国人"),
        ("nih", "你好"),
        ("renm", "人们"),
        ("beijdx", "北京大学"),
        ("zhonggr", "中国人"),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(
            cands.iter().any(|c| c.word == want),
            "{input} 的候选里必须出现 {want}(简拼层保底配额): 末尾 {:?}",
            cands.iter().rev().take(8).map(|c| &c.word).collect::<Vec<_>>()
        );
    }

    // **装满组（2026-10-05 换输入）**：窗口 100 → 512 后，`xian`（实测 186 条）
    // 与 `ky`（226 条）不再装满——「装满时简拼层仍保底」这一面改用池子远大于
    // 窗口的 `zh`/`sh` 见证（实测各 512 条，其中简拼层 294/336 条）。
    // 只断结构：简拼层 ≥ ABBREV_QUOTA 且 (u_min 桶, 层序) 排序键整体非降
    // （原断言口径不变，见下面 `assert_bucket_layer_order`）。
    let has_abbrev = engine.dict.has_abbrev();
    let seg = Segmenter::new();
    for input in ["zh", "sh"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert_eq!(cands.len(), MAX_CANDIDATES, "{input} 应装满");
        let n_abbrev = cands
            .iter()
            .filter(|c| c.spelling == SpellingKind::Abbrev)
            .count();
        assert!(
            n_abbrev >= ABBREV_QUOTA,
            "{input} 简拼层应至少保底 {ABBREV_QUOTA} 条(实际 {n_abbrev})"
        );
        assert_bucket_layer_order(input, &cands, &seg, has_abbrev);
    }
    // **⑤⑩⑯ 裁决(2026-10-03,第 8 步删通道)**:可达输入的跨界读法
    // (xian→xia+nian 下年度、e+nuo 婀娜多姿 一族)随旧通道删除——
    // 可达输入走精确格,结构上没有简拼层;窗口变大、索引驱动的混合读法
    // 并入之后这一条照样钉住 0,不许以任何形式回潮。
    let xian = engine
        .lookup_candidates("xian", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(
        xian
            .iter()
            .filter(|c| c.spelling == SpellingKind::Abbrev)
            .count(),
        0,
        "可达输入 xian 不该出现简拼层(跨界读法已按 ⑤⑩⑯ 删除)"
    );
    assert_bucket_layer_order("xian", &xian, &seg, has_abbrev);
}

/// 残余字母联想交给简拼层后的可见性(真实词库 + 万象 gram):
/// `nih`/`renm`/`zhongguor` 的目标词必须出现且位于简拼层,
/// 而不再被「只解释尾部 1 个字母的词」挤出前 95 名
/// (修前:你好 97 / 人们 96 / 中国人 96)。排名上限是当前实测值
/// 的回归锉(不许变差),不是验收目标——目标是前 20。
#[test]
fn mixed_abbrev_targets_are_visible() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    let gram = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .map(|b| b.join("xwx/wanxiang-lts-zh-hans.gram"));
    let (Some(gram_path), true, true) = (gram, dict_path.is_file(), abbrev_path.is_file())
    else {
        return; // 环境缺文件:跳过
    };
    if !gram_path.is_file() {
        return;
    }
    let mut engine =
        ImeEngine::new_with_lm(Some(&dict_path), Some(&gram_path)).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    // (输入, 目标词, 排名上限回归锉);上限 = 实测值,不允许再往后退。
    // 三段式(首键从「裸未解释字母数」改为「u_min 桶」后,名次锚
    // 不变):原断言「中国人/人们 ≤ 1、你好 ≤ 2」——裸 u 首键时
    // 的实测位;新规则(u_min 桶 + 覆盖罚分 0.2)下实测位不变
    // (中国人/人们 #1、你好 #2,见交付报告 B 组表)——目标词靠
    // 「简拼层恒 0 桶 + 配额」保护,段内碎片字靠 u_min>0 桶沉底,
    // 与系数无关,名次锚原样保留,不是放松。
    // **2026-10-02 改一条上限(逐条说明)**:
    // - `nih`/你好:旧上限 2 是**按当时不完整的召回列表**标定的——
    //   混合简拼的收集早停按探索顺序截断,把词频更高的「你会」
    //   (-15.836)、「你回」(-15.878)丢了,「你好」(-15.967)才排到第 2。
    //   T1 修好召回后这三个词都进来了,按词频排序「你好」落到第 4。
    //   新上限 4 = 修理后实测位;这是**召回变全**的直接后果,不是排序退化
    //   (旧列表里的 你还/你和/你很 一个都没掉,只是前面多了两个更高频的)。
    let has_abbrev = engine.dict.has_abbrev();
    let seg = Segmenter::new();
    for (input, want, rank_ceiling) in [
        ("nih", "你好", 4usize),
        ("renm", "人们", 1usize),
        ("zhongguor", "中国人", 1usize),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let pos = cands
            .iter()
            .position(|c| c.word == want)
            .unwrap_or_else(|| panic!("{input} 应含 {want}: {:?}", cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
        assert!(
            pos < rank_ceiling,
            "{input} 的 {want} 排第 {} 名,不应比修理后实测位({rank_ceiling})更靠后",
            pos + 1
        );
        // 目标词必须在简拼层,且排序键(u_min 桶, 层序)整体
        // 非降——同桶内的层序保证未被破坏(三段式:原断言
        // 「(未解释字母数, 层序) 整体非降」;改为 (u_min 桶, 层序)
        // 整体非降——首键换成排序真正用的桶,跨桶时 0 桶简拼词
        // 合法地排在 u_min>0 桶精确字之前,这是首键的预期效果,
        // 不是交错)。
        assert_eq!(
            cands[pos].spelling,
            SpellingKind::Abbrev,
            "{input} 的 {want} 应来自简拼层"
        );
        assert_bucket_layer_order(input, &cands, &seg, has_abbrev);
    }
}

/// 配额只是保底不是封顶,且不浪费名额(回归:`ren` 旧行为是
/// head 21 条 + 简拼联想补满 100,曾有一版实现把简拼层无条件
/// 截到配额导致退化)。真实词库 + 空 LM(无需 gram):`ren` 的
/// head ≪ 100,简拼层应补满到 100;`zhongguo`(整词精确,
/// 简拼层为空)不受影响,全拼照旧。
#[test]
fn abbrev_quota_is_floor_not_cap() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = dict_path.with_file_name("abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let mut engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);

    // head 很短时:简拼层补满到 MAX_CANDIDATES(保底≠封顶)
    let ren = engine
        .lookup_candidates("ren", &LmContext::empty())
        .expect("查询失败");
    // **⑤⑩⑯ 裁决(2026-10-03,第 8 步删通道)**:ren 是可达输入,旧通道的
    // 跨界补全读法(re+n... 一族,原占 100 行里的 79 行)随通道删除——
    // 可达输入的页面回到纯精确补全。配额机制本身已删;「保底不是封顶」
    // 的格子世界对应面由上面 `abbrev_layer_survives_full_exact_layer`
    // 的 ky 腿钉住。
    assert!(!ren.is_empty(), "ren 候选非空");
    let n_abbrev = ren
        .iter()
        .filter(|c| c.spelling == SpellingKind::Abbrev)
        .count();
    assert_eq!(
        n_abbrev, 0,
        "可达输入 ren 不该再有简拼层(跨界读法已按 ⑤⑩⑯ 删除)"
    );
    // 排序键未被破坏:(u_min 桶, 层序) 整体非降(三段式:原断言
    // 「(未解释字母数, 层序) 整体非降」;改为 (u_min 桶, 层序) 整体
    // 非降——同桶内的层序保证原样钉住,跨桶时 0 桶简拼词合法排在
    // u_min>0 桶精确字之前)。
    let pair_keys: Vec<(usize, SpellingKind)> = ren
        .iter()
        .map(|c| (sort_bucket("ren", c), c.spelling))
        .collect();
    assert!(
        pair_keys.windows(2).all(|w| w[0] <= w[1]),
        "ren (u_min 桶, 层序) 应整体非降: {:?}",
        ren.iter()
            .map(|c| (&c.word, c.spelling, sort_bucket("ren", c)))
            .collect::<Vec<_>>()
    );

    // 简拼层为空时:head 照旧,不因配额浪费名额(逐位不变由对拍保证,
    // 这里断结构:无 Abbrev 候选、条数与无配额时一致)。
    let zhongguo = engine
        .lookup_candidates("zhongguo", &LmContext::empty())
        .expect("查询失败");
    // **2026-10-02 改断言(逐条说明)**:旧断言是「zhongguo 不应有简拼层」。
    // T2 的末尾半音节读法让**整串合法全拼**的输入也会产出 Abbrev 层候选
    // (`zhongguo` → 中国话/中国国际…这类「最后一个音节之后还有内容」的
    // 补全),它们走主索引、与副索引配额无关。
    // 本测试的本意是「简拼层为空时配额不浪费名额」,所以改成断结构:
    // 候选非空、**前 10 名仍是精确层**(配额不会把 Abbrev 顶到前面)。
    assert!(!zhongguo.is_empty(), "zhongguo 候选非空");
    assert!(
        zhongguo.iter().take(10).all(|c| c.spelling == SpellingKind::Exact),
        "前 10 名应仍是精确层(配额只影响简拼层,不挤占精确层): {:?}",
        zhongguo.iter().take(10).map(|c| (&c.word, c.spelling)).collect::<Vec<_>>()
    );
}

/// `z`/`c`/`s` 双前缀(玩具词库):它们既是 z/c/s 系音节的声母,
/// 又是 zh/ch/sh 系音节的前缀——枚举必须**两套都枚进来**
/// (`starts_with` 天然覆盖,不需要特判;这条测试钉住它,防后人
/// 「优化」成只枚一套)。
///
/// 观测点(负向控制下变红):只用「在/中出现」断不住(内存库的
/// freq 降序前缀联想在旧实现下也能捞到它们)。真正能区分的是
/// **逐音节限流的形状**:z 系灌 `SYLLABLE_ENUM_QUOTA + 5` 条高频
/// zai 字,若走了枚举,zh 系的「中」必定占位(z 系被配额压住);
/// 若没走枚举(单次前缀联想),zai 系 21 条高频字会按 freq 把
/// 「中」(freq 仅 1)挤到候选列表截断线之外——MAX_CANDIDATES=100
/// 救不了它,因为这里总条数 < 100、不打满,但 zai 系 21 条全在
/// 「中」前面。断「中」必须落在前 `SYLLABLE_ENUM_QUOTA + 1` 名内。
#[test]
fn single_letter_initial_covers_both_sibilant_families() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // z 系灌 SYLLABLE_ENUM_QUOTA + 5 条高频 zai 字:足够在「无配额
    // 单次前缀联想」下把低频「中」压到很靠后,又不触发 100 条截断。
    for i in 0..(SYLLABLE_ENUM_QUOTA + 5) as u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine
            .dict
            .upsert(&w, "zai", 10_000 - i as u64)
            .expect("插入失败");
    }
    engine.dict.upsert("中", "zhong", 1).expect("插入失败");

    let cands = engine
        .lookup_candidates("z", &LmContext::empty())
        .expect("查询失败");
    let pos_zhong = cands
        .iter()
        .position(|c| c.word == "中")
        .unwrap_or_else(|| panic!("z 应召回 zh 系的「中」: {:?}",
            cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()));
    assert!(
        pos_zhong <= SYLLABLE_ENUM_QUOTA,
        "枚举路径下 zai 系被配额({SYLLABLE_ENUM_QUOTA})限流,「中」不应排第 {} 名这么靠后: {:?}",
        pos_zhong + 1,
        cands.iter().take(30).map(|c| &c.word).collect::<Vec<_>>()
    );
}

/// 多音字按 word 去重(玩具词库):逐音节枚举会把同一汉字的每个
/// 读音都捞出来(「这」挂在 zhe 与 zhei 两个 key 下),同一个字
/// 不得以多条 (word, 不同拼音) 重复占位。
#[test]
fn enum_expansion_dedups_polyphonic_char_by_word() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("这", "zhe", 100).expect("插入失败");
    engine.dict.upsert("这", "zhei", 100).expect("插入失败");

    let cands = engine
        .lookup_candidates("zh", &LmContext::empty())
        .expect("查询失败");
    let n = cands.iter().filter(|c| c.word == "这").count();
    assert_eq!(n, 1, "「这」只能出现一次(zhe/zhei 两条应合并): {:?}",
        cands.iter().map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>());
}
