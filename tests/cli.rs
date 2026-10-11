//! 命令行参数解析:`--exec` 的**值**永远不能被当成输出文件。
//!
//! 旧实现用 `args.iter().find(|a| !a.starts_with("--"))` 找输出路径,
//! 于是 `xwx_ime --xml --exec ~/.local/bin/xwx_ime` 会把 `--exec` 的值当
//! 输出文件、用 XML 截断它(把正在用的输入法二进制写坏)。

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_xwx_ime")
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xwx-cli-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录失败");
    dir
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(bin()).args(args).output().expect("执行失败")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn exec_value_is_never_used_as_the_output_path() {
    let dir = tmp_dir("exec");
    let victim = dir.join("pretend-binary");
    std::fs::write(&victim, b"ORIGINAL-BYTES").expect("写预置文件失败");

    let out = run(&["--xml", "--exec", victim.to_str().unwrap()]);
    assert!(out.status.success(), "应成功: {}", stderr_of(&out));
    assert_eq!(
        std::fs::read(&victim).expect("读预置文件失败"),
        b"ORIGINAL-BYTES",
        "`--exec` 的值被当成了输出文件(旧 bug)"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("<?xml") || stdout.contains("<component"),
        "没有输出路径时应写 stdout: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn positional_path_is_written_and_exec_lands_in_the_xml() {
    let dir = tmp_dir("out");
    let out_file = dir.join("xwx.xml");

    let out = run(&["--xml", out_file.to_str().unwrap(), "--exec", "/usr/libexec/ibus-engine-xwx"]);
    assert!(out.status.success(), "应成功: {}", stderr_of(&out));
    assert!(out.stdout.is_empty(), "写了文件就不该再往 stdout 写");
    let xml = std::fs::read_to_string(&out_file).expect("读输出文件失败");
    assert!(
        xml.contains("/usr/libexec/ibus-engine-xwx"),
        "--exec 的值应写进 XML: {xml}"
    );

    // 参数顺序无关:位置参数在 `--exec` 之后也一样。
    let out_file2 = dir.join("xwx2.xml");
    let out = run(&["--xml", "--exec", "/opt/xwx_ime", out_file2.to_str().unwrap()]);
    assert!(out.status.success(), "应成功: {}", stderr_of(&out));
    let xml2 = std::fs::read_to_string(&out_file2).expect("读输出文件失败");
    assert!(xml2.contains("/opt/xwx_ime"), "exec 路径: {xml2}");

    // `-` 仍是 stdout。
    let out = run(&["--xml", "-"]);
    assert!(out.status.success(), "应成功: {}", stderr_of(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("<?xml"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_arguments_fail_loudly() {
    for args in [
        vec!["--xml", "--exec"],
        vec!["--xml", "--exec", "/a", "--exec", "/b"],
        vec!["--xml", "--bogus"],
        vec!["--xml", "a", "b"],
        vec!["--xml", "-", "a"],
    ] {
        let out = run(&args);
        assert!(
            !out.status.success(),
            "{args:?} 应当报错退出,实际成功、stdout={}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            !stderr_of(&out).is_empty(),
            "{args:?} 应在 stderr 给出原因"
        );
    }
}
