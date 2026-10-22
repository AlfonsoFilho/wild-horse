//! Token-based highlighting of `wh` code from `wh_syntax::lexer`.

use std::ops::Range;

use wh_syntax::lexer::{TokenKind, lex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    Keyword,
    Type,
    Function,
    String,
    Number,
    Comment,
    Operator,
    Decorator,
    Constant,
}

const SOFT: &[&str] = &[
    "match", "case", "on", "initial", "emit", "ignore", "target", "provide", "entry", "depends", "given", "expect", "bench",
    "mutate", "pattern", "note", "complexity", "optimize", "budget", "alloc", "unsafe", "no_panic", "key", "when", "result",
    "old", "self", "sensitive", "trace", "log", "parallel",
];

/// Highlight ranges (relative to `src`) for one `wh` block's code.
pub fn highlight(src: &str) -> Vec<(Range<usize>, Class)> {
    let (toks, _) = lex(src, 0);
    let sig: Vec<usize> = (0..toks.len()).filter(|&i| !toks[i].kind.is_trivia()).collect();
    let mut out = Vec::new();
    for (k, &i) in sig.iter().enumerate() {
        let t = toks[i];
        let r = t.span.range();
        let text = &src[r.clone()];
        let next = sig.get(k + 1).map(|&j| toks[j].kind);
        let prev = k.checked_sub(1).map(|j| toks[sig[j]].kind);
        let class = match t.kind {
            TokenKind::Ident => {
                if matches!(text, "True" | "False" | "None") {
                    Some(Class::Constant)
                } else if prev == Some(TokenKind::At) {
                    Some(Class::Decorator)
                } else if wh_syntax::is_hard_keyword(text) || (SOFT.contains(&text) && line_start_or_clause(&toks, &sig, k)) {
                    Some(Class::Keyword)
                } else if text.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    Some(Class::Type)
                } else if next == Some(TokenKind::LParen) || prev == Some(TokenKind::PipeGt) {
                    Some(Class::Function)
                } else {
                    None
                }
            }
            TokenKind::Str | TokenKind::RawStr | TokenKind::TripleStr | TokenKind::Bytes => Some(Class::String),
            TokenKind::Int | TokenKind::Float | TokenKind::Duration => Some(Class::Number),
            TokenKind::At => Some(Class::Decorator),
            TokenKind::PipeGt | TokenKind::Arrow | TokenKind::Pipe => Some(Class::Operator),
            _ => None,
        };
        if let Some(c) = class {
            out.push((r, c));
        }
    }
    for t in &toks {
        if t.kind == TokenKind::Comment {
            out.push((t.span.range(), Class::Comment));
        }
    }
    out.sort_by_key(|(r, _)| r.start);
    out
}

/// Soft keywords only count at the start of a line (or after a decorator/`@`).
fn line_start_or_clause(toks: &[wh_syntax::lexer::Token], sig: &[usize], k: usize) -> bool {
    let i = sig[k];
    let text_before = toks[..i].iter().rev().take_while(|t| t.kind != TokenKind::Newline).all(|t| t.kind.is_trivia());
    let after_dot = k > 0 && toks[sig[k - 1]].kind == TokenKind::Dot;
    (text_before || matches!(toks.get(sig.get(k + 1).copied().unwrap_or(usize::MAX)).map(|t| t.kind), Some(TokenKind::LParen)))
        && !after_dot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        let src = "@error\nenum E:\n    A(from X)\n\ndef f(x: Email) -> int:  # hi\n    ensures result == \"a\"\n    note \"n\"\n";
        let h = highlight(src);
        let class_of = |s: &str| h.iter().find(|(r, _)| &src[r.clone()] == s).map(|(_, c)| *c);
        assert_eq!(class_of("def"), Some(Class::Keyword));
        assert_eq!(class_of("enum"), Some(Class::Keyword));
        assert_eq!(class_of("Email"), Some(Class::Type));
        assert_eq!(class_of("f"), Some(Class::Function));
        assert_eq!(class_of("\"a\""), Some(Class::String));
        assert_eq!(class_of("# hi"), Some(Class::Comment));
        assert_eq!(class_of("ensures"), Some(Class::Keyword));
        assert_eq!(class_of("note"), Some(Class::Keyword));
        assert_eq!(class_of("error"), Some(Class::Decorator));
    }
}
