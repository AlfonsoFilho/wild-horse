use serde::Serialize;

use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Severity {
    Error,
    Warning,
    Lint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self { severity: Severity::Error, message: message.into(), span }
    }

    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Self { severity: Severity::Warning, message: message.into(), span }
    }

    pub fn lint(message: impl Into<String>, span: Span) -> Self {
        Self { severity: Severity::Lint, message: message.into(), span }
    }

    /// `file:line:col: severity: message`
    pub fn render(&self, file: &str, src: &str) -> String {
        let (line, col) = crate::span::line_col(src, self.span.start);
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Lint => "lint",
        };
        format!("{file}:{}:{}: {sev}: {}", line + 1, col + 1, self.message)
    }
}
