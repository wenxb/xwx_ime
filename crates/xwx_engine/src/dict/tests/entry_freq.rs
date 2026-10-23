use super::*;

#[test]
fn borrowed_frequency_matches_lookup_first_entry_for_both_versions() {
    for version in [1, 2] {
        let path = temp_path(&format!("freq_{version}"));
        fs::write(&path, build_test_bin(version)).unwrap();
        let dict = MmapDict::open(&path).unwrap();
        fs::remove_file(path).unwrap();
        for key in ["ni", "hao", "ni hao", "missing"] {
            for word in ["你", "好", "你好", "missing"] {
                let expected = dict
                    .lookup(key)
                    .unwrap()
                    .into_iter()
                    .find(|c| c.word == word)
                    .map(|c| c.freq);
                assert_eq!(dict.entry_freq(key, word).unwrap(), expected);
            }
        }
    }
}

#[test]
fn frequency_lookup_keeps_first_duplicate_even_when_its_frequency_is_zero() {
    let mut blob = build_test_bin(2);
    let entries = u64::from_le_bytes(blob[44..52].try_into().unwrap()) as usize;
    let first = entries + (1 + "好".len() + 4);
    blob[first + 1 + "你".len()..first + 1 + "你".len() + 4].fill(0);
    let second = first + (1 + "你".len() + 4);
    blob[second + 1..second + 1 + "妮".len()].copy_from_slice("你".as_bytes());
    let path = temp_path("freq_duplicate");
    fs::write(&path, blob).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(dict.entry_freq("ni", "你").unwrap(), Some(0));
    assert_eq!(dict.lookup("ni").unwrap()[0].freq, 0);
}

#[test]
fn frequency_lookup_still_validates_entries_after_a_match() {
    let mut blob = build_test_bin(2);
    let entries = u64::from_le_bytes(blob[44..52].try_into().unwrap()) as usize;
    // hao 的一条词之后是 ni 的「你」「妮」「尼」；破坏「妮」的 UTF-8。
    let second_ni = entries + (1 + "好".len() + 4) + (1 + "你".len() + 4);
    blob[second_ni + 1] = 0xff;
    let path = temp_path("freq_corrupt");
    fs::write(&path, blob).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(path).unwrap();
    let old_error = dict.lookup("ni").unwrap_err().to_string();
    assert_eq!(
        dict.entry_freq("ni", "你").unwrap_err().to_string(),
        old_error
    );
    assert_eq!(
        dict.entry_freq("ni", "missing").unwrap_err().to_string(),
        old_error
    );
}
