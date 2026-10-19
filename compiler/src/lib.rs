pub mod analysis;
pub mod build;
mod builtins;
pub mod codegen;
pub mod config;
pub mod error;
pub mod extensions;
pub mod lsp;
pub mod model;
pub mod notation;
pub mod parser;
pub mod project;
pub mod registry;

pub use error::{Error, Result};
