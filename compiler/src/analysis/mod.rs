//! Shared source analysis for the CLI and editor. No block graph, ZIP, or media
//! decoding is needed to publish diagnostics.
pub(crate) mod bind;
mod source;

use crate::{Error, Result, config, parser, project, registry::Registry};
pub use source::{DiskSource, OverlaySource, SourceProvider, normalize_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisDiagnostic {
    pub path: PathBuf,
    /// Half-open UTF-8 byte range in this file's current snapshot.
    pub start: usize,
    pub end: usize,
    pub message: String,
    /// LSP severity: 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u32,
}

#[derive(Debug, Clone)]
pub struct AnalysisResult {
    pub diagnostics: Vec<AnalysisDiagnostic>,
    pub registry: Registry,
}

#[derive(Default)]
pub struct AnalysisCache {
    extensions: BTreeMap<Vec<config::ExtensionSpec>, CachedRegistry>,
    #[cfg(test)]
    extension_extractions: usize,
}

struct CachedRegistry {
    fingerprint: Vec<(PathBuf, String)>,
    value: Result<Registry>,
}

impl AnalysisCache {
    /// Cache the complete validated registry for an ordered configuration. The
    /// original filenames and aliases remain part of the key: canonical paths
    /// alone would lose the namespace, including unsaved configuration edits.
    fn registry(&mut self, extensions: &[config::ExtensionSpec]) -> Result<Registry> {
        if extensions.is_empty() {
            return Ok(Registry::new());
        }
        let fingerprint = extensions
            .iter()
            .map(|extension| {
                let path = &extension.path;
                let bytes = fs::read(path).map_err(|e| {
                    Error::new(format!("读取扩展 {}: {e}", path.display())).in_file(path)
                })?;
                Ok((normalize_path(path), format!("{:x}", md5::compute(bytes))))
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(cached) = self.extensions.get(extensions)
            && cached.fingerprint == fingerprint
        {
            return cached.value.clone();
        }
        #[cfg(test)]
        {
            self.extension_extractions += 1;
        }
        let mut registry = Registry::new();
        let value =
            crate::extensions::load_extensions(extensions, &mut registry).map(|()| registry);
        // Bound memory in a long-lived server; clearing only discards reusable
        // metadata, never the current source snapshot or diagnostics.
        if self.extensions.len() >= 32 {
            self.extensions.clear();
        }
        self.extensions.insert(
            extensions.to_vec(),
            CachedRegistry {
                fingerprint,
                value: value.clone(),
            },
        );
        value
    }
}

/// Analyze one document and its nearest config's complete import graph.
pub fn analyze_document(
    path: &Path,
    overlays: &BTreeMap<PathBuf, String>,
    cache: &mut AnalysisCache,
    allow_extensions: bool,
) -> AnalysisResult {
    analyze_documents(&[path.to_path_buf()], overlays, cache, allow_extensions)
        .remove(&normalize_path(path))
        .expect("the requested document is always analyzed")
}

/// Analyze each project once within an immutable editor snapshot. A new call
/// rebuilds the ASTs from current buffers/disk; only extension metadata persists.
/// Unimported open files still receive their own syntax diagnostics.
pub fn analyze_documents(
    paths: &[PathBuf],
    overlays: &BTreeMap<PathBuf, String>,
    cache: &mut AnalysisCache,
    allow_extensions: bool,
) -> BTreeMap<PathBuf, AnalysisResult> {
    let provider = OverlaySource::new(overlays);
    let mut projects = BTreeMap::<PathBuf, (AnalysisResult, BTreeSet<PathBuf>)>::new();
    let mut results = BTreeMap::new();
    for path in paths {
        let path = provider.identity(path);
        if results.contains_key(&path) {
            continue;
        }
        let (mut result, included) = match config::discover_for_document(&path, &provider) {
            Ok(config) => {
                let key = provider.identity(config.config_path.as_deref().unwrap_or(&config.entry));
                let (result, files) = projects.entry(key).or_insert_with(|| {
                    analyze_project(&config, &provider, cache, allow_extensions)
                });
                (result.clone(), files.contains(&path))
            }
            Err(error) => {
                let mut result = AnalysisResult {
                    diagnostics: Vec::new(),
                    registry: Registry::new(),
                };
                add_error(&mut result, error, &path, &provider);
                (result, false)
            }
        };
        if !included
            && path
                .extension()
                .is_some_and(|extension| extension == "sbdsl")
        {
            match provider.read_to_string(&path) {
                Ok(source) => {
                    for error in parser::parse_recovering(&path, &source).errors {
                        add_error(&mut result, error, &path, &provider);
                    }
                }
                Err(error) => add_error(&mut result, error, &path, &provider),
            }
        }
        result.diagnostics.sort_by(|left, right| {
            (&left.path, left.start, left.end, &left.message).cmp(&(
                &right.path,
                right.start,
                right.end,
                &right.message,
            ))
        });
        results.insert(path, result);
    }
    results
}

fn analyze_project(
    config: &config::BuildConfig,
    provider: &dyn SourceProvider,
    cache: &mut AnalysisCache,
    allow_extensions: bool,
) -> (AnalysisResult, BTreeSet<PathBuf>) {
    let mut result = AnalysisResult {
        diagnostics: Vec::new(),
        registry: Registry::new(),
    };
    let report = project::load_recovering(&config.entry, &config.src_dir, provider);
    for error in report.errors {
        add_error(&mut result, error, &config.entry, provider);
    }
    // Untrusted workspaces still get built-in semantics and import validation.
    // Skip binding only when configured extensions were intentionally not loaded;
    // otherwise their missing schema would create misleading follow-on errors.
    let can_bind = if allow_extensions || config.extensions.is_empty() {
        match cache.registry(&config.extensions) {
            Ok(registry) => {
                result.registry = registry;
                true
            }
            Err(error) => {
                add_error(
                    &mut result,
                    error,
                    config.config_path.as_deref().unwrap_or(&config.entry),
                    provider,
                );
                false
            }
        }
    } else {
        false
    };
    if can_bind {
        for error in bind::diagnose_project(&report.project, &result.registry) {
            add_error(&mut result, error, &config.entry, provider);
        }
    }
    (result, report.files)
}

fn add_error(
    result: &mut AnalysisResult,
    error: Error,
    fallback: &Path,
    provider: &dyn SourceProvider,
) {
    let span = error.span();
    let path = provider.identity(span.map_or(fallback, |s| Path::new(&s.file)));
    let source = provider.read_to_string(&path).unwrap_or_default();
    let start = source.floor_char_boundary(span.map_or(0, |s| s.start));
    let mut end = source
        .floor_char_boundary(span.map_or(start, |s| s.end))
        .max(start);
    if end == start {
        end += source[start..].chars().next().map_or(0, char::len_utf8);
    }
    let diagnostic = AnalysisDiagnostic {
        path,
        start,
        end,
        message: error.0,
        severity: 1,
    };
    if !result.diagnostics.contains(&diagnostic) {
        result.diagnostics.push(diagnostic);
    }
}

#[cfg(test)]
mod tests;
