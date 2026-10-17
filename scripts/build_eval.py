#!/usr/bin/env python3
"""从 dict.bin 构建候选排序评测集（第 4 步：建评测集 + 修量级失衡；
第 4b 步：新增 `unbiased` 无偏对照小集）。

样本来源（三组，输出同一份 JSONL，字段 `kind` 区分）：

1. `main`（无上文）：dict.bin 里 2~4 字全汉字词条，拼音去掉空格作为
   输入，词本身作为期望首选。过滤：
   - 歧义剔除：对同一输入串（如 "xian" 同时对应 先/现 与 西安），只保留
     词频 ≥ RATIO × 组内次高 的词条（RATIO 默认 10，可用 --ratio 调）；
   - 音节合法性：key 的每个音节必须在引擎音节表内（从
     crates/xwx_engine/src/segmenter.rs 的 SYLLABLES 常量提取），否则
     引擎根本切不出该输入，样本不可达；
   - 词条频 ≥ 1、纯汉字、key 全小写字母。

2. `ctx`（带上文）：4 字词条拆 2+2 得到真实搭配对 (w1, w2)（词条本身
   就是搭配存在的证据，如 今天天气 → (今天, 天气)），w1 作上下文、输入
   = w2 的拼音、期望首选 w2。附带 `dom` 字段标记 w2 是否为该输入的
   组内最高频（非最高频的样本正是「上文能不能翻盘」的敏感样本）。
   说明：/usr/share/rime-data/essay.txt 是「词<TAB>频」频率表，无
   连续文本可用（无相邻词对），因此上文样本改用词库自洽构造。

3. `unbiased`（无偏反词频小集）：人工选定 ~100 条常用搭配 (ctx, word)
   作样本，期望首选 = word 本身，但严格执行「word 不是该拼音输入组内
   最高频的词」。

   **为什么这组无偏**：
   - `main` 组的期望答案就是「该拼音下词频最高的词」——空 LM（纯按词频
     排序）在这一组几乎必然满分，所以 main 组测的是「词频表 + 可达性」，
     不是语言知识（实测空 LM main top1 99.8% > 万象 92.2%，正是这把尺
     被词频答案刻出来的假象）。
   - `unbiased` 组的期望答案**刻意排除**最高频词：答案由人的日常
     搭配知识给出（如（春节, 放假）→ 放假，而该输入的组内最高频
     是房价），不依赖词库词频表、不依赖万象 `.gram`。构建时词库只在
     「可达性」层面介入（期望词/上下文必须存在于 dict.bin 且音节
     合法），不做任何频率断言；反而断言「词不是组内最高频」——
     这与 `main` 的取频断言完全相反。

   **局限（必须写在报告里）**：
   - 规模小（约 100 条），统计误差 ~±5 个百分点（1σ），只够做
     交叉验证标尺（「万象 vs 空 LM」的方向性判断），不适合做调参的
     主最小化目标——调 `lm_weight` 的主指标应看 `ctx` / `main`。
   - 人工选词不服从真实输入分布（主观、口语），不是采样样本；
     若用它汇报绝对命中率，只能代表“常见搭配在上下文模型上的
     表现”，不能推广到真实键列。
   - 构造时**只用词库可达性过滤，不用任何语言模型的输出**，因此
     本组可以干净地比较「万象 vs 空 LM」（这与方向 1 「用万象 .gram
     的命中来选样本」相反——那条路会循环论证，本组不会）。
   - 「期望词非组内最高频」只能保证「按词频排的答案不对」，不能保证
     该词在 .gram 里必有可查的搭配；LM 若在这组上命中率低，可能是
     模型约束，也可能是搭配未入 .gram——两者都是有效信号，但分不清
     是哪一个，报告时应给「带上文/无上文」两列对照。

   为什么不做「多字词整句」组（方向 2，实测否决）：
   用 5~8 字词条的拼音整串作输入、整词作期望（无上文），在
   `eval_rank` 上实测空 LM top1 ≈ 93.5%，万象（w=0.5/1/2/5）只有
   81.5~82%：空上下文时万象对每个词只加常数，无法用上下文知识把
   整词路径抬起来，观测到的反而是「逐词加常数」对切分结构的副作用。
   该组给出的结论与 `main` 组同类（万象更差），不能作为无偏新尺；
   真正有区分度的是带上下文的 `ctx` / `unbiased`。

用法：
    python3 scripts/build_eval.py [dict.bin] [out.jsonl]
    # 默认 dict.bin = 仓库 data/dict.bin（与 ~/.local/share/xwx/dict.bin
    # 同一文件，md5 相同），输出 scripts/eval_samples.jsonl
"""

import ast
import json
import os
import re
import struct
import sys

MAGIC = b"XWXIDM1\0"
HEADER_LEN = 52

HAN_RE = re.compile(r"^[\u4e00-\u9fff]+$")


def read_dict(path: str):
    """dict.bin → (entries: list[(pinyin_key, word, freq)], total_freq)。"""
    with open(path, "rb") as f:
        b = f.read()
    # 只读 v1/v2 共有的前 52 字节（v2 只是在尾部追加了 total_freq）。
    magic, _ver, _pc, _total, _ro, _so, eo = struct.unpack("<8sIQQQQQ", b[:HEADER_LEN])
    assert magic == MAGIC, "不是 dict.bin"
    pos = eo
    entries = []
    total_freq = 0
    while pos < len(b):
        wl = b[pos]
        word = b[pos + 1 : pos + 1 + wl].decode("utf-8")
        freq = struct.unpack("<I", b[pos + 1 + wl : pos + 5 + wl])[0]
        entries.append((None, word, freq))  # pinyin 占位，下面按块补
        total_freq += freq
        pos += 1 + wl + 4
    # 第二遍读拼音索引，把 pinyin 填回去（内存占用可接受：186 万条）
    pc = struct.unpack("<Q", b[12:20])[0]
    ro, so = struct.unpack("<QQ", b[28:44])
    recs = []
    for i in range(pc):
        str_off, str_len, ent_off, ent_len = struct.unpack(
            "<IHIH", b[ro + i * 12 : ro + i * 12 + 12]
        )
        py = b[so + str_off : so + str_off + str_len].decode("utf-8")
        recs.append((py, eo + ent_off, ent_len))
    recs.sort(key=lambda r: r[1])
    recs.append((None, len(b), 0))
    eidx = 0
    for (py, off, _elen), (_py2, off2, _e2) in zip(recs, recs[1:]):
        cnt = 0
        p = off
        while p < off2:
            wl = b[p]
            p += 1 + wl + 4
            cnt += 1
        for j in range(cnt):
            entries[eidx + j] = (py, entries[eidx + j][1], entries[eidx + j][2])
        eidx += cnt
    return entries, total_freq


def syllable_set(segmenter_rs: str) -> set[str]:
    """从 segmenter.rs 的 SYLLABLES 常量提取音节集合（构建期静态提取）。"""
    m = re.search(r"pub const SYLLABLES: &\[&str\] = &\[(.*?)\];", segmenter_rs, re.S)
    assert m, "找不到 SYLLABLES 常量"
    body = "\n".join(
        line for line in m.group(1).splitlines() if not line.lstrip().startswith("//")
    )
    return {s for s in ast.literal_eval("[" + body + "]") if isinstance(s, str)}


def build_main(entries, syllables, ratio: float, min_freq: int):
    """无上文样本：输入 = key 去空格，词 = 期望首选，带歧义过滤。"""
    # input -> [(word, freq)]，同一输入跨 key 汇总（xian 与 xi an 是同一输入）
    by_input: dict[str, list[tuple[str, int]]] = {}
    for py, word, freq in entries:
        if freq < 1 or not word or not HAN_RE.match(word):
            continue
        L = len(word)
        if L < 2 or L > 4:
            continue
        if freq < min_freq:
            continue
        syls = py.split(" ")
        if not all(s in syllables for s in syls):
            continue
        inp = py.replace(" ", "")
        if not inp.isascii() or not inp.islower():
            continue
        by_input.setdefault(inp, []).append((word, freq))

    samples = []
    dropped_ambig = 0
    for inp, group in by_input.items():
        group.sort(key=lambda wf: wf[1], reverse=True)
        top_word, top_freq = group[0]
        if len(top_word) < 2 or len(top_word) > 4:
            continue
        second = group[1][1] if len(group) > 1 else 0
        if top_freq < ratio * second:
            dropped_ambig += 1
            continue
        samples.append(
            {
                "kind": "main",
                "input": inp,
                "word": top_word,
                "len": len(top_word),
                "ctx": "",
                "freq": top_freq,
            }
        )
    return samples, dropped_ambig


def build_ctx(entries, syllables, w2_ratio: float, w_freq_min: int):
    """带上文样本：4 字词条拆 2+2 → (w1 上文, w2 期望)，w2 拼音取最高频 key。"""
    word_freq: dict[str, int] = {}  # 词 -> 累计频（跨拼音）
    word_keys: dict[str, list[tuple[str, int]]] = {}  # 词 -> [(key, freq)]
    for py, word, freq in entries:
        if freq < 1 or not HAN_RE.match(word):
            continue
        word_freq[word] = word_freq.get(word, 0) + freq
        word_keys.setdefault(word, []).append((py, freq))

    # w2 的拼音 key：取该词频最高的 key（README 对 tencent 无注音词条
    # 的同一策略），且该 key 全部音节合法
    def best_key(word: str):
        cands = sorted(word_keys[word], key=lambda kf: kf[1], reverse=True)
        for key, _ in cands:
            if all(s in syllables for s in key.split(" ")) and key.replace(" ", "").islower():
                return key
        return None

    # 输入组（跨 key 汇总），供 w2 主导性判断
    by_input: dict[str, list[tuple[str, int]]] = {}
    for py, word, freq in entries:
        inp = py.replace(" ", "")
        by_input.setdefault(inp, []).append((word, freq))

    pairs = {}  # (w1, w2) -> 4 字词条累计频（即搭配强度）
    for py, word, freq in entries:
        if freq < w_freq_min or len(word) != 4 or not HAN_RE.match(word):
            continue
        w1, w2 = word[:2], word[2:]
        if w1 not in word_keys or w2 not in word_keys:
            continue
        if word_freq.get(w1, 0) < w_freq_min:
            continue
        pairs[(w1, w2)] = pairs.get((w1, w2), 0) + freq

    samples = []
    for (w1, w2), strength in pairs.items():
        key = best_key(w2)
        if key is None:
            continue
        inp = key.replace(" ", "")
        group = sorted(by_input.get(inp, []), key=lambda wf: wf[1], reverse=True)
        w2f = word_freq[w2]
        dom = bool(group) and group[0][0] == w2 and (
            len(group) == 1 or w2f >= w2_ratio * group[1][1]
        )
        # 敏感样本：w2 不是组内最高频，但差距 ≤100×，上文可能翻盘
        if not dom:
            topf = group[0][1] if group else 0
            if w2f < topf / 100.0:
                continue
        samples.append(
            {
                "kind": "ctx",
                "input": inp,
                "word": w2,
                "len": 2,
                "ctx": w1,
                "dom": dom,
                "strength": strength,
            }
        )
    return samples


# 人工「常用搭配」候选表（第 4b 步，kind=unbiased）。
#
# 列 (ctx, word) 是自然语言的常见搭配（写表格时凭普通语言常识，不看
# 词频表、不看 .gram）；构建期（build_unbiased）对每行做硬性过滤：
#   - word 必须在词典里（按拼音可查询、音节合法）；
#   - 期望 word 必须**不是**该输入拼音组内的最高频词（否则剔除——
#     ——这正是「无偏」的定义点）；
#   - ctx 也必须在词典里（网格以外无法给上文）。
# 构造不依赖任何语言模型输出；.gram 只用于评测排序，不用于造样本。
# 注释里的「组首」是该输入组内最高频词（构建期从 dict.bin 查得，
# 这里仅供查阅；代码运行时以 dict.bin 实际数据为准）。
UNBIASED_PAIRS = [
    ("春节", "放假"),  # 组首=房价
    ("国庆", "长假"),  # 组首=厂家
    ("网络", "断线"),  # 组首=短线
    ("咖啡", "提神"),  # 组首=替身
    ("牛奶", "补钙"),  # 组首=不该
    ("窗户", "关上"),  # 组首=观赏
    ("车票", "退换"),  # 组首=退还
    ("歌曲", "演唱"),  # 组首=延长
    ("身体", "恢复"),  # 组首=回复
    ("眼睛", "近视"),  # 组首=仅是
    ("近视", "度数"),  # 组首=读书
    ("语文", "背诵"),  # 组首=北宋
    ("钢琴", "练习"),  # 组首=联系
    ("考试", "及格"),  # 组首=几个
    ("就业", "失业"),  # 组首=事业
    ("政策", "实施"),  # 组首=事实
    ("国家", "统一"),  # 组首=同意
    ("人民", "富裕"),  # 组首=赋予
    ("树木", "移植"),  # 组首=一直
    ("草坪", "修剪"),  # 组首=修建
    ("动物", "栖息"),  # 组首=气息
    ("鸟类", "迁徙"),  # 组首=前夕
    ("台风", "登陆"),  # 组首=登录
    ("矛盾", "激化"),  # 组首=计划
    ("病人", "就医"),  # 组首=就已
    ("婴儿", "哺乳"),  # 组首=不如
    ("春节", "拜年"),  # 组首=百年
    ("农",   "耕田"),  # 组首=更添
    ("写字", "工整"),  # 组首=公正
    ("投资", "回报"),  # 组首=汇报
    ("数学", "例题"),  # 组首=立体
    ("数学", "几何"),  # 组首=集合
    ("数学", "公式"),  # 组首=公示
    ("英语", "语法"),  # 组首=愈发
    ("历史", "事件"),  # 组首=时间
    ("地理", "纬度"),  # 组首=唯独
    ("化学", "原子"),  # 组首=源自
    ("天文", "行星"),  # 组首=星星
    ("老师", "布置"),  # 组首=不知
    ("学校", "放假"),  # 组首=房价
    ("公司", "报表"),  # 组首=爆表
    ("工作", "简历"),  # 组首=建立
    ("工作", "试用"),  # 组首=使用
    ("书房", "书架"),  # 组首=暑假
    ("宽带", "光纤"),  # 组首=光线
    ("股票", "抄底"),  # 组首=超低
    ("基金", "净值"),  # 组首=精致
    ("基金", "定投"),  # 组首=顶头
    ("保险", "保费"),  # 组首=报废
    ("病人", "输液"),  # 组首=树叶
    ("医生", "处方"),  # 组首=厨房
    ("鸡蛋", "煎蛋"),  # 组首=简单
    ("牛奶", "喝奶"),  # 组首=很爱
    ("夏天", "游泳"),  # 组首=有用
    ("冬天", "滑雪"),  # 组首=化学
    ("洪水", "救灾"),  # 组首=就在
    ("法庭", "陈词"),  # 组首=趁此
    ("旅游", "景点"),  # 组首=经典
    ("名胜", "古迹"),  # 组首=估计
    ("历史", "遗迹"),  # 组首=以及
    ("小说", "人物"),  # 组首=任务
    ("剧情", "反转"),  # 组首=翻转
    ("钢琴", "琴键"),  # 组首=勤俭
    ("象棋", "棋子"),  # 组首=妻子
    ("篮球", "投篮"),  # 组首=偷懒
    ("体操", "吊环"),  # 组首=调换
    ("小吃", "豆浆"),  # 组首=都将
    ("针灸", "穴位"),  # 组首=学位
    ("早睡", "早起"),  # 组首=早期
    ("熬夜", "伤身"),  # 组首=上身
    ("环保", "废气"),  # 组首=废弃
    ("节能", "低碳"),  # 组首=地毯
    ("减排", "低碳"),  # 组首=地毯
    ("人工", "智能"),  # 组首=只能
    ("算法", "编程"),  # 组首=变成
    ("会议", "纪要"),  # 组首=既要
    ("会议", "议程"),  # 组首=已成
    ("招投标", "标书"),  # 组首=表述
    ("投标", "标书"),  # 组首=表述
    ("快递", "驿站"),  # 组首=一战
    ("驿站", "取件"),  # 组首=区间
    ("登机", "安检"),  # 组首=按键
    ("火车", "硬座"),  # 组首=应做
    ("地铁", "换乘"),  # 组首=换成
    ("地铁", "站台"),  # 组首=展台
    ("路面", "湿滑"),  # 组首=实话
    ("谣言", "散布"),  # 组首=散步
    ("房租", "减免"),  # 组首=见面
    ("银行", "取现"),  # 组首=曲线
    ("重启", "恢复"),  # 组首=回复
    ("充电", "接口"),  # 组首=借口
    ("皮肤", "过敏"),  # 组首=国民
    ("咳嗽", "不止"),  # 组首=不知
    ("心里", "踏实"),  # 组首=他是
    ("欠税", "补缴"),  # 组首=不叫
    ("罚款", "缴款"),  # 组首=交款
    ("零钱", "找零"),  # 组首=诏令
    ("现金", "找零"),  # 组首=诏令
    ("金额", "找零"),  # 组首=诏令
    ("结算", "找零"),  # 组首=诏令
    ("航班", "延误"),  # 组首=厌恶
    ("机场", "接机"),  # 组首=阶级
    ("机场", "安检"),  # 组首=按键
    ("飞行", "耗油"),  # 组首=好友
    ("攻略", "线路"),  # 组首=显露
]


def build_unbiased(entries, syllables):
    """人工小集：期望词必须「非输入组内最高频」，与词频排名无关。

    对 UNBIASED_PAIRS 逐行断言（不通过则从最终样本中剔除并计数）：
    1. 期望词 word 在词库存在、其取频最高的 key 全部音节合法（可达）；
    2. 期望词 word **不是** 输入拼音组的最高频词（无偏的核心保证）；
    3. 上下文 ctx 在词库存在（保证上文能查、LM 有语境可借）。

    断言只用词库数据，不读 .gram、不看任何引擎输出；输出样本自带
    组内最高频词（`top`）与频比，便于审计「期望答案确实不按词频选」。
    """
    word_freq: dict[str, int] = {}
    word_keys: dict[str, list[tuple[str, int]]] = {}
    for py, word, freq in entries:
        if freq < 1:
            continue
        word_freq[word] = word_freq.get(word, 0) + freq
        word_keys.setdefault(word, []).append((py, freq))

    def best_key(w: str):
        cands = sorted(word_keys.get(w, []), key=lambda kf: kf[1], reverse=True)
        for key, _ in cands:
            if all(s in syllables for s in key.split(" ")) and key.replace(" ", "").islower():
                return key
        return None

    by_input: dict[str, list[tuple[str, int]]] = {}
    for py, word, freq in entries:
        inp = py.replace(" ", "")
        by_input.setdefault(inp, []).append((word, freq))

    out = []
    dropped = {"不可达": 0, "词是组内最高频": 0, "期望词长度非2": 0}
    for ctx, word in UNBIASED_PAIRS:
        if len(word) != 2:
            dropped["期望词长度非2"] += 1
            continue
        key = best_key(word)
        if key is None or best_key(ctx) is None:
            dropped["不可达"] += 1
            continue
        inp = key.replace(" ", "")
        group = sorted(by_input.get(inp, []), key=lambda wf: wf[1], reverse=True)
        top_word, top_freq = group[0]
        if top_word == word:
            dropped["词是组内最高频"] += 1
            continue
        out.append(
            {
                "kind": "unbiased",
                "input": inp,
                "word": word,
                "len": 2,
                "ctx": ctx,
                "freq": word_freq[word],
                "top": top_word,
                "topfreq": top_freq,
            }
        )
    return out, dropped


def main() -> None:
    dict_path = sys.argv[1] if len(sys.argv) > 1 else "data/dict.bin"
    out_path = sys.argv[2] if len(sys.argv) > 2 else "scripts/eval_samples.jsonl"
    ratio = float(sys.argv[3]) if len(sys.argv) > 3 else 10.0
    w_freq_min = int(sys.argv[4]) if len(sys.argv) > 4 else 2000
    main_min_freq = int(sys.argv[5]) if len(sys.argv) > 5 else 5000
    seed = int(sys.argv[6]) if len(sys.argv) > 6 else 42

    entries, total_freq = read_dict(dict_path)
    print(
        f"dict.bin: {len(entries)} 条，总词频 {total_freq} "
        f"(log10={__import__('math').log10(total_freq):.3f})"
    )

    seg_rs = os.path.join(
        os.path.dirname(__file__), "..", "crates", "xwx_engine", "src", "segmenter.rs"
    )
    with open(seg_rs, encoding="utf-8") as f:
        syllables = syllable_set(f.read())
    print(f"音节表: {len(syllables)} 个（从 segmenter.rs 提取）")

    all_main, dropped = build_main(entries, syllables, ratio, main_min_freq)
    print(
        f"main 候选: {len(all_main)}（歧义剔除 {dropped} 组，词频下限 {main_min_freq}）"
    )
    # 每词长桶内随机采样（固定种子，可复现）：2 字 8000 / 3 字 12000 / 4 字 12000
    import random

    rng = random.Random(seed)
    caps = {2: 8000, 3: 12000, 4: 12000}
    main_samples = []
    by_len = {}
    for L in (2, 3, 4):
        bucket = [s for s in all_main if s["len"] == L]
        rng.shuffle(bucket)
        bucket = bucket[: caps[L]]
        main_samples.extend(bucket)
        by_len[L] = len(bucket)
    print("  main 按词长（采样后）:", dict(sorted(by_len.items())))

    ctx_samples = build_ctx(entries, syllables, ratio, w_freq_min)
    dom_all = [s for s in ctx_samples if s.get("dom")]
    sens_all = [s for s in ctx_samples if not s.get("dom")]
    rng.shuffle(dom_all)
    rng.shuffle(sens_all)
    ctx_samples = dom_all[:8000] + sens_all[:8000]
    dom = sum(1 for s in ctx_samples if s.get("dom"))
    print(
        f"ctx 可造: 主导 {len(dom_all)} / 敏感 {len(sens_all)}；"
        f"采样后 {len(ctx_samples)}（主导 {dom}，敏感 {len(ctx_samples) - dom}）"
    )

    unbiased, dropped_u = build_unbiased(entries, syllables)
    print(
        f"unbiased 可选: {len(unbiased)}"
        f"（剔除: {', '.join(f'{k}={v}' for k, v in dropped_u.items())}）"
    )

    with open(out_path, "w", encoding="utf-8") as f:
        for s in main_samples + ctx_samples + unbiased:
            f.write(json.dumps(s, ensure_ascii=False) + "\n")
    print(
        f"写出 {len(main_samples) + len(ctx_samples) + len(unbiased)} 条 "
        f"→ {out_path}"
    )


if __name__ == "__main__":
    main()
