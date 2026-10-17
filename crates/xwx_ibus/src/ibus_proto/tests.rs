use super::*;

/// 中/英属性的线上形状必须与 ibus 1.5.29 的 `ibus_property_serialize`
/// 逐字段对齐：本机唯一的钉法（真面板跑在 GNOME Shell 里），
/// 错一处面板就静默忽略或报错。
///
/// 类型串 = `("IBusProperty", a{sv}, s:key, u:type, v:label, s:icon,
/// v:tooltip, b:sensitive, b:visible, u:state, v:sub_props, v:symbol)`。
#[test]
fn mode_property_matches_the_upstream_wire_shape() {
    let prop = ibus_property(PROP_KEY_MODE, "中", "中文", IBUS_PROP_STATE_UNCHECKED)
        .expect("属性构造失败");
    assert_eq!(
        prop.value_signature().to_string(),
        "(sa{sv}suvsvbbuvv)",
        "IBusProperty 字段顺序/类型必须与上游一致"
    );
    let list = ibus_prop_list(PROP_KEY_MODE, "英", "English", IBUS_PROP_STATE_CHECKED)
        .expect("属性表构造失败");
    assert_eq!(
        list.value_signature().to_string(),
        "(sa{sv}av)",
        "IBusPropList 必须是 (类型名, 附加属性, 属性数组)"
    );
}

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
    // → Verbatim 大写,进串组合
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
    // `'`(0x27)也放行——B2 音节分隔符(会话层:拼音串非空时进拼音串
    // 当强制切分点,空闲时透传)
    assert_eq!(translate_keyval(0x27, 0), Some(InputEvent::Char('\'')), "单引号(B2 音节分隔符)");
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
