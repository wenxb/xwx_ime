//! gram_probe —— 验证 Rust 能否读取 Rime octagram `.gram` 语言模型（纯可行性验证）
//!
//! 子命令：
//!   gram_probe <gram> [context word]   原有行为：文件头 + 抽查 + 查询
//!   gram_probe enum [gram] [--full] [--dump file]
//!       默认：单遍全量扫描（键总数、叶节点、字节标签直方图）
//!             + 20 万次随机游走采样真实键（长度分布、分隔符检查、解码样例）
//!       --full：再从 root 做 DFS 枚举全部键（慢，默认不启用）
//!   gram_probe coverage [gram] [dict] [--top=N] [--per=N]  词对覆盖率
//!
//! 格式依据（均为 BSD-3-Clause 上游源码）：
//! - lotem/librime-octagram src/gram_db.h: Metadata 结构（44 字节）
//! - librime 仓库 src/rime/dict/mapped_file.h: OffsetPtr 的相对偏移语义
//! - librime vendored 的 darts-clone: include/darts.h（DARTS_VERSION "0.32"）:
//!   DoubleArrayUnit 位布局、traverse() 与 commonPrefixSearch()
//! - lotem/librime-octagram src/gram_db.cc: Build()/Lookup()、octagram.cc: Query()

use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;

// ---------------------------------------------------------------------------
// 文件头
// ---------------------------------------------------------------------------

/// librime-octagram src/gram_db.h:
/// struct Metadata { char format[32]; uint32_t db_checksum;
///                   uint32_t double_array_size; OffsetPtr<char> double_array; }
/// = 44 字节，无 padding。
struct Header {
    format: String,
    db_checksum: u32,
    double_array_size: u32, // 单元数（GramDb::Build: array_size = trie_->size()）
    array_file_offset: usize, // double-array 在文件中的起始字节偏移
}

impl Header {
    fn parse(buf: &[u8]) -> Result<Header, String> {
        if buf.len() < 44 {
            return Err("文件头不足 44 字节".into());
        }
        let zero_pos = buf[..32].iter().position(|&b| b == 0).unwrap_or(32);
        let format = String::from_utf8_lossy(&buf[..zero_pos]).into_owned();
        let db_checksum = u32::from_le_bytes(buf[32..36].try_into().unwrap());
        let double_array_size = u32::from_le_bytes(buf[36..40].try_into().unwrap());
        // OffsetPtr<char> 是 int32，语义（librime mapped_file.h）：
        //   T* get() const { return (char*)&offset_ + offset_; }
        // 即“相对该偏移字段自身地址的符号偏移”。字段位于文件偏移 40。
        let ptr = i32::from_le_bytes(buf[40..44].try_into().unwrap());
        let array_file_offset = 40i64 + ptr as i64;
        if array_file_offset < 0 || array_file_offset as usize > buf.len() {
            return Err(format!("OffsetPtr 值异常: {ptr}"));
        }
        Ok(Header {
            format,
            db_checksum,
            double_array_size,
            array_file_offset: array_file_offset as usize,
        })
    }
}

// ---------------------------------------------------------------------------
// gram_encoding.cc 的 encode()：逐 Unicode 码点编码（非 UTF-8 原样）
// ---------------------------------------------------------------------------
fn encode_grammar(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in s.chars() {
        encode_cp(ch as u32, &mut out);
    }
    out
}

/// 单码点编码；语义与 C++ uint32 完全一致（含回绕）。
fn encode_cp(mut u: u32, out: &mut Vec<u8>) {
    if u < 0x80 {
        out.push(if u == 0 { 0xE0 } else { u as u8 });
    } else if (0x4000..0xA000).contains(&u) {
        if (u & 0xFF) == 0 {
            out.push(0xE1);
            out.push(((u >> 8) + 0x40) as u8);
        } else {
            out.push(((u >> 8) + 0x40) as u8);
            out.push((u & 0xFF) as u8);
        }
    } else {
        let mut bits = 32;
        while bits > 0 && (u & 0xFE00_0000) == 0 {
            bits -= 7;
            u = u.wrapping_shl(7);
        }
        let mut bytes_to_encode = (bits + 6) / 7;
        out.push(0xE0 | bytes_to_encode as u8);
        while bytes_to_encode > 0 {
            bytes_to_encode -= 1;
            out.push((((u >> 25) & 0x7F) | 0x80) as u8);
            u = u.wrapping_shl(7);
        }
    }
}

// ---------------------------------------------------------------------------
// double-array 访问（darts-clone 0.32：单元 4 字节）
// ---------------------------------------------------------------------------
struct DoubleArray<'a> {
    data: &'a [u8],
    num_units: usize, // 单元个数
}

impl<'a> DoubleArray<'a> {
    fn new(mmap: &'a Mmap, header: &Header) -> Result<DoubleArray<'a>, String> {
        let num_units = header.double_array_size as usize;
        let bytes = num_units
            .checked_mul(4)
            .ok_or("double_array_size 溢出")?;
        if header.array_file_offset + bytes > mmap.len() {
            return Err(format!(
                "数组越界: offset {} + {} > 文件长度 {}",
                header.array_file_offset,
                bytes,
                mmap.len()
            ));
        }
        Ok(DoubleArray {
            data: &mmap[header.array_file_offset..header.array_file_offset + bytes],
            num_units,
        })
    }

    #[inline]
    fn unit(&self, idx: usize) -> Option<u32> {
        if idx >= self.num_units {
            return None;
        }
        let off = idx * 4;
        let b = self.data.get(off..off + 4)?;
        Some(u32::from_le_bytes(b.try_into().unwrap()))
    }
}

// darts-clone 0.32 DoubleArrayUnit 位布局（librime include/darts.h）
#[inline]
fn u_label(u: u32) -> u32 {
    u & 0x8000_00FF
}
#[inline]
fn u_offset(u: u32) -> usize {
    ((u >> 10) << ((u & 0x200) >> 6)) as usize
}
#[inline]
fn u_has_leaf(u: u32) -> bool {
    ((u >> 8) & 1) == 1
}
#[inline]
fn u_value(u: u32) -> u32 {
    u & 0x7FFF_FFFF
}

impl<'a> DoubleArray<'a> {
    /// 从 node_pos 沿 key 前进；返回 (成功?, 最终 node_pos, 已匹配字节数)
    fn traverse(&self, key: &[u8], start_node: usize) -> (bool, usize, usize) {
        let mut node = start_node;
        let mut matched = 0usize;
        for &b in key {
            let u = match self.unit(node) {
                Some(u) => u,
                None => return (false, node, matched),
            };
            node ^= u_offset(u) ^ b as usize;
            match self.unit(node) {
                Some(nu) if u_label(nu) as u8 == b => {
                    matched += 1;
                }
                _ => return (false, node, matched),
            }
        }
        (true, node, matched)
    }

    /// commonPrefixSearch：从 node_pos 出发在 key 上做前缀搜索。
    fn common_prefix_search(
        &self,
        key: &[u8],
        node_pos: usize,
        max_results: usize,
    ) -> Vec<(i32, usize)> {
        let mut res = Vec::new();
        let mut node = node_pos;
        let unit0 = match self.unit(node) {
            Some(u) => u,
            None => return res,
        };
        node ^= u_offset(unit0);
        let mut pos = 0usize;
        for &b in key {
            pos += 1;
            node ^= b as usize;
            let unit = match self.unit(node) {
                Some(u) => u,
                None => break,
            };
            if u_label(unit) as u8 != b {
                break;
            }
            let has_leaf = u_has_leaf(unit);
            node ^= u_offset(unit);
            if has_leaf {
                if let Some(leaf_unit) = self.unit(node) {
                    let v = u_value(leaf_unit) as i32;
                    res.push((v, pos));
                    if res.len() >= max_results {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        res
    }
}

// 解码匹配字节数 → unicode 字符数（gram_encoding.cc advance() 规则）
fn unicode_length(encoded: &[u8], byte_len: usize) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < byte_len {
        let b = encoded[i];
        let adv = if (b & 0x80) == 0 {
            1
        } else if (b & 0xF0) == 0xE0 {
            (b & 0x0F) as usize + 1
        } else {
            2
        };
        i += adv;
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// 查询（对齐 librime-octagram src/gram_db.cc 的 Lookup：
//   traverse(context) 若 key_pos == context.length() 则 commonPrefixSearch(word)）
// ---------------------------------------------------------------------------
#[allow(dead_code)]
fn lookup(
    da: &DoubleArray,
    context: &str,
    word: &str,
) -> Option<Vec<(i32, usize)>> {
    let ctx = encode_grammar(context);
    let wd = encode_grammar(word);
    let (ok, node, matched) = da.traverse(&ctx, 0);
    if !ok || matched != ctx.len() {
        return None;
    }
    Some(da.common_prefix_search(&wd, node, 8))
}

/// 对齐 octagram.cc Query() 的滑动窗口语义：
/// - context 取末尾 n 字（n = min(8, collocation_max_length-1)，本文档最大长度 6 → 5）
/// - word 取开头 n 字
/// - context 后缀从长到短逐级 Lookup（k 从 n 递减到 1）
/// 返回 (是否命中, 是否「整段 context 精确命中」, 最大 log 值)
fn query_like_octagram(
    da: &DoubleArray,
    context: &str,
    word: &str,
) -> (bool, bool, f64) {
    let n = 5usize;
    let ctx: Vec<char> = context.chars().collect();
    let wd: Vec<char> = word.chars().collect();
    let ctx_n = ctx.len().min(n);
    let wd_n = wd.len().min(n);
    let wd_str: String = wd[..wd_n].iter().collect();
    let wd_enc = encode_grammar(&wd_str);
    let mut best = -1e18f64;
    let mut any = false;
    let mut exact = false;
    for k in (1..=ctx_n).rev() {
        let sub: String = ctx[ctx.len() - k..].iter().collect();
        let ctx_enc = encode_grammar(&sub);
        let (ok, node, matched) = da.traverse(&ctx_enc, 0);
        if !ok || matched != ctx_enc.len() {
            continue;
        }
        let res = da.common_prefix_search(&wd_enc, node, 8);
        if !res.is_empty() {
            any = true;
            if k == ctx.len() {
                exact = true;
            }
            for (v, _) in res {
                best = best.max(v as f64 / 10000.0);
            }
        }
    }
    (any, exact, best)
}

// ---------------------------------------------------------------------------
// 键枚举：从根 DFS，还原全部键（编码字节）
// ---------------------------------------------------------------------------
fn enumerate_keys(
    da: &DoubleArray,
    mut visit: impl FnMut(&[u8], i32) -> bool,
) {
    /// DFS 统计：被剪枝的伪路径数（double-array 中偶发 label 误匹配/环）
    struct Ctx {
        pruned: u64,
        capped: u64,
    }
    fn walk(
        da: &DoubleArray,
        pos: usize,
        path: &mut Vec<u8>,
        path_pos: &mut Vec<usize>,
        depth: usize,
        cull: &mut Ctx,
        visit: &mut impl FnMut(&[u8], i32) -> bool,
    ) -> bool {
        let Some(u) = da.unit(pos) else {
            return true;
        };
        let base = pos ^ u_offset(u);
        if u_has_leaf(u) {
            if let Some(leaf) = da.unit(base) {
                if !visit(path, u_value(leaf) as i32) {
                    return false;
                }
            }
        }
        for b in 1u16..=255u16 {
            let cpos = base ^ b as usize;
            if path_pos.contains(&cpos) {
                cull.pruned += 1; // 位置环：同一路径上重复访问 → 伪节点
                continue;
            }
            let Some(nu) = da.unit(cpos) else {
                continue;
            };
            if u_label(nu) as u8 == b as u8 {
                if depth >= 48 {
                    cull.capped += 1; // 超过合理键长（≤32B），剪枝
                    continue;
                }
                path.push(b as u8);
                path_pos.push(cpos);
                let ok = walk(da, cpos, path, path_pos, depth + 1, cull, visit);
                path_pos.pop();
                path.pop();
                if !ok {
                    return false;
                }
            }
        }
        true
    }
    let mut cull = Ctx { pruned: 0, capped: 0 };
    let mut path = Vec::with_capacity(24);
    let mut path_pos = Vec::with_capacity(24);
    walk(da, 0, &mut path, &mut path_pos, 0, &mut cull, &mut visit);
    if cull.pruned > 0 || cull.capped > 0 {
        eprintln!(
            "[enum] 剪枝统计: 疑似伪节点环 {} 个, 超深 {} 个（不影响真键）",
            cull.pruned, cull.capped
        );
    }
}

// ---------------------------------------------------------------------------
// 键解码：编码在 0x4000..0xA000 之外非双射，用「码点 → 编码」反查表。
// ---------------------------------------------------------------------------
fn build_decode_map() -> HashMap<Vec<u8>, char> {
    let mut map: HashMap<Vec<u8>, char> = HashMap::new();
    for (lo, hi) in [
        (0x0000u32, 0xD7FFu32),
        (0xE000u32, 0x2FFFFu32),
        (0x30000u32, 0x3134Au32),
    ] {
        for cp in lo..=hi {
            if (0xD800..0xE000).contains(&cp) {
                continue;
            }
            let mut enc = Vec::with_capacity(4);
            encode_cp(cp, &mut enc);
            map.entry(enc).or_insert_with(|| char::from_u32(cp).unwrap_or('�'));
        }
    }
    map
}

/// 把键的编码字节解码成显示文本；未知字节按 advance() 步长输出 <hex>。
fn decode_key_display(encoded: &[u8], map: &HashMap<Vec<u8>, char>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < encoded.len() {
        let b = encoded[i];
        let adv = if (b & 0x80) == 0 {
            1
        } else if (b & 0xF0) == 0xE0 {
            (b & 0x0F) as usize + 1
        } else {
            2
        };
        let end = (i + adv).min(encoded.len());
        match map.get(&encoded[i..end]) {
            Some(c) => s.push(*c),
            None => s.push_str(&format!("<{:02x}>", b)),
        }
        i = end;
    }
    s
}

// ---------------------------------------------------------------------------
// 子命令：enum —— 单遍全量扫描 + 随机游走采样（快），--full 做 DFS（慢）
// ---------------------------------------------------------------------------
fn run_enum(args: &mut impl Iterator<Item = String>) {
    let default_gram = "/home/youngwen/.local/share/xwx/wanxiang-lts-zh-hans.gram";
    let mut gram = default_gram.to_string();
    let mut full = false;
    while let Some(a) = args.next() {
        if a == "--full" {
            full = true;
        } else if a.starts_with('-') {
            // ignore
        } else if gram == default_gram {
            gram = a;
        }
    }
    let f = File::open(&gram).expect("无法打开 .gram 文件");
    let mmap = unsafe { Mmap::map(&f) }.expect("mmap 失败");
    let header = Header::parse(&mmap[..44]).expect("文件头解析失败");
    let da = DoubleArray::new(&mmap, &header).expect("double-array 初始化失败");
    println!(
        "═══ 键枚举 ═══ format={} array_size={} 单元（{}B）",
        header.format, header.double_array_size, header.double_array_size as usize * 4
    );

    let t0 = std::time::Instant::now();
    let map = build_decode_map();
    println!("解码反查表构建：{} ms（{} 个码点）", t0.elapsed().as_millis(), map.len());

    // ---------- Pass A：单遍全量扫描（无递归）----------
    let ta = std::time::Instant::now();
    let mut n_units = 0u64;
    let mut n_leaf = 0u64; // MSB=1：darts.h 注明 leaf unit 的 label() MSB 恒为 1
    let mut n_has_leaf = 0u64; // 父边上的 has_leaf 标志数
    let mut label_hist = [0u64; 256]; // 非叶单元的 label 字节分布 = 键中出现的全部字节
    for i in 0..da.num_units {
        n_units += 1;
        let u = da.unit(i).unwrap_or(0);
        if u & 0x8000_0000 != 0 {
            n_leaf += 1;
        }
        if u_has_leaf(u) {
            n_has_leaf += 1;
        }
        if u & 0x8000_0000 == 0 {
            label_hist[(u & 0xFF) as usize] += 1;
        }
    }
    println!("\n═══ Pass A：全量单遍扫描（{} ms）═══", ta.elapsed().as_millis());
    println!("  单元总数        : {n_units}");
    println!("  叶单元(MSB=1)   : {n_leaf} ← 全部键数（每个键对应一个叶）");
    println!("  has_leaf 边数   : {n_has_leaf}（交叉验证，应与键数一致）");
    println!("  键/单元 密度     : {:.3}", n_leaf as f64 / n_units as f64);
    let space_edges = label_hist[0x20];
    let dollar_edges = label_hist[0x24];
    let mut low_ctl = 0u64;
    for &b in label_hist[..0x20].iter() {
        low_ctl += b;
    }
    println!("  键中出现 0x20（空格）的边数 : {space_edges}  → {}",
        if space_edges == 0 { "任何键中都不含空格 ✓" } else { "存在空格！（词级特征）" });
    println!("  键中出现 0x24（$）的边数   : {dollar_edges}");
    println!("  其它 <0x20 控制字节边数合计: {low_ctl}");
    let mut top: Vec<(usize, u64)> = label_hist.iter().enumerate()
        .map(|(b, &v)| (b, v)).filter(|&(_, v)| v > 0).collect();
    top.sort_by_key(|&(_, v)| std::cmp::Reverse(v));
    println!("  键中出现的字节 top12（值=出现边数）: {:?}",
        top.iter().take(12).map(|(b, v)| {
            let ch = if *b >= 0x20 && *b <= 0x7E { format!("'{}'", *b as u8 as char) } else { format!("0x{b:02x}") };
            format!("{ch}×{v}")
        }).collect::<Vec<_>>());

    // ---------- Pass B：随机游走采样 ----------
    let mut rng: u64 = 0x9E3779B97F4A7C15;
    let mut next_rng = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    const WALKS: u64 = 500_000;
    let mut len_chars = [0u64; 33];
    let mut len_bytes = [0u64; 64];
    let mut walk_bytes = [0u64; 256];
    let mut leaf_seen = 0u64;
    let mut sample_keys: Vec<(Vec<u8>, i32)> = Vec::with_capacity(4096);
    let mut walk = Vec::with_capacity(16);
    let tb = std::time::Instant::now();
    for _ in 0..WALKS {
        walk.clear();
        let mut node = 0usize;
        let mut leaves: Vec<(i32, usize)> = Vec::new(); // (value, byte_len)
        for _step in 0..12 {
            let Some(u) = da.unit(node) else { break };
            let base = node ^ u_offset(u);
            if u_has_leaf(u) {
                if let Some(leaf) = da.unit(base) {
                    leaves.push((u_value(leaf) as i32, walk.len()));
                }
            }
            let mut children: Vec<(u8, usize)> = Vec::with_capacity(16);
            for b in 1u16..=255u16 {
                let cpos = base ^ b as usize;
                if let Some(nu) = da.unit(cpos) {
                    if u_label(nu) as u8 == b as u8 {
                        children.push((b as u8, cpos));
                    }
                }
            }
            if children.is_empty() {
                break;
            }
            let pick = (next_rng() as usize) % children.len();
            let (b, cpos) = children[pick];
            walk.push(b);
            node = cpos;
        }
        // 取本游走抵达最深叶子作为采样键（长键略偏重，但结构足够）
        if let Some(&(v, blen)) = leaves.last() {
            let key = walk[..blen].to_vec();
            let chars = unicode_length(&key, key.len());
            if chars < 33 {
                len_chars[chars] += 1;
            }
            if key.len() < 64 {
                len_bytes[key.len()] += 1;
            }
            for &b in &key {
                walk_bytes[b as usize] += 1;
            }
            leaf_seen += 1;
            if sample_keys.len() < 4096 {
                sample_keys.push((key, v));
            }
        }
    }
    println!("\n═══ Pass B：{WALKS} 次随机游走（{} ms，采到叶键 {leaf_seen} 条）═══", tb.elapsed().as_millis());
    println!("  采样键字符长度分布:");
    for i in 1..33 {
        if len_chars[i] > 0 {
            println!("    {i:2} 字: {:<10} 占采样 {:.2}%", len_chars[i], len_chars[i] as f64 * 100.0 / leaf_seen as f64);
        }
    }
    println!("  采样键中出现的字节（0x20 空格={}, 0x24 $={}）", walk_bytes[0x20], walk_bytes[0x24]);

    println!("\n【样例键】按字符长度分组（每组随机取至多 12 条）：");
    for l in 1..=12 {
        let grp: Vec<&(Vec<u8>, i32)> = sample_keys.iter()
            .filter(|(k, _)| unicode_length(k, k.len()) == l).collect();
        if grp.is_empty() { continue; }
        let show: Vec<String> = grp.iter().take(12)
            .map(|(k, v)| format!("{}×{}", decode_key_display(k, &map), v)).collect();
        println!("  {l}字 (采样 {} 条): {}", grp.len(), show.join("  "));
    }

    let dollar_keys: Vec<&(Vec<u8>, i32)> = sample_keys.iter()
        .filter(|(k, _)| k.last() == Some(&0x24)).collect();
    println!("\n【$ 尾哨兵键】采样 {} 条，样例: {}",
        dollar_keys.len(),
        dollar_keys.iter().take(8)
            .map(|(k, v)| format!("{}×{}", decode_key_display(k, &map), v)).collect::<Vec<_>>().join("  "));
    let space_keys: Vec<&(Vec<u8>, i32)> = sample_keys.iter()
        .filter(|(k, _)| k.contains(&0x20)).collect();
    println!("【含空格键】采样中出现 {} 条{}", space_keys.len(),
        if space_keys.is_empty() { "（无）" } else { "" });
    if !space_keys.is_empty() {
        println!("  前 10 条（空格显示为 ␣，另附 16 进制）:");
        for (k, v) in space_keys.iter().take(10) {
            let shown = decode_key_display(k, &map).replace(' ', "␣");
            let hex = k.iter().map(|b| format!("{b:02x}")).collect::<String>();
            println!("    {shown}  hex={hex} value={v}");
        }
    }
    let ctrl_keys: Vec<&(Vec<u8>, i32)> = sample_keys.iter()
        .filter(|(k, _)| k.iter().any(|&b| b < 0x20)).collect();
    println!("【含 <0x20 控制字节键】采样 {} 条，前 8 条:", ctrl_keys.len());
    for (k, v) in ctrl_keys.iter().take(8) {
        let shown = decode_key_display(k, &map).chars()
            .map(|c| if (c as u32) < 0x20 { '�' } else { c }).collect::<String>();
        let hex = k.iter().map(|b| format!("{b:02x}")).collect::<String>();
        println!("    {shown}  hex={hex} value={v}");
    }
    // $ 尾键按字符长度分桶
    let mut dol_by_len = [0u64; 33];
    for (k, _) in sample_keys.iter() {
        if k.last() == Some(&0x24) {
            let c = unicode_length(k, k.len());
            if c < 33 {
                dol_by_len[c] += 1;
            }
        }
    }
    println!("【$ 尾键】按字符长度: {:?}",
        dol_by_len.iter().enumerate().filter(|(_, &v)| v > 0)
            .map(|(i, v)| format!("{i}字×{v}")).collect::<Vec<_>>());

    // ---------- 跨词边界判别 ----------
    println!("\n【跨词边界判别】查指定键是否存在:");
    let probes = [
        ("今天天气", "今天+天气（2字×2字 词对）"),
        ("天天气", "1字上下文“天”+“天气”（跨 今天|天气 边界）"),
        ("天天气很", "跨 今天|天气|很 的 4 字窗"),
        ("天气很好", "天气+很好（2×2）"),
        ("气很好", "1字上下文“气”+“很好”"),
        ("今天气", "今天+气（2×1）"),
        ("今天天气很好", "6 字整句窗"),
        ("天气很", "天气+很"),
        ("天气$", "word + $ 尾哨兵（octagram rear 用键）"),
        ("天$", "1字词 + $"),
    ];
    for (s, note) in probes {
        println!("  “{s}”（{note}）: {}", if exist_key(&da, s) { "存在 ✓" } else { "不存在" });
    }

    // ---------- --full：DFS 全量枚举（慢）----------
    if full {
        println!("\n═══ --full DFS 全量枚举 ═══（慢，等待…）");
        let td = std::time::Instant::now();
        let mut df_total = 0u64;
        let mut df_chars = [0u64; 33];
        enumerate_keys(&da, |key, _| {
            df_total += 1;
            let c = unicode_length(key, key.len());
            if c < 33 {
                df_chars[c] += 1;
            }
            true
        });
        println!("DFS 枚举键数: {df_total}（与 Pass A 叶单元数 {n_leaf} 比对）");
        for i in 1..33 {
            if df_chars[i] > 0 {
                println!("  {}字: {}", i, df_chars[i]);
            }
        }
        let dfd = td.elapsed();
        println!("DFS 用时 {dfd:?}");
    }

    println!("\n═══ 汇总 ═══");
    println!("  键总数（叶单元精确计数）: {n_leaf}");
    println!("  空格字节: 全量边直方图 {space_edges} 处、随机采样含空格键 {} 条", space_keys.len());
    println!("  $ 哨兵: 采样键中 {} 条（全量直方图 $ 边 {dollar_edges} 处）", dollar_keys.len());
}

fn exist_key(da: &DoubleArray, s: &str) -> bool {
    let enc = encode_grammar(s);
    let (ok, node, matched) = da.traverse(&enc, 0);
    if !ok || matched != enc.len() {
        return false;
    }
    da.unit(node).map(|u| u_has_leaf(u)).unwrap_or(false)
}

// 子命令：coverage —— 用 dict.bin 真实词表测词对覆盖率
// ---------------------------------------------------------------------------
fn run_coverage(args: &mut impl Iterator<Item = String>) {
    let default_gram = "/home/youngwen/.local/share/xwx/wanxiang-lts-zh-hans.gram";
    let default_dict = "/home/youngwen/.local/share/xwx/dict.bin";
    let mut gram = default_gram.to_string();
    let mut dict = default_dict.to_string();
    let mut top_n = 4000usize;
    let mut per_group = 250usize;
    while let Some(a) = args.next() {
        if let Some(v) = a.strip_prefix("--top=") {
            top_n = v.parse().unwrap_or(4000);
        } else if let Some(v) = a.strip_prefix("--per=") {
            per_group = v.parse().unwrap_or(250);
        } else if a.starts_with('-') {
            // ignore
        } else if gram == default_gram {
            gram = a;
        } else if dict == default_dict {
            dict = a;
        }
    }

    let f = File::open(&gram).expect("无法打开 .gram 文件");
    let mmap = unsafe { Mmap::map(&f) }.expect("mmap 失败");
    let header = Header::parse(&mmap[..44]).expect("文件头解析失败");
    let da = DoubleArray::new(&mmap, &header).expect("double-array 初始化失败");

    let d = std::fs::read(&dict).expect("无法读取 dict.bin");
    let words = parse_dict(&d);
    println!("═══ coverage ═══ dict.bin: {} 词条，取前 {} 高频词", words.len(), top_n);

    let mut by_len: [Vec<(String, u32)>; 5] = Default::default();
    for (w, f) in words.into_iter().take(top_n) {
        let n = w.chars().count();
        let idx = if n == 0 { 0 } else { (n - 1).min(4) };
        by_len[idx].push((w, f));
    }
    let labels = ["1字", "2字", "3字", "4字", "4字+"];
    for (i, g) in by_len.iter().enumerate() {
        println!("  词长 {} 组: {} 词（前3: {}）", labels[i], g.len(),
            g.iter().take(3).map(|(w, _)| w.clone()).collect::<Vec<_>>().join("、"));
    }

    let t0 = std::time::Instant::now();
    let mut total = (0u64, 0u64, 0u64);
    for a in 0..5 {
        for b in 0..5 {
            let pa = &by_len[a];
            let pb = &by_len[b];
            if pa.is_empty() || pb.is_empty() {
                continue;
            }
            let pool_a: Vec<&String> = pa.iter().map(|(w, _)| w).collect();
            let pool_b: Vec<&String> = pb.iter().map(|(w, _)| w).collect();
            let n = pa.len().min(pb.len()).min(per_group);
            let mut hit = 0u64;
            let mut exact = 0u64;
            for i in 0..n {
                let wa = pool_a[(i * 7 + 1) % pool_a.len()];
                let wb = pool_b[(i * 13 + 5) % pool_b.len()];
                let (any, is_exact, _) = query_like_octagram(&da, wa, wb);
                if any {
                    hit += 1;
                }
                if is_exact {
                    exact += 1;
                }
            }
            total.0 += n as u64;
            total.1 += hit;
            total.2 += exact;
            let pct = |x: u64| format!("{:.1}%", x as f64 * 100.0 / n as f64);
            let p_hit = pct(hit);
            let p_exact = pct(exact);
            println!(
                "  上下文{la} × 候选{lb}: {hit}/{n} 命中 ({p_hit}), 全上下文精确 {exact}/{n} ({p_exact})",
                la = labels[a], lb = labels[b]
            );
        }
    }

    // 真实场景对照：高频上文（不限词长）× 高频候选
    let all_words: Vec<(String, u32)> = {
        let mut v: Vec<(String, u32)> = Vec::new();
        for g in by_len.iter() {
            v.extend(g.iter().cloned());
        }
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    };
    let ctxs: Vec<&String> = all_words.iter().take(400).map(|(w, _)| w).collect();
    let cands: Vec<&String> = all_words.iter().take(600).map(|(w, _)| w).collect();
    let mut rhit = 0u64;
    let mut rexact = 0u64;
    let rtotal = (ctxs.len() * cands.len()) as u64;
    for c in &ctxs {
        for wd in &cands {
            let (any, ex, _) = query_like_octagram(&da, c, wd);
            if any {
                rhit += 1;
            }
            if ex {
                rexact += 1;
            }
        }
    }
    println!("\n【真实场景对照】高频上文 {} × 高频候选 {} = {} 组", ctxs.len(), cands.len(), rtotal);
    println!("  任意窗口命中: {rhit}/{rtotal} = {:.2}%", rhit as f64 * 100.0 / rtotal as f64);
    println!("  整词精确命中: {rexact}/{rtotal} = {:.2}%", rexact as f64 * 100.0 / rtotal as f64);
    let dt = t0.elapsed();
    println!("\n═══ 汇总 ═══");
    println!("  共测 {} 组（上下文×候选）词对", total.0);
    println!("  任意窗口命中（模型能给出上下文分）: {}/{} = {:.2}%", total.1, total.0, total.1 as f64 * 100.0 / total.0 as f64);
    println!("  全上下文精确命中（整词匹配）: {}/{} = {:.2}%", total.2, total.0, total.2 as f64 * 100.0 / total.0 as f64);
    println!("  用时 {:?}", dt);
}

// 解析 dict.bin（小端 XWXIDM1 格式）
fn parse_dict(d: &[u8]) -> Vec<(String, u32)> {
    assert_eq!(&d[..8], b"XWXIDM1\0", "dict.bin magic 不符");
    let n_p = u64::from_le_bytes(d[12..20].try_into().unwrap()) as usize;
    let total = u64::from_le_bytes(d[20..28].try_into().unwrap()) as usize;
    let py_off = u64::from_le_bytes(d[28..36].try_into().unwrap()) as usize;
    let word_off = u64::from_le_bytes(d[44..52].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(total);
    for i in 0..n_p {
        let o = py_off + i * 12;
        let blk_o = u32::from_le_bytes(d[o + 6..o + 10].try_into().unwrap()) as usize;
        let blk_l = u16::from_le_bytes(d[o + 10..o + 12].try_into().unwrap()) as usize;
        let start = word_off + blk_o;
        let end = start + blk_l;
        let mut pos = start;
        while pos + 5 <= end {
            let wl = d[pos] as usize;
            pos += 1;
            let w = std::str::from_utf8(&d[pos..pos + wl])
                .unwrap_or("")
                .to_string();
            pos += wl;
            let freq = u32::from_le_bytes(d[pos..pos + 4].try_into().unwrap());
            pos += 4;
            out.push((w, freq));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

// ---------------------------------------------------------------------------
// 原有行为（默认无子命令）
// ---------------------------------------------------------------------------
fn run_default(mut args: impl Iterator<Item = String>) {
    let file_path = args
        .next()
        .unwrap_or_else(|| "/home/youngwen/.local/share/xwx/wanxiang-lts-zh-hans.gram".to_string());
    let custom_context = args.next();
    let custom_word = args.next();

    let f = File::open(&file_path).expect("无法打开 .gram 文件");
    let mmap = unsafe { Mmap::map(&f) }.expect("mmap 失败");

    let header = Header::parse(&mmap[..44]).expect("文件头解析失败");
    let da = DoubleArray::new(&mmap, &header).expect("double-array 初始化失败");

    // ---------- 0) 自定义查询（可选） ----------
    if let (Some(ctx), Some(wd)) = (custom_context, custom_word) {
        println!("═══ 0) 自定义查询 ═══");
        match lookup(&da, &ctx, &wd) {
            None => println!("{ctx} + {wd} : context 不在词典中"),
            Some(res) if res.is_empty() => println!("{ctx} + {wd} : 无匹配"),
            Some(res) => {
                for (v, blen) in res {
                    let nchars = unicode_length(&encode_grammar(&wd), blen);
                    let exp = (v as f64 / 10000.0).exp();
                    println!("{ctx} + {wd} : 匹配{nchars}字 value={v} exp≈{exp:.3}");
                }
            }
        }
        return;
    }

    // ---------- 0) 性能粗测 ----------
    let t0 = std::time::Instant::now();
    let n = 100_000u32;
    for _ in 0..n {
        let _ = lookup(&da, "今天", "天气");
    }
    let dt = t0.elapsed();
    println!(
        "═══ 0) 性能粗测 ═══\n{n} 次 lookup(\"今天\",\"天气\") 共 {dt:?}（约 {:.0} 次/秒）\n",
        n as f64 / dt.as_secs_f64()
    );

    // ---------- 1) 文件头 ----------
    println!("═══ 1) 文件头 ═══");
    println!("format            : {}", header.format);
    println!("db_checksum       : {}", header.db_checksum);
    println!("double_array_size : {} 单元 ({}B)", header.double_array_size,
             header.double_array_size as usize * 4);
    println!("数组在文件偏移    : {}", header.array_file_offset);
    let expect = 44usize + header.double_array_size as usize * 4;
    println!("44 + 单元数×4     : {} B  → {}",
             expect,
             if expect == mmap.len() { "与文件大小一致 ✓" } else { "不一致 ✗" });
    let tail = mmap.len() - expect;
    println!("文件尾部剩余字节  : {}", tail);

    // ---- 2. 单元位布局抽查 ----
    println!("\n══ 2) double-array 抽查 ═══");
    for i in [0usize, 1, 2] {
        let u = da.unit(i).unwrap();
        println!("unit[{i}] = 0x{u:08x} | label=0x{:02x} offset={} has_leaf={}",
                 u_label(u), u_offset(u), u_has_leaf(u));
    }

    // ---- 3. 搭配查询 ----
    println!("\n══ 3) 搭配查询（value 为 int32(log(freq)×10000)，exp 为还原频率）═══");
    let pairs = [
        ("今天", "天气"),
        ("我们", "一起"),
        ("非常", "开心"),
        ("外卖", "好吃"),
        ("天气", "不错"),
        ("问题", "严重"),
        ("人民", "群众"),
        ("生活", "幸福"),
    ];
    for (ctx, wd) in pairs {
        match lookup(&da, ctx, wd) {
            None => println!("{ctx} + {wd} : context 不在词典中"),
            Some(res) if res.is_empty() => println!("{ctx} + {wd} : 无匹配"),
            Some(res) => {
                let mut s = String::new();
                for (v, blen) in res {
                    let nchars = unicode_length(&encode_grammar(wd), blen);
                    let exp = (v as f64 / 10000.0).exp();
                    s.push_str(&format!(
                        "  [匹配{nchars}字 v={v} exp≈{exp:.3}]"
                    ));
                }
                println!("{ctx} + {wd} :{s}");
            }
        }
    }

    // ---- 4. 对照组（不应存在/显著更低） ----
    println!("\n══ 4) 对照组 ═══");
    let bad = [
        ("龘魈", "鬻"),
        ("吔", "哞"),
        ("焞", "燚"),
        ("甾体", "亥伯龙"),
        ("今天", "氧化锌"),
        ("的", "龘"),
    ];
    for (ctx, wd) in bad {
        match lookup(&da, ctx, wd) {
            None => println!("{ctx} + {wd} : context 不存在"),
            Some(res) if res.is_empty() => println!("{ctx} + {wd} : 无匹配"),
            Some(res) => println!(
                "{ctx} + {wd} : {} 项匹配 {:?}",
                res.len(),
                res.iter().map(|(v, l)| format!("({v},{l}B)")).collect::<Vec<_>>().join(",")
            ),
        }
    }

    println!("\n══ 5) 结论 ═══");
    println!("文件头与上游源码定义一一对应；数组占满文件剩余全部字节；");
    println!("常见搭配在 double-array 中均能命中并给出连续、可还原的数值；");
    println!("未见词典的词组路径无法匹配。Rust 直接读取该 .gram 可行。");
    println!("\n（更深入的结构分析请用：gram_probe enum / gram_probe coverage）");
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("enum") => run_enum(&mut args),
        Some("coverage") => run_coverage(&mut args),
        _ => {
            // run_default 需要拿到“文件路径 上下文 候选词”完整参数；
            // 这里重新从 argv 构造迭代器，避免第一个参数被上面的 next() 吃掉。
            run_default(std::env::args().skip(1))
        }
    }
}
