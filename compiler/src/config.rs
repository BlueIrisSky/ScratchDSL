pub use crate::model::Asset;
use crate::{
    Error, Result,
    analysis::{DiskSource, SourceProvider},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "sbdsl.buildsb";

pub fn is_config(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "buildsb")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ExtensionSpec {
    pub path: PathBuf,
    pub alias: Option<String>,
}

impl From<PathBuf> for ExtensionSpec {
    fn from(path: PathBuf) -> Self {
        Self { path, alias: None }
    }
}

impl<'de> Deserialize<'de> for ExtensionSpec {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Path(PathBuf),
            Named(Named),
        }

        // Named objects are emitted internally by the build notation parser.
        // Its source syntax permits only `"path" as "alias"`.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Named {
            path: PathBuf,
            alias: Option<String>,
        }

        let spec = match Input::deserialize(deserializer)? {
            Input::Path(path) => Self::from(path),
            Input::Named(Named { path, alias }) => Self { path, alias },
        };
        if let Some(alias) = &spec.alias {
            validate_extension_alias(alias).map_err(serde::de::Error::custom)?;
        }
        Ok(spec)
    }
}

pub(crate) fn validate_extension_alias(alias: &str) -> std::result::Result<(), &'static str> {
    if alias.trim().is_empty() {
        return Err("拓展别名不能为空或仅包含空白");
    }
    if alias.chars().any(char::is_control) {
        return Err("拓展别名不能包含控制字符");
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub name: Option<String>,
    pub entry: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub src_dir: Option<PathBuf>,
    #[serde(default)]
    pub extensions: Vec<ExtensionSpec>,
    pub runtime: Option<RuntimeConfig>,
    #[serde(default)]
    pub stage: StageConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeConfig {
    pub fps: Option<f64>,
    pub stage_width: Option<u32>,
    pub stage_height: Option<u32>,
    pub max_clones: Option<u32>,
    pub fencing: Option<bool>,
    pub misc_limits: Option<bool>,
    pub hq_pen: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageConfig {
    #[serde(default)]
    pub backdrops: Vec<Asset>,
    #[serde(default)]
    pub sounds: Vec<Asset>,
}

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub config: Config,
    pub base_dir: PathBuf,
    pub entry: PathBuf,
    pub src_dir: PathBuf,
    pub output: PathBuf,
    pub extensions: Vec<ExtensionSpec>,
    pub config_path: Option<PathBuf>,
}

/// Local paths only. A Windows drive prefix is not a URI scheme.
pub fn local_path(path: &Path) -> Result<()> {
    let s = path.to_string_lossy();
    if let Some(colon) = s.find(':') {
        let drive = colon == 1
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.as_bytes()
                .get(2)
                .is_some_and(|c| *c == b'/' || *c == b'\\');
        let scheme = colon > 0
            && s[..colon].chars().enumerate().all(|(i, c)| {
                c.is_ascii_alphabetic()
                    || i > 0 && (c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
            });
        if scheme && !drive {
            return Err(Error::new(format!("只支持本地文件路径，不支持 URL: {s}")));
        }
    }
    Ok(())
}

pub fn resolve(base: &Path, path: &Path) -> Result<PathBuf> {
    local_path(path)?;
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

pub fn discover(input: Option<&Path>, output: Option<&Path>, cwd: &Path) -> Result<BuildConfig> {
    discover_with_provider(input, output, cwd, &DiskSource)
}

/// CLI discovery semantics, with reads/existence supplied by a source snapshot.
pub fn discover_with_provider(
    input: Option<&Path>,
    output: Option<&Path>,
    cwd: &Path,
    provider: &dyn SourceProvider,
) -> Result<BuildConfig> {
    let input = input.map(|p| resolve(cwd, p)).transpose()?;
    let direct_source = input
        .as_ref()
        .filter(|p| p.extension().is_some_and(|e| e == "sbdsl"));
    let config_path = if let Some(source) = direct_source {
        source.parent().and_then(|parent| {
            parent
                .ancestors()
                .map(|p| p.join(CONFIG_FILE))
                .find(|p| provider.is_file(p))
        })
    } else {
        Some(input.clone().unwrap_or_else(|| cwd.join(CONFIG_FILE)))
    };
    let config = if let Some(path) = &config_path {
        if !is_config(path) {
            return Err(
                Error::new("构建输入必须是 .sbdsl 源文件或 .buildsb 配置文件").in_file(path),
            );
        }
        let data = provider.read_to_string(path)?;
        crate::notation::parse_build::<Config>(path, &data)?
    } else {
        Config::default()
    };
    let error_path = config_path
        .clone()
        .or_else(|| input.clone())
        .unwrap_or_else(|| cwd.join(CONFIG_FILE));
    (|| {
        let base_dir = config_path
            .as_ref()
            .and_then(|p| p.parent())
            .or_else(|| direct_source.and_then(|p| p.parent()))
            .unwrap_or(cwd)
            .to_path_buf();
        let entry = if let Some(source) = direct_source {
            source.clone()
        } else {
            resolve(
                &base_dir,
                config
                    .entry
                    .as_deref()
                    .ok_or_else(|| Error::new("配置缺少唯一的 entry 源文件"))?,
            )?
        };
        if entry.extension().is_none_or(|e| e != "sbdsl") {
            return Err(Error::new("entry 必须是单个 .sbdsl 文件"));
        }
        let src_dir = match &config.src_dir {
            Some(p) => resolve(&base_dir, p)?,
            None if config_path.is_some() => base_dir.join("src"),
            None => entry.parent().unwrap_or(&base_dir).to_path_buf(),
        };
        let output = if let Some(path) = output {
            resolve(cwd, path)?
        } else if let Some(path) = &config.output {
            resolve(&base_dir, path)?
        } else {
            entry.with_extension("sb3")
        };
        if output == entry || config_path.as_ref().is_some_and(|p| *p == output) {
            return Err(Error::new("输出不能覆盖输入源码或配置"));
        }
        let extensions = config
            .extensions
            .iter()
            .map(|spec| {
                Ok(ExtensionSpec {
                    path: resolve(&base_dir, &spec.path)?,
                    alias: spec.alias.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        validate(&config)?;
        Ok(BuildConfig {
            config,
            base_dir,
            entry,
            src_dir,
            output,
            extensions,
            config_path,
        })
    })()
    .map_err(|error| error.in_file(&error_path))
}

/// Editor discovery deliberately keeps the nearest config's declared entry.
/// Passing the opened module to CLI discovery would incorrectly override it.
pub fn discover_for_document(path: &Path, provider: &dyn SourceProvider) -> Result<BuildConfig> {
    let path = provider.identity(path);
    let parent = path.parent().unwrap_or(Path::new("."));
    let config = if is_config(&path) {
        Some(path.clone())
    } else {
        parent
            .ancestors()
            .map(|p| p.join(CONFIG_FILE))
            .find(|p| provider.is_file(p))
    };
    discover_with_provider(
        Some(config.as_deref().unwrap_or(&path)),
        None,
        parent,
        provider,
    )
}

fn validate(config: &Config) -> Result<()> {
    if let Some(r) = &config.runtime
        && (r.fps.is_some_and(|v| !v.is_finite() || v <= 0.)
            || r.stage_width == Some(0)
            || r.stage_height == Some(0))
    {
        return Err(Error::new(
            "runtime 的 fps、stageWidth、stageHeight 必须为正数",
        ));
    }
    Ok(())
}

pub fn json_literal(value: &Value) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn no_url_but_windows_drives() {
        for p in ["https://example/a.js", "file:///a.js", "data:text/plain,a"] {
            assert!(local_path(Path::new(p)).is_err());
        }
        for p in [
            "C:/项目/自定义拓展.js",
            "C:\\项目\\自定义拓展.js",
            "./extensions/自定义拓展.js",
        ] {
            assert!(local_path(Path::new(p)).is_ok());
        }
    }
    #[test]
    fn source_discovers_config_and_cli_wins() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("sbdsl.buildsb"),
            r#"{"entry":"src/other.sbdsl","output":"dist/config.sb3"}"#,
        )
        .unwrap();
        let source = temp.path().join("src/main.sbdsl");
        let c = discover(Some(&source), Some(Path::new("cli.sb3")), temp.path()).unwrap();
        assert_eq!(c.entry, source);
        assert_eq!(c.output, temp.path().join("cli.sb3"));
        assert_eq!(c.src_dir, temp.path().join("src"));
    }

    #[test]
    fn build_notation_accepts_minimal_and_nested_configuration() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(CONFIG_FILE),
            "# 构建配置\nentry: \"src/main.sbdsl\"\noutput: \"dist/成品.sb3\"\nruntime: {\n fps: 60\n stageWidth: 640\n fencing: false\n}\nextensions: [\n \"自定义拓展.js\"\n]\n",
        ).unwrap();
        let config = discover(None, None, temp.path()).unwrap();
        assert_eq!(config.entry, temp.path().join("src/main.sbdsl"));
        assert_eq!(config.output, temp.path().join("dist/成品.sb3"));
        assert_eq!(
            config.extensions,
            [temp.path().join("自定义拓展.js").into()]
        );
        let runtime = config.config.runtime.unwrap();
        assert_eq!(runtime.stage_width, Some(640));
        assert_eq!(runtime.fencing, Some(false));
    }

    #[test]
    fn extension_specs_resolve_paths_and_preserve_exact_aliases() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(CONFIG_FILE),
            "entry: \"main.sbdsl\"\nextensions: [\n \"extensions/first.js\"\n \"extensions/second.js\" as \" 张量-拓展 \"\n]\n",
        )
        .unwrap();
        let config = discover(None, None, temp.path()).unwrap();
        assert_eq!(
            config.config.extensions,
            [
                PathBuf::from("extensions/first.js").into(),
                ExtensionSpec {
                    path: PathBuf::from("extensions/second.js"),
                    alias: Some(" 张量-拓展 ".into()),
                },
            ]
        );
        assert_eq!(
            config.extensions,
            [
                temp.path().join("extensions/first.js").into(),
                ExtensionSpec {
                    path: temp.path().join("extensions/second.js"),
                    alias: Some(" 张量-拓展 ".into()),
                },
            ]
        );
    }

    #[test]
    fn extension_alias_validation_also_applies_to_deserialization() {
        assert_eq!(
            serde_json::from_str::<ExtensionSpec>(r#""first.js""#).unwrap(),
            PathBuf::from("first.js").into()
        );
        for alias in ["", " \u{3000}", "line\nbreak", "nul\0", "delete\u{7f}"] {
            let value = serde_json::json!({"path":"first.js", "alias":alias});
            assert!(serde_json::from_value::<ExtensionSpec>(value).is_err());
        }
        let spec = ExtensionSpec {
            path: PathBuf::from("自定义拓展.js"),
            alias: Some("张量 运算-1".into()),
        };
        assert_eq!(
            serde_json::from_value::<ExtensionSpec>(serde_json::to_value(&spec).unwrap()).unwrap(),
            spec
        );
    }

    #[test]
    fn explicit_build_file_uses_its_own_entry_and_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(CONFIG_FILE), "entry: \"default.sbdsl\"\n").unwrap();
        let selected = temp.path().join("release.buildsb");
        fs::write(
            &selected,
            "entry: \"release.sbdsl\"\noutput: \"dist/release.sb3\"\n",
        )
        .unwrap();
        let config = discover_for_document(&selected, &DiskSource).unwrap();
        assert_eq!(
            DiskSource.identity(&config.entry),
            DiskSource.identity(&temp.path().join("release.sbdsl"))
        );
        assert_eq!(
            DiskSource.identity(&config.output),
            DiskSource.identity(&temp.path().join("dist/release.sb3"))
        );
        assert_eq!(config.config_path, Some(DiskSource.identity(&selected)));
    }

    #[test]
    fn unrelated_json_does_not_become_a_project_configuration() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.json"), "not a ScratchDSL config").unwrap();
        let source = temp.path().join("main.sbdsl");
        let config = discover(Some(&source), None, temp.path()).unwrap();
        assert!(config.config_path.is_none());
        assert_eq!(config.entry, source);
        assert!(discover(Some(Path::new("config.json")), None, temp.path()).is_err());
    }

    #[test]
    fn build_notation_errors_keep_the_configuration_location() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(CONFIG_FILE);
        for source in [
            "entry: \"main.sbdsl\"\nentry: \"other.sbdsl\"\n",
            "entry: \"main.sbdsl\"\nsprites: {}\n",
            "entry: \"main.sbdsl\"\nruntime: { fps: false }\n",
        ] {
            fs::write(&file, source).unwrap();
            let error = discover(None, None, temp.path()).unwrap_err();
            let span = error
                .span()
                .expect("configuration errors must have source locations");
            assert_eq!(Path::new(&span.file), file);
            assert_eq!(span.line, 2, "{error}");
        }
        fs::write(
            &file,
            "entry: \"main.sbdsl\"\nextensions: [\"https://example.test/a.js\"]\n",
        )
        .unwrap();
        assert!(
            discover(None, None, temp.path())
                .unwrap_err()
                .0
                .contains("只支持本地")
        );
    }
}
