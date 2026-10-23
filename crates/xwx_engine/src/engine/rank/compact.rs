//! 去掉 ASCII 空格后的拼音视图，字节语义与原 String 完全一致。
pub(super) fn letters(pinyin: &str) -> impl DoubleEndedIterator<Item = u8> + Clone + '_ {
    pinyin.bytes().filter(|&b| b != b' ')
}

pub(super) fn starts_with(pinyin: &str, prefix: &str) -> bool {
    let mut bytes = letters(pinyin);
    prefix.bytes().all(|b| bytes.next() == Some(b))
}

pub(super) fn is_prefix_of(pinyin: &str, input: &str) -> bool {
    let mut input = input.bytes();
    letters(pinyin).all(|b| input.next() == Some(b))
}

pub(super) fn is_suffix_of(pinyin: &str, input: &str) -> bool {
    let mut input = input.bytes().rev();
    letters(pinyin).rev().all(|b| input.next() == Some(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_views_match_original_string_including_unicode_and_empty() {
        for pinyin in [
            "",
            " ",
            "ni hao",
            " ni  hao ",
            "lü e",
            "你 好",
            "zhong guo ren",
        ] {
            let compact: String = pinyin.chars().filter(|&c| c != ' ').collect();
            for input in [
                "", "n", "ni", "nihao", "nihaoq", "qnihao", "lüe", "你好", "ren",
            ] {
                assert_eq!(starts_with(pinyin, input), compact.starts_with(input));
                assert_eq!(is_prefix_of(pinyin, input), input.starts_with(&compact));
                assert_eq!(is_suffix_of(pinyin, input), input.ends_with(&compact));
                assert_eq!(letters(pinyin).count(), compact.len());
            }
        }
    }
}
