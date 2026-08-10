//! IBus 协议适配：键值常量、按键翻译、GVariant 序列化构造。
//!
//! 本模块是 xwx_ibus 与 ibus-daemon 之间的"翻译层"，所有序列化格式
//! 均对齐 **ibus 1.5.29**（Ubuntu 24.04 携带的版本），要点如下。
//!
//! ## IBus 可序列化对象的 GVariant 格式
//!
//! 所有 `IBusSerializable` 对象（`IBusText`、`IBusComponent` 等）在
//! D-Bus 上统一序列化为一个**结构体**，最前面是类型名字符串，
//! 后跟一个空的 `a{sv}` 附加属性字典，再往后才是各对象自己的字段：
//!
//! ```text
//! ("IBusText",         a{sv}, 文本,  属性列表v)
//! ("IBusAttrList",     a{sv}, 属性av)
//! ("IBusEngineDesc",   a{sv}, 8×s, u, 8×s)
//! ("IBusComponent",    a{sv}, 8×s, av, av)
//! ("IBusLookupTable",  a{sv}, u, u, b, b, i, av, av)
//! ```
//!
//! 反序列化端（`ibus_serializable_deserialize`）用 `g_type_from_name`
//! 按类型名字符串分派对象类，字段顺序必须逐字节一致，不能自由增删。
//!
//! 类型串速查（供调试 `busctl monitor` 时对照）：
//!
//! | 对象 | 完整类型串 |
//! | --- | --- |
//! | IBusText | `(sa{sv}sv)` |
//! | IBusAttrList | `(sa{sv}av)` |
//! | IBusEngineDesc | `(sa{sv}ssssssssussssssss)` |
//! | IBusComponent | `(sa{sv}ssssssssavav)` |
//! | IBusLookupTable | `(sa{sv}uubbiavav)` |
//!
//! ## 关键 D-Bus 约定
//!
//! - `RegisterComponent(v)`：组件包在 variant 里注册给 ibus-daemon；
//! - `Factory.CreateEngine(s) -> (o)`：daemon 按引擎名向工厂要实例，
//!   工厂负责选路径并导出对象、把路径返回给 daemon；
//! - 引擎信号：`CommitText(v)`、`UpdatePreeditText(v,u,b,u)`、
//!   `HidePreeditText()`、`UpdateLookupTable(v,b)`、`HideLookupTable()`。

use std::collections::HashMap;

use serde::Serialize;
use thiserror::Error;
use xwx_engine::InputEvent;
use zvariant::{OwnedValue, Structure, Value};

/// 引擎进程要占用的 D-Bus 服务名（也是组件的 name 字段）。
pub const ENGINE_SERVICE: &str = "org.freedesktop.IBus.Engine.xwx";
/// ibus-daemon 的服务名/路径/接口。
pub const IBUS_SERVICE: &str = "org.freedesktop.IBus";
pub const IBUS_PATH: &str = "/org/freedesktop/IBus";
pub const IBUS_INTERFACE: &str = "org.freedesktop.IBus";
/// Factory 对象路径（引擎进程在自己名下导出）。
pub const FACTORY_PATH: &str = "/org/freedesktop/IBus/Factory";
/// 引擎对象路径（首个实例的规范路径）。
pub const ENGINE_PATH: &str = "/org/freedesktop/IBus/Engine/xwx";
/// 引擎名（组件里注册的名字，daemon 用它来激活）。
pub const ENGINE_NAME: &str = "xwx";

/// IBus 按键修饰键掩码（ibuskeysyms.h / ibus.h 的 `IBUS_*_MASK`）。
pub const IBUS_SHIFT_MASK: u32 = 1 << 0;
pub const IBUS_LOCK_MASK: u32 = 1 << 1;
pub const IBUS_RELEASE_MASK: u32 = 1 << 30;
pub const IBUS_CONTROL_MASK: u32 = 1 << 2;
pub const IBUS_MOD1_MASK: u32 = 1 << 3;
pub const IBUS_SUPER_MASK: u32 = 1 << 26;
pub const IBUS_HYPER_MASK: u32 = 1 << 27;
pub const IBUS_META_MASK: u32 = 1 << 28;

/// 本输入法用到的 IBus 键值（ibuskeysyms.h）。
pub mod keyval {
    /// 空格（0x20）。
    pub const SPACE: u32 = 0x20;
    /// 数字 0（0x30..=0x39 为数字区间）。
    pub const ZERO: u32 = 0x30;
    /// 大写 A（0x41..=0x5A 为大写字母区间）。
    pub const UPPER_A: u32 = 0x41;
    /// 小写 a（0x61..=0x7A 为小写字母区间）。
    pub const LOWER_A: u32 = 0x61;
    /// BackSpace。
    pub const BACKSPACE: u32 = 0xff08;
    /// Return / Enter。
    pub const RETURN: u32 = 0xff0d;
    /// Escape。
    pub const ESCAPE: u32 = 0xff1b;
    /// 方向键。
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    /// 翻页键。
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
}

/// 协议层错误。
#[derive(Debug, Error)]
pub enum ProtoError {
    /// GVariant 构造失败。
    #[error("GVariant 构造失败: {0}")]
    Variant(#[from] zvariant::Error),
}

/// 把 IBus 键值翻译成引擎按键事件。
///
/// - **Shift / CapsLock 判定在这里做**(修饰键只存在于 `state`,
///   `keyval` 本身不携带),对应 librime `ascii_composer` 的
///   Shift+字母 / CapsLock 行为:
///   - Shift+字母 → [`InputEvent::Verbatim`] 大写(如 `G`),由
///     会话层直接上屏,不进拼音串(`GPU` 能打出来);
///   - CapsLock 开(无 Shift)→ `Verbatim` 大写;
///   - **CapsLock + Shift 叠加 → 小写进拼音串**(常规终端/编辑器
///     语义:两者都翻转大小写,叠加相互抵消——ibus 在 CapsLock+Shift
///     下报的 keyval 本来就是小写,这里只是显式归一);
///   - 都不生效 → 字母归一为小写,`Char` 进拼音串;
/// - 数字键翻译为 `Select(n)`(`Select(0)` 在会话里代表第 10 个候选);
/// - ASCII 可打印标点翻译为 `InputEvent::Char(c)`(原始半角字符)。
///   注意:**这里只负责放行,输出全角还是半角由会话层按中/英模式
///   决定**——中文模式下会话层把它上屏为全角标点(见
///   `xwx_engine::punctuator`),英文模式下会话层返回 `Noop`,
///   IBus 层据此返回 `false`,daemon 把按键透传给应用,应用收到
///   原始半角字符(键不会被吃掉);
/// - 不识别的键返回 `None`(调用方应把按键透传给应用)。
///
/// `state` 是 IBus 修饰键掩码。**英文模式的拦截不在这里**:调用方
/// (engine.rs)在英文模式下根本不调本函数,直接把按键透传给应用——
/// 应用才能收到带 Shift/CapsLock 状态的原始按键(Shift+G 上屏大写、
/// CapsLock 下 Shift+g 上屏小写);若先经这里归一,原始按键信息就丢了。
pub fn translate_keyval(keyval: u32, state: u32) -> Option<InputEvent> {
    match keyval {
        keyval::SPACE => Some(InputEvent::Space),
        // KP_Space:小键盘空格(X11 旧键值),同样按空格处理
        0xff00 => Some(InputEvent::Space),
        keyval::RETURN => Some(InputEvent::Enter),
        keyval::ESCAPE => Some(InputEvent::Escape),
        keyval::BACKSPACE => Some(InputEvent::Backspace),
        keyval::UP => Some(InputEvent::Up),
        keyval::DOWN => Some(InputEvent::Down),
        keyval::LEFT => Some(InputEvent::Left),
        keyval::RIGHT => Some(InputEvent::Right),
        keyval::PAGE_UP => Some(InputEvent::PageUp),
        keyval::PAGE_DOWN => Some(InputEvent::PageDown),
        keyval::ZERO..=0x39 => Some(InputEvent::Select((keyval - keyval::ZERO) as u8)),
        keyval::UPPER_A..=0x5a | keyval::LOWER_A..=0x7a => {
            let shift = state & IBUS_SHIFT_MASK != 0;
            let caps = state & IBUS_LOCK_MASK != 0;
            let lower = (keyval as u8 as char).to_ascii_lowercase();
            // Shift 与 CapsLock 各翻转一次大小写:恰好一个生效时出
            // 大写(Verbatim 直接上屏);叠加抵消或都不生效时小写进
            // 拼音串。
            if shift != caps {
                Some(InputEvent::Verbatim(lower.to_ascii_uppercase()))
            } else {
                Some(InputEvent::Char(lower))
            }
        }
        // ASCII 可打印标点(0x21..=0x7e 去掉字母数字):原样放行,
        // 含 `'`——它是 B2 音节分隔符的预留键,会话层当前对它 Noop
        // 透传,行为与本轮之前逐位一致(见 punctuator.rs 模块文档)。
        // 注意:`=` `+` `-` `#` `%` `&` `*` `@` `|` 等没有全角映射的
        // 字符也走这条路,会话层对它们 Noop 透传(键不会被吃掉)。
        0x21..=0x7e => Some(InputEvent::Char(keyval as u8 as char)),
        _ => None,
    }
}

/// 空的 `a{sv}` 附加属性字典（所有 IBus 对象序列化的固定第二字段）。
fn empty_props() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

/// `("IBusAttrList", a{sv}, av)` -> `(sa{sv}av)`。
///
/// 属性列表在本输入法里始终为空（TODO: 高亮/下划线等候选标注）。
fn attr_list_value() -> Value<'static> {
    Value::from(Structure::from((
        "IBusAttrList",
        empty_props(),
        Vec::<Value<'static>>::new(),
    )))
}

/// `("IBusText", a{sv}, 文本, 属性v)` -> `(sa{sv}sv)`。
fn text_value(text: &str) -> Value<'_> {
    Value::from(Structure::from((
        "IBusText",
        empty_props(),
        text,
        attr_list_value(),
    )))
}

/// 构造 IBusText 序列化（`commit_text` / `update_preedit_text` 信号用）。
pub fn ibus_text(text: &str) -> Result<OwnedValue, ProtoError> {
    Ok(Structure::from((
        "IBusText",
        empty_props(),
        text,
        attr_list_value(),
    ))
    .try_into()?)
}

/// 构造 IBusLookupTable 序列化（`update_lookup_table` 信号用）。
///
/// - `page_candidates`：当前页候选文本（按序）；
/// - `cursor_in_page`：高亮候选在当前页内的下标；
/// - `page_size`：每页候选数。
///
/// 类型串：`("IBusLookupTable", a{sv}, u, u, b, b, i, av, av)`
/// = `(sa{sv}uubbiavav)`。候选数组是 `av`（每个候选是完整的
/// IBusText 序列化，包在 variant 里），labels 留空让面板自动编号。
pub fn ibus_lookup_table(
    page_candidates: &[String],
    cursor_in_page: usize,
    page_size: usize,
) -> Result<OwnedValue, ProtoError> {
    let candidates: Vec<Value<'_>> = page_candidates.iter().map(|w| text_value(w)).collect();
    Ok(Structure::from((
        "IBusLookupTable",
        empty_props(),
        page_size as u32,
        cursor_in_page as u32,
        true, // cursor_visible
        true, // round：候选循环翻页
        0i32, // orientation：IBUS_ORIENTATION_HORIZONTAL
        candidates,
        Vec::<Value<'static>>::new(), // labels
    ))
    .try_into()?)
}

/// IBusEngineDesc 的 19 个字段。
///
/// 用命名字段结构体承载而非元组：`zvariant::Structure::from` 的元组
/// 实现只覆盖到 16 个元素，而引擎描述有 19 个字段。
/// 派生宏按字段声明顺序生成 `(...)` 签名与序列化，与 C 端逐字段对应。
#[derive(Serialize, zvariant::Type)]
struct EngineDescFields<'a> {
    type_name: &'a str,
    props: HashMap<&'a str, Value<'a>>,
    name: &'a str,
    longname: &'a str,
    description: &'a str,
    language: &'a str,
    license: &'a str,
    author: &'a str,
    icon: &'a str,
    layout: &'a str,
    rank: u32,
    hotkeys: &'a str,
    symbol: &'a str,
    setup: &'a str,
    layout_variant: &'a str,
    layout_option: &'a str,
    version: &'a str,
    textdomain: &'a str,
    icon_prop_key: &'a str,
}

/// 引擎描述：`("IBusEngineDesc", a{sv}, 8×s, u, 8×s)`
/// = `(sa{sv}ssssssssussssssss)`。
///
/// 字段顺序（ibus 1.5.29，跨应用兼容，勿改）：
/// name, longname, description, language, license, author, icon,
/// layout, rank(u), hotkeys, symbol, setup, layout_variant,
/// layout_option, version, textdomain, icon_prop_key。
///
/// 注意：结构体有 19 个字段，`Value::from` 的元组转换只支持到 16 个；
/// 且 `Value`/`OwnedValue` 的反序列化（`deserialize_any`）要求缓冲区
/// 以类型串开头。因此先把结构体序列化成 D-Bus 值字节，手工拼上
/// variant 头（`[len u8][签名][nul]`，对齐到 8），再反序列化成
/// 拥有所有权的 [`OwnedValue`]。
fn engine_desc_value() -> Result<OwnedValue, ProtoError> {
    let fields = EngineDescFields {
        type_name: "IBusEngineDesc",
        props: empty_props(),
        name: ENGINE_NAME, // name
        longname: "XWX 拼音",
        description: "XWX 拼音输入法（Rust 实现）",
        language: "zh_CN",
        license: "MIT",
        author: "xwx",
        icon: "",
        layout: "", // 使用默认键盘布局
        rank: 0,
        hotkeys: "",
        symbol: "",
        setup: "",
        layout_variant: "",
        layout_option: "",
        version: env!("CARGO_PKG_VERSION"),
        textdomain: "",
        icon_prop_key: "",
    };
    let ctx = zvariant::serialized::Context::new_dbus(zvariant::Endian::Little, 0);
    let sig: zvariant::Signature = "(sa{sv}ssssssssussssssss)".try_into()?;
    let values = zvariant::to_bytes_for_signature(ctx, sig.clone(), &fields)?;
    // D-Bus variant = ['g' 签名：len u8][签名][nul] + 对齐到 8 + 值
    let mut full = Vec::new();
    full.push(sig.len() as u8);
    full.extend_from_slice(sig.as_str().as_bytes());
    full.push(0);
    while full.len() % 8 != 0 {
        full.push(0);
    }
    full.extend_from_slice(&values);
    let data = zvariant::serialized::Data::new(&full, ctx);
    Ok(data.deserialize()?.0)
}

/// 完整组件：`("IBusComponent", a{sv}, 8×s, av, av)`
/// = `(sa{sv}ssssssssavav)`，用于 `RegisterComponent`。
///
/// 字段顺序（ibus 1.5.29）：name, description, version, license,
/// author, homepage, exec, textdomain, observed_paths(av), engines(av)。
/// 其中 name 同时是引擎进程要占用的 D-Bus 服务名。
pub fn component_value() -> Result<Value<'static>, ProtoError> {
    let engines = vec![Value::from(engine_desc_value()?)];
    Ok(Value::from(Structure::from((
        "IBusComponent",
        empty_props(),
        ENGINE_SERVICE, // name
        "XWX 拼音输入法",
        env!("CARGO_PKG_VERSION"),
        "MIT",
        "xwx",
        "", // homepage
        "", // exec：引擎由用户自行启动，无需 daemon 拉起
        "", // textdomain
        Vec::<Value<'static>>::new(), // observed_paths
        engines,                      // engines
    ))))
}

/// 生成标准 IBus component XML（供 `--xml` 模式与 `data/xwx.xml` 使用）。
///
/// 字段与 [`component_value`] / [`engine_desc_value`] 保持一致：
/// 组件名是引擎进程占用的 D-Bus 服务名 [`ENGINE_SERVICE`]，引擎名是
/// [`ENGINE_NAME`]（daemon 拿它调 `Factory.CreateEngine`）。
/// `exec` 传入引擎可执行文件的绝对路径（由调用方注入，确保 daemon
/// 拉起的进程与当前二进制一致）。
pub fn component_xml(exec: &str) -> String {
    let exec = if exec.is_empty() {
        "xwx_ime --ibus".to_string()
    } else {
        format!("{exec} --ibus")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<component>
  <name>{service}</name>
  <description>XWX 拼音输入法（Rust 实现）</description>
  <version>{version}</version>
  <license>MIT</license>
  <author>xwx</author>
  <homepage></homepage>
  <exec>{exec}</exec>
  <textdomain></textdomain>
  <engines>
    <engine>
      <name>{engine}</name>
      <longname>XWX 拼音</longname>
      <description>XWX 拼音输入法（Rust 实现）</description>
      <language>zh_CN</language>
      <license>MIT</license>
      <author>xwx</author>
      <icon></icon>
      <layout>us</layout>
      <rank>0</rank>
      <hotkeys></hotkeys>
      <symbol></symbol>
      <setup></setup>
      <layout_variant></layout_variant>
      <layout_option></layout_option>
      <version>{version}</version>
      <textdomain></textdomain>
      <icon_prop_key></icon_prop_key>
    </engine>
  </engines>
</component>
"#,
        service = ENGINE_SERVICE,
        engine = ENGINE_NAME,
        version = env!("CARGO_PKG_VERSION"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 ibus 1.5.29 反序列化端注册的类型串逐字对照。
    #[test]
    fn gvariant_signatures_match_ibus_1529() {
        let text = Structure::from(("IBusText", empty_props(), "你", attr_list_value()));
        assert_eq!(text.full_signature().as_str(), "(sa{sv}sv)");

        let attrs = Structure::from((
            "IBusAttrList",
            empty_props(),
            Vec::<Value<'static>>::new(),
        ));
        assert_eq!(attrs.full_signature().as_str(), "(sa{sv}av)");

        let table = Structure::from((
            "IBusLookupTable",
            empty_props(),
            9u32,
            0u32,
            true,
            true,
            0i32,
            Vec::<Value<'static>>::new(),
            Vec::<Value<'static>>::new(),
        ));
        assert_eq!(table.full_signature().as_str(), "(sa{sv}uubbiavav)");

        let desc = Structure::try_from(engine_desc_value().expect("引擎描述构造失败"))
            .expect("应为结构体");
        assert_eq!(desc.full_signature().as_str(), "(sa{sv}ssssssssussssssss)");

        let engines = vec![Value::from(desc)];
        let comp = Structure::from((
            "IBusComponent",
            empty_props(),
            "org.freedesktop.IBus.Engine.xwx", "", "", "MIT", "xwx", "", "", "",
            Vec::<Value<'static>>::new(),
            engines,
        ));
        assert_eq!(comp.full_signature().as_str(), "(sa{sv}ssssssssavav)");
    }

    #[test]
    fn translate_keyval_basics() {
        // 三段式·签名适配:原断言 `translate_keyval(keyval)`(单参),
        // 改为 `translate_keyval(keyval, state)`(双参,state=0 即
        // 无修饰键)。除大写那条外行为断言逐字不变——无修饰键时
        // 翻译结果与旧版完全一致。
        assert_eq!(
            translate_keyval(keyval::LOWER_A, 0).unwrap(),
            InputEvent::Char('a')
        );
        // 三段式·语义变更:原断言「大写 keyval 无条件归一为
        // Char('a')」;改成「无修饰键时大写 keyval 仍归一为
        // Char('a')」。新行为对的理由:旧版把 Shift+G 也归一成
        // 小写 g 进拼音串,`GPU` 永远打不出来;现在无修饰键的
        // 大写 keyval(ibus 某些路径下也会报大写)仍是拼音,
        // 而 Shift+大写走 Verbatim(见 translate_keyval_shift_caps)。
        assert_eq!(
            translate_keyval(keyval::UPPER_A, 0).unwrap(),
            InputEvent::Char('a'),
            "无修饰键时大写 keyval 仍归一为小写"
        );
        assert_eq!(
            translate_keyval(0x37, 0).unwrap(),
            InputEvent::Select(7),
            "数字 7 选第 7 个候选"
        );
        assert_eq!(translate_keyval(0xff00, 0).unwrap(), InputEvent::Space, "KP_Space 按空格处理");
        assert!(translate_keyval(0xffe9, 0).is_none(), "Shift 等修饰键不处理");
    }

    #[test]
    fn translate_keyval_shift_caps() {
        // Shift+小写 keyval(常见路径:ibus 报小写 keyval + SHIFT_MASK)
        // → Verbatim 大写,直接上屏,不进拼音串
        assert_eq!(
            translate_keyval(keyval::LOWER_A + 6, IBUS_SHIFT_MASK).unwrap(),
            InputEvent::Verbatim('G'),
            "Shift+g 应出 Verbatim('G')"
        );
        // Shift+大写 keyval(ibus 直接报大写 keyval 的路径)同上
        assert_eq!(
            translate_keyval(keyval::UPPER_A + 6, IBUS_SHIFT_MASK).unwrap(),
            InputEvent::Verbatim('G'),
            "Shift+G 应出 Verbatim('G')"
        );
        // CapsLock 开(无 Shift):小写 keyval → Verbatim 大写
        assert_eq!(
            translate_keyval(keyval::LOWER_A + 6, IBUS_LOCK_MASK).unwrap(),
            InputEvent::Verbatim('G'),
            "CapsLock 下 g 应出 Verbatim('G')"
        );
        // CapsLock + Shift 叠加:小写进拼音串(常规终端/编辑器语义,
        // 两者各翻转一次相互抵消;ibus 此时报的 keyval 本来就是小写)
        assert_eq!(
            translate_keyval(keyval::LOWER_A + 6, IBUS_SHIFT_MASK | IBUS_LOCK_MASK).unwrap(),
            InputEvent::Char('g'),
            "CapsLock+Shift+g 应回到小写拼音"
        );
        // Shift 不影响非字母键:数字仍是选词,标点仍原样放行
        assert_eq!(
            translate_keyval(0x37, IBUS_SHIFT_MASK).unwrap(),
            InputEvent::Select(7),
            "Shift+7 仍是选词(选词语义在会话层,不被 Shift 劫持)"
        );
        assert_eq!(
            translate_keyval(0x2c, IBUS_SHIFT_MASK).unwrap(),
            InputEvent::Char(','),
            "Shift+逗号仍走标点路径(上屏什么由会话层决定)"
        );
        // CapsLock 不影响数字/标点
        assert_eq!(
            translate_keyval(0x2e, IBUS_LOCK_MASK).unwrap(),
            InputEvent::Char('.'),
            "CapsLock 下句号不变"
        );
    }

    #[test]
    fn translate_keyval_punctuation() {
        // 标点键放行(原样半角字符,输出全角与否由会话层按模式决定)
        assert_eq!(translate_keyval(0x2c, 0), Some(InputEvent::Char(',')), "逗号");
        assert_eq!(translate_keyval(0x2e, 0), Some(InputEvent::Char('.')), "句号");
        assert_eq!(translate_keyval(0x3b, 0), Some(InputEvent::Char(';')), "分号");
        assert_eq!(translate_keyval(0x3a, 0), Some(InputEvent::Char(':')), "冒号");
        assert_eq!(translate_keyval(0x3f, 0), Some(InputEvent::Char('?')), "问号");
        assert_eq!(translate_keyval(0x21, 0), Some(InputEvent::Char('!')), "叹号");
        assert_eq!(translate_keyval(0x5c, 0), Some(InputEvent::Char('\\')), "反斜杠");
        assert_eq!(translate_keyval(0x3c, 0), Some(InputEvent::Char('<')), "小于号");
        assert_eq!(translate_keyval(0x3e, 0), Some(InputEvent::Char('>')), "大于号");
        assert_eq!(translate_keyval(0x24, 0), Some(InputEvent::Char('$')), "美元号");
        assert_eq!(translate_keyval(0x5e, 0), Some(InputEvent::Char('^')), "抑扬符");
        assert_eq!(translate_keyval(0x5f, 0), Some(InputEvent::Char('_')), "下划线");
        assert_eq!(translate_keyval(0x7e, 0), Some(InputEvent::Char('~')), "波浪号");
        // `'`(0x27)也放行——它是 B2 音节分隔符预留键,会话层对它 Noop
        // 透传,与本轮之前「translate 返回 None 透传」的可观察行为一致
        assert_eq!(translate_keyval(0x27, 0), Some(InputEvent::Char('\'')), "单引号(B2 预留)");
        // DEL 与控制字符仍不识别
        assert!(translate_keyval(0x7f, 0).is_none(), "DEL 不处理");
        assert!(translate_keyval(0x09, 0).is_none(), "Tab 不处理");
    }

    #[test]
    fn text_builder_roundtrip_to_owned() {
        let text = ibus_text("你好").expect("构造不应失败");
        // OwnedValue 解出结构体后字段可读回
        let structure = Structure::try_from(text).expect("应为结构体");
        let fields = structure.fields();
        assert_eq!(
            fields[0].downcast_ref::<&str>().expect("应为字符串"),
            "IBusText"
        );
        assert_eq!(
            fields[2].downcast_ref::<&str>().expect("应为字符串"),
            "你好"
        );
    }
}
