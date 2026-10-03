//! 回归探针：把「候选列表 + 会话行为 + 单次延迟」钉成文本，改动前后逐字节对拍。
//!
//! 判退化的方式是**逐位比对候选列表本身**（顺序、词、拼音、层、分数），
//! 不用「前 10 重合率」这类指标。三段分开看：
//! - A：`lookup_candidates` 的完整候选列表（对拍主体）；
//! - B：`commit_immediately` 取第 1 个候选后的剩余串；
//! - C：真实按键序列（`Char`/`Space`/`Backspace`/`CandidateClicked`/`PageDown`/
//!   `DeleteCandidate`）的每一步输出。
//!
//! B 与真实交互**不等价**：`commit_immediately` 立即学习并返回文本；`handle` 把
//! 选中的一段留在组合里、上推学习到组合结束。确认/回退/学习时序只看 C 段。
//! 每块的 `min_ms` 是 30 次查询的最短耗时，用来盯延迟门槛。
//!
//! 打对的输入基线必须逐字节不变；为容错解码**故意**改变的输入要在改动说明里点名，
//! 其余差异都算退化。
//!
//! 词库/副索引/万象模型三个文件必须存在且真的加载成功，否则直接报错退出——引擎自身
//! 会「缺文件就退回空库」静默降级（`engine/api.rs::new_with_lm`），探针不能跟着降级，
//! 否则空库基线看着也是绿的。写文件时另存 `<out>.id.txt`：输出哈希与行数、Git SHA/
//! 是否有改动、三个数据文件的 sha256 与字节数，基线因此可追溯（`tests/baselines/`）。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use xwx_engine::{ImeEngine, InputEvent, LmContext, LmStore, SessionOutput, SpellingKind};

/// 场景矩阵:正确串 / 多打 / 少打 / 替换 / 调换 / 乱码,错在头/中/尾。
const SCENARIOS: &[&str] = &[
    // —— 回归对拍集(这些改动前后必须逐字节相同)——
    "nihao", "ni", "xian", "ky", "mei", "meiy", "zhongg", "zhongguo", "zhongguor", "women",
    "nimen", "nhao", "feicde", "sh", "zh", "zho", "niha", "beijin", "meiyo", "xianzai", "jintian",
    "gongyuan", "zzzz", "zhzhzhzh", "aaaaaaaa", "wsm", "zgr", "nih",
    // —— 短词错误矩阵 ——
    "nihaoq",    // 多打(尾)
    "qnihao",    // 乱码(头)
    "niqhao",    // 多打(中)
    "nihoa",     // 调换(用户报的 nihoa)
    "nhao",      // 少打
    "nihaoo",    // 重复字母
    "shenem",    // 调换(CHIME 例子:shenme)
    "shanghaai", // 重复字母(CHIME 例子:shanghai)
    // —— 长串错误矩阵 ——
    "womenjintianqugongyuan",  // 正确基线
    "womenjintianqugongyuann", // 多打(尾)
    "womenjintianqqugongyuan", // 多打(中)
    "womenjintianzzgongyuan",  // 乱码(中)
    "womemjintianqugongyuan",  // 替换(头,m/n)
    "womenjinxtianqugongyuan", // 多打(音节内)
    "womenjitianqugongyuan",   // 少打(音节内,jintian→jitian)
    "womenjintianqugonguan",   // 少打(尾音节,gongyuan→gonguan)
    "womenjintianqugongyuam",  // 替换(尾,n/m)
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    let backend = Backend::resolve(&args.data)?;
    let engine = open_engine(&backend)?;

    let mut out = String::new();
    // v1 的标题行必须逐字节保持历史样子,否则老基线 diff 全红。
    let title = match args.format {
        Format::V1 => "# xwx_ime 长串容错基线(无用户层;词库+万象 LM)",
        Format::V2 => "# xwx_ime 回归基线 v2(无用户层;词库+万象 LM)",
    };
    let _ = writeln!(out, "{title}");

    write_lookup_section(&mut out, &engine, args.format)?;
    write_commit_section(&mut out, &engine, args.format)?;
    if args.format == Format::V2 {
        write_session_section(&mut out, &engine)?;
    }

    let report = Report::of(&out, &backend, args.format, args.out.path());
    match &args.out {
        Out::Stdout => {
            print!("{out}");
        }
        Out::File(path) => {
            if let Some(dir) = path.parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir)?;
                }
            }
            std::fs::write(path, &out)?;
            let id_path = path.with_extension("id.txt");
            std::fs::write(&id_path, report.identity())?;
            println!(
                "baseline written: {} ({} bytes, {} lines)\nidentity: {}",
                path.display(),
                out.len(),
                out.lines().count(),
                id_path.display()
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- 参数与数据

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    V1,
    V2,
}

impl Format {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "v1" => Ok(Self::V1),
            "v2" => Ok(Self::V2),
            other => Err(format!("未知 --format {other:?}(支持 v1/v2)")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

enum Out {
    Stdout,
    File(PathBuf),
}

impl Out {
    fn path(&self) -> Option<&Path> {
        match self {
            Self::Stdout => None,
            Self::File(p) => Some(p),
        }
    }
}

struct Args {
    out: Out,
    data: PathBuf,
    format: Format,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let mut out: Option<PathBuf> = None;
        let mut stdout = false;
        let mut data: Option<PathBuf> = None;
        let mut format = Format::V2;

        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--help" | "-h" => {
                    println!("{}", USAGE);
                    std::process::exit(0);
                }
                "--stdout" => stdout = true,
                "--data" => {
                    data = Some(PathBuf::from(
                        it.next().ok_or("--data 后面要跟目录")?,
                    ));
                }
                "--format" => {
                    format = Format::parse(&it.next().ok_or("--format 后面要跟 v1/v2")?)?;
                }
                other if other.starts_with("--") => {
                    return Err(format!("未知参数 {other:?}\n{USAGE}").into());
                }
                other => {
                    if out.replace(PathBuf::from(other)).is_some() {
                        return Err(format!("只能给一个输出路径\n{USAGE}").into());
                    }
                }
            }
        }

        // 默认**不写** /tmp:基线要么显式给路径(可追溯、随基线一起提交),
        // 要么显式 `--stdout` 走管道对拍。
        let out = match (out, stdout) {
            (Some(_), true) => return Err(format!("--stdout 与输出路径不能同时给\n{USAGE}").into()),
            (Some(p), false) => Out::File(p),
            (None, true) => Out::Stdout,
            (None, false) => return Err(format!("要给出输出路径或 --stdout\n{USAGE}").into()),
        };
        let data = match data {
            Some(d) => d,
            None => PathBuf::from(std::env::var("HOME")?).join(".local/share/xwx"),
        };
        Ok(Self { out, data, format })
    }
}

const USAGE: &str = "\
用法: regression_probe [--data DIR] [--format v1|v2] (--stdout | OUT)
  --data DIR    词库/副索引/万象模型的目录(默认 $HOME/.local/share/xwx)
  --format v1   逐字节兼容的历史格式(A/B 段);v2(默认)加 bits/covered 与 C 段
  --stdout      写到标准输出(对拍用),不写文件、不写身份文件
  OUT           写到这里,并额外写 OUT.id.txt(输出哈希 + 数据哈希 + Git SHA)";

/// 三个数据文件。全部必须存在——探针不做任何降级。
struct Backend {
    dir: PathBuf,
    dict: PathBuf,
    abbrev: PathBuf,
    gram: PathBuf,
}

impl Backend {
    fn resolve(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let backend = Self {
            dir: dir.to_path_buf(),
            dict: dir.join("dict.bin"),
            abbrev: dir.join("abbrev.bin"),
            gram: dir.join("wanxiang-lts-zh-hans.gram"),
        };
        let mut missing = Vec::new();
        for p in [&backend.dict, &backend.abbrev, &backend.gram] {
            if !p.is_file() {
                missing.push(p.display().to_string());
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "数据文件缺失(探针不做降级,请先准备或改 --data):{}",
                missing.join("、")
            )
            .into());
        }
        Ok(backend)
    }
}

/// 打开引擎并**验证后端真的加载了**:引擎对缺失/损坏文件是静默退回空库的,
/// 只靠 `Ok(engine)` 判断不了(空库探针也会产出「正常」的基线)。
fn open_engine(backend: &Backend) -> Result<Arc<ImeEngine>, Box<dyn std::error::Error>> {
    let mut engine = ImeEngine::new_with_lm(Some(&backend.dict), Some(&backend.gram))?;
    engine.dict = engine.dict.with_abbrev(&backend.abbrev);
    let engine = Arc::new(engine);

    // 注意:`LmStore::vocab_size()` 对万象变体返回 0(它自己统计在
    // `WanxiangLM::num_units`),所以这里按**变体**判断,不能看规模字段。
    match &engine.lm {
        LmStore::Wanxiang(lm) if lm.num_units() > 0 => {}
        LmStore::Wanxiang(_) => {
            return Err(format!(
                "万象模型加载成空表(num_units=0):{} 损坏",
                backend.gram.display()
            )
            .into());
        }
        LmStore::Bigram(_) => {
            return Err(format!(
                "万象模型没加载(引擎退回了空 bigram):{} 损坏或格式不对,拒绝产出会误导的基线",
                backend.gram.display()
            )
            .into());
        }
        LmStore::Mixed(_) => {
            return Err("探针不接用户层(混合后端):确定性基线不允许用户数据介入".into());
        }
    }
    let ctx = LmContext::empty();
    let real = engine.lookup_candidates("nihao", &ctx)?;
    if !real.iter().any(|c| !c.echo) {
        return Err(format!(
            "词库没加载(查 nihao 只有回声候选):{} 损坏或为空",
            backend.dict.display()
        )
        .into());
    }
    let mixed = engine.lookup_candidates("zhongg", &ctx)?;
    if !mixed.iter().any(|c| c.spelling == SpellingKind::Abbrev) {
        return Err(format!(
            "副索引没装上(查 zhongg 没有简拼候选):{} 损坏",
            backend.abbrev.display()
        )
        .into());
    }
    Ok(engine)
}

// ---------------------------------------------------------------- 各段输出

fn write_lookup_section(
    out: &mut String,
    engine: &ImeEngine,
    format: Format,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = writeln!(out, "\n## A. lookup_candidates");
    let ctx = LmContext::empty();
    for input in SCENARIOS {
        for _ in 0..5 {
            let _ = engine.lookup_candidates(input, &ctx)?;
        }
        let mut min_ms = f64::MAX;
        let mut cands = Vec::new();
        for _ in 0..30 {
            let t0 = Instant::now();
            cands = engine.lookup_candidates(input, &ctx)?;
            min_ms = min_ms.min(t0.elapsed().as_secs_f64() * 1000.0);
        }
        let _ = writeln!(out, "\n### {input}\nn={} min_ms={min_ms:.3}", cands.len());
        for (i, c) in cands.iter().enumerate() {
            // v1 = 逐字节兼容历史格式;v2 追加浮点位模式与 coverage。
            if format == Format::V2 {
                let _ = writeln!(
                    out,
                    "{i}|{}|{}|{}|{:.4}|bits={}|covered={}|{:?}|echo={}",
                    c.word, c.pinyin, c.freq, c.score, c.score.to_bits(), c.covered, c.spelling,
                    c.echo
                );
            } else {
                let _ = writeln!(
                    out,
                    "{i}|{}|{}|{}|{:.4}|{:?}|echo={}",
                    c.word, c.pinyin, c.freq, c.score, c.spelling, c.echo
                );
            }
        }
    }
    Ok(())
}

/// B 段:**立即提交**接口(非交互路径)。保留只为跟历史基线逐字节对比。
fn write_commit_section(
    out: &mut String,
    engine: &Arc<ImeEngine>,
    format: Format,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = writeln!(
        out,
        "\n## B. session(打字 → 选第 1 个候选{})",
        if format == Format::V1 {
            ""
        } else {
            ";commit_immediately 立即提交接口,非交互路径"
        }
    );
    for input in SCENARIOS {
        let mut s = engine.create_session();
        let mut before: Vec<String> = Vec::new();
        for ch in input.chars() {
            if let SessionOutput::Updated(u) = s.handle(InputEvent::Char(ch))? {
                before = u.candidates.iter().take(3).map(|c| c.word.clone()).collect();
            }
        }
        let committed = s.commit_immediately(0);
        let _ = writeln!(
            out,
            "\n### {input}\ntop3_before={before:?}\ncommitted={committed:?}\npreedit_after={:?}",
            s.pinyin(),
        );
    }
    Ok(())
}

/// C 段:真实按键序列(走 `handle`),每一步的输出都记下来。
fn write_session_section(
    out: &mut String,
    engine: &Arc<ImeEngine>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = writeln!(
        out,
        "\n## C. session(真实按键序列:handle;确认段/退格/点击/翻页/Ctrl+Delete)"
    );
    for (name, keys) in session_scripts() {
        let mut s = engine.create_session();
        let _ = writeln!(out, "\n### {name}");
        for key in &keys {
            let result = s.handle(*key)?;
            let _ = writeln!(out, "{:<34} -> {}", describe(key), describe_out(&result));
        }
        let _ = writeln!(out, "final pinyin={:?} active={}", s.pinyin(), s.is_active());
    }
    Ok(())
}

/// 按键脚本(运行时构造:拼音串逐字符 + 收尾动作)。
fn session_scripts() -> Vec<(&'static str, Vec<InputEvent>)> {
    let letters = |s: &str| -> Vec<InputEvent> { s.chars().map(InputEvent::Char).collect() };
    let mut scripts: Vec<(&'static str, Vec<InputEvent>)> = Vec::new();
    let mut push = |name: &'static str, body: &str, tail: &[InputEvent]| {
        let mut keys = letters(body);
        keys.extend(tail.iter().copied());
        scripts.push((name, keys));
    };

    push("nihao", "nihao", &[InputEvent::Space, InputEvent::Space]);
    push(
        "womenjintian",
        "womenjintian",
        &[InputEvent::Space, InputEvent::Space],
    );
    push(
        "womenjintianqubeij",
        "womenjintianqubeij",
        &[InputEvent::Space],
    );
    push("beijin", "beijin", &[InputEvent::Space]);
    push("xian-click2", "xian", &[InputEvent::CandidateClicked(2)]);
    push(
        "zhongguo-pagedown-click",
        "zhongguo",
        &[InputEvent::PageDown, InputEvent::CandidateClicked(0)],
    );
    push(
        "nihao-space-backspace-backspace",
        "nihao",
        &[
            InputEvent::Space,
            InputEvent::Backspace,
            InputEvent::Backspace,
            InputEvent::Space,
        ],
    );
    push(
        "nihao-space-backspace-ao",
        "nihao",
        &[
            InputEvent::Space,
            InputEvent::Backspace,
            InputEvent::Char('a'),
            InputEvent::Char('o'),
            InputEvent::Space,
        ],
    );
    push("hello-enter", "hello", &[InputEvent::Enter]);
    push("nihoa-typo", "nihoa", &[InputEvent::Space]);
    push("shandian-separator", "shan'dian", &[InputEvent::Space]);
    push(
        "nihao-delete-candidate",
        "nihao",
        &[InputEvent::DeleteCandidate, InputEvent::Space],
    );
    scripts
}

fn describe(key: &InputEvent) -> String {
    match key {
        InputEvent::Char(c) => format!("key Char({c})"),
        InputEvent::Verbatim(c) => format!("key Verbatim({c})"),
        InputEvent::Select(n) => format!("key Select({n})"),
        InputEvent::CandidateClicked(i) => format!("key CandidateClicked({i})"),
        InputEvent::Backspace => "key Backspace".to_owned(),
        InputEvent::Enter => "key Enter".to_owned(),
        InputEvent::Space => "key Space".to_owned(),
        InputEvent::Escape => "key Escape".to_owned(),
        InputEvent::Up => "key Up".to_owned(),
        InputEvent::Down => "key Down".to_owned(),
        InputEvent::Left => "key Left".to_owned(),
        InputEvent::Right => "key Right".to_owned(),
        InputEvent::PageUp => "key PageUp".to_owned(),
        InputEvent::PageDown => "key PageDown".to_owned(),
        InputEvent::DeleteCandidate => "key DeleteCandidate".to_owned(),
        InputEvent::ToggleAscii => "key ToggleAscii".to_owned(),
    }
}

fn describe_out(out: &SessionOutput) -> String {
    match out {
        SessionOutput::Commit(text) => format!("Commit({text:?})"),
        SessionOutput::Updated(u) => {
            let top3: Vec<&str> = u.candidates.iter().take(3).map(|c| c.word.as_str()).collect();
            format!(
                "Updated {{ preedit={:?} cursor={} page={}/{} n={} top3={top3:?} }}",
                u.preedit,
                u.cursor,
                u.page,
                u.page_size,
                u.candidates.len()
            )
        }
        SessionOutput::Noop => "Noop".to_owned(),
    }
}

// ---------------------------------------------------------------- 身份记录

/// 这份输出的身份:输出本身 + 数据文件 + 代码版本。基线文件旁存一份,
/// 换错模型/缺副索引/代码变了都能一眼对出来。
struct Report {
    identity: String,
}

impl Report {
    fn of(text: &str, backend: &Backend, format: Format, out_path: Option<&Path>) -> Self {
        let mut id = String::new();
        let _ = writeln!(id, "# xwx_ime 回归探针身份(自动生成,随基线一起提交)");
        let _ = writeln!(id, "format={}", format.name());
        // **不含 `min_ms=` 计时行**:那几行每次跑都不一样(取 30 次最小值),
        // 而逐字节对拍本来就要 `grep -v 'min_ms='`;基线的身份必须是可复现的。
        let stable = stable_text(text);
        let _ = writeln!(id, "out_sha256={}", sha256_of_bytes(stable.as_bytes()));
        let _ = writeln!(id, "out_bytes={}", text.len());
        let _ = writeln!(id, "out_lines={}", text.lines().count());
        // git_head 是**生成基线时**的 HEAD,通常等于这份基线所在提交的父提交
        // (改了探针才能生成新基线,两者不可能同属一个提交);基线自身的内容
        // 身份看 out_sha256。
        let _ = writeln!(id, "git_head={}", git(&["rev-parse", "HEAD"]));
        // 工作区改动**排除基线文件自己**(否则这份基线永远是 dirty 的),
        // 剩下还有改动就说明基线不是从干净工作区生成的——那句话对下游有用。
        let dirty: Vec<String> = git(&["status", "--porcelain"])
            .lines()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .filter(|l| !own_output(l, out_path))
            .collect();
        let _ = writeln!(id, "git_dirty={}", if dirty.is_empty() { "no" } else { "yes" });
        if !dirty.is_empty() {
            let _ = writeln!(id, "git_dirty_paths={dirty:?}");
        }
        let _ = writeln!(id, "data_dir={}", backend.dir.display());
        for path in [&backend.dict, &backend.abbrev, &backend.gram] {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let _ = writeln!(
                id,
                "{name}: bytes={bytes} sha256={}",
                sha256_of_file(path).unwrap_or_else(|| "<读取失败>".to_owned())
            );
        }
        Self { identity: id }
    }

    fn identity(&self) -> &str {
        &self.identity
    }
}

/// 这一行 `git status` 是不是本探针自己的输出(基线文件 + 身份文件)。
/// 去掉计时行之后的输出:同一二进制跨进程重复生成必须完全一致,
/// 也跟历史基线 diff 的口径(`grep -v 'min_ms='`)一致。
fn stable_text(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for line in text.lines() {
        if !line.contains("min_ms=") {
            s.push_str(line);
            s.push('\n');
        }
    }
    s
}

fn own_output(status_line: &str, out_path: Option<&Path>) -> bool {
    let Some(path) = out_path else { return false };
    let name = path.to_string_lossy();
    let id_name = path.with_extension("id.txt");
    status_line.contains(name.as_ref()) || status_line.contains(id_name.to_string_lossy().as_ref())
}

fn git(args: &[&str]) -> String {
    match Command::new("git").args(args).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        _ => "<未知>".to_owned(),
    }
}

/// sha256 走 `sha256sum`(与 justfile 的 install 摘要同一套工具,不引入新依赖)。
fn sha256_of_file(path: &Path) -> Option<String> {
    let out = Command::new("sha256sum").arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().map(str::to_owned)
}

fn sha256_of_bytes(bytes: &[u8]) -> String {
    match Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child
                .stdin
                .as_mut()
                .ok_or_else(|| std::io::Error::other("no stdin"))?
                .write_all(bytes)?;
            child.wait_with_output()
        }) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("<空>")
            .to_owned(),
        _ => "<未知>".to_owned(),
    }
}
