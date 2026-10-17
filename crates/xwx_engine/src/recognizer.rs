//! recognizer(librime `recognizer` 组件的最小等价物):识别「这串输入
//! 不是拼音,是邮箱 / 网址」。
//!
//! librime 的配置(`/usr/share/rime-data/default.yaml:52-56`):
//!
//! ```yaml
//! recognizer:
//!   patterns:
//!     email: "^[A-Za-z][-_.0-9A-Za-z]*@.*$"
//!     uppercase: "[A-Z][-_+.'0-9A-Za-z]*$"
//!     url: "^(www[.]|https?:|ftp[.:]|mailto:|file:).*$|^[a-z]+[.].+$"
//! ```
//!
//! `uppercase` 一条由会话层的大写组合承接(`Verbatim` 大写进串、原样
//! 上屏),这里只做 **email** 与 **url** 两类。
//!
//! ## 为什么不抄正则,用手写匹配
//!
//! 「不许新增 crate 依赖」是硬约束;而这五条(合并后)模式结构简单
//! (前缀匹配 + 字符类),不需要通用正则引擎。librime 原生语义里
//! `url` 的 `^[a-z]+[.].+$` 子句在**中文拼音语境**下必须收窄——
//! 它会把 `ni.hao`、甚至「先翻了页再随手敲的任意 `字母+.`」整段吞成
//! 网址(见下方 url 一节)。
//!
//! ## 什么时候判:「此刻该识别吗」
//!
//! 会话层每接纳一个新字符(或回删一个字符)后,拿**完整拼音串**
//! 调 [`recognize`]:
//!
//! - 返回 `Some(_)` → 进入识别态(候选列表被一条「原样上屏」的回声
//!   候选取代,拼音候选消失);
//! - 返回 `None` 且此前处于识别态 → 退出识别态,回到正常拼音输入
//!   (Backspace 把 `abc@` 删成 `abc` 就经这条路)。
//!
//! 判定是纯函数、只看串内容,不在本模块保存状态——「识别态」由会话层
//! 持有(见 `session.rs` 的 `recognized` 字段),这样回溯(进出识别态)
//! 天然由「每次按键后重判」给出,不存在「识别成立后继续打字又反悔」的
//! 中间态:`zhangsan@gmail` 每多敲一个字符都重新整串判定,仍命中
//! email,识别态保持。
//!
//! ## 逐条 pattern 的落地语义(与 librime 的异同都写在这里)
//!
//! ### email:`^[A-Za-z][-_.0-9A-Za-z]*@.*$`
//!
//! 逐字照搬:首字符字母 → 接下来零到多个 `[-_.0-9A-Za-z]` → 一个 `@`
//! → 之后任意(含空)。会话层拼音串只收小写字母(大写已被 Shift 那轮
//! 拦去 Verbatim),所以首字符实际只会是小写字母,不改变语义。
//!
//! ### url(分两条独立子句,任一命中即识别)
//!
//! **(a) 显式前缀**:`www.` / `http:` / `https:` / `ftp.` / `ftp:` /
//! `mailto:` / `file:`(librime `^(www[.]|https?:|ftp[.:]|mailto:|file:)`)。
//! 前缀本身是字面量;唯一的设计分歧是**前缀后即判成立**还是要求
//! 后面还有内容。librime 的正则是「前缀 + `.*$`」,即 `www.` 单独
//! 就成立;这里**收窄为「前缀后至少还有一个字符」**,理由是会话层
//! 的按键顺序:
//!
//! - 识别触发键(`.`/`:`)落下时,新字符**还没进串**。若按 librime
//!   语义「前缀 + 空后缀」也算命中,`www` + `.` 在落键瞬间就被
//!   吞进识别态——`.`,这个在中文模式下首先是**句号/翻页键**的
//!   字符,会在「用户可能只想给 `www` 敲个句号」的场景被吃掉。
//! - 收窄后:`www` + `.` → 不命中 → `.` 照旧翻页/出「。」;
//!   用户在 `.` 之后**再敲一个字母**(`www.e`)才进识别态——
//!   此刻「在打网址」的意图已经没有歧义。`www.` 这个点随后
//!   由识别态的「字符直通」补进串里(见 session.rs),最终
//!   上屏的 `www.example.com` 一个字符不缺。
//! - 「继续打字不反悔」不受影响:识别态的保持条件是「新串仍
//!   命中」,`www.e` 之后每多一个字符都仍命中 url (a)。
//! - **`www` 不带点绝不识别**——`www` 单独打仍出「万维网」等
//!   拼音候选(验收判据 A 的不退化项)。
//!
//! **(b) `^[a-z]+[.].+$`(小写字母串 + `.` + 任意非空后缀)——
//! 有意排除(维护者已裁决:不做,维持现状)。** 这条会把一切
//! 「字母 + 点 + 后缀」当网址,包括 `ni.hao`、`xian.a`——在中文
//! 输入法里 `.` 首先是句号键(候选窗开着时还是翻页键),句号在
//! 中文输入里的使用频率远高于裸域名,用户打完拼音敲 `.` 的
//! **预期**是上屏「。」或翻页,不是把整串吞成 raw。librime 有
//! tag 机制把「已确认的拼音段」和「raw 段」分开,我们没有;照搬
//! 这条子句等于把 `.` 从标点/翻页键位上拆走。后果(既定行为,
//! 有测试钉住,别误"修"):`zhangsan@gmail.com` 里的 `.com` 不受
//! 影响(email 的 `@.*$` 已吞掉 `@` 后一切);`www.example.com`
//! 由 (a) 的 `www.` 前缀覆盖;裸 `example.com` 这种没有前缀的
//! 网址**识别不了**,仍走拼音路径。
//!
//! ## `:` 触发臂(维护者裁决 2,后补)
//!
//! 初版里 `:` 落键时「协议前缀 + 空后缀」不识别,按标点既有语义
//! 上屏「http:」「mailto:」——实测得到的是**坏结果**而不是
//! 「缺失」:`http://x.com` 出「http:、、xcom」(`/` 被标点表
//! 映射成顿号)、`mailto:a@b.c` 出「买:a@b.c」(`mailto` 被当
//! 拼音上屏成「买」)。裁决:`:` 与 `.` 同权,落键时做**前向
//! 判定**——「串 + `:`」等于某条 url 前缀则 `:` 直接进串
//! (session.rs「recognizer 入口 2」),不进标点分支;识别随即
//! 成立(串含前缀,下一键使「前缀 + 非空后缀」命中),此后
//! `/`、`@`、字母全部走识别态直通臂。`mailto` 单独打(不接 `:`)
//! 永不触发,拼音候选不受影响。唯一让渡:对四个协议名前缀串
//! 敲 `:` 出的是识别态前缀而不是「拼音首选 + `:`」。
//!
//! ## 复杂度
//!
//! 每次判定是对串做一次 O(n) 前缀比对 + 字符类扫描(n = 拼音串长度,
//! 通常 < 30),无分配;在按键路径上的代价可忽略(实测见任务书 D 项
//! 延迟数据,识别判定前后 `lookup_bench` 数字不变)。

/// 识别结果(目前只有「是不是」两种,枚举为将来区分 email/url 注释
/// 展示留位;`SessionUpdate` 不做这个区分,先不暴露)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recognized {
    /// 邮箱(`local@...`)。
    Email,
    /// 网址 / 协议串(`www.`、`http(s):`、`ftp[.:]`、`mailto:`、`file:` 开头)。
    Url,
}

/// email 本地部分允许的字符(除首字符外):`[-_.0-9A-Za-z]`。
fn is_email_local_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')
}

/// 显式 url 前缀表(小写字面量,按长度降序无必要——全部用
/// `strip_prefix` 逐一试,命中即返回)。
///
/// `pub` 的理由:会话层 `session.rs` 的 `:` 触发臂用它做前向判定
/// (「串 + `:`」等于某条前缀时 `:` 直接进串,不走标点分支)——
/// 表只有一个真相来源,两边不会飘。
pub const URL_PREFIXES: &[&str] = &[
    "https:", "http:", "mailto:", "file:", "ftp.", "ftp:", "www.",
];

/// 判定整串是否应被识别为「非拼音原文」。
///
/// 输入是当前**完整拼音串**(会话层只收小写 ASCII 字母与 `'`、`.`、
/// `@`、`:` 等符号——识别成立后标点键也进串,见 session.rs)。
///
/// 返回 `None` 表示「正常拼音输入,别干预」。
pub fn recognize(input: &str) -> Option<Recognized> {
    // 空串不识别。
    let first = *input.as_bytes().first()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }

    // —— url (a):显式前缀。前缀后必须还有内容(收窄,见模块文档)——
    for &prefix in URL_PREFIXES {
        if let Some(rest) = input.strip_prefix(prefix) {
            if !rest.is_empty() {
                return Some(Recognized::Url);
            }
        }
    }

    // —— email:首字符字母(上面已查),中间 `[-_.0-9A-Za-z]*`,
    //    然后一个 `@`,`@` 后任意(含空)。`@` 必须在「本地部分」之后
    //    出现;本地部分里的 `.`/`_`/`-` 都合法。 ——
    let bytes = input.as_bytes();
    // 从第二个字符起扫描本地部分,找到第一个不属于本地字符集的字符。
    let mut i = 1;
    while i < bytes.len() && is_email_local_char(bytes[i]) {
        i += 1;
    }
    // 本地部分至少 0 个额外字符(首字符就是本地部分的第一个),
    // 停下的位置必须是 `@`(且确实存在)。
    if i < bytes.len() && bytes[i] == b'@' {
        return Some(Recognized::Email);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // —— email ——

    #[test]
    fn email_basic() {
        assert_eq!(recognize("zhangsan@gmail.com"), Some(Recognized::Email));
        assert_eq!(recognize("abc@"), Some(Recognized::Email), "@ 后允许为空(librime .*)");
        assert_eq!(recognize("a@b.c"), Some(Recognized::Email));
    }

    #[test]
    fn email_local_part_charset() {
        assert_eq!(recognize("zhang.san@gmail"), Some(Recognized::Email));
        assert_eq!(recognize("zhang_san-99@gmail"), Some(Recognized::Email));
        assert_eq!(recognize("z9@gmail"), Some(Recognized::Email));
    }

    #[test]
    fn email_needs_letter_first() {
        // 首字符非字母:不识别(数字/符号开头)。
        assert_eq!(recognize("9abc@x"), None);
        assert_eq!(recognize("@abc"), None);
        assert_eq!(recognize(""), None);
    }

    #[test]
    fn email_at_required() {
        assert_eq!(recognize("zhangsan"), None, "没有 @ 不是邮箱");
        assert_eq!(recognize("gmail.com"), None, "b 子句刻意不做:裸域名不识别");
    }

    // —— url (a) 显式前缀 ——

    #[test]
    fn url_explicit_prefixes() {
        for (input, why) in [
            ("www.example.com", "www. 前缀"),
            ("http://x.com", "http: 前缀"),
            ("https://x.com", "https: 前缀"),
            ("mailto:a@b.c", "mailto: 前缀"),
            ("ftp.x.com", "ftp. 前缀"),
            ("ftp://x", "ftp: 前缀"),
            ("file:/x", "file: 前缀"),
        ] {
            assert_eq!(recognize(input), Some(Recognized::Url), "{why}: {input}");
        }
    }

    #[test]
    fn url_prefix_alone_is_not_recognized() {
        // 收窄(与 librime 不同):前缀单独出现不识别,后面要有内容。
        // 这保证「`www` + `.` 落键瞬间」不被吞成网址——`.` 仍按
        // 标点/翻页既有规则处理,识别从 `www.e` 才开始(见模块文档)。
        for bare in ["www.", "http:", "https:", "ftp.", "ftp:", "mailto:", "file:"] {
            assert_eq!(recognize(bare), None, "前缀单独不识别: {bare}");
        }
    }

    #[test]
    fn www_without_dot_is_pinyin() {
        // 不退化判据的核心:`www` 单独打(没有后续 `.x`)仍是拼音。
        assert_eq!(recognize("www"), None);
        assert_eq!(recognize("abc"), None);
        assert_eq!(recognize("nihao"), None);
    }

    #[test]
    fn url_b_clause_deliberately_dropped() {
        // `^[a-z]+[.].+$` 不做:`ni.hao` 必须不被吞成网址
        // (`.` 在中文模式首先是句号/翻页键)。
        assert_eq!(recognize("ni.hao"), None);
        assert_eq!(recognize("example.com"), None, "裸域名不识别(代价,已上报)");
    }
}
