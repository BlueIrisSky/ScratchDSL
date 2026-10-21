//! Source identities and reads shared by disk builds and editor snapshots.
use crate::{Error, Result};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

/// Existing paths use their real identity. New files resolve the nearest existing
/// ancestor before appending the missing components, so saving does not normally
/// change their identity (including projects opened through directory symlinks).
pub fn normalize_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let canonical = if let Ok(path) = absolute.canonicalize() {
        path
    } else {
        let mut lexical = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    lexical.pop();
                }
                other => lexical.push(other.as_os_str()),
            }
        }
        let mut ancestor = lexical.as_path();
        let mut missing = Vec::new();
        loop {
            if let Ok(mut base) = ancestor.canonicalize() {
                for part in missing.iter().rev() {
                    base.push(part);
                }
                break base;
            }
            let Some(name) = ancestor.file_name() else {
                break lexical;
            };
            missing.push(name.to_os_string());
            let Some(parent) = ancestor.parent() else {
                break lexical;
            };
            ancestor = parent;
        }
    };
    // Fold ASCII case for Windows file URIs/imports, including unsaved files.
    // Unicode lowercasing can change a filename (e.g. İ -> i + combining dot),
    // so existing non-ASCII aliases are resolved only by canonicalize above.
    #[cfg(windows)]
    let canonical = PathBuf::from(canonical.as_os_str().to_ascii_lowercase());
    canonical
}

pub trait SourceProvider {
    fn read_to_string(&self, path: &Path) -> Result<String>;
    fn is_file(&self, path: &Path) -> bool;
    fn identity(&self, path: &Path) -> PathBuf {
        normalize_path(path)
    }
}

pub struct DiskSource;
impl SourceProvider for DiskSource {
    fn read_to_string(&self, path: &Path) -> Result<String> {
        fs::read_to_string(path).map_err(|e| {
            Error::new(format!("读取 UTF-8 文件 {}: {e}", path.display())).in_file(path)
        })
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// Own a frozen copy of the editor buffers. Disk is consulted only for unopened
/// paths, including existence tests during import/config discovery.
pub struct OverlaySource {
    overlays: BTreeMap<PathBuf, String>,
}
impl OverlaySource {
    pub fn new(overlays: &BTreeMap<PathBuf, String>) -> Self {
        Self {
            overlays: overlays
                .iter()
                .map(|(p, text)| (normalize_path(p), text.clone()))
                .collect(),
        }
    }

    pub(crate) fn insert(&mut self, path: PathBuf, text: String) {
        self.overlays.insert(path, text);
    }
}
impl SourceProvider for OverlaySource {
    fn read_to_string(&self, path: &Path) -> Result<String> {
        self.overlays
            .get(&self.identity(path))
            .cloned()
            .map_or_else(|| DiskSource.read_to_string(path), Ok)
    }
    fn is_file(&self, path: &Path) -> bool {
        self.overlays.contains_key(&self.identity(path)) || path.is_file()
    }
}
