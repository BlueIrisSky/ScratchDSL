//! Shared helpers for integration tests.
// Each test binary compiles this module separately and uses a different subset.
#![allow(dead_code)]

use serde_json::{Map, Value};
use std::{fs, path::Path};

/// Write `main.sbdsl` under `root` and compile it with CLI discovery semantics.
pub fn compile_at(root: &Path, source: &str) -> sbdsl::Result<sbdsl::model::GeneratedProject> {
    fs::write(root.join("main.sbdsl"), source).unwrap();
    let config = sbdsl::config::discover(Some(Path::new("main.sbdsl")), None, root)?;
    sbdsl::build::compile(&config)
}

/// Every `next`/input edge must have a matching `parent` back-edge.
pub fn assert_parent_edges(blocks: &Map<String, Value>) {
    for (id, block) in blocks {
        if let Some(next) = block["next"].as_str() {
            assert_eq!(blocks[next]["parent"], id.as_str(), "next parent of {id}");
        }
        for input in block["inputs"].as_object().unwrap().values() {
            for descriptor in input.as_array().unwrap().iter().skip(1) {
                if let Some(child) = descriptor.as_str() {
                    assert_eq!(blocks[child]["parent"], id.as_str(), "input parent of {id}");
                }
            }
        }
    }
}

/// Parent edges across all targets, plus the top-level parent-less invariant.
pub fn assert_graph(project: &Value) {
    for target in project["targets"].as_array().unwrap() {
        let blocks = target["blocks"].as_object().unwrap();
        assert_parent_edges(blocks);
        for block in blocks.values() {
            if block["topLevel"] == true {
                assert!(block["parent"].is_null());
            }
        }
    }
}
