use sbdsl::{
    analysis::{AnalysisCache, analyze_document},
    build, config,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};

fn setup(source: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("main.sbdsl"), source).unwrap();
    fs::write(
        directory.path().join("flow.js"),
        include_str!("fixtures/control-extension.js"),
    )
    .unwrap();
    fs::write(
        directory.path().join("sbdsl.buildsb"),
        "entry: \"main.sbdsl\"\nextensions: [\"flow.js\"]\n",
    )
    .unwrap();
    directory
}

fn compile(source: &str) -> sbdsl::Result<sbdsl::model::GeneratedProject> {
    let directory = setup(source);
    let config = config::discover(None, None, directory.path())?;
    build::compile(&config)
}

fn block<'a>(blocks: &'a serde_json::Map<String, Value>, opcode: &str) -> &'a Value {
    blocks
        .values()
        .find(|block| block["opcode"] == opcode)
        .unwrap()
}

#[test]
fn extension_control_inputs_branches_and_procedure_dependencies_are_mapped() {
    let project = compile(include_str!("fixtures/extension-controls.sbdsl")).unwrap();
    let blocks = project.json["targets"][1]["blocks"].as_object().unwrap();
    let pick = block(blocks, "flow_pick");
    for (name, expected) in [
        ("INDEX", json!([1, [4, "3"]])),
        ("LABEL", json!([1, [10, "B"]])),
        ("AMOUNT", json!([1, [4, "42"]])),
        ("TAIL", json!([1, [10, "tail"]])),
    ] {
        assert_eq!(pick["inputs"][name], expected, "{name}");
    }
    for name in ["SUBSTACK", "SUBSTACK2", "SUBSTACK3"] {
        assert_eq!(pick["inputs"][name][0], 2);
        assert!(pick["inputs"][name][1].is_string());
    }
    let third = pick["inputs"]["SUBSTACK3"][1].as_str().unwrap();
    assert_eq!(blocks[third]["opcode"], "procedures_call");
    assert!(
        blocks
            .values()
            .any(|block| block["opcode"] == "procedures_definition")
    );
    assert!(
        blocks
            .values()
            .any(|block| block["opcode"] == "data_addtolist")
    );
    let once = block(blocks, "flow_once");
    assert!(once["inputs"]["SUBSTACK"][1].is_string());
    assert_eq!(
        blocks[once["next"].as_str().unwrap()]["opcode"],
        "data_changevariableby"
    );
    for (id, block) in blocks {
        if let Some(next) = block["next"].as_str() {
            assert_eq!(blocks[next]["parent"], id.as_str());
        }
        for input in block["inputs"].as_object().unwrap().values() {
            for child in input
                .as_array()
                .unwrap()
                .iter()
                .skip(1)
                .filter_map(Value::as_str)
            {
                assert_eq!(blocks[child]["parent"], id.as_str());
            }
        }
    }
}

#[test]
fn display_text_arrays_and_empty_branches_compile() {
    let project = compile(
        "start:\n    flow.选择 (2) 第二 (\"label\") 第三 (5) 结束 (\"footer\"):\n        pass\n    else:\n        flow.执行 (2) 次:\n            pass\n    else:\n        pass\n",
    )
    .unwrap();
    let blocks = project.json["targets"][0]["blocks"].as_object().unwrap();
    let pick = block(blocks, "flow_pick");
    assert_eq!(pick["inputs"]["SUBSTACK"], json!([2, null]));
    assert_eq!(pick["inputs"]["SUBSTACK3"], json!([2, null]));
    assert!(pick["inputs"]["SUBSTACK2"][1].is_string());
    assert_eq!(
        block(blocks, "flow_times")["inputs"]["SUBSTACK"],
        json!([2, null])
    );
}

#[test]
fn control_shapes_and_branch_counts_are_checked() {
    for source in [
        "start:\n    flow.执行 (1) 次\n",
        "start:\n    flow.执行 (1) 次:\n        pass\n    else:\n        pass\n",
        "start:\n    flow.如果 <true> 否则 (1):\n        pass\n",
        "start:\n    flow.如果 <true> 否则 (1):\n        pass\n    else:\n        pass\n    else:\n        pass\n",
        "start:\n    flow::\"pick\"((1), (\"label\"), (5), (\"footer\")):\n        pass\n    else:\n        pass\n",
        "start:\n    说 (\"hello\"):\n        pass\n",
        "flow.执行 (1) 次:\n    pass\n",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    let error = compile("start:\n    flow.执行 (1) 次:\n        移动 (1) 步\n").unwrap_err();
    assert!(error.0.contains("舞台不支持"), "{error}");
}

#[test]
fn diagnostics_check_every_extension_branch_and_clear_after_repair() {
    let source = "start:\n    flow.如果 <true> 否则 (1):\n        删除 (missing) 的全部项目\n    else:\n        显示变量 (@missing)\n    else:\n        移动 (1) 步\n";
    let directory = setup(source);
    let path = directory.path().join("main.sbdsl");
    let mut overlays = BTreeMap::from([(path.clone(), source.into())]);
    let mut cache = AnalysisCache::default();
    let result = analyze_document(&path, &overlays, &mut cache, true);
    for fragment in ["需要 2 个分支", "missing", "舞台不支持"] {
        assert!(
            result
                .diagnostics
                .iter()
                .any(|error| error.message.contains(fragment)),
            "{fragment}: {:?}",
            result.diagnostics
        );
    }
    for header in [
        "flow.如果 <true> 否则 (1)",
        "删除 (missing) 的全部项目",
        "显示变量 (@missing)",
        "移动 (1) 步",
    ] {
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| source[diagnostic.start..diagnostic.end].contains(header)),
            "{header}: {:?}",
            result.diagnostics
        );
    }
    overlays.insert(
        path.clone(),
        "start:\n    flow.如果 <true> 否则 (1):\n        pass\n    else:\n        pass\n".into(),
    );
    let fixed = analyze_document(&path, &overlays, &mut cache, true);
    assert!(fixed.diagnostics.is_empty(), "{:?}", fixed.diagnostics);
}
