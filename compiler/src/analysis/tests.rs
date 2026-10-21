use super::*;
use crate::model::Expr;
use serde_json::json;

#[test]
fn recovery_reports_syntax_import_and_binding_errors_across_the_project() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let broken = directory.path().join("broken.sbdsl");
    let healthy = directory.path().join("healthy.sbdsl");
    let source = "broken = \nbrokenList = {1,}\nimport missing\nsprite \"玩家\":\n    init:\n        pass\n    start:\n        不存在的动作\n        删除 (missing) 的全部项目\n        说 (@missing)\n        后续动作\nimport broken\nimport healthy\n";
    let broken_source =
        "alsoBroken = \nsprite \"玩家\" add:\n    start:\n        另一个不存在的动作\n";
    fs::write(&broken, broken_source).unwrap();
    fs::write(&healthy, "def 后续动作:\n    等待 (0) 秒\n").unwrap();
    let overlays = BTreeMap::from([(main.clone(), source.into())]);
    let result = analyze_document(&main, &overlays, &mut AnalysisCache::default(), true);
    let main_errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.path == normalize_path(&main))
        .collect();
    let module_errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.path == normalize_path(&broken))
        .collect();
    assert_eq!(main_errors.len(), 6, "{:?}", result.diagnostics);
    assert_eq!(module_errors.len(), 2, "{:?}", result.diagnostics);
    assert!(
        main_errors
            .iter()
            .any(|d| d.message.contains("找不到模块 missing"))
    );
    for text in ["不存在的动作", "删除 (missing) 的全部项目", "说 (@missing)"] {
        assert!(
            main_errors.iter().any(|d| &source[d.start..d.end] == text),
            "{text}: {main_errors:?}"
        );
    }
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.path == normalize_path(&healthy))
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("后续动作"))
    );
    assert!(!main.exists(), "the entry buffer must not need saving");
}

#[test]
fn snapshot_analysis_keeps_project_and_unimported_document_diagnostics_separate() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let library = directory.path().join("library.sbdsl");
    let orphan = directory.path().join("orphan.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    let overlays = BTreeMap::from([
        (config, r#"{"entry":"main.sbdsl"}"#.into()),
        (main.clone(), "import library\nbad = \n".into()),
        (library.clone(), "alsoBad = \n".into()),
        (orphan.clone(), "a = \nb = {1,}\n".into()),
    ]);
    let paths = vec![main.clone(), library.clone(), orphan.clone()];
    let results = analyze_documents(&paths, &overlays, &mut AnalysisCache::default(), false);
    assert_eq!(results[&normalize_path(&main)].diagnostics.len(), 2);
    assert_eq!(
        results[&normalize_path(&main)].diagnostics,
        results[&normalize_path(&library)].diagnostics
    );
    let orphan_result = &results[&normalize_path(&orphan)];
    assert_eq!(
        orphan_result.diagnostics.len(),
        4,
        "{:?}",
        orphan_result.diagnostics
    );
    assert!(
        !results[&normalize_path(&main)]
            .diagnostics
            .iter()
            .any(|d| d.path == normalize_path(&orphan))
    );
}

#[test]
fn unavailable_extensions_do_not_hide_project_syntax_errors() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let library = directory.path().join("library.sbdsl");
    let extension = directory.path().join("自定义拓展.js");
    fs::write(&extension, "throw new Error('broken schema');").unwrap();
    let overlays = BTreeMap::from([
        (
            directory.path().join("sbdsl.buildsb"),
            r#"{"entry":"main.sbdsl","extensions":["自定义拓展.js"]}"#.into(),
        ),
        (main.clone(), "bad = \nimport library\n".into()),
        (library.clone(), "alsoBad = \n".into()),
    ]);
    let mut cache = AnalysisCache::default();
    let trusted = analyze_document(&main, &overlays, &mut cache, true);
    assert_eq!(trusted.diagnostics.len(), 3, "{:?}", trusted.diagnostics);
    assert!(trusted.diagnostics.iter().any(|d| d.path
        == normalize_path(&directory.path().join("sbdsl.buildsb"))
        && d.message.contains("broken schema")));
    let untrusted = analyze_document(&main, &overlays, &mut cache, false);
    assert_eq!(
        untrusted.diagnostics.len(),
        2,
        "{:?}",
        untrusted.diagnostics
    );
    assert_eq!(cache.extension_extractions, 1);
}

#[test]
fn unsaved_import_and_config_keep_the_project_entry() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let part = directory.path().join("parts.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    let overlays = BTreeMap::from([
        (config, r#"{"entry":"main.sbdsl"}"#.into()),
        (
            main,
            "sprite \"玩家\":\n    init:\n        pass\nimport parts\n".into(),
        ),
        (
            part.clone(),
            "start:\n    不存在的积木\nsprite \"玩家\" add:\n    start:\n        移动 (1) 步\n"
                .into(),
        ),
    ]);
    let result = analyze_document(&part, &overlays, &mut AnalysisCache::default(), true);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!part.exists());
}

#[test]
fn config_overlay_changes_entry_and_invalid_config_does_not_reuse_old_state() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    fs::write(&config, r#"{"entry":"missing.sbdsl"}"#).unwrap();
    let mut overlays = BTreeMap::from([
        (main.clone(), "score = 1\n".into()),
        (config.clone(), r#"{"entry":"main.sbdsl"}"#.into()),
    ]);
    let mut cache = AnalysisCache::default();
    assert!(
        analyze_document(&main, &overlays, &mut cache, true)
            .diagnostics
            .is_empty()
    );
    overlays.insert(config.clone(), "{\n  \"entry\":\n}".into());
    let result = analyze_document(&main, &overlays, &mut cache, true);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].path, normalize_path(&config));
    assert!(result.diagnostics[0].start > 0);
}

#[test]
fn syntax_error_retains_utf8_range_and_plain_message() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("中文 😀.sbdsl");
    let text = "名字 = 1\r\n💥";
    let error = parser::parse(&path, text).unwrap_err();
    let span = error.span().unwrap();
    assert_eq!(span.start, text.find('💥').unwrap());
    assert_eq!(span.end - span.start, '💥'.len_utf8());
    assert_eq!((span.line, span.column), (2, 1));
    assert!(!error.0.contains(".sbdsl"));
    assert!(error.to_string().contains(".sbdsl:2:1:"));
    let overlays = BTreeMap::from([(path.clone(), text.into())]);
    let result = analyze_document(&path, &overlays, &mut AnalysisCache::default(), true);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].start, span.start);
    assert_eq!(result.diagnostics[0].end, span.end);
}

#[test]
fn syntax_in_an_open_unimported_module_is_still_reported() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let orphan = directory.path().join("orphan.sbdsl");
    let overlays = BTreeMap::from([
        (
            directory.path().join("sbdsl.buildsb"),
            r#"{"entry":"main.sbdsl"}"#.into(),
        ),
        (main, "score = 0\n".into()),
        (orphan.clone(), "start:\n    [说 \"unfinished".into()),
    ]);
    let result = analyze_document(&orphan, &overlays, &mut AnalysisCache::default(), true);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].path, normalize_path(&orphan));
}

#[test]
fn diagnostics_check_targets_without_loading_or_decoding_assets() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.sbdsl");
    let source = "sprite \"玩家\":\n    init:\n        \"costumes\":[{\"name\":\"missing\",\"path\":\"does-not-exist.svg\"}]\n    start:\n        移动 (1) 步\n";
    let mut overlays = BTreeMap::from([(path.clone(), source.into())]);
    let mut cache = AnalysisCache::default();
    assert!(
        analyze_document(&path, &overlays, &mut cache, true)
            .diagnostics
            .is_empty()
    );
    overlays.insert(path.clone(), "start:\n    移动 (1) 步".into());
    let result = analyze_document(&path, &overlays, &mut cache, true);
    assert_eq!(result.diagnostics.len(), 1);
    assert!(result.diagnostics[0].message.contains("舞台不支持"));
    assert_eq!(
        &overlays[&path][result.diagnostics[0].start..result.diagnostics[0].end],
        "移动 (1) 步"
    );
}

#[test]
fn shared_binding_keeps_public_lexical_scope_and_implicit_globals() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.sbdsl");
    let source = "x = 3\ndef 公共动作:\n    x = 1\n    implicit = 2\nsprite \"玩家\":\n    init:\n        pass\n    x = 9\n    start:\n        公共动作\n";
    let provider = OverlaySource::new(&BTreeMap::from([(path.clone(), source.into())]));
    let project = project::load_with_provider(&path, directory.path(), &provider).unwrap();
    let bound = bind::bind_project(&project, &Registry::new()).unwrap();
    assert_eq!(bound.targets[0].variables["x"], json!(3));
    assert_eq!(bound.targets[0].variables["implicit"], json!(0));
    assert_eq!(bound.targets[1].variables["x"], json!(9));
    assert!(matches!(
        &bound.procedures["p1"].body[0].invocation.args[0],
        Expr::Symbol { global: true, .. }
    ));
}

#[test]
fn public_target_restrictions_apply_only_when_instantiated() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.sbdsl");
    let mut overlays = BTreeMap::from([(path.clone(), "def 动作:\n    移动 (1) 步\n".into())]);
    let mut cache = AnalysisCache::default();
    assert!(
        analyze_document(&path, &overlays, &mut cache, true)
            .diagnostics
            .is_empty()
    );
    overlays
        .get_mut(&path)
        .unwrap()
        .push_str("start:\n    动作\n");
    let result = analyze_document(&path, &overlays, &mut cache, true);
    assert!(result.diagnostics[0].message.contains("舞台不支持"));
}

#[test]
fn extension_schema_cache_tracks_saved_content_and_caches_failures() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.sbdsl");
    let extension = directory.path().join("自定义拓展.js");
    let js = "Scratch.extensions.register({getInfo(){return {id:'example',blocks:[{opcode:'run',blockType:'command',text:'做'}]}}});";
    fs::write(&extension, js).unwrap();
    let mut overlays = BTreeMap::from([
        (
            directory.path().join("sbdsl.buildsb"),
            r#"{"entry":"main.sbdsl","extensions":["自定义拓展.js"]}"#.into(),
        ),
        (path.clone(), "start:\n    \"自定义拓展\".做".into()),
    ]);
    let mut cache = AnalysisCache::default();
    assert!(
        analyze_document(&path, &overlays, &mut cache, true)
            .diagnostics
            .is_empty()
    );
    overlays
        .get_mut(&path)
        .unwrap()
        .push_str("\n# edited DSL\n");
    assert!(
        analyze_document(&path, &overlays, &mut cache, true)
            .diagnostics
            .is_empty()
    );
    assert_eq!(cache.extension_extractions, 1);
    fs::write(&extension, js.replace("text:'做'", "text:'其他动作'")).unwrap();
    assert!(
        !analyze_document(&path, &overlays, &mut cache, true)
            .diagnostics
            .is_empty()
    );
    assert_eq!(cache.extension_extractions, 2);
    fs::write(&extension, "throw new Error('broken schema');").unwrap();
    for _ in 0..2 {
        let result = analyze_document(&path, &overlays, &mut cache, true);
        assert_eq!(result.diagnostics.len(), 1);
        assert!(result.registry.extension_sources.is_empty());
    }
    assert_eq!(cache.extension_extractions, 3);
    let untrusted = analyze_document(&path, &overlays, &mut cache, false);
    assert!(untrusted.diagnostics.is_empty());
    assert_eq!(cache.extension_extractions, 3);
    assert!(untrusted.registry.extension_sources.is_empty());
}

#[test]
fn extension_alias_overlay_refreshes_cached_namespaces_and_completion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    fs::write(
        directory.path().join("自定义拓展.js"),
        "Scratch.extensions.register({getInfo(){return {id:'example',blocks:[{opcode:'run',blockType:'command',text:'做'}]}}});",
    )
    .unwrap();
    let config_source = |alias: &str| {
        format!("entry: \"main.sbdsl\"\nextensions: [\"自定义拓展.js\" as \"{alias}\"]\n")
    };
    fs::write(&config, config_source("first")).unwrap();
    let mut overlays = BTreeMap::from([(path.clone(), "start:\n    first.做".into())]);
    let mut cache = AnalysisCache::default();
    let first = analyze_document(&path, &overlays, &mut cache, true);
    assert!(first.diagnostics.is_empty(), "{:?}", first.diagnostics);
    assert_eq!(cache.extension_extractions, 1);

    // Only the unsaved `as` clause changes: the JavaScript bytes and disk
    // configuration stay identical, so a file-content-only cache is stale.
    overlays.insert(config.clone(), config_source("second"));
    let renamed = analyze_document(&path, &overlays, &mut cache, true);
    assert_eq!(renamed.diagnostics.len(), 1, "{:?}", renamed.diagnostics);
    assert_eq!(cache.extension_extractions, 2);
    for namespace in ["first", "自定义拓展"] {
        assert!(
            !renamed
                .registry
                .templates
                .iter()
                .any(|template| { template.namespace.as_deref() == Some(namespace) }),
            "obsolete namespace {namespace}"
        );
        let source = format!("start:\n    \"{namespace}\".做");
        assert!(
            crate::lsp::completion::complete(
                &source,
                lsp_types::Position::new(
                    1,
                    source.lines().last().unwrap().encode_utf16().count() as u32
                ),
                &renamed.registry,
            )
            .is_empty()
        );
    }
    let source = "start:\n    second.做";
    let completions = crate::lsp::completion::complete(
        source,
        lsp_types::Position::new(
            1,
            source.lines().last().unwrap().encode_utf16().count() as u32,
        ),
        &renamed.registry,
    );
    assert_eq!(completions.len(), 1, "{completions:?}");
    assert_eq!(completions[0].label, "second.做");
    overlays.insert(path.clone(), "start:\n    second.做".into());
    assert!(
        analyze_document(&path, &overlays, &mut cache, true)
            .diagnostics
            .is_empty()
    );
    assert_eq!(cache.extension_extractions, 2);

    // Closing the unsaved config restores the disk alias and can reuse its
    // original registry without preserving the temporary alias.
    overlays.remove(&config);
    let restored = analyze_document(&path, &overlays, &mut cache, true);
    assert_eq!(restored.diagnostics.len(), 1);
    assert!(
        restored
            .registry
            .templates
            .iter()
            .any(|template| { template.namespace.as_deref() == Some("first") })
    );
    assert!(
        !restored
            .registry
            .templates
            .iter()
            .any(|template| { template.namespace.as_deref() == Some("second") })
    );
    assert_eq!(cache.extension_extractions, 2);
}

#[test]
fn missing_path_identity_is_stable_when_saved() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("new-folder/../new.sbdsl");
    let before = normalize_path(&path);
    fs::write(directory.path().join("new.sbdsl"), "").unwrap();
    assert_eq!(
        before,
        normalize_path(&directory.path().join("./new.sbdsl"))
    );
}

#[test]
fn untrusted_builtin_projects_still_report_semantics_and_import_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.sbdsl");
    let mut overlays = BTreeMap::from([(path.clone(), "start:\n    移动 (1) 步".into())]);
    let mut cache = AnalysisCache::default();
    let result = analyze_document(&path, &overlays, &mut cache, false);
    assert!(result.diagnostics[0].message.contains("舞台不支持"));
    overlays.insert(
        directory.path().join("sbdsl.buildsb"),
        r#"{"entry":"main.sbdsl","extensions":["never-execute.js"]}"#.into(),
    );
    overlays.insert(path.clone(), "import missing\n".into());
    let result = analyze_document(&path, &overlays, &mut cache, false);
    assert!(result.diagnostics[0].message.contains("找不到模块"));
    assert_eq!(cache.extension_extractions, 0);
}

#[cfg(windows)]
#[test]
fn windows_identity_preserves_unicode_filename_and_matches_ascii_case() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("İ-Module.sbdsl");
    let before = normalize_path(&path);
    fs::write(&path, "value = 1\n").unwrap();
    assert_eq!(before, normalize_path(&path));
    assert_eq!(
        before,
        normalize_path(&directory.path().join("İ-MODULE.SBDSL"))
    );
    assert!(
        DiskSource
            .read_to_string(&before)
            .unwrap()
            .contains("value")
    );
}
