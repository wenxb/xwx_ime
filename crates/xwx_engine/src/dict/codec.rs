//! `dict.bin` / `abbrev.bin` v3 紧凑编码编解码原语。
//!
//! v3 在不丢失任何词条、词频、拼音的前提下压缩三处冗余：
//! 1. 索引记录改为 `(count + 1) × 8B` 哨兵差分（`str_off: u32, ent_off: u32`），省去长度字段；
//! 2. 基本汉字区（`U+4E00..=U+9FFF`）词条按 2B/字存储，词频按 2-bit 档位变长编码（`100` → 0B，`<=65535` → 2B，其余 4B）；
//! 3. `dict.bin` 拼音键按 1~2B 音节编号存储，`abbrev.bin` 音节分桶下标按 6-bit 紧凑打包。

use super::{read_u16, read_u32, DictError, MmapDict, MmapRecord};

/// 解码 `dict.bin` v3 头部与 `records_off` 之间的音节字典表。
pub(super) fn parse_v3_syl_table(
    bytes: &[u8],
    start: usize,
    records_off: usize,
) -> Result<Vec<Box<[u8]>>, DictError> {
    if start + 2 > records_off {
        return Err(DictError::Corrupt("v3 音节表越界"));
    }
    let count = read_u16(bytes, start)? as usize;
    let mut cur = start + 2;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let slen = *bytes
            .get(cur)
            .ok_or(DictError::Corrupt("v3 音节表越界"))? as usize;
        cur += 1;
        let raw = bytes
            .get(cur..cur + slen)
            .ok_or(DictError::Corrupt("v3 音节表越界"))?;
        if std::str::from_utf8(raw).is_err() {
            return Err(DictError::Corrupt("v3 音节不是 UTF-8"));
        }
        out.push(raw.to_vec().into_boxed_slice());
        cur += slen;
    }
    if cur > records_off {
        return Err(DictError::Corrupt("v3 音节表超出 records 起点"));
    }
    Ok(out)
}

/// 将 2B/字的 `U+4E00..=U+9FFF` 偏移序列还原成 3B/字的 UTF-8 字符串。
#[inline]
pub(super) fn decode_cjk2_to_utf8<'a>(
    raw: &[u8],
    clen: usize,
    wbuf: &'a mut [u8; 256],
) -> Result<&'a str, DictError> {
    if clen == 0 || clen * 3 > wbuf.len() || raw.len() != clen * 2 {
        return Err(DictError::Corrupt("v3 词长越界"));
    }
    for i in 0..clen {
        let d = u16::from_le_bytes([raw[i * 2], raw[i * 2 + 1]]) as u32;
        let cp = 0x4E00u32 + d;
        if cp > 0x9FFF {
            return Err(DictError::Corrupt("v3 CJK 码位越界"));
        }
        wbuf[i * 3] = (0xE0 | (cp >> 12)) as u8;
        wbuf[i * 3 + 1] = (0x80 | ((cp >> 6) & 0x3F)) as u8;
        wbuf[i * 3 + 2] = (0x80 | (cp & 0x3F)) as u8;
    }
    // 0x4E00..=0x9FFF 必定生成合法 3 字节 UTF-8。
    std::str::from_utf8(&wbuf[..clen * 3]).map_err(|_| DictError::Corrupt("词条不是 UTF-8"))
}

/// 解码 `dict.bin` v3 的单条词条，返回 `(word, freq, next_pos)`。
#[inline]
pub(super) fn read_v3_dict_entry<'a>(
    block: &'a [u8],
    pos: usize,
    wbuf: &'a mut [u8; 256],
) -> Result<(&'a str, u64, usize), DictError> {
    let hdr = *block.get(pos).ok_or(DictError::Corrupt("词块越界"))?;
    let freq_code = hdr >> 6;
    let clen = (hdr & 0x3F) as usize;
    if freq_code < 3 {
        let wbytes = clen * 2;
        let raw = block
            .get(pos + 1..pos + 1 + wbytes)
            .ok_or(DictError::Corrupt("词块越界"))?;
        let word = decode_cjk2_to_utf8(raw, clen, wbuf)?;
        let foff = pos + 1 + wbytes;
        let (freq, flen) = match freq_code {
            0 => (100u64, 0usize),
            1 => (read_u16(block, foff)? as u64, 2usize),
            _ => (read_u32(block, foff)? as u64, 4usize),
        };
        Ok((word, freq, foff + flen))
    } else {
        let wl = *block.get(pos + 1).ok_or(DictError::Corrupt("词块越界"))? as usize;
        let raw = block
            .get(pos + 2..pos + 2 + wl)
            .ok_or(DictError::Corrupt("词块越界"))?;
        let word =
            std::str::from_utf8(raw).map_err(|_| DictError::Corrupt("词条不是 UTF-8"))?;
        let freq = read_u32(block, pos + 2 + wl)? as u64;
        Ok((word, freq, pos + 2 + wl + 4))
    }
}

/// 读取 `dict.bin` v3 词块首条词频（零拷贝、不解码词串）。
#[inline]
pub(super) fn first_v3_entry_freq(block: &[u8]) -> Result<u64, DictError> {
    let hdr = *block.first().ok_or(DictError::Corrupt("词块为空"))?;
    let freq_code = hdr >> 6;
    let clen = (hdr & 0x3F) as usize;
    match freq_code {
        0 => Ok(100),
        1 => Ok(read_u16(block, 1 + clen * 2)? as u64),
        2 => Ok(read_u32(block, 1 + clen * 2)? as u64),
        _ => {
            let wl = *block.get(1).ok_or(DictError::Corrupt("词块越界"))? as usize;
            Ok(read_u32(block, 2 + wl)? as u64)
        }
    }
}

/// 解码 `abbrev.bin` v3 的单条词条，将解包后的音节桶下标写入 `sbuf[..key_len]`。
#[inline]
pub(super) fn read_v3_abbrev_entry<'a>(
    block: &'a [u8],
    pos: usize,
    key_len: usize,
    wbuf: &'a mut [u8; 256],
    sbuf: &mut [u8; 64],
) -> Result<(&'a str, u64, usize), DictError> {
    if key_len > sbuf.len() {
        return Err(DictError::Corrupt("副索引键过长"));
    }
    let hdr = *block.get(pos).ok_or(DictError::Corrupt("词块越界"))?;
    let freq_code = hdr >> 6;
    let clen = (hdr & 0x3F) as usize;
    let syl_bytes = (key_len * 6).div_ceil(8);
    let (word, syl_off) = if freq_code < 3 {
        let wbytes = clen * 2;
        let raw = block
            .get(pos + 1..pos + 1 + wbytes)
            .ok_or(DictError::Corrupt("词块越界"))?;
        (decode_cjk2_to_utf8(raw, clen, wbuf)?, pos + 1 + wbytes)
    } else {
        let wl = *block.get(pos + 1).ok_or(DictError::Corrupt("词块越界"))? as usize;
        let raw = block
            .get(pos + 2..pos + 2 + wl)
            .ok_or(DictError::Corrupt("词块越界"))?;
        let w = std::str::from_utf8(raw).map_err(|_| DictError::Corrupt("词条不是 UTF-8"))?;
        (w, pos + 2 + wl)
    };
    let packed = block
        .get(syl_off..syl_off + syl_bytes)
        .ok_or(DictError::Corrupt("词块越界"))?;
    for (k, slot) in sbuf.iter_mut().take(key_len).enumerate() {
        let bit_off = k * 6;
        let b_idx = bit_off >> 3;
        let shift = bit_off & 7;
        let w = (packed[b_idx] as u16)
            | ((packed.get(b_idx + 1).copied().unwrap_or(0) as u16) << 8);
        *slot = ((w >> shift) & 0x3F) as u8;
    }
    let foff = syl_off + syl_bytes;
    let (freq, flen) = match freq_code {
        0 => (100u64, 0usize),
        1 => (read_u16(block, foff)? as u64, 2usize),
        _ => (read_u32(block, foff)? as u64, 4usize),
    };
    Ok((word, freq, foff + flen))
}

/// 解码 `abbrev.bin` v2 或 v3 的单条词条，返回 `(word, syl_ids, freq, next_pos)`。
#[inline]
pub(super) fn read_v23_abbrev_entry<'a>(
    version: u32,
    block: &'a [u8],
    pos: usize,
    key_len: usize,
    wbuf: &'a mut [u8; 256],
    sbuf: &'a mut [u8; 64],
) -> Result<(&'a str, &'a [u8], u64, usize), DictError> {
    if version >= 3 {
        let (w, f, np) = read_v3_abbrev_entry(block, pos, key_len, wbuf, sbuf)?;
        Ok((w, &sbuf[..key_len], f, np))
    } else {
        let wl = *block.get(pos).ok_or(DictError::Corrupt("词块越界"))? as usize;
        let wb = block
            .get(pos + 1..pos + 1 + wl)
            .ok_or(DictError::Corrupt("词块越界"))?;
        let w = std::str::from_utf8(wb).map_err(|_| DictError::Corrupt("词条不是 UTF-8"))?;
        let ids = block
            .get(pos + 1 + wl..pos + 1 + wl + key_len)
            .ok_or(DictError::Corrupt("词块越界"))?;
        let f = read_u32(block, pos + 1 + wl + key_len)? as u64;
        Ok((w, ids, f, pos + 1 + wl + key_len + 4))
    }
}

impl MmapDict {
    /// 获取第 `rec` 条记录的拼音字节切片（v1/v2 直接借用 mmap，v3 解码音节编号至栈缓冲）。
    #[inline]
    pub(super) fn record_key<'a>(
        &'a self,
        rec: MmapRecord,
        kbuf: &'a mut [u8; 256],
    ) -> Result<&'a [u8], DictError> {
        let raw = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
        if self.version < 3 {
            return Ok(raw);
        }
        let mut pos = 0usize;
        let mut out_len = 0usize;
        while pos < raw.len() {
            let b = raw[pos];
            pos += 1;
            let sid = if b < 254 {
                b as usize
            } else {
                let lo = *raw.get(pos).ok_or(DictError::Corrupt("v3 拼音编码越界"))? as usize;
                pos += 1;
                254 + ((b as usize - 254) << 8) + lo
            };
            let syl = self
                .syl_table
                .get(sid)
                .ok_or(DictError::Corrupt("v3 音节编号越界"))?;
            if out_len > 0 {
                if out_len >= kbuf.len() {
                    return Err(DictError::Corrupt("v3 拼音展开超长"));
                }
                kbuf[out_len] = b' ';
                out_len += 1;
            }
            if out_len + syl.len() > kbuf.len() {
                return Err(DictError::Corrupt("v3 拼音展开超长"));
            }
            kbuf[out_len..out_len + syl.len()].copy_from_slice(syl);
            out_len += syl.len();
        }
        Ok(&kbuf[..out_len])
    }
}
