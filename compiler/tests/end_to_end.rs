mod common;
use common::{assert_graph, compile_at as compile};
use sbdsl::{build, config};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn tiny_wav() -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&52u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&[0; 16]);
    wav
}

#[test]
fn creates_real_sb3_and_balanced_graph() {
    let dir = tempfile::tempdir().unwrap();
    let p = compile(dir.path(), include_str!("../../examples/hello/main.sbdsl")).unwrap();
    assert_eq!(p.json["targets"].as_array().unwrap().len(), 2);
    assert_graph(&p.json);
    let zip = build::sb3_bytes(&p).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
    assert!(archive.by_name("project.json").is_ok());
    for (name, bytes) in &p.assets {
        assert_eq!(
            name.split('.').next().unwrap(),
            format!("{:x}", md5::compute(bytes))
        );
        assert!(archive.by_name(name).is_ok());
    }
    let again = compile(dir.path(), include_str!("../../examples/hello/main.sbdsl")).unwrap();
    assert_eq!(p.json, again.json);
    assert_eq!(
        build::sb3_bytes(&p).unwrap(),
        build::sb3_bytes(&again).unwrap()
    );
}

#[test]
fn cli_initializes_and_builds_with_the_new_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("示例 工程");
    let init = std::process::Command::new(env!("CARGO_BIN_EXE_sbdsl"))
        .arg("init")
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let config = fs::read_to_string(root.join("sbdsl.buildsb")).unwrap();
    assert!(config.starts_with("entry:"));
    assert!(!config.contains(','));
    assert!(!root.join("config.json").exists());
    let source = fs::read_to_string(root.join("src/main.sbdsl")).unwrap();
    assert!(source.starts_with("sprite \"main\""));
    let icon = fs::read(root.join("assets/sbdsl.svg")).unwrap();
    assert!(std::str::from_utf8(&icon).unwrap().contains("<svg"));
    for command in ["check", "build"] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_sbdsl"))
            .arg(command)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let mut archive =
        zip::ZipArchive::new(fs::File::open(root.join("dist/project.sb3")).unwrap()).unwrap();
    let project: Value = serde_json::from_reader(archive.by_name("project.json").unwrap()).unwrap();
    let main = &project["targets"][1];
    assert_eq!(main["name"], "main");
    assert_eq!(main["x"], -100.0);
    assert_eq!(main["y"], 0.0);
    assert_eq!(main["size"], 100.0);
    assert_eq!(main["direction"], 90.0);
    assert!(main["blocks"].as_object().unwrap().is_empty());
    let costume = &main["costumes"][0];
    assert_eq!(costume["name"], "main");
    assert_eq!(costume["rotationCenterX"], 48.0);
    assert_eq!(costume["rotationCenterY"], 48.0);
    let mut image = archive
        .by_name(costume["md5ext"].as_str().unwrap())
        .unwrap();
    let mut packed = Vec::new();
    std::io::Read::read_to_end(&mut image, &mut packed).unwrap();
    assert_eq!(packed, icon);
    assert_graph(&project);
}

#[test]
fn cli_init_preserves_an_existing_icon_before_writing_project_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("assets")).unwrap();
    let icon = root.path().join("assets/sbdsl.svg");
    fs::write(&icon, "user's costume").unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_sbdsl"))
        .arg("init")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read_to_string(icon).unwrap(), "user's costume");
    assert!(!root.path().join("sbdsl.buildsb").exists());
    assert!(!root.path().join("src/main.sbdsl").exists());
}

#[test]
fn simplified_operators_generate_the_expected_scratch_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let project = compile(
        directory.path(),
        r#"hp = 1
score = 101
sprite "运算":
    init:
        pass
    end
    start:
        说 (1 + 2)
        说 (1 - 2)
        说 (2 * 3)
        说 (8 / 2)
        说 (7 % 3)
        if <hp < 1>:
            说 ("低血量")
        end
        if <score > 100>:
            说 ("高分")
        end
        if <score = 100>:
            说 ("满分")
        end
        说 (1+(2*3))
        说 (1e-5--2)
        说 (8 /
            (6 / 2))
        if <<hp < 2> and <score>100>>:
            score = (score - 1)
        end
        说 ("后续命令")
    end
end
"#,
    )
    .unwrap();
    let blocks = project.json["targets"][1]["blocks"].as_object().unwrap();
    for opcode in [
        "operator_add",
        "operator_subtract",
        "operator_multiply",
        "operator_divide",
        "operator_mod",
        "operator_lt",
        "operator_gt",
        "operator_equals",
        "operator_and",
    ] {
        assert!(
            blocks.values().any(|block| block["opcode"] == opcode),
            "{opcode}"
        );
    }
    assert!(
        blocks
            .values()
            .any(|block| block["inputs"]["MESSAGE"][1][1] == "后续命令")
    );
    assert!(
        blocks.values().any(|block| {
            block["opcode"] == "operator_divide"
                && block["inputs"]["NUM2"][1]
                    .as_str()
                    .is_some_and(|right| blocks[right]["opcode"] == "operator_divide")
        }),
        "8 / (6 / 2) must retain the right-hand grouping"
    );
    assert_graph(&project.json);
}

#[test]
fn import_order_overwrite_cycle_and_discarded_stage_scripts() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("A.sbdsl"),"import B\nscore = 1\nlogs = {\"A\"}\nstart:\n    说 (not_created)\nend\nsprite \"玩家\":\n    init:\n        pass\n    end\n    start:\n        score += 1\n    end\nend\n").unwrap();
    fs::write(
        dir.path().join("B.sbdsl"),
        "import main\nscore = 2\nlogs = {\"B\", 3}\n",
    )
    .unwrap();
    let p = compile(
        dir.path(),
        "import A\nimport B\nsprite \"玩家\" add:\n    当按下 'space' 键:\n        移动 (1) 步\n    end\nend\n",
    )
    .unwrap();
    let stage = &p.json["targets"][0];
    let variables = stage["variables"].as_object().unwrap();
    assert_eq!(variables.len(), 1);
    assert_eq!(variables.values().next().unwrap(), &json!(["score", 1]));
    assert_eq!(
        stage["lists"].as_object().unwrap().values().next().unwrap(),
        &json!(["logs", ["A"]])
    );
    assert!(stage["blocks"].as_object().unwrap().is_empty());
    assert_graph(&p.json);
}

#[test]
fn add_never_creates_a_sprite() {
    let dir = tempfile::tempdir().unwrap();
    let err = compile(
        dir.path(),
        "sprite \"玩家\" add:\n    pass\nend\nsprite \"玩家\":\n    init:\n        pass\n    end\nend\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("add"));
    assert!(err.to_string().contains(":1:"));
}

#[test]
fn comments_menus_and_division_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let p = compile(
        dir.path(),
        r##"# 单行注释
#
跨行注释
#
sprite "玩家":
    init:
        pass
    end
    当按下 'space' 键:
        说 ("# / 不变") # 行尾
        hp = (8 / 2)
        if <hp < 5>:
            说 ("好")
        else:
            说 ("否")
        end
    end
end
"##,
    )
    .unwrap();
    assert_graph(&p.json);
    let blocks = p.json["targets"][1]["blocks"].as_object().unwrap();
    assert!(blocks.values().any(|b| b["opcode"] == "operator_divide"));
    assert!(blocks.values().any(|b| b["opcode"] == "operator_lt"));
}

#[test]
fn public_body_keeps_global_binding_inside_private_scope() {
    let dir = tempfile::tempdir().unwrap();
    let p = compile(
        dir.path(),
        r#"score = 1
def 增加分数 数量: @n @warp:
    score += @n
end
sprite "玩家":
    init:
        pass
    end
    score = 100
    start:
        增加分数 数量: (2)
        score += 3
        global:score += 4
    end
end
"#,
    )
    .unwrap();
    let global = p.json["targets"][0]["variables"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let local = p.json["targets"][1]["variables"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    assert_ne!(global, local);
    let blocks = p.json["targets"][1]["blocks"].as_object().unwrap();
    let changes: Vec<_> = blocks
        .values()
        .filter(|b| b["opcode"] == "data_changevariableby")
        .collect();
    assert_eq!(changes.len(), 3);
    assert_eq!(
        changes
            .iter()
            .filter(|b| b["fields"]["VARIABLE"][1] == global.as_str())
            .count(),
        2
    );
    assert_graph(&p.json);
}

#[test]
fn local_extension_namespace_and_inline_source() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/extensions");
    let c = config::discover(Some(Path::new("sbdsl.buildsb")), None, &base).unwrap();
    let p = build::compile(&c).unwrap();
    let blocks = p.json["targets"][1]["blocks"].as_object().unwrap();
    for opcode in [
        "motion_movesteps",
        "custom_move",
        "custom_double",
        "custom_ready",
        "custom_mode",
    ] {
        assert!(
            blocks.values().any(|b| b["opcode"] == opcode),
            "missing {opcode}"
        );
    }
    assert_eq!(p.json["extensions"], json!(["custom"]));
    assert!(
        p.json["extensionURLs"]["custom"]
            .as_str()
            .unwrap()
            .starts_with("data:application/javascript;charset=utf-8;base64,")
    );
    let menu = blocks
        .values()
        .find(|b| b["opcode"] == "custom_mode")
        .unwrap();
    assert_eq!(menu["fields"]["MODE"][0], "fast");
    assert_graph(&p.json);
}

#[test]
fn extension_aliases_change_source_names_without_changing_runtime_identity() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let directory = tempfile::tempdir().unwrap();
    let extension = include_bytes!("../../examples/extensions/自定义拓展.js");
    fs::write(directory.path().join("自定义拓展.js"), extension).unwrap();
    let config_path = directory.path().join("sbdsl.buildsb");
    fs::write(
        &config_path,
        "entry: \"main.sbdsl\"\nextensions: [\"自定义拓展.js\" as \"scratch-tensor-extension\"]\n",
    )
    .unwrap();
    // The fixture includes a command, reporter, menu, and explicit boolean
    // opcode alongside Scratch's built-in move block with the same text.
    let source = include_str!("../../examples/extensions/main.sbdsl")
        .replace("自定义拓展", "scratch-tensor-extension");
    let project = compile(directory.path(), &source).unwrap();
    assert_graph(&project.json);
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(build::sb3_bytes(&project).unwrap())).unwrap();
    let packed: Value = serde_json::from_reader(archive.by_name("project.json").unwrap()).unwrap();
    assert_eq!(packed["extensions"], json!(["custom"]));
    let urls = packed["extensionURLs"].as_object().unwrap();
    assert_eq!(urls.len(), 1);
    let embedded = urls["custom"]
        .as_str()
        .unwrap()
        .strip_prefix("data:application/javascript;charset=utf-8;base64,")
        .unwrap();
    assert_eq!(STANDARD.decode(embedded).unwrap().as_slice(), extension);
    let blocks = packed["targets"][1]["blocks"].as_object().unwrap();
    for opcode in [
        "motion_movesteps",
        "custom_move",
        "custom_double",
        "custom_ready",
        "custom_mode",
    ] {
        assert!(
            blocks.values().any(|block| block["opcode"] == opcode),
            "missing {opcode}"
        );
    }
    let menu = blocks
        .values()
        .find(|block| block["opcode"] == "custom_mode")
        .unwrap();
    assert_eq!(menu["fields"]["MODE"][0], "fast");

    for namespace in ["scratch-tensor-extension", "自定义拓展.min"] {
        fs::write(
            &config_path,
            format!("entry: \"main.sbdsl\"\nextensions: [\"自定义拓展.js\" as \"{namespace}\"]\n"),
        )
        .unwrap();
        for qualifier in [namespace.to_owned(), format!("\"{namespace}\"")] {
            let source = include_str!("../../examples/extensions/main.sbdsl")
                .replace("自定义拓展", &qualifier);
            let renamed = compile(directory.path(), &source).unwrap();
            assert_eq!(renamed.json["extensions"], packed["extensions"]);
            assert_eq!(renamed.json["extensionURLs"], packed["extensionURLs"]);
            assert_eq!(renamed.json["targets"], packed["targets"]);
        }
    }

    // An alias replaces the filename namespace; the old name must not remain
    // as an invisible fallback for either text matching or explicit opcodes.
    for invocation in [
        "\"自定义拓展\".移动 (20) 步",
        "\"自定义拓展\"::\"move\"((20))",
    ] {
        let source = format!(
            "sprite \"玩家\":\n    init:\n        pass\n    end\n    start:\n        {invocation}\n    end\nend\n"
        );
        assert!(compile(directory.path(), &source).is_err(), "{invocation}");
    }

    // Omitting `as` keeps the original filename-based behavior.
    fs::write(
        &config_path,
        "entry: \"main.sbdsl\"\nextensions: [\"自定义拓展.js\"]\n",
    )
    .unwrap();
    let unaliased = compile(
        directory.path(),
        include_str!("../../examples/extensions/main.sbdsl"),
    )
    .unwrap();
    assert_eq!(unaliased.json["extensionURLs"], packed["extensionURLs"]);
    assert_eq!(unaliased.json["extensions"], packed["extensions"]);
}

#[test]
fn emits_turbowarp_settings_in_runtime_format() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("sbdsl.buildsb"),
        r#"{"runtime":{"fps":60,"stageWidth":640,"stageHeight":480,"fencing":false}}"#,
    )
    .unwrap();
    let p = compile(dir.path(), "x = 1\n").unwrap();
    let comments = p.json["targets"][0]["comments"].as_object().unwrap();
    let text = comments.values().next().unwrap()["text"].as_str().unwrap();
    let line = text
        .lines()
        .last()
        .unwrap()
        .strip_suffix(" // _twconfig_")
        .unwrap();
    let settings: Value = serde_json::from_str(line).unwrap();
    assert_eq!(settings["framerate"], 60.);
    assert_eq!(settings["width"], 640);
    assert_eq!(settings["runtimeOptions"]["fencing"], false);
}

#[test]
fn rejects_unresolved_lists_bad_parameters_and_stage_motion() {
    let dir = tempfile::tempdir().unwrap();
    for source in [
        "start:\n    删除 (missing) 的全部项目",
        "def 测试:\n    等待 (@unknown) 秒",
        "def 测试:\n    移动 (1) 步\nstart:\n    测试",
        "start:\n    等待 ((1) + * (3)) 秒\n",
    ] {
        assert!(
            compile(dir.path(), source).is_err(),
            "should fail: {source}"
        );
    }
}

#[test]
fn packs_stage_config_assets_and_sprite_source_assets_with_real_metadata() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("assets")).unwrap();
    fs::write(
        dir.path().join("assets/bg.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 640 480"/>"#,
    )
    .unwrap();
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 0, 255, 0]).unwrap();
    }
    fs::write(dir.path().join("assets/player.png"), &png).unwrap();
    let wav = tiny_wav();
    fs::write(dir.path().join("assets/tone.wav"), &wav).unwrap();
    fs::write(
        dir.path().join("sbdsl.buildsb"),
        r#"{"stage":{"backdrops":[{"name":"背景","path":"assets/bg.svg"}]}}"#,
    )
    .unwrap();
    let source = r#"sprite "玩家":
    init:
        "x": 12,
        "rotationStyle": "left-right",
        "costumes": [{"name": "造型", "path": "assets/player.png"}],
        "sounds": [{"name": "音效", "path": "assets/tone.wav"}]
    end
end
"#;
    let p = compile(dir.path(), source).unwrap();
    let target = &p.json["targets"][1];
    assert_eq!(target["x"], 12.);
    assert_eq!(target["rotationStyle"], "left-right");
    assert_eq!(target["costumes"][0]["rotationCenterX"], 1.);
    assert_eq!(target["sounds"][0]["rate"], 8000);
    assert_eq!(target["sounds"][0]["sampleCount"], 8);
    assert_eq!(p.json["targets"][0]["costumes"][0]["rotationCenterY"], 240.);
    assert_eq!(p.assets.len(), 3);
    fs::write(dir.path().join("assets/player.png"), &png[..24]).unwrap();
    assert!(
        compile(dir.path(), source)
            .unwrap_err()
            .to_string()
            .contains("PNG")
    );
}

#[test]
fn sprite_init_sets_static_state_without_initialization_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let p = compile(
        dir.path(),
        r#"sprite "初态":
    init:
        "x": -123.5,
        "y": 45,
        "size": 75,
        "direction": -90,
        "visible": false,
        "rotationStyle": "don't rotate"
    end
end
sprite "默认":
    init:
        pass
    end
end
"#,
    )
    .unwrap();
    let initialized = &p.json["targets"][1];
    for (field, expected) in [
        ("x", -123.5),
        ("y", 45.),
        ("size", 75.),
        ("direction", -90.),
    ] {
        assert_eq!(initialized[field].as_f64(), Some(expected), "{field}");
    }
    assert_eq!(initialized["visible"], false);
    assert_eq!(initialized["rotationStyle"], "don't rotate");
    let defaults = &p.json["targets"][2];
    for (field, expected) in [("x", 0.), ("y", 0.), ("size", 100.), ("direction", 90.)] {
        assert_eq!(defaults[field].as_f64(), Some(expected), "{field}");
    }
    assert_eq!(defaults["visible"], true);
    assert_eq!(defaults["rotationStyle"], "all around");
    for target in p.json["targets"].as_array().unwrap() {
        assert!(target["blocks"].as_object().unwrap().is_empty());
        assert_eq!(target["costumes"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn imported_sprite_assets_resolve_from_the_declaring_module() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("modules/assets")).unwrap();
    fs::create_dir(dir.path().join("assets")).unwrap();
    let sprite_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="20"/>"#;
    fs::write(dir.path().join("modules/assets/player.svg"), sprite_svg).unwrap();
    fs::write(dir.path().join("modules/assets/tone.wav"), tiny_wav()).unwrap();
    // The same relative name at the entry file is deliberately different.
    fs::write(
        dir.path().join("assets/player.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="200"/>"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("modules/player.sbdsl"),
        r#"sprite "模块角色":
    init:
        "costumes": [{"name": "模块造型", "path": "assets/player.svg"}],
        "sounds": [{"name": "模块声音", "path": "assets/tone.wav"}]
    end
end
"#,
    )
    .unwrap();
    let p = compile(dir.path(), "import modules.player\n").unwrap();
    let target = &p.json["targets"][1];
    assert_eq!(target["name"], "模块角色");
    assert_eq!(target["costumes"][0]["rotationCenterX"], 15.);
    assert_eq!(
        target["costumes"][0]["assetId"],
        format!("{:x}", md5::compute(sprite_svg))
    );
    assert_eq!(target["sounds"][0]["sampleCount"], 8);
    assert!(
        p.assets
            .values()
            .any(|bytes| bytes == sprite_svg.as_bytes())
    );
}

#[test]
fn sprite_add_preserves_init_and_duplicate_creation_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("player.sbdsl"),
        "sprite \"玩家\":\n    init:\n        \"x\":42,\"size\":80,\"visible\":false\n    end\nend\n",
    )
    .unwrap();
    let p = compile(
        dir.path(),
        "import player\nsprite \"玩家\" add:\n    start:\n        移动 (1) 步\n    end\nend\n",
    )
    .unwrap();
    assert_eq!(p.json["targets"].as_array().unwrap().len(), 2);
    let target = &p.json["targets"][1];
    assert_eq!(target["x"], 42.);
    assert_eq!(target["size"], 80.);
    assert_eq!(target["visible"], false);
    assert_eq!(target["blocks"].as_object().unwrap().len(), 2);
    assert_graph(&p.json);
    for source in [
        "import player\nsprite \"玩家\":\n    init:\n        pass\n    end\nend\n",
        "sprite \"玩家\":\n    init:\n        pass\n    end\nend\nsprite \"玩家\":\n    init:\n        pass\n    end\nend\n",
    ] {
        let error = compile(dir.path(), source).unwrap_err().to_string();
        assert!(error.contains("玩家"), "{error}");
        assert!(error.contains("add"), "{error}");
        assert!(
            error.contains(":2:") || error.contains(":4:") || error.contains(":6:"),
            "{error}"
        );
    }
}

#[test]
fn config_rejects_sprites_field() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("sbdsl.buildsb"), r#"{"sprites":{}}"#).unwrap();
    let error = compile(dir.path(), "sprite \"玩家\":\n    init:\n        pass\n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown field `sprites`"), "{error}");
}

#[test]
fn init_is_required_first_unique_and_forbidden_in_add() {
    let dir = tempfile::tempdir().unwrap();
    for source in [
        "sprite \"玩家\":\n    pass\n",
        "sprite \"玩家\":\n    score = 0\n    init:\n        pass\n",
        "sprite \"玩家\":\n    start:\n        pass\n    init:\n        pass\n",
        "sprite \"玩家\":\n    init:\n        pass\n    init:\n        pass\n",
        "sprite \"玩家\":\n    init:\n        pass\nsprite \"玩家\" add:\n    init:\n        pass\n",
        "init:\n    pass\n",
    ] {
        assert!(
            compile(dir.path(), source).is_err(),
            "should fail: {source}"
        );
    }
}

#[test]
fn costume_order_is_preserved_and_names_are_unique_per_asset_kind() {
    let dir = tempfile::tempdir().unwrap();
    for (name, width) in [("a.svg", 10), ("b.svg", 20)] {
        fs::write(
            dir.path().join(name),
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="10"/>"#),
        )
        .unwrap();
    }
    fs::write(dir.path().join("tone.wav"), tiny_wav()).unwrap();
    let p = compile(
        dir.path(),
        r#"sprite "玩家":
    init:
        "costumes": [{"name":"第二","path":"b.svg"},{"name":"第一","path":"a.svg"}],
        "sounds": [{"name":"第二","path":"tone.wav"}]
    end
end
"#,
    )
    .unwrap();
    let target = &p.json["targets"][1];
    assert_eq!(target["costumes"][0]["name"], "第二");
    assert_eq!(target["costumes"][1]["name"], "第一");
    assert_eq!(target["costumes"][0]["rotationCenterX"], 10.);
    assert_eq!(target["costumes"][1]["rotationCenterX"], 5.);
    assert_eq!(target["sounds"][0]["name"], "第二");
    for init in [
        r#"{"costumes":[{"name":"重复","path":"a.svg"},{"name":"重复","path":"b.svg"}]}"#,
        r#"{"sounds":[{"name":"重复","path":"tone.wav"},{"name":"重复","path":"tone.wav"}]}"#,
    ] {
        let source = format!(
            "sprite \"玩家\":\n    init:\n        {}\n",
            &init[1..init.len() - 1]
        );
        assert!(
            compile(dir.path(), &source).is_err(),
            "should fail: {source}"
        );
    }
}

#[test]
fn init_rejects_unknown_duplicate_mistyped_and_invalid_values() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("a.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#,
    )
    .unwrap();
    for init in [
        r#"{"unknown":1}"#,
        r#"{"name":"玩家"}"#,
        r#"{"x":"1"}"#,
        r#"{"x":null}"#,
        r#"{"x":1,"x":2}"#,
        r#"{"x":1e999}"#,
        r#"{"size":0}"#,
        r#"{"size":-1}"#,
        r#"{"visible":1}"#,
        r#"{"rotationStyle":"sideways"}"#,
        r#"{"costumes":{}}"#,
        r#"{"costumes":[{"name":"造型","path":"a.svg","unknown":1}]}"#,
        r#"{"costumes":[{"name":"造型","name":"其他","path":"a.svg"}]}"#,
        r#"{"costumes":[{"name":"造型","path":"a.svg","path":"a.svg"}]}"#,
        r#"{"costumes":[{"name":"造型","path":"a.svg","rotationCenterX":null}]}"#,
        r#"{"costumes":[{"name":"造型","path":"a.svg","bitmapResolution":0}]}"#,
        r#"{"costumes":[{"name":"造型","path":"https://example.test/a.svg"}]}"#,
        r#"{"sounds":[{"name":"声音","path":"data:audio/wav;base64,AAAA"}]}"#,
        r#"{visible: true size: 100}"#,
    ] {
        let source = format!(
            "sprite \"玩家\":\n    init:\n        {}\n",
            &init[1..init.len() - 1]
        );
        assert!(
            compile(dir.path(), &source).is_err(),
            "should fail: {source}"
        );
    }
}
