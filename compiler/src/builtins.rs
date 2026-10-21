//! Scratch's documented DSL templates with explicit runtime slot metadata.
use crate::model::*;
use crate::registry::{input, tokens_from_text};
use serde_json::json;

// Runtime-owned blocks that are not exposed by the current DSL template table.
// Keep these reserved too: a local extension must not replace VM primitives,
// prototype blocks, or editor-only shadows by reusing their complete opcode.
pub(crate) const RESERVED_OPCODES: &[&str] = &[
    "argument_reporter_boolean",
    "argument_reporter_string_number",
    "control_all_at_once",
    "control_clear_counter",
    "control_for_each",
    "control_get_counter",
    "control_if_else",
    "control_incr_counter",
    "control_repeat_until",
    "data_hidelist",
    "data_listcontents",
    "data_showlist",
    "data_variable",
    "event_whenbackdropswitchesto",
    "event_whengreaterthan",
    "event_whentouchingobject",
    "looks_backdropnumbername",
    "looks_changeeffectby",
    "looks_changestretchby",
    "looks_cleargraphiceffects",
    "looks_costumenumbername",
    "looks_goforwardbackwardlayers",
    "looks_gotofrontback",
    "looks_hideallsprites",
    "looks_nextbackdrop",
    "looks_seteffectto",
    "looks_setstretchto",
    "looks_switchbackdroptoandwait",
    "motion_align_scene",
    "motion_glideto",
    "motion_glideto_menu",
    "motion_pointtowards",
    "motion_pointtowards_menu",
    "motion_scroll_right",
    "motion_scroll_up",
    "motion_setrotationstyle",
    "motion_xscroll",
    "motion_yscroll",
    "procedures_call",
    "procedures_definition",
    "procedures_prototype",
    "sensing_coloristouchingcolor",
    "sensing_current",
    "sensing_dayssince2000",
    "sensing_loud",
    "sensing_loudness",
    "sensing_of",
    "sensing_of_object_menu",
    "sensing_setdragmode",
    "sensing_touchingcolor",
    "sensing_userid",
    "sensing_username",
    "sound_beats_menu",
    "sound_changeeffectby",
    "sound_changevolumeby",
    "sound_cleareffects",
    "sound_effects_menu",
    "sound_play",
    "sound_playuntildone",
    "sound_seteffectto",
    "sound_setvolumeto",
    "sound_sounds_menu",
    "sound_stopallsounds",
    "sound_volume",
    "math_number",
    "math_integer",
    "math_whole_number",
    "math_positive_number",
    "math_angle",
    "colour_picker",
    "text",
];

fn field(name: &str, kind: ArgType) -> Slot {
    let mut s = input(name, kind);
    s.storage = Storage::Field;
    s.shadow = None;
    s
}
fn menu(
    name: &str,
    opcode: Option<&str>,
    items: Option<Vec<(String, String)>>,
    default: &str,
) -> Slot {
    let accepts = opcode.is_some();
    Slot {
        name: name.into(),
        arg_type: ArgType::String,
        storage: if accepts {
            Storage::Input
        } else {
            Storage::Field
        },
        default: json!(default),
        shadow: opcode.map(|op| Shadow {
            opcode: op.into(),
            field: name.into(),
            primitive: None,
        }),
        menu: Some(Menu {
            items,
            accept_reporters: accepts,
        }),
    }
}
fn options(items: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
    Some(
        items
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    )
}
fn num(name: &str) -> Slot {
    input(name, ArgType::Number)
}
fn any(name: &str) -> Slot {
    input(name, ArgType::Any)
}
fn boolean(name: &str) -> Slot {
    input(name, ArgType::Boolean)
}
fn variable() -> Slot {
    field("VARIABLE", ArgType::Variable)
}
fn list() -> Slot {
    field("LIST", ArgType::List)
}

pub fn templates() -> Vec<Template> {
    use Shape::*;
    let mut all = Vec::new();
    let mut add = |opcode: &str, shape, text: &str, slots: Vec<Slot>, sprite_only| {
        all.push(Template {
            opcode: opcode.into(),
            branch_count: if shape == Control {
                if opcode == "control_if_else" { 2 } else { 1 }
            } else {
                0
            },
            shape,
            namespace: None,
            extension_id: None,
            local_opcode: None,
            tokens: tokens_from_text(text, &slots).expect("valid builtin template"),
            sprite_only,
            stage_only: opcode == "event_whenstageclicked",
            procedure: None,
            warp: false,
        });
    };
    add(
        "motion_movesteps",
        Command,
        "移动 [STEPS] 步",
        vec![num("STEPS")],
        true,
    );
    add(
        "motion_turnright",
        Command,
        "右转 [DEGREES] 度",
        vec![num("DEGREES")],
        true,
    );
    add(
        "motion_turnleft",
        Command,
        "左转 [DEGREES] 度",
        vec![num("DEGREES")],
        true,
    );
    add(
        "motion_gotoxy",
        Command,
        "移到 x: [X] y: [Y]",
        vec![num("X"), num("Y")],
        true,
    );
    add(
        "motion_goto",
        Command,
        "移到 [TO]",
        vec![menu("TO", Some("motion_goto_menu"), None, "_random_")],
        true,
    );
    add(
        "motion_glidesecstoxy",
        Command,
        "在 [SECS] 秒内滑行到 x: [X] y: [Y]",
        vec![num("SECS"), num("X"), num("Y")],
        true,
    );
    add(
        "motion_pointindirection",
        Command,
        "面向 [DIRECTION] 方向",
        vec![input("DIRECTION", ArgType::Angle)],
        true,
    );
    add(
        "motion_changexby",
        Command,
        "将 x 坐标增加 [DX]",
        vec![num("DX")],
        true,
    );
    add(
        "motion_setx",
        Command,
        "将 x 坐标设为 [X]",
        vec![num("X")],
        true,
    );
    add(
        "motion_changeyby",
        Command,
        "将 y 坐标增加 [DY]",
        vec![num("DY")],
        true,
    );
    add(
        "motion_sety",
        Command,
        "将 y 坐标设为 [Y]",
        vec![num("Y")],
        true,
    );
    add(
        "motion_ifonedgebounce",
        Command,
        "碰到边缘就反弹",
        vec![],
        true,
    );
    for (op, text) in [
        ("motion_xposition", "x 坐标"),
        ("motion_yposition", "y 坐标"),
        ("motion_direction", "方向"),
    ] {
        add(op, Reporter, text, vec![], true);
    }

    add(
        "looks_sayforsecs",
        Command,
        "说 [MESSAGE] [SECS] 秒",
        vec![any("MESSAGE"), num("SECS")],
        true,
    );
    add(
        "looks_say",
        Command,
        "说 [MESSAGE]",
        vec![any("MESSAGE")],
        true,
    );
    add(
        "looks_thinkforsecs",
        Command,
        "思考 [MESSAGE] [SECS] 秒",
        vec![any("MESSAGE"), num("SECS")],
        true,
    );
    add(
        "looks_think",
        Command,
        "思考 [MESSAGE]",
        vec![any("MESSAGE")],
        true,
    );
    add(
        "looks_switchcostumeto",
        Command,
        "换成 [COSTUME] 造型",
        vec![menu("COSTUME", Some("looks_costume"), None, "造型1")],
        true,
    );
    add("looks_nextcostume", Command, "下一个造型", vec![], true);
    add(
        "looks_switchbackdropto",
        Command,
        "换成 [BACKDROP] 背景",
        vec![menu("BACKDROP", Some("looks_backdrops"), None, "背景1")],
        false,
    );
    add(
        "looks_changesizeby",
        Command,
        "将大小增加 [CHANGE]",
        vec![num("CHANGE")],
        true,
    );
    add(
        "looks_setsizeto",
        Command,
        "将大小设为 [SIZE] %",
        vec![num("SIZE")],
        true,
    );
    add("looks_show", Command, "显示", vec![], true);
    add("looks_hide", Command, "隐藏", vec![], true);
    add("looks_size", Reporter, "大小", vec![], true);

    let mut keys = options(&[
        ("空格", "space"),
        ("上移键", "up arrow"),
        ("下移键", "down arrow"),
        ("左移键", "left arrow"),
        ("右移键", "right arrow"),
        ("任意键", "any"),
    ])
    .unwrap();
    keys.extend(
        ('a'..='z')
            .chain('0'..='9')
            .map(|c| (c.to_string(), c.to_string())),
    );
    add("event_whenflagclicked", Hat, "start", vec![], false);
    add(
        "event_whenthisspriteclicked",
        Hat,
        "当角色被点击",
        vec![],
        true,
    );
    add("event_whenstageclicked", Hat, "当舞台被点击", vec![], false);
    add(
        "event_whenkeypressed",
        Hat,
        "当按下 [KEY_OPTION] 键",
        vec![menu("KEY_OPTION", None, Some(keys.clone()), "space")],
        false,
    );
    add(
        "event_whenbroadcastreceived",
        Hat,
        "当收到广播 [BROADCAST_OPTION]",
        vec![field("BROADCAST_OPTION", ArgType::Broadcast)],
        false,
    );
    add(
        "event_broadcast",
        Command,
        "广播 [BROADCAST_INPUT]",
        vec![input("BROADCAST_INPUT", ArgType::Broadcast)],
        false,
    );
    add(
        "event_broadcastandwait",
        Command,
        "广播 [BROADCAST_INPUT] 并等待",
        vec![input("BROADCAST_INPUT", ArgType::Broadcast)],
        false,
    );
    add(
        "control_wait",
        Command,
        "等待 [DURATION] 秒",
        vec![num("DURATION")],
        false,
    );
    add(
        "control_wait_until",
        Command,
        "等待直到 [CONDITION]",
        vec![boolean("CONDITION")],
        false,
    );
    add(
        "control_stop",
        Command,
        "停止 [STOP_OPTION]",
        vec![menu(
            "STOP_OPTION",
            None,
            options(&[
                ("全部脚本", "all"),
                ("这个脚本", "this script"),
                ("该角色的其他脚本", "other scripts in sprite"),
            ]),
            "all",
        )],
        false,
    );
    add(
        "control_start_as_clone",
        Hat,
        "当作为克隆体启动时",
        vec![],
        true,
    );
    add(
        "control_create_clone_of",
        Command,
        "克隆 [CLONE_OPTION]",
        vec![menu(
            "CLONE_OPTION",
            Some("control_create_clone_of_menu"),
            None,
            "_myself_",
        )],
        false,
    );
    add(
        "control_delete_this_clone",
        Command,
        "删除此克隆体",
        vec![],
        true,
    );
    add(
        "control_repeat",
        Control,
        "loop [TIMES]",
        vec![num("TIMES")],
        false,
    );
    add("control_forever", Control, "while true", vec![], false);
    add(
        "control_while",
        Control,
        "while [CONDITION]",
        vec![boolean("CONDITION")],
        false,
    );
    for keyword in ["if", "elif"] {
        add(
            "control_if",
            Control,
            &format!("{keyword} [CONDITION]"),
            vec![boolean("CONDITION")],
            false,
        );
    }
    add(
        "sensing_touchingobject",
        Boolean,
        "碰到 [TOUCHINGOBJECTMENU] ?",
        vec![menu(
            "TOUCHINGOBJECTMENU",
            Some("sensing_touchingobjectmenu"),
            None,
            "_mouse_",
        )],
        true,
    );
    add(
        "sensing_keypressed",
        Boolean,
        "按下 [KEY_OPTION] 键?",
        vec![menu(
            "KEY_OPTION",
            Some("sensing_keyoptions"),
            Some(keys),
            "space",
        )],
        false,
    );
    add("sensing_mousedown", Boolean, "鼠标按下?", vec![], false);
    add(
        "sensing_distanceto",
        Reporter,
        "到 [DISTANCETOMENU] 的距离",
        vec![menu(
            "DISTANCETOMENU",
            Some("sensing_distancetomenu"),
            None,
            "_mouse_",
        )],
        true,
    );
    add(
        "sensing_askandwait",
        Command,
        "询问 [QUESTION] 并等待",
        vec![any("QUESTION")],
        false,
    );
    for (op, text) in [
        ("sensing_answer", "回答"),
        ("sensing_mousex", "鼠标的 x 坐标"),
        ("sensing_mousey", "鼠标的 y 坐标"),
        ("sensing_timer", "计时器"),
    ] {
        add(op, Reporter, text, vec![], false);
    }
    add("sensing_resettimer", Command, "计时器归零", vec![], false);

    for (op, text) in [
        ("operator_add", "[NUM1] + [NUM2]"),
        ("operator_subtract", "[NUM1] - [NUM2]"),
        ("operator_multiply", "[NUM1] * [NUM2]"),
        ("operator_divide", "[NUM1] / [NUM2]"),
        ("operator_mod", "[NUM1] % [NUM2]"),
        ("operator_mod", "[NUM1] 除以 [NUM2] 的余数"),
    ] {
        add(op, Reporter, text, vec![num("NUM1"), num("NUM2")], false);
    }
    add(
        "operator_random",
        Reporter,
        "在 [FROM] 和 [TO] 之间取随机数",
        vec![num("FROM"), num("TO")],
        false,
    );
    for (op, text) in [
        ("operator_gt", "[OPERAND1] > [OPERAND2]"),
        ("operator_lt", "[OPERAND1] < [OPERAND2]"),
        ("operator_equals", "[OPERAND1] = [OPERAND2]"),
    ] {
        add(
            op,
            Boolean,
            text,
            vec![any("OPERAND1"), any("OPERAND2")],
            false,
        );
    }
    add(
        "operator_and",
        Boolean,
        "[OPERAND1] 与 [OPERAND2]",
        vec![boolean("OPERAND1"), boolean("OPERAND2")],
        false,
    );
    add(
        "operator_or",
        Boolean,
        "[OPERAND1] 或 [OPERAND2]",
        vec![boolean("OPERAND1"), boolean("OPERAND2")],
        false,
    );
    add(
        "operator_not",
        Boolean,
        "不成立 [OPERAND]",
        vec![boolean("OPERAND")],
        false,
    );
    add(
        "operator_join",
        Reporter,
        "连接 [STRING1] 和 [STRING2]",
        vec![any("STRING1"), any("STRING2")],
        false,
    );
    add(
        "operator_letter_of",
        Reporter,
        "[STRING] 的第 [LETTER] 个字符",
        vec![any("STRING"), num("LETTER")],
        false,
    );
    add(
        "operator_length",
        Reporter,
        "[STRING] 的字符数",
        vec![any("STRING")],
        false,
    );
    add(
        "operator_contains",
        Boolean,
        "[STRING1] 包含 [STRING2] ?",
        vec![any("STRING1"), any("STRING2")],
        false,
    );
    add(
        "operator_round",
        Reporter,
        "四舍五入 [NUM]",
        vec![num("NUM")],
        false,
    );
    let math_ops = [
        "abs", "floor", "ceiling", "sqrt", "sin", "cos", "tan", "asin", "acos", "atan", "ln",
        "log", "e ^", "10 ^",
    ];
    let mut math_items: Vec<_> = math_ops
        .iter()
        .map(|s| (s.to_string(), s.to_string()))
        .collect();
    math_items.extend([
        ("绝对值".into(), "abs".into()),
        ("向下取整".into(), "floor".into()),
        ("向上取整".into(), "ceiling".into()),
        ("平方根".into(), "sqrt".into()),
    ]);
    add(
        "operator_mathop",
        Reporter,
        "[OPERATOR] [NUM]",
        vec![menu("OPERATOR", None, Some(math_items), "abs"), num("NUM")],
        false,
    );

    add(
        "data_setvariableto",
        Command,
        "[VARIABLE] = [VALUE]",
        vec![variable(), any("VALUE")],
        false,
    );
    add(
        "data_changevariableby",
        Command,
        "[VARIABLE] += [VALUE]",
        vec![variable(), num("VALUE")],
        false,
    );
    add(
        "data_showvariable",
        Command,
        "显示变量 [VARIABLE]",
        vec![variable()],
        false,
    );
    add(
        "data_hidevariable",
        Command,
        "隐藏变量 [VARIABLE]",
        vec![variable()],
        false,
    );
    add(
        "data_addtolist",
        Command,
        "将 [ITEM] 加入 [LIST]",
        vec![any("ITEM"), list()],
        false,
    );
    add(
        "data_deleteoflist",
        Command,
        "删除 [LIST] 的第 [INDEX] 项",
        vec![list(), any("INDEX")],
        false,
    );
    add(
        "data_deletealloflist",
        Command,
        "删除 [LIST] 的全部项目",
        vec![list()],
        false,
    );
    add(
        "data_insertatlist",
        Command,
        "在 [LIST] 的第 [INDEX] 项前插入 [ITEM]",
        vec![list(), any("INDEX"), any("ITEM")],
        false,
    );
    add(
        "data_replaceitemoflist",
        Command,
        "将 [LIST] 的第 [INDEX] 项替换为 [ITEM]",
        vec![list(), any("INDEX"), any("ITEM")],
        false,
    );
    add(
        "data_itemoflist",
        Reporter,
        "[LIST] 的第 [INDEX] 项",
        vec![list(), any("INDEX")],
        false,
    );
    add(
        "data_itemnumoflist",
        Reporter,
        "[LIST] 中第一个 [ITEM] 的编号",
        vec![list(), any("ITEM")],
        false,
    );
    add(
        "data_lengthoflist",
        Reporter,
        "[LIST] 的项目数",
        vec![list()],
        false,
    );
    add(
        "data_listcontainsitem",
        Boolean,
        "[LIST] 包含 [ITEM] ?",
        vec![list(), any("ITEM")],
        false,
    );
    all
}
