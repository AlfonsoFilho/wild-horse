//! Syntax of the Wild Horse literate language: lexer, parser and AST for
//! `.wh.md` documents. See `docs/language-spec.md`.

pub mod ast;
pub mod diag;
pub mod document;
pub mod lexer;
pub mod parser;
pub mod span;

pub use diag::{Diagnostic, Severity};
pub use document::{Parsed, parse_document};
pub use parser::{is_hard_keyword, parse_expr, parse_type};
pub use span::Span;
