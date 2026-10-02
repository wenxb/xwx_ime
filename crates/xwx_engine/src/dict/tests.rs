//! `dict` 的单测(从 `dict.rs` 拆出,2026-10-02)。测试是 `dict` 的子模块,
//! 因此照旧看得见各子模块里的 `pub(super)` 条目。

use super::*;

use std::fs;

/// 测试词库的条目组:(拼音, [(词, freq)])。
type TestGroups<'a> = Vec<(&'a [u8], Vec<(&'a str, u32)>)>;
/// 测试简拼副索引的条目组:(首字母 key, [(词, 全拼, freq)])。
type TestAbbrevGroups<'a> = Vec<(&'a [u8], Vec<(&'a str, &'a str, u32)>)>;

/// 按 dict.bin v1 布局手工构造一个迷你词库（拼音已按字节序排好）。
fn build_test_bin(version: u32) -> Vec<u8> {
    // (拼音, [(词, freq)])，顺序即拼音字节序：hao < ni < "ni hao"
    let groups: TestGroups = vec![
        (b"hao", vec![("好", 80)]),
        (b"ni", vec![("你", 300), ("妮", 200), ("尼", 20)]),
        (b"ni hao", vec![("你好", 400)]),
    ];
    let mut strings = Vec::new();
    let mut entries = Vec::new();
    let mut records = Vec::new();
    let mut total = 0u64;
    for (py, ws) in &groups {
        let str_off = strings.len() as u32;
        strings.extend_from_slice(py);
        let ent_off = entries.len() as u32;
        let ent_start = entries.len();
        for (w, f) in ws {
            entries.push(w.len() as u8);
            entries.extend_from_slice(w.as_bytes());
            entries.extend_from_slice(&f.to_le_bytes());
        }
        records.extend_from_slice(&str_off.to_le_bytes());
        records.extend_from_slice(&(py.len() as u16).to_le_bytes());
        records.extend_from_slice(&ent_off.to_le_bytes());
        records.extend_from_slice(&((entries.len() - ent_start) as u16).to_le_bytes());
        total += ws.len() as u64;
    }
    let records_off = MMAP_HEADER_LEN as u64;
    let strings_off = records_off + (groups.len() as u64) * MMAP_RECORD_LEN as u64;
    let entries_off = strings_off + strings.len() as u64;

    let mut out = Vec::new();
    out.extend_from_slice(MMAP_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(groups.len() as u64).to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&records_off.to_le_bytes());
    out.extend_from_slice(&strings_off.to_le_bytes());
    out.extend_from_slice(&entries_off.to_le_bytes());
    out.extend_from_slice(&records);
    out.extend_from_slice(&strings);
    out.extend_from_slice(&entries);
    out
}

/// 测试用临时路径（进程 id 后缀避免并发冲突）。
fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("xwx_test_dict_{name}_{}.bin", std::process::id()))
}

#[test]
fn mmap_lookup_exact() {
    let path = temp_path("lookup");
    fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写测试词库失败");
    let dict = MmapDict::open(&path).expect("打开失败");
    fs::remove_file(&path).ok();

    let ni = dict.lookup("ni").expect("查询失败");
    let words: Vec<(&str, u64)> = ni.iter().map(|c| (c.word.as_str(), c.freq)).collect();
    assert_eq!(words, vec![("你", 300), ("妮", 200), ("尼", 20)],
        "应返回全部词条且组内按 freq 降序");
    assert!(ni.iter().all(|c| c.pinyin == "ni"));

    assert!(dict.lookup("hao").expect("查询失败").len() == 1);
    assert!(dict.lookup("x").expect("查询失败").is_empty(), "不存在的拼音应返回空");
}

#[test]
fn mmap_lookup_phrase_with_space() {
    // 含空格的拼音 key（"ni hao"）在排序数组中与 "ni" 相邻，二分应能区分
    let path = temp_path("phrase");
    fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写测试词库失败");
    let dict = MmapDict::open(&path).expect("打开失败");
    fs::remove_file(&path).ok();

    let hi = dict.lookup("ni hao").expect("查询失败");
    assert_eq!(hi.len(), 1);
    assert_eq!(hi[0].word, "你好");
}

#[test]
fn mmap_lookup_prefix_range() {
    let path = temp_path("prefix");
    fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写测试词库失败");
    let dict = MmapDict::open(&path).expect("打开失败");
    fs::remove_file(&path).ok();

    let words: Vec<String> = dict
        .lookup_prefix("n")
        .expect("查询失败")
        .into_iter()
        .map(|c| c.word)
        .collect();
    assert_eq!(words, vec!["你", "妮", "尼", "你好"],
        "前缀 'n' 应覆盖 'ni' 与 'ni hao' 两个拼音，且按拼音序");
    assert!(dict.lookup_prefix("x").expect("查询失败").is_empty(), "无匹配前缀应返回空");
}

#[test]
fn mmap_rejects_corrupt_magic() {
    let path = temp_path("corrupt");
    fs::write(&path, b"this is not a dict.bin file").expect("写文件失败");
    assert!(MmapDict::open(&path).is_err(), "坏魔数应报错");
    fs::remove_file(&path).ok();
}

#[test]
fn mmap_rejects_version_mismatch() {
    let path = temp_path("version");
    fs::write(&path, build_test_bin(MMAP_VERSION + 1)).expect("写测试词库失败");
    assert!(MmapDict::open(&path).is_err(), "版本不兼容应报错");
    fs::remove_file(&path).ok();
}

#[test]
fn dict_store_routes_to_sqlite() {
    let store = DictStore::Sqlite(SqliteDict::open_in_memory().expect("内存词库失败"));
    store.upsert("你好", "ni hao", 10).expect("upsert 应成功");
    assert_eq!(store.lookup("ni hao").expect("查询失败").len(), 1);
    assert!(!store.lookup_prefix("ni").expect("查询失败").is_empty());
}

/// 按 abbrev.bin v1 布局手工构造一个迷你简拼副索引(key 已按字节序排好)。
///
/// 词条区格式:word_len u8 + word + pinyin_len u8 + pinyin + freq u32 LE。
fn build_test_abbrev_bin(version: u32) -> Vec<u8> {
    // (首字母 key, [(词, 全拼, freq)]):bjdx < zgr
    let groups: TestAbbrevGroups = vec![
        (b"bjdx", vec![("北京大学", "bei jing da xue", 211845)]),
        (b"zgr", vec![
            ("中国人", "zhong guo ren", 451625),
            ("这个人", "zhe ge ren", 167480),
        ]),
    ];
    let mut strings = Vec::new();
    let mut entries = Vec::new();
    let mut records = Vec::new();
    let mut total = 0u64;
    for (key, ws) in &groups {
        let str_off = strings.len() as u32;
        strings.extend_from_slice(key);
        let ent_off = entries.len() as u32;
        let ent_start = entries.len();
        for (w, py, f) in ws {
            entries.push(w.len() as u8);
            entries.extend_from_slice(w.as_bytes());
            entries.push(py.len() as u8);
            entries.extend_from_slice(py.as_bytes());
            entries.extend_from_slice(&f.to_le_bytes());
        }
        records.extend_from_slice(&str_off.to_le_bytes());
        records.extend_from_slice(&(key.len() as u16).to_le_bytes());
        records.extend_from_slice(&ent_off.to_le_bytes());
        records.extend_from_slice(&((entries.len() - ent_start) as u16).to_le_bytes());
        total += ws.len() as u64;
    }
    let records_off = MMAP_HEADER_LEN as u64;
    let strings_off = records_off + (groups.len() as u64) * MMAP_RECORD_LEN as u64;
    let entries_off = strings_off + strings.len() as u64;
    let mut out = Vec::new();
    out.extend_from_slice(ABBREV_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(groups.len() as u64).to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&records_off.to_le_bytes());
    out.extend_from_slice(&strings_off.to_le_bytes());
    out.extend_from_slice(&entries_off.to_le_bytes());
    out.extend_from_slice(&records);
    out.extend_from_slice(&strings);
    out.extend_from_slice(&entries);
    out
}

#[test]
fn abbrev_index_lookup_pure() {
    let path = temp_path("abbrev_pure");
    fs::write(&path, build_test_abbrev_bin(ABBREV_VERSION)).expect("写副索引失败");
    let idx = AbbrevIndex::open(&path).expect("打开副索引失败");
    fs::remove_file(&path).ok();

    let res = idx.lookup_prefix("zgr").expect("简拼查询失败");
    assert!(res.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"),
        "zgr 应返回带全拼的中国人: {res:?}");
    // 组内按 freq 降序
    assert_eq!(res[0].word, "中国人");
    let bjdx = idx.lookup_prefix("bjdx").expect("简拼查询失败");
    assert!(bjdx.iter().any(|c| c.word == "北京大学" && c.pinyin == "bei jing da xue"));
    assert!(idx.lookup_prefix("q").expect("查询失败").is_empty(), "无匹配应返回空");
}

#[test]
fn abbrev_index_rejects_bad_magic() {
    let path = temp_path("abbrev_bad");
    fs::write(&path, b"not an abbrev index").expect("写文件失败");
    assert!(AbbrevIndex::open(&path).is_err(), "坏魔数应报错");
    fs::remove_file(&path).ok();
}

#[test]
fn mmap_dict_without_abbrev_degrades_gracefully() {
    // 副索引缺失:lookup_abbrev 返回空(不 panic),精确查询不受影响。
    let path = temp_path("no_abbrev");
    fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写词库失败");
    let dict = MmapDict::open(&path).expect("打开词库失败");
    fs::remove_file(&path).ok();

    assert!(!dict.has_abbrev(), "未装配副索引时 has_abbrev 应为 false");
    assert!(dict.lookup_abbrev("zgr").expect("缺副索引不应报错").is_empty(),
        "缺副索引时简拼查询应退化为空");
    // 精确查询路径不受副索引缺失影响
    assert_eq!(dict.lookup("ni").expect("精确查询失败").len(), 3);
}

#[test]
fn mmap_dict_with_missing_abbrev_file_still_works() {
    // 装配一个不存在的副索引路径:with_abbrev 仅告警置 None,不 panic。
    let path = temp_path("dict_ok");
    fs::write(&path, build_test_bin(MMAP_VERSION)).expect("写词库失败");
    let dict = MmapDict::open(&path)
        .expect("打开词库失败")
        .with_abbrev("/nonexistent/path/abbrev.bin");
    fs::remove_file(&path).ok();

    assert!(!dict.has_abbrev(), "缺失副索引应退化为 None");
    assert!(dict.lookup_abbrev("zgr").expect("查询不应 panic").is_empty());
    assert_eq!(dict.lookup("ni hao").expect("精确查询失败")[0].word, "你好");
}

#[test]
fn real_abbrev_bin_lookup() {
    // 真实副索引(若已生成):zgr → 中国人(带全拼)、bjdx → 北京大学。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败")
        .with_abbrev(&abbrev_path);
    assert!(dict.has_abbrev(), "abbrev.bin 应被装配");

    let zgr = dict.lookup_abbrev("zgr").expect("zgr 查询失败");
    assert!(zgr.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"),
        "zgr 应含中国人(带全拼 zhong guo ren)");
    assert_eq!(zgr[0].word, "中国人", "zgr 第一名应为中国人: {:?}",
        zgr.iter().take(3).map(|c| &c.word).collect::<Vec<_>>());

    let bjdx = dict.lookup_abbrev("bjdx").expect("bjdx 查询失败");
    assert!(bjdx.iter().any(|c| c.word == "北京大学"),
        "bjdx 应含北京大学");
}

#[test]
fn real_abbrev_mixed_lookup() {
    // 混合简拼:zhongguor → 中国人(切 [zhong, guo] + 残余 r 过滤)。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败");
    let res = dict.lookup_mixed("zhongguor").expect("混合简拼查询失败");
    assert!(res.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"),
        "zhongguor 应含中国人: {:?}",
        res.iter().take(5).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>());
}

#[test]
fn real_abbrev_mixed_initial_at_any_position() {
    // 本轮新增能力:首字母可以出现在**开头**或**中间**,不再只支持末尾。
    //
    // 旧实现要求第一个音节必须是**完整音节**(`segment_input` 走不出音节
    // 就直接返回空),所以 `nhao` 整条查询召回为 0、只剩回声候选;
    // `feicde` 只能靠「非常地恩爱」这类长词偶然覆盖,目标词进不了前列。
    //
    // 失败见证:把交错分支(DFS 的 (b))删掉 → `nhao` 变回空 → 红。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败");

    // **开头**缩写:n(in) + hao。全拼必须是补全后的 `nin hao`,
    // 不能是被截断的 `n hao`(候选的 pinyin 直接取索引 key)。
    let res = dict.lookup_mixed("nhao").expect("nhao 查询失败");
    assert!(
        res.iter().any(|c| c.word == "您好" && c.pinyin == "nin hao"),
        "nhao 应含「您好」且全拼为 nin hao(不得截断成 n hao): {:?}",
        res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
    );
    assert!(
        res.iter().any(|c| c.word == "你好" && c.pinyin == "ni hao"),
        "nhao 也应含「你好」(ni + hao): {:?}",
        res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
    );

    // **中间**缩写:fei + c(hang) + de。
    let res = dict.lookup_mixed("feicde").expect("feicde 查询失败");
    assert!(
        res.iter().any(|c| c.word == "非常地" && c.pinyin == "fei chang de"),
        "feicde 应含「非常地」: {:?}",
        res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
    );
}

#[test]
fn real_abbrev_mixed_full_pinyin_uses_the_exact_path() {
    // **冻结约束**的落点:整串是合法全拼时走原实现,不得出现「只有缩略
    // 解释才可能命中」的词。
    //
    // `hao` 是合法音节,但 `h` 当缩写再补全成 `he`/`hou` 也能凑出
    // `he ao`/`hou ao`——交错 DFS 单独用会真的加进「河岙」「后澳」
    // (a/e/o 是单字母音节,`ao` 是合法音节)。分路之后它们必须消失。
    //
    // 失败见证:去掉 `is_full_pinyin` 分流 → 这两条重新出现 → 红。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败");
    let res = dict.lookup_mixed("hao").expect("hao 查询失败");
    for w in ["河岙", "后澳"] {
        assert!(
            !res.iter().any(|c| c.word == w),
            "「hao」是合法全拼,不得出现只有缩略解释才命中得上的 {w:?}: {:?}",
            res.iter().take(6).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
        );
    }
    assert!(
        res.iter().any(|c| c.word == "好"),
        "「hao」必须仍能命中「好」"
    );
}

#[test]
fn real_abbrev_mixed_requires_a_full_syllable() {
    // 纯首字母串是副索引 `lookup_abbrev` 的职责,`lookup_mixed` 对它返回空
    // ——与旧实现 `if syls.is_empty() { continue; }` 同口径。这条同时挡住
    // 「单字母输入从前缀联想里拿到几百条候选」。
    //
    // 失败见证:去掉 `used_literal` 判断 → `zgr`/`n` 会返回大量前缀词 → 红。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败");
    for input in ["zgr", "n", "bjdx"] {
        let res = dict.lookup_mixed(input).expect("查询失败");
        assert!(
            res.is_empty(),
            "纯首字母串 {input:?} 应由副索引负责,lookup_mixed 必须返回空,实测 {} 条",
            res.len()
        );
    }
}

#[test]
fn real_abbrev_mixed_pathological_input_is_bounded() {
    // 预算守卫:病态输入不得报错、不得突破 `MAX_LOOKUP_RESULTS`。
    // 耗时上界由 `examples/abbrev_bench.rs` 的预算守卫组实测(实测最坏
    // `zhzhzhzh` ≈1.4ms,门槛 5ms),这里钉的是「不 panic、不失控」。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败");
    // `zhongg` 是**命中量很大**的混合输入(实测正好顶到上限),把它和病态
    // 输入放在一起:前者证明早停真的生效,后者证明病态形状不 panic。
    // 失败见证:去掉 `collect` 里的早停 → `zhongg` 返回远超 500 条 → 红。
    for input in ["zzzz", "zhzhzhzh", "aaaaaaaa", "zhzhzhzhzhzhzhzh", "zhongg"] {
        let res = dict.lookup_mixed(input).expect("病态输入不得报错");
        assert!(
            res.len() <= MAX_LOOKUP_RESULTS as usize,
            "{input:?} 返回 {} 条,超过上限",
            res.len()
        );
    }
}

#[test]
fn real_abbrev_mixed_recalls_partial_last_syllable() {
    // **T2(2026-10-02)**:末尾「半个音节」必须能召回整词。
    //
    // 两种旧行为都覆盖不到它:
    // - `zho`(尾巴不构成音节):旧 DFS 只把首字母当缩写,试的是
    //   `zhong + o`(把尾巴末位当独立音节),中国永远不出现;
    // - `niha`/`beijin`/`meiyo`(尾巴「ha」/「jin」/「yo」碰巧是合法
    //   音节):整串被判成全拼走精确匹配,只找 key 恰好等于它的词,
    //   于是 你好(hao)/北京(jing)/没有(you)全被排除。
    //
    // 失败见证:把 `lookup_mixed` 里那段半音节补充(partial_tail_readings
    // 那一轮)注释掉 → 「你好」「北京」「没有」三条全红。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败")
        .with_abbrev(&abbrev_path);
    for (input, want) in [
        ("zho", "中"),       // 尾巴整个是半音节(zhong):只补到「中」这一位
        ("niha", "你好"),    // 尾巴 ha 是 hao 的半个
        ("beijin", "北京"),  // 尾巴 jin 是 jing 的半个
        ("meiyo", "没有"),   // 尾巴 yo 是 you 的半个
    ] {
        // 不测 `sh`→是:那个词由**精确层/副索引**提供(用户可见行为正常),
        // 而混合层要在探索上界内扫完整个 `sh*` 区间才轮得到 `shi`,
        // 属于「音节区间太大」的已知代价(见 `MAX_MIXED_COLLECT`)。
        //
        // `zho` 的期望从「中国」改成「中」:**一字一字母**(2026-10-02
        // 用户报 `yij` 出现「已经是」)之后,末尾残缺音节之后不许再接音节
        // ——`zho` 只覆盖「中」的一半,「国」一个字母都没打。四个形态里
        // 只有 `zho` 是这条(另外三个的尾巴就是整词的最后一个音节),
        // 所以它成了「半音节补全」与「不许补出没打过的音节」的分界线。
        let res = dict.lookup_mixed(input).expect("混合简拼查询失败");
        assert!(
            res.iter().any(|c| c.word == want),
            "{input} 应召回「{want}」: 前 8 = {:?}",
            res.iter().take(8).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
    // 反面:同一规则不得把「没打过的音节」带出来。
    let res = dict.lookup_mixed("zho").expect("混合简拼查询失败");
    assert!(
        !res.iter().any(|c| c.word == "中国"),
        "`zho` 不得召回「中国」(guo 一个字母都没打): {:?}",
        res.iter().take(8).map(|c| &c.word).collect::<Vec<_>>()
    );
}

#[test]
fn real_abbrev_mixed_truncates_by_frequency_not_by_exploration_order() {
    // **T1(2026-10-02)**:收集期的早停必须按词频,不能按探索顺序。
    //
    // `meiy` = mei + y,第二个音节下挂着几百个 key(每X/没X/美X…)。
    // 旧实现在收满 500 条就停,而截断发生在**排序之前**——按探索顺序
    // 先到先得,于是词频最高的「没有」(504685)被丢掉,列表第 1 名反而是
    // 词频 500354 的「每一个」。修法:收集期不做质量决策,排序后按词频
    // 截断(探索上界提到 8 倍 + 整 key 跳过控制成本)。
    //
    // 失败见证:把 `lookup_mixed` 末尾的 `out.truncate(MAX_LOOKUP_RESULTS)`
    // 去掉、改成收集期 `out.len() >= MAX_LOOKUP_RESULTS` 就返回 → 红。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败")
        .with_abbrev(&abbrev_path);
    let res = dict.lookup_mixed("meiy").expect("meiy 查询失败");
    let pos = res.iter().position(|c| c.word == "没有")
        .unwrap_or_else(|| panic!("「没有」必须被召回(旧实现把它丢了): 前 5 = {:?}",
            res.iter().take(5).map(|c| (&c.word, c.freq)).collect::<Vec<_>>()));
    assert!(pos < 3, "「没有」应在前 3(它是这串里词频最高的): 第 {} 名", pos + 1);
    // 列表本身必须是词频降序:截断按词频的前提。
    assert!(
        res.windows(2).all(|w| w[0].freq >= w[1].freq),
        "返回值应按词频降序"
    );
    // 上限不变。
    assert!(res.len() <= MAX_LOOKUP_RESULTS as usize, "仍不得超过 500 条");
}

#[test]
fn real_abbrev_early_stop_caps_results() {
    // 副索引(`lookup_abbrev`)的早停:xx 命中上万条,返回不得超过
    // MAX_LOOKUP_RESULTS(500)。副索引的每个 key 独立、块内已按 freq
    // 降序,所以这里的截断本来就是「按质量」的;混合简拼那条路原来不是,
    // 见 real_abbrev_mixed_truncates_by_frequency_not_by_exploration_order。
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败")
        .with_abbrev(&abbrev_path);
    let res = dict.lookup_abbrev("xx").expect("xx 查询失败");
    assert!(res.len() <= MAX_LOOKUP_RESULTS as usize,
        "早停应把结果限制在 500 条内,实际 {}", res.len());
    assert_eq!(res.len(), MAX_LOOKUP_RESULTS as usize,
        "xx 命中远超 500,应恰好截到 500(证明是早停而非全扫)");
}

/// **一字一字母(2026-10-02)**:副索引是按 key 前缀匹配的,`wsm` 会连带
/// 命中 key 更长的 `wsmy`(为什么要/为什么会……),候选比输入字母还多字。
/// 用户口径:候选字数不得超过输入字母数,故 `lookup_abbrev` 按**候选全拼
/// 的音节数**过滤(不用 key 长度:key 里 `zh`/`ch`/`sh` 可能两字母一个音节)。
///
/// 失败见证:把 `lookup_abbrev` 里的 `retain` 去掉 → 红(实测 `wsm` 356
/// 条里 276 条超长、`zgr` 337 条里 292 条超长)。
#[test]
fn real_abbrev_index_never_returns_more_characters_than_letters() {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return;
    }
    let dict = MmapDict::open(&dict_path).expect("词库加载失败")
        .with_abbrev(&abbrev_path);
    for input in ["wsm", "zgr", "ky", "zho", "sh", "zh", "n", "w"] {
        let res = dict.lookup_abbrev(input).expect("副索引查询失败");
        let over: Vec<_> = res.iter()
            .filter(|c| c.pinyin.split(' ').count() > input.len())
            .map(|c| (&c.word, &c.pinyin))
            .collect();
        assert!(over.is_empty(), "`{input}` 出现字数 > 字母数的候选: {over:?}");
    }
    // 界限不得误杀:`wsm` 仍要给出「为什么」(key 恰好等于输入)。
    let res = dict.lookup_abbrev("wsm").expect("副索引查询失败");
    assert!(
        res.iter().any(|c| c.word == "为什么"),
        "`wsm` 仍须给出「为什么」: {:?}",
        res.iter().take(5).map(|c| (&c.word, &c.pinyin)).collect::<Vec<_>>()
    );
}
