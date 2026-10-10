"""英文词库的**质量过滤**：把 wordfreq 里混进来的非单词挡在门外。

为什么需要它：wordfreq 的 large 表是从语料统计出来的，除了真词，还带一批
**碎片**——脱掉撇号的缩合（`dont`/`didnt`）、社媒拼写（`lol`/`wtf`）、
被切碎的词头（`gra`/`lun`/`bri`/`holi`/`bre`）、以及大量两三个字母的
URL/缩写残渣（`ve`/`im`/`fc`/`gg`/`aj`/`zz`）。中文模式下打的是拼音缩写，
这些碎片会和拼音撞车：`ve` 这类输入连中文候选都没有，于是它排在第一位，
按空格就把一个不是词的字符串上屏了。

规则（按顺序，命中即定）：

1. **人工清单永远保留**（`--entries` / `--overrides`）：那是最权威的来源，
   过滤规则不能推翻人的判断；
2. 命中任一**参考词表**（`--reference`，可给多个，取并集）→ 保留；
3. 1～2 个字母：只有白名单里的真词保留（两字母英文词是一个封闭的小集合）；
4. 3 个字母以上：去掉常见词尾（`s`/`es`/`ed`/`ing`/`er`/`est`/`ly`…）后
   能在参考词表里找到词干 → 保留（hunspell 那种只存词干 + 词尾标志的
   词表，`colors`/`centers`/`rumors` 都得靠这条救回来）；
5. 3 个字母以上且 `zipf_centi >= 400` → 保留。真人写出来的高频词
   （`math`/`lol`/`uh`/品牌名）宁可留着：**别把用户真会打的词删掉**；
6. 其余丢弃。

参考词表只是**输入**（就像 `--wordfreq` 那样），不进仓库：调用方给路径，
`meta.json` 记它的路径与 SHA-256，所以一次构建可复现、可追溯。
"""

#: 1～2 个字母的真英文词白名单。两字母英文词是封闭集合，人工枚举比查表可靠：
#: 参考词表里塞满了 `ll`/`dr`/`vs`/`yo` 这类缩写与语气词。
SHORT_WORDS = frozenset("""
a i am an as at be by do go he hi if in is it me my no of ok on or ox so to tv up us we ya
ah ha oh id ex vs pm ad
""".split())

#: 常见词尾（长的排在前面，先匹配长的）。
SUFFIXES = ("ings", "ing", "ers", "est", "ies", "ed", "es", "er", "ly", "s", "d")

#: 第 5 条的高频门槛（百分之一 Zipf）。
FREQUENT_CENTI = 400


def parse_reference_file(path):
    """读一份参考词表：每行 `word` / `word count` / `word/FLAGS count`。

    三种形态都要吃：SymSpell 的一元表是 `word count`，hunspell 的 `.dic`
    是 `word/FLAGS`（`github/SM`）。**必须先把 `/` 后面的词尾标志剥掉**——
    否则 `github/SM` 过不了「纯字母」检查，整份 hunspell 里带标志的词
    （也就是绝大多数）都会被当成无效行丢掉，参考表缩水一大半，
    过滤随之变得过于激进。
    """
    words = set()
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            token = parts[0].strip().lower().lstrip("\ufeff")
            w = token.split("/", 1)[0]
            if w and w.isalpha() and w.isascii():
                words.add(w)
    if not words:
        raise ValueError(f"参考词表 {path} 里没有任何有效词")
    return words


def load_references(paths):
    """把多份参考词表并成一份。"""
    ref = set()
    for p in paths or ():
        ref |= parse_reference_file(p)
    return ref


def _stem_hit(word, ref):
    for suf in SUFFIXES:
        if not word.endswith(suf) or len(word) <= len(suf) + 1:
            continue
        stem = word[: -len(suf)]
        if stem in ref or stem + "e" in ref:
            return True
        if suf == "ies" and stem + "y" in ref:
            return True
        # 双写辅音：`running`/`stopped`/`bigger` 去掉词尾得到 `runn`/`stopp`/
        # `bigger`→`bigg`，把多出来的那个字母再脱一层才是词干。词表里存的是
        # `run`/`stop`/`big`，不做这一步这三个常见形态全被判成碎片。
        if len(stem) >= 3 and stem[-1] == stem[-2] and stem[:-1] in ref:
            return True
    return False


def keep_word(word, zipf_centi, ref):
    """返回 `(是否保留, 原因)`；原因进 meta 的统计，便于复核。"""
    if not ref:
        return True, "no_reference"  # 没给参考词表 = 不做过滤（旧行为）
    if len(word) <= 2:
        return (True, "short") if word in SHORT_WORDS else (False, "short")
    if word in ref:
        return True, "reference"
    if _stem_hit(word, ref):
        return True, "stem"
    if zipf_centi >= FREQUENT_CENTI:
        return True, "frequent"
    return False, "junk"


def filter_entries(entries, ref, protected=frozenset()):
    """按规则过滤 `{key: (display, zipf_centi)}`，返回 `(保留, 统计)`。

    `protected` 是人工清单里的 key：它们无条件保留。
    """
    kept, reasons = {}, {}
    for key, value in entries.items():
        if key in protected:
            kept[key] = value
            reasons[key] = "protected"
            continue
        ok, why = keep_word(key, value[1], ref)
        if ok:
            kept[key] = value
        reasons[key] = why
    stats = {}
    for why in reasons.values():
        stats[why] = stats.get(why, 0) + 1
    return kept, stats
