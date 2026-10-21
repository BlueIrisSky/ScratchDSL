use super::*;
use crate::analysis::{OverlaySource, normalize_path};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn recovery_preserves_valid_project_order_scopes_and_entry_scripts() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let module = directory.path().join("module.sbdsl");
    let overlays = BTreeMap::from([
        (
            main.clone(),
            r#"score = 1
import module
score = 3
stage:
    start:
        等待 (1) 秒
sprite "玩家" add:
    hp = 9
    global:shared = 4
    def 动作 @private:
        pass
    start:
        移动 (2) 步
"#
            .into(),
        ),
        (
            module,
            r#"import main
score = 2
shared = 2
logs = {"module"}
def 动作 @public:
    pass
start:
    不存在的舞台脚本
sprite "玩家":
    init:
        "x":5,"costumes":[{"name":"造型","path":"cat.svg"}]
    hp = 5
    bag = {1}
    start:
        移动 (1) 步
"#
            .into(),
        ),
    ]);
    let provider = OverlaySource::new(&overlays);
    let strict = load_with_provider(&main, directory.path(), &provider).unwrap();
    let report = load_recovering(&main, directory.path(), &provider);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(format!("{:?}", strict), format!("{:?}", report.project));
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.project.stage.variables["score"], json!(3));
    assert_eq!(report.project.stage.variables["shared"], json!(4));
    assert_eq!(report.project.stage.scripts.len(), 1);
    assert_eq!(report.project.public_procedures.len(), 1);
    let sprite = &report.project.sprites[0];
    assert_eq!(sprite.variables["hp"], json!(9));
    assert_eq!(sprite.lists["bag"], json!([1]));
    assert_eq!(sprite.procedures.len(), 1);
    assert_eq!(sprite.scripts.len(), 2);
}

#[test]
fn syntax_errors_in_multiple_modules_do_not_hide_later_imports() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let broken = directory.path().join("broken.sbdsl");
    let later = directory.path().join("later.sbdsl");
    let overlays = BTreeMap::from([
        (
            main.clone(),
            "@unknown\nimport broken\n@other\nimport later\nfinal = 3\n".into(),
        ),
        (broken.clone(), "@bad\nrecovered = 1\n@also_bad\n".into()),
        (later.clone(), "imported = 2\n".into()),
    ]);
    let provider = OverlaySource::new(&overlays);
    let report = load_recovering(&main, directory.path(), &provider);
    assert_eq!(report.errors.len(), 4, "{:?}", report.errors);
    for path in [&main, &broken] {
        assert_eq!(
            report
                .errors
                .iter()
                .filter(|error| Path::new(&error.span().unwrap().file) == provider.identity(path))
                .count(),
            2
        );
    }
    assert_eq!(report.files.len(), 3);
    assert_eq!(report.project.stage.variables["recovered"], json!(1));
    assert_eq!(report.project.stage.variables["imported"], json!(2));
    assert_eq!(report.project.stage.variables["final"], json!(3));
    assert!(load_with_provider(&main, directory.path(), &provider).is_err());
}

#[test]
fn missing_modules_report_each_import_and_keep_remaining_declarations() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let module = directory.path().join("module.sbdsl");
    let overlays = BTreeMap::from([
        (
            main.clone(),
            "import missing\nimport module\nmain_value = 1\n".into(),
        ),
        (
            module.clone(),
            "import another_missing\nmodule_value = 2\n".into(),
        ),
    ]);
    let provider = OverlaySource::new(&overlays);
    let report = load_recovering(&main, directory.path(), &provider);
    assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
    assert!(
        report
            .errors
            .iter()
            .all(|error| error.0.contains("找不到模块"))
    );
    for (error, path) in report.errors.iter().zip([&main, &module]) {
        let span = error.span().unwrap();
        assert_eq!(Path::new(&span.file), provider.identity(path));
        assert_eq!((span.line, span.column), (1, 1));
        assert_eq!(span.start, 0);
        assert!(span.end > 0);
    }
    assert_eq!(report.project.stage.variables.len(), 2);
    assert_eq!(report.files.len(), 2);
}

#[test]
fn bad_declarations_do_not_leak_private_contents_or_stop_siblings() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let source = r#"sprite "玩家":
    init:
        pass
    def 动作 @one:
        pass
    def 动作 @two:
        pass
    hp = 3
sprite "玩家":
    init:
        pass
    leaked = 1
    global:leaked_global = 1
    def 不应存在:
        pass
sprite "未创建" add:
    leaked_list = {}
    def 也不应存在:
        pass
def 坏定义 @x @x:
    pass
def 动作 @first:
    pass
def 动作 @second:
    pass
final = 4
"#;
    let provider = OverlaySource::new(&BTreeMap::from([(main.clone(), source.into())]));
    let report = load_recovering(&main, directory.path(), &provider);
    assert_eq!(report.errors.len(), 5, "{:?}", report.errors);
    assert!(report.errors.iter().all(|error| error.span().is_some()));
    assert_eq!(report.project.sprites.len(), 1);
    assert_eq!(report.project.sprites[0].procedures.len(), 1);
    assert_eq!(report.project.sprites[0].variables.len(), 1);
    assert_eq!(report.project.sprites[0].variables["hp"], json!(3));
    assert_eq!(report.project.public_procedures.len(), 1);
    assert_eq!(report.project.stage.variables.len(), 1);
    assert_eq!(report.project.stage.variables["final"], json!(4));
    assert!(report.project.stage.lists.is_empty());
    assert_eq!(
        load_with_provider(&main, directory.path(), &provider)
            .unwrap_err()
            .0,
        report.errors[0].0
    );
}

#[test]
fn broken_init_keeps_role_scope_without_duplicate_init_errors() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let source = r#"sprite "玩家":
    init:
        "size":-1
    hp = 3
    def 私有动作:
        pass
    start:
        移动 (1) 步
final = 4
"#;
    let provider = OverlaySource::new(&BTreeMap::from([(main.clone(), source.into())]));
    let report = load_recovering(&main, directory.path(), &provider);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.project.sprites.len(), 1);
    let sprite = &report.project.sprites[0];
    assert!(sprite.init.is_none());
    assert_eq!(sprite.variables["hp"], json!(3));
    assert_eq!(sprite.procedures.len(), 1);
    assert_eq!(sprite.scripts.len(), 1);
    assert_eq!(report.project.stage.variables.len(), 1);
    assert!(report.project.public_procedures.is_empty());
}

#[test]
fn overlays_keep_nearby_import_precedence_fallback_and_cycle_identity() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let main = root.join("main.sbdsl");
    let module = root.join("parts/module.sbdsl");
    let nearby = root.join("parts/helper.sbdsl");
    let src_dir = root.join("src");
    let fallback = src_dir.join("fallback.sbdsl");
    let shadowed = src_dir.join("helper.sbdsl");
    let overlays = BTreeMap::from([
        (
            main.clone(),
            "import parts.module\nimport parts.module\nshared = 4\n".into(),
        ),
        (
            module,
            "import helper\nimport fallback\nshared = 3\n".into(),
        ),
        (nearby, "import module\nshared = 1\nnearby = 1\n".into()),
        (fallback, "shared = 2\nfallback = 2\n".into()),
        (shadowed.clone(), "shadowed = 9\n".into()),
    ]);
    let provider = OverlaySource::new(&overlays);
    let report = load_recovering(&main, &src_dir, &provider);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.files.len(), 4);
    assert!(!report.files.contains(&provider.identity(&shadowed)));
    assert_eq!(report.project.stage.variables["shared"], json!(4));
    assert_eq!(report.project.stage.variables["nearby"], json!(1));
    assert_eq!(report.project.stage.variables["fallback"], json!(2));
    assert!(!report.project.stage.variables.contains_key("shadowed"));
}

#[test]
fn source_read_errors_keep_target_file_and_do_not_stop_siblings() {
    struct FailingSource {
        main: PathBuf,
        broken: PathBuf,
    }
    impl SourceProvider for FailingSource {
        fn read_to_string(&self, path: &Path) -> Result<String> {
            if self.identity(path) == self.main {
                Ok("import broken\nimport broken\nrecovered = 1\n".into())
            } else {
                Err(Error::new("read failed"))
            }
        }
        fn is_file(&self, path: &Path) -> bool {
            [self.main.clone(), self.broken.clone()].contains(&self.identity(path))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let provider = FailingSource {
        main: normalize_path(&directory.path().join("main.sbdsl")),
        broken: normalize_path(&directory.path().join("broken.sbdsl")),
    };
    let report = load_recovering(&provider.main, directory.path(), &provider);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].0, "read failed");
    assert_eq!(
        Path::new(&report.errors[0].span().unwrap().file),
        provider.broken
    );
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.project.stage.variables["recovered"], json!(1));

    let missing = directory.path().join("missing.sbdsl");
    let report = load_recovering(&missing, directory.path(), &provider);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(
        Path::new(&report.errors[0].span().unwrap().file),
        provider.identity(&missing)
    );
    assert_eq!(report.files, BTreeSet::from([provider.identity(&missing)]));
}
