use crate::model::Span;
use std::{fmt, path::Path};

#[derive(Debug, Clone)]
pub struct Error(pub String, pub Option<Span>);

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into(), None)
    }
    pub fn at(mut self, span: &Span) -> Self {
        // Context can be added by several layers; retain the most specific origin.
        if self.1.is_none() {
            self.1 = Some(span.clone());
        }
        self
    }
    pub fn span(&self) -> Option<&Span> {
        self.1.as_ref()
    }
    pub fn in_file(self, path: &Path) -> Self {
        self.at(&Span {
            file: path.display().to_string(),
            line: 1,
            column: 1,
            ..Span::default()
        })
    }
    pub fn at_source(self, path: &Path, source: &str, start: usize, end: usize) -> Self {
        self.at(&Span::from_source(path, source, start, end))
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(span) = &self.1 {
            write!(f, "{}:{}:{}: ", span.file, span.line, span.column)?;
        }
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(e.to_string())
    }
}
impl From<zip::result::ZipError> for Error {
    fn from(e: zip::result::ZipError) -> Self {
        Self::new(e.to_string())
    }
}
