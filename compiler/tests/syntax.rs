mod common;
use common::assert_parent_edges;
use sbdsl::{
    analysis::{AnalysisCache, analyze_document},
    build, config,
    model::{Expr, Invocation, Item, Shape, Statement, Token},
    parser::{
        assignment_parts, parse, parse_recovering, parse_reference, scan_atom, strip_comments,
    },
    registry::{Registry, procedure_template},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

fn compile(source: &str) -> sbdsl::Result<sbdsl::model::GeneratedProject> {
    let directory = tempfile::tempdir().unwrap();
    common::compile_at(directory.path(), source)
}

fn input_block<'a>(blocks: &'a Value, block: &Value, name: &str) -> &'a Value {
    &blocks[block["inputs"][name][1].as_str().unwrap()]
}

fn expression_tree(expr: &Expr) -> String {
    match expr {
        Expr::Number(value) => value.to_string(),
        Expr::Symbol { name, global, .. } => {
            format!("${}{}", if *global { "global:" } else { "" }, name)
        }
        Expr::Param { name, .. } => format!("@{name}"),
        Expr::Call(call) => format!(
            "{}({})",
            call.template
                .opcode
                .strip_prefix("operator_")
                .unwrap_or(&call.template.opcode),
            call.args
                .iter()
                .map(expression_tree)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => panic!("unexpected arithmetic operand: {other:?}"),
    }
}

#[test]
fn arithmetic_precedence_associativity_grouping_and_symbol_operands() {
    let mut registry = Registry::new();
    for (source, expected) in [
        (
            "(a + b - c * d / f % e)",
            "subtract(add($a,$b),mod(divide(multiply($c,$d),$f),$e))",
        ),
        ("(a - (b - c))", "subtract($a,subtract($b,$c))"),
        ("((a + b) * c)", "multiply(add($a,$b),$c)"),
        ("(24 / 3 / 2)", "divide(divide(24,3),2)"),
        ("(24 / (3 / 2))", "divide(24,divide(3,2))"),
        ("(18 % 5 * 2)", "multiply(mod(18,5),2)"),
        ("(-a / b)", "divide(subtract(0,$a),$b)"),
        ("(a / -b)", "divide($a,subtract(0,$b))"),
        ("(1e-5 - -2 * 3)", "subtract(0.00001,multiply(-2,3))"),
        (
            "(-a + -(b * c))",
            "add(subtract(0,$a),subtract(0,multiply($b,$c)))",
        ),
        (
            "(global:a + @step * b)",
            "add($global:a,multiply(@step,$b))",
        ),
        ("(计数 / 2 + 余数 % 3)", "add(divide($计数,2),mod($余数,3))"),
        ("(1 + 6 / 3 * 2)", "add(1,multiply(divide(6,3),2))"),
        ("((1 + 2) * 3)", "multiply(add(1,2),3)"),
        ("(1)", "1"),
        ("(a)", "$a"),
        ("((a))", "$a"),
    ] {
        let value = registry
            .value(source, &[])
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(expression_tree(&value), expected, "{source}");
    }
    let Invocation { template, .. } = registry.parse("(1 + 2 * 3)", Shape::Reporter, &[]).unwrap();
    assert_eq!(template.opcode, "operator_add");
    assert_eq!(
        expression_tree(&registry.value("((计时器) + 1)", &[]).unwrap()),
        "add(sensing_timer(),1)"
    );
    for source in ["(计时器)", "((计时器))", "(((计时器)))"] {
        assert_eq!(
            expression_tree(&registry.value(source, &[]).unwrap()),
            "sensing_timer()",
            "{source}"
        );
    }
    let mut reporter = registry
        .templates
        .iter()
        .find(|template| template.opcode == "sensing_timer")
        .unwrap()
        .clone();
    reporter.opcode = "first_reporter".into();
    reporter.tokens = vec![Token::Literal("a".into())];
    registry.templates.push(reporter.clone());
    for source in ["(a)", "((a))"] {
        assert_eq!(
            expression_tree(&registry.value(source, &[]).unwrap()),
            "first_reporter()",
            "{source}"
        );
    }
    reporter.opcode = "second_reporter".into();
    registry.templates.push(reporter);
    for source in ["(a)", "((a))", "((a) + 1)"] {
        let error = registry.value(source, &[]).unwrap_err();
        assert!(error.0.contains("积木调用有歧义"), "{source}: {error}");
    }
}

#[test]
fn comment_scanning_keeps_json_escapes_confined_to_init_suites() {
    let source = "sprite \"角色\":\n    init:\n        costumes: [{name: \"\\u4e2d#文\", path: \"asset.svg\"}] # init comment\n    start:\n        说 (\"ok#text\") # command comment\n";
    let clean = strip_comments(source).unwrap();
    assert_eq!(clean.len(), source.len());
    assert!(clean.contains(r"\u4e2d#文"));
    assert!(clean.contains("ok#text"));
    assert!(!clean.contains("init comment"));
    assert!(!clean.contains("command comment"));
    assert!(strip_comments(&source.replace("ok#text", r"\u4e2d")).is_err());
}

#[test]
fn assignments_lower_precedence_and_compound_operators() {
    for (source, target, operator, value) in [
        ("score=10", "score", "=", "10"),
        (" score += a+b*2 ", "score", "+=", "a+b*2"),
        ("global:score/=2", "global:score", "/=", "2"),
        ("玩家_１ %= 3", "玩家_１", "%=", "3"),
        (r#"global:score = "a=b""#, "global:score", "=", r#""a=b""#),
        ("score = ", "score", "=", ""),
    ] {
        let assignment = assignment_parts(source).unwrap();
        assert_eq!(assignment.target, target, "{source}");
        assert_eq!(assignment.operator, operator, "{source}");
        assert_eq!(assignment.value, value, "{source}");
    }
    for source in ["说 (\"a=b\")", "var score = 0", "<score = 10>", "score + 1"] {
        assert!(assignment_parts(source).is_none(), "{source}");
    }

    let registry = Registry::new();
    let assigned = registry
        .parse("score = a+b-c*d/f%e", Shape::Command, &[])
        .unwrap();
    assert_eq!(assigned.template.opcode, "data_setvariableto");
    assert_eq!(
        expression_tree(&assigned.args[1]),
        "subtract(add($a,$b),mod(divide(multiply($c,$d),$f),$e))"
    );
    let changed = registry
        .parse("score += 2*3+1", Shape::Command, &[])
        .unwrap();
    assert_eq!(changed.template.opcode, "data_changevariableby");
    assert_eq!(expression_tree(&changed.args[1]), "add(multiply(2,3),1)");
    for (operator, opcode) in [
        ("-=", "subtract"),
        ("*=", "multiply"),
        ("/=", "divide"),
        ("%=", "mod"),
    ] {
        let source = format!("global:score {operator} 1+2");
        let assigned = registry.parse(&source, Shape::Command, &[]).unwrap();
        assert_eq!(assigned.template.opcode, "data_setvariableto");
        assert_eq!(
            expression_tree(&assigned.args[1]),
            format!("{opcode}($global:score,add(1,2))")
        );
    }
    assert_eq!(
        expression_tree(
            &registry
                .parse("score = (a + b)*-c/2", Shape::Command, &[])
                .unwrap()
                .args[1]
        ),
        "divide(multiply(add($a,$b),subtract(0,$c)),2)"
    );
}

#[test]
fn assignments_accept_values_and_reject_old_commands_and_invalid_targets() {
    let mut registry = Registry::new();
    let mut reporter = procedure_template("值 @n", "unused").unwrap();
    reporter.shape = Shape::Reporter;
    reporter.namespace = Some("ext".into());
    reporter.local_opcode = Some("read".into());
    reporter.opcode = "ext_read".into();
    reporter.procedure = None;
    registry.templates.push(reporter);
    for source in [
        "score = 1",
        "score = \"a+b\"",
        "score = other",
        "score = (计时器)",
        "score = <true>",
        "score = <other > 0>",
        "score = @value",
        "score = <@ready>",
        "score = (ext.值 (1))",
        "score = (ext::\"read\"((1)))+2",
        "计数 = global:total / 2",
        "if = 1",
        "loop=1",
        "var = 1",
        "start=1",
    ] {
        assert!(
            registry.parse(source, Shape::Command, &[]).is_ok(),
            "{source}"
        );
    }
    for source in [
        "将 (score) 设为 (10)",
        "将 (score) 增加 (1)",
        "(score) = (10)",
        "(score) += (1)",
        "score =",
        "score == 1",
        "score := 1",
        "score **= 2",
        "score //= 2",
        "score = 1 +",
        "score = a b",
        "score = a = 1",
        "score = 'menu'",
        "list:items = 1",
        "global:list:items += 1",
        "var:score = 1",
        "var:\"a b\" = 1",
        "global:\"score\" = 1",
        "@value = 1",
        "(计时器) = 1",
        "\"score\" = 1",
        "1 = 2",
        "$score = 1",
    ] {
        assert!(
            registry.parse(source, Shape::Command, &[]).is_err(),
            "{source}"
        );
    }
}

#[test]
fn assignment_contexts_spans_recovery_and_multiline_boundaries() {
    let source = "def 改变 @value:\n    global:score = @value\nend\nstart:\n    计数 += 2\n    @value = 1\n    score =\n    score /= 2\nend\n";
    let report = parse_recovering(Path::new("assign.sbdsl"), source);
    assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
    let Item::Script { body, .. } = &report.items[1] else {
        panic!("expected recovered start script");
    };
    assert_eq!(body.len(), 2);
    let Statement::Command {
        source: command,
        span,
    } = &body[0]
    else {
        panic!("expected assignment command");
    };
    assert_eq!(command, "计数 += 2");
    assert_eq!((span.line, span.column), (5, 5));
    assert_eq!(&source[span.start..span.end], command);
    assert_eq!(report.errors[0].span().unwrap().line, 6);
    assert_eq!(report.errors[1].span().unwrap().line, 7);

    let mut source = String::from("start:\n");
    for _ in 0..100 {
        source.push_str("    flag = <score < limit>\n");
    }
    for expression in [
        "<score >\n        0>",
        "<score >\n        global:limit>",
        "<score >\n        global>",
        "<score >\n        @limit>",
        "<<score < limit> 与\n        <score > 0>>",
        "<(score +\n        1) > limit>",
    ] {
        source.push_str(&format!("    flag = {expression}\n"));
    }
    source.push_str("    说 (flag)\nend\n");
    let items = parse(Path::new("assign.sbdsl"), &source).unwrap();
    let Item::Script { body, .. } = &items[0] else {
        panic!("expected start script");
    };
    assert_eq!(body.len(), 107);
}

#[test]
fn bare_and_qualified_references_keep_names_scopes_and_template_precedence() {
    let mut registry = Registry::new();
    for (source, name, global) in [
        ("score", "score", false),
        ("玩家_１", "玩家_１", false),
        ("global:score", "score", true),
        ("logs", "logs", false),
        ("global:logs", "logs", true),
    ] {
        assert_eq!(scan_atom(source, 0).unwrap(), source.len(), "{source}");
        assert_eq!(
            parse_reference(source).unwrap(),
            (name.to_owned(), global),
            "{source}"
        );
        assert_eq!(
            registry.value(source, &[]).unwrap(),
            Expr::Symbol {
                name: name.into(),
                list: false,
                global,
            },
            "{source}"
        );
    }
    for source in [
        r#""a b""#,
        "var:score",
        "list:logs",
        "global:list:logs",
        r#"global:"a b""#,
        "$score",
        "$global:score",
        "global:",
        "list:",
        "var:",
        "a.b",
        "a-b",
        "1score",
    ] {
        assert!(parse_reference(source).is_err(), "{source}");
    }

    let fallback = procedure_template("选择 @value", "generic").unwrap();
    let fixed = procedure_template("选择 固定", "fixed").unwrap();
    assert_eq!(
        registry
            .parse(
                "选择 固定",
                Shape::Command,
                &[fallback.clone(), fixed.clone()]
            )
            .unwrap()
            .template
            .procedure
            .as_deref(),
        Some("fixed")
    );
    assert_eq!(
        registry
            .parse("选择(固定)", Shape::Command, &[fallback, fixed])
            .unwrap()
            .template
            .procedure
            .as_deref(),
        Some("generic")
    );
    for label in ["list:", "var:", "global:", "global:list:"] {
        let template = procedure_template(&format!("检查 {label} @value"), "check").unwrap();
        let invocation = registry
            .parse(
                &format!("检查 {label} (分数)"),
                Shape::Command,
                std::slice::from_ref(&template),
            )
            .unwrap();
        assert_eq!(
            invocation.args,
            [Expr::Symbol {
                name: "分数".into(),
                list: false,
                global: false,
            }]
        );
    }
    let mut template = procedure_template("选择 <@ready>: @value", "select").unwrap();
    template.shape = Shape::Boolean;
    template.namespace = Some("ext".into());
    template.procedure = None;
    registry.templates.push(template);
    for condition in ["<true>", "<score > 0>"] {
        let value = format!("<ext.选择 {condition}:\n        (1)>");
        assert_eq!(scan_atom(&value, 0).unwrap(), value.len());
        assert!(registry.value(&value, &[]).is_ok(), "{value}");
        assert!(
            parse(
                Path::new("test.sbdsl"),
                &format!("start:\n    if {value}:\n        说 (1)\n    end\nend\n")
            )
            .is_ok()
        );
    }
}

#[test]
fn typed_matching_and_inferred_data_kinds_resolve_lists_and_reject_conversions() {
    let registry = Registry::new();
    let types = BTreeMap::from([
        (("score".into(), false), false),
        (("text".into(), false), false),
        (("logs".into(), false), true),
        (("logs".into(), true), true),
    ]);
    for (source, opcode) in [
        ("<(logs) 包含 (\"x\") ?>", "data_listcontainsitem"),
        ("<(text) 包含 (\"x\") ?>", "operator_contains"),
    ] {
        let call = registry
            .parse_typed(source, Shape::Boolean, &[], &types)
            .unwrap();
        assert_eq!(call.template.opcode, opcode);
    }
    for source in [
        "logs = 1",
        "logs += 1",
        "logs *= 2",
        "score = logs",
        "score += logs",
        "score = logs+1",
        "说 (logs)",
        "将 (1) 加入 (score)",
        "将 (logs) 加入 (logs)",
        "显示变量 (logs)",
    ] {
        assert!(
            registry
                .parse_typed(source, Shape::Command, &[], &types)
                .is_err(),
            "{source}"
        );
    }

    let project = compile(
        "score = 10\nitems = {1, \"two\"}\nstart:\n    fresh = {3, \"four\"}\n    score += 1\n    fresh = {}\nend\n",
    )
    .unwrap();
    let stage = &project.json["targets"][0];
    assert!(
        stage["variables"]
            .as_object()
            .unwrap()
            .values()
            .any(|value| value == &json!(["score", 10]))
    );
    assert!(
        stage["lists"]
            .as_object()
            .unwrap()
            .values()
            .any(|value| value == &json!(["items", [1, "two"]]))
    );
    assert!(
        stage["lists"]
            .as_object()
            .unwrap()
            .values()
            .any(|value| value == &json!(["fresh", []]))
    );
    let blocks = stage["blocks"].as_object().unwrap();
    let hat = blocks
        .values()
        .find(|block| block["opcode"] == "event_whenflagclicked")
        .unwrap();
    let mut next = hat["next"].as_str();
    let mut sequence = Vec::new();
    while let Some(id) = next {
        let block = &blocks[id];
        sequence.push(block["opcode"].as_str().unwrap());
        next = block["next"].as_str();
    }
    assert_eq!(
        sequence,
        [
            "data_deletealloflist",
            "data_addtolist",
            "data_addtolist",
            "data_changevariableby",
            "data_deletealloflist",
        ]
    );
    assert_parent_edges(blocks);

    let project = compile(
        "def reset:\n    entries = {\"global\"}\nend\nsprite \"player\":\n    init:\n        pass\n    end\n    entries = {\"private\"}\n    start:\n        score = ((future) 的项目数)\n        if <score = 0>:\n            future = {1}\n        end\n        entries = {\"changed\"}\n        reset\n    end\nend\n",
    )
    .unwrap();
    let global_id = project.json["targets"][0]["lists"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, value)| value[0] == "entries")
        .unwrap()
        .0;
    let private_id = project.json["targets"][1]["lists"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, value)| value[0] == "entries")
        .unwrap()
        .0;
    let resets: Vec<_> = project.json["targets"][1]["blocks"]
        .as_object()
        .unwrap()
        .values()
        .filter(|block| {
            block["opcode"] == "data_deletealloflist" && block["fields"]["LIST"][0] == "entries"
        })
        .collect();
    assert_eq!(resets.len(), 2);
    assert!(
        resets
            .iter()
            .any(|block| block["fields"]["LIST"][1] == global_id.as_str())
    );
    assert!(
        resets
            .iter()
            .any(|block| block["fields"]["LIST"][1] == private_id.as_str())
    );
}

#[test]
fn data_kind_conflicts_and_live_diagnostics_are_enforced_across_scopes() {
    for source in [
        "name = 1\nname = {}\n",
        "name = {}\nname = 1\n",
        "start:\n    name = 1\n    name = {}\nend\n",
        "start:\n    name = {}\n    name = 1\nend\n",
        "start:\n    name = {}\n    name += 1\nend\n",
        "name = {}\nstart:\n    name = 1\nend\n",
        "name = 1\nstart:\n    name = {}\nend\n",
        "name = 1\nsprite \"a\":\n    init:\n        pass\n    end\n    name = {}\nend\n",
        "sprite \"a\":\n    init:\n        pass\n    end\n    name = 1\nend\nstart:\n    name = {}\nend\n",
        "sprite \"甲\":\n    init:\n        pass\n    end\n    value = {}\nend\nsprite \"乙\":\n    init:\n        pass\n    end\n    start:\n        value = 1\n    end\nend\n",
        "sprite \"玩家\":\n    init:\n        pass\n    end\n    value = 1\nend\nsprite \"玩家\" add:\n    value = {}\nend\n",
    ] {
        let error = compile(source).unwrap_err();
        assert!(error.span().is_some(), "{source}: {error}");
        assert!(
            error.0.contains("不能") || error.0.contains("冲突"),
            "{source}: {error}"
        );
    }

    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("helper.sbdsl"), "shared = {}\n").unwrap();
    fs::write(
        directory.path().join("main.sbdsl"),
        "import helper\nshared = 1\n",
    )
    .unwrap();
    let configuration =
        config::discover(Some(Path::new("main.sbdsl")), None, directory.path()).unwrap();
    let error = build::compile(&configuration).unwrap_err();
    assert_eq!(error.span().unwrap().line, 2);

    let main = directory.path().join("main.sbdsl");
    let source = "score = 1\nscore = {}\nlogs = {}\nstart:\n    logs = 1\n    score = logs\n    将 (1) 加入 (score)\n    score += 1\nend\n";
    let mut overlays = BTreeMap::from([(main.clone(), source.into())]);
    let mut cache = AnalysisCache::default();
    let result = analyze_document(&main, &overlays, &mut cache, true);
    for statement in [
        "score = {}",
        "logs = 1",
        "score = logs",
        "将 (1) 加入 (score)",
    ] {
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| &source[diagnostic.start..diagnostic.end] == statement),
            "{statement}: {:?}",
            result.diagnostics
        );
    }
    overlays.insert(
        main.clone(),
        "score = 1\nlogs = {}\nstart:\n    score = ((logs) 的项目数)\n    将 (score) 加入 (logs)\nend\n"
            .into(),
    );
    let corrected = analyze_document(&main, &overlays, &mut cache, true);
    assert!(
        corrected.diagnostics.is_empty(),
        "{:?}",
        corrected.diagnostics
    );
}

#[test]
fn slot_shells_leading_inputs_and_boolean_statement_boundaries() {
    let mut registry = Registry::new();
    for source in [
        "等待(1)秒",
        "等待 (1) 秒",
        "说(连接(\"你好\")和(\"世界\"))",
        "说(score)",
        "说(a+b*2)",
        "等待直到<score>0>",
        "移到('鼠标指针')",
        "显示变量(score)",
    ] {
        registry.parse(source, Shape::Command, &[]).unwrap();
    }
    for source in [
        "[等待 1 秒]",
        "等待1秒",
        "等待 1 秒",
        "说\"你好\"",
        "说 score",
        "说(连接\"a\"和\"b\")",
        "移到'鼠标指针'",
        "显示变量 score",
    ] {
        assert!(
            registry.parse(source, Shape::Command, &[]).is_err(),
            "{source}"
        );
    }
    assert!(registry.parse("当按下'空格'键", Shape::Hat, &[]).is_ok());
    assert!(registry.parse("当按下('空格')键", Shape::Hat, &[]).is_err());

    let templates = [
        procedure_template("检查<@ok>后", "suffix").unwrap(),
        procedure_template("<@ok>判断", "leading").unwrap(),
        procedure_template("@steps 步", "steps").unwrap(),
        procedure_template("检查<@ok>再 @n 后", "following").unwrap(),
        procedure_template("结束", "empty").unwrap(),
    ];
    let mut boolean = procedure_template("模板<@ok>@n", "unused").unwrap();
    boolean.shape = Shape::Boolean;
    boolean.namespace = Some("ext".into());
    boolean.procedure = None;
    registry.templates.push(boolean);
    let commands = [
        "(1)步",
        "<true>判断",
        "检查<score<1>后",
        "检查<score>100>后",
        "<score<1>判断",
        "<score>100>判断",
        "检查<score<1>再(2)后",
        "检查<score>100>再(\n        3)后",
        "检查<ext.模板<score<1>\n        (1)>再(3)后",
        "检查<ext.模板<score>1>\n        (2)>再(4)后",
        "等待直到<score<1>",
        "结束",
        "等待直到<score>100>",
        "等待直到<score\n        >\n        100>",
        "等待直到<score >\n        global:limit>",
        "等待直到<score >\n        (limit)>",
        "等待直到<score >\n        (x 坐标)>",
        "等待直到<score >\nlimit\n>",
        "等待(1)秒",
    ];
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "start:\n    {} # final\nend\n",
            commands.join(" # next\n    ")
        )
        .replace('\n', newline);
        let parsed = parse(Path::new("boundaries.sbdsl"), &source).unwrap();
        let recovering = parse_recovering(Path::new("boundaries.sbdsl"), &source);
        assert!(recovering.errors.is_empty(), "{:?}", recovering.errors);
        let Item::Script { body, .. } = &parsed[0] else {
            panic!()
        };
        assert_eq!(body.len(), commands.len());
        for (statement, expected) in body.iter().zip(commands) {
            let Statement::Command { source, .. } = statement else {
                panic!()
            };
            assert_eq!(source.replace("\r\n", "\n"), expected);
            registry.parse(source, Shape::Command, &templates).unwrap();
        }
    }
}

#[test]
fn python_syntax_generates_precedence_tree_elif_while_and_scoped_targets() {
    let project = compile(
        r#"a = 1
b = 2
c = 3
d = 4
f = 5
e = 6
score = 10
logs = {"开始", 1, "开始"}

sprite "玩家":
    init:
        x: -100
    end
    score = 0
    def 增加 @amount:
        score += @amount
    end
    start:
        score = global:score
        score = (a + b - c * d / f % e)
        if <score > 100>:
            说 ("高")
        elif <score > 50>:
            说 ("中")
        elif <score > 0>:
            说 ("低")
        else:
            说 ("零")
        end
        while <score < 3>:
            score += 1
        end
        while true:
            增加 (1)
        end
        停止 'this script'
    end
end
"#,
    )
    .unwrap();
    let stage = &project.json["targets"][0];
    let sprite = &project.json["targets"][1];
    assert_eq!(sprite["x"], -100.0);
    let id = |target: &Value, name: &str| {
        target["variables"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, entry)| entry[0] == name)
            .unwrap()
            .0
            .clone()
    };
    let global_score = id(stage, "score");
    let local_score = id(sprite, "score");
    assert_ne!(global_score, local_score);

    let blocks = &sprite["blocks"];
    let hat = blocks
        .as_object()
        .unwrap()
        .values()
        .find(|block| block["opcode"] == "event_whenflagclicked")
        .unwrap();
    let first_assign = &blocks[hat["next"].as_str().unwrap()];
    assert_eq!(first_assign["fields"]["VARIABLE"][1], local_score);
    assert_eq!(first_assign["inputs"]["VALUE"][1][2], global_score);

    let assignment = &blocks[first_assign["next"].as_str().unwrap()];
    assert_eq!(assignment["opcode"], "data_setvariableto");
    let subtract = input_block(blocks, assignment, "VALUE");
    assert_eq!(subtract["opcode"], "operator_subtract");
    assert_eq!(
        input_block(blocks, subtract, "NUM1")["opcode"],
        "operator_add"
    );
    let modulo = input_block(blocks, subtract, "NUM2");
    assert_eq!(modulo["opcode"], "operator_mod");
    let divide = input_block(blocks, modulo, "NUM1");
    assert_eq!(divide["opcode"], "operator_divide");
    assert_eq!(
        input_block(blocks, divide, "NUM1")["opcode"],
        "operator_multiply"
    );
    let branch = &blocks[assignment["next"].as_str().unwrap()];
    assert_eq!(branch["opcode"], "control_if_else");
    let elif = input_block(blocks, branch, "SUBSTACK2");
    assert_eq!(elif["opcode"], "control_if_else");
    let last_elif = input_block(blocks, elif, "SUBSTACK2");
    assert_eq!(last_elif["opcode"], "control_if_else");
    assert_eq!(
        input_block(blocks, last_elif, "SUBSTACK2")["opcode"],
        "looks_say"
    );
    let conditional = &blocks[branch["next"].as_str().unwrap()];
    assert_eq!(conditional["opcode"], "control_while");
    assert_eq!(
        input_block(blocks, conditional, "CONDITION")["opcode"],
        "operator_lt"
    );
    let forever = &blocks[conditional["next"].as_str().unwrap()];
    assert_eq!(forever["opcode"], "control_forever");
    assert!(forever["inputs"].get("CONDITION").is_none());
    assert_eq!(
        blocks[forever["next"].as_str().unwrap()]["opcode"],
        "control_stop"
    );
    assert_parent_edges(blocks.as_object().unwrap());

    for source in [
        "当绿旗被点击 { 等待 (1) 秒 }",
        "start { 等待 (1) 秒 }",
        "var score = 0\n",
        "list logs = {}\n",
        "logs = []\n",
        "@sprite \"玩家\":\n    init:\n        pass\n",
        "定义自制积木 测试:\n    pass\n",
        "当绿旗被点击:\n    pass\n",
        "start:\n    如果 <true> 那么:\n        pass\n",
        "start:\n    repeat 2:\n        pass\n",
        "start:\n    forever:\n        pass\n",
        "start:\n    until <true>:\n        pass\n",
        "start:\n    停止 /'全部脚本'/\n",
        "start:\n    说 (7 mod 3)\n",
        "start:\n    [等待 (1) 秒]\n",
        "start:\n    等待 1 秒\n",
    ] {
        assert!(compile(source).is_err(), "accepted old syntax: {source}");
    }
}
