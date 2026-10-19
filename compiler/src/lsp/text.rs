//! The protocol uses UTF-16; the compiler keeps UTF-8 byte offsets.
use lsp_types::Position;
use std::path::{Path, PathBuf};

pub fn position(source: &str, byte: usize) -> Position {
    let mut end = byte.min(source.len());
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &source[..end];
    Position::new(
        prefix.bytes().filter(|&b| b == b'\n').count() as u32,
        prefix
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .trim_end_matches('\r')
            .encode_utf16()
            .count() as u32,
    )
}

pub fn uri_path(uri: &str) -> Option<PathBuf> {
    let url = url::Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path()
        .ok()
        .map(|p| crate::analysis::normalize_path(&p))
}

pub fn path_uri(path: &Path) -> Option<String> {
    // canonicalize on Windows returns verbatim paths, which are not file URIs.
    #[cfg(windows)]
    let cleaned = {
        let value = path.to_string_lossy();
        if let Some(rest) = value
            .strip_prefix(r"\\?\UNC\")
            .or_else(|| value.strip_prefix(r"\\?\unc\"))
        {
            PathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = value.strip_prefix(r"\\?\") {
            PathBuf::from(rest)
        } else {
            path.to_path_buf()
        }
    };
    #[cfg(not(windows))]
    let cleaned = path.to_path_buf();
    url::Url::from_file_path(cleaned).ok().map(Into::into)
}

/// Publish an unopened file under the same path spelling as the document that
/// imported it. Canonical paths identify sources internally, but an editor may
/// have opened the project through a junction, symlink, or Windows 8.3 name.
pub fn related_uri(path: &Path, document_uri: &str) -> Option<String> {
    let original = url::Url::parse(document_uri).ok()?.to_file_path().ok()?;
    for parent in original.parent()?.ancestors() {
        let canonical = crate::analysis::normalize_path(parent);
        let Ok(relative) = path.strip_prefix(&canonical) else {
            continue;
        };
        let candidate = parent.join(relative);
        // A descendant junction can point somewhere else. Never publish a URI
        // merely because its lexical suffix matches the analyzed source.
        if crate::analysis::normalize_path(&candidate) == path {
            return path_uri(&candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_emoji_crlf_and_partial_utf8() {
        let text = "中😀\r\n[错]";
        assert_eq!(position(text, 7), Position::new(0, 3));
        assert_eq!(position(text, 8), Position::new(0, 3));
        assert_eq!(position(text, 10), Position::new(1, 1));
        assert_eq!(position(text, 11), Position::new(1, 1));
        assert_eq!(position(text, text.len()), Position::new(1, 3));
    }
    #[test]
    fn file_uri_round_trip_with_spaces_and_chinese() {
        let directory = tempfile::tempdir().unwrap();
        let path = crate::analysis::normalize_path(&directory.path().join("中文 空格😀.sbdsl"));
        let uri = path_uri(&path).unwrap();
        assert!(uri.contains('%'));
        assert_eq!(uri_path(&uri).unwrap(), path);
        assert!(uri_path("untitled:Untitled-1").is_none());
    }
}
