//! Hand-written lexer for `wh` blocks.
//!
//! Produces every byte of the input as a token (including trivia), so the
//! token stream is lossless. A second pass ([`logical_lines`]) turns the raw
//! tokens into the significant stream the parser consumes: trivia is dropped,
//! newlines inside brackets or before a `|>` continuation are dropped, and
//! every significant token knows the indentation of the line it starts.

use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // trivia
    Whitespace,
    Comment,
    Newline,
    // literals
    Ident,
    Int,
    Float,
    Str,
    RawStr,
    TripleStr,
    Bytes,
    Duration,
    // punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    Arrow,
    Pipe,
    PipeGt,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    SlashSlash,
    Percent,
    At,
    // logical
    Eof,
    Error,
}

impl TokenKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, TokenKind::Whitespace | TokenKind::Comment | TokenKind::Newline)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

/// Lex `src`, whose first byte sits at absolute document offset `base`.
pub fn lex(src: &str, base: u32) -> (Vec<Token>, Vec<LexError>) {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut errors = Vec::new();
    let mut i = 0usize;
    let span = |a: usize, b: usize| Span::new(base + a as u32, base + b as u32);

    while i < bytes.len() {
        let start = i;
        let c = bytes[i];
        let kind = match c {
            b' ' | b'\t' => {
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    if bytes[i] == b'\t' {
                        errors.push(LexError {
                            message: "tabs are not allowed; indent with 4 spaces".into(),
                            span: span(i, i + 1),
                        });
                    }
                    i += 1;
                }
                TokenKind::Whitespace
            }
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                i += 2;
                TokenKind::Newline
            }
            b'\n' => {
                i += 1;
                TokenKind::Newline
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                TokenKind::Comment
            }
            b'"' => {
                if src[i..].starts_with("\"\"\"") {
                    i += 3;
                    match src[i..].find("\"\"\"") {
                        Some(end) => {
                            i += end + 3;
                        }
                        None => {
                            errors.push(LexError {
                                message: "unterminated triple-quoted string".into(),
                                span: span(start, bytes.len()),
                            });
                            i = bytes.len();
                        }
                    }
                    TokenKind::TripleStr
                } else {
                    i = scan_string(bytes, i + 1, true, &mut errors, base, start);
                    TokenKind::Str
                }
            }
            b'r' if bytes.get(i + 1) == Some(&b'"') => {
                i = scan_string(bytes, i + 2, false, &mut errors, base, start);
                TokenKind::RawStr
            }
            b'b' if bytes.get(i + 1) == Some(&b'"') => {
                i = scan_string(bytes, i + 2, true, &mut errors, base, start);
                TokenKind::Bytes
            }
            c if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 => {
                while i < bytes.len()
                    && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric() || bytes[i] >= 0x80)
                {
                    i += 1;
                }
                TokenKind::Ident
            }
            c if c.is_ascii_digit() => scan_number(src, &mut i),
            _ => {
                let two = src.get(i..i + 2).unwrap_or("");
                let (k, n) = match two {
                    "->" => (TokenKind::Arrow, 2),
                    "|>" => (TokenKind::PipeGt, 2),
                    "==" => (TokenKind::EqEq, 2),
                    "!=" => (TokenKind::NotEq, 2),
                    "<=" => (TokenKind::Le, 2),
                    ">=" => (TokenKind::Ge, 2),
                    "**" => (TokenKind::StarStar, 2),
                    "//" => (TokenKind::SlashSlash, 2),
                    _ => match c {
                        b'(' => (TokenKind::LParen, 1),
                        b')' => (TokenKind::RParen, 1),
                        b'[' => (TokenKind::LBracket, 1),
                        b']' => (TokenKind::RBracket, 1),
                        b'{' => (TokenKind::LBrace, 1),
                        b'}' => (TokenKind::RBrace, 1),
                        b',' => (TokenKind::Comma, 1),
                        b':' => (TokenKind::Colon, 1),
                        b'.' => (TokenKind::Dot, 1),
                        b'|' => (TokenKind::Pipe, 1),
                        b'=' => (TokenKind::Eq, 1),
                        b'<' => (TokenKind::Lt, 1),
                        b'>' => (TokenKind::Gt, 1),
                        b'+' => (TokenKind::Plus, 1),
                        b'-' => (TokenKind::Minus, 1),
                        b'*' => (TokenKind::Star, 1),
                        b'/' => (TokenKind::Slash, 1),
                        b'%' => (TokenKind::Percent, 1),
                        b'@' => (TokenKind::At, 1),
                        _ => {
                            // consume one full UTF-8 char
                            let ch_len = src[i..].chars().next().map_or(1, |ch| ch.len_utf8());
                            errors.push(LexError {
                                message: format!("unexpected character {:?}", &src[i..i + ch_len]),
                                span: span(i, i + ch_len),
                            });
                            (TokenKind::Error, ch_len)
                        }
                    },
                };
                i += n;
                k
            }
        };
        toks.push(Token { kind, span: span(start, i) });
    }
    (toks, errors)
}

fn scan_string(
    bytes: &[u8],
    mut i: usize,
    escapes: bool,
    errors: &mut Vec<LexError>,
    base: u32,
    start: usize,
) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if escapes => i += 2,
            b'"' => return i + 1,
            b'\n' => break,
            _ => i += 1,
        }
    }
    errors.push(LexError {
        message: "unterminated string literal".into(),
        span: Span::new(base + start as u32, base + i.min(bytes.len()) as u32),
    });
    i.min(bytes.len())
}

const INT_SUFFIXES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "usize", "isize",
];
const FLOAT_SUFFIXES: &[&str] = &["f32", "f64"];
pub const DURATION_UNITS: &[&str] = &["ns", "us", "ms", "s"];

fn scan_number(src: &str, i: &mut usize) -> TokenKind {
    let bytes = src.as_bytes();
    let digits = |i: &mut usize, pred: fn(u8) -> bool| {
        while *i < bytes.len() && (pred(bytes[*i]) || bytes[*i] == b'_') {
            *i += 1;
        }
    };
    if bytes[*i] == b'0' && matches!(bytes.get(*i + 1), Some(b'x' | b'b' | b'o')) {
        let radix = bytes[*i + 1];
        *i += 2;
        match radix {
            b'x' => digits(i, |c| c.is_ascii_hexdigit()),
            b'b' => digits(i, |c| c == b'0' || c == b'1'),
            _ => digits(i, |c| (b'0'..=b'7').contains(&c)),
        }
        eat_suffix(src, i, INT_SUFFIXES);
        return TokenKind::Int;
    }
    digits(i, |c| c.is_ascii_digit());
    let mut float = false;
    if bytes.get(*i) == Some(&b'.') && bytes.get(*i + 1).is_some_and(|c| c.is_ascii_digit()) {
        float = true;
        *i += 1;
        digits(i, |c| c.is_ascii_digit());
    }
    if matches!(bytes.get(*i), Some(b'e' | b'E')) {
        let mut j = *i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(|c| c.is_ascii_digit()) {
            float = true;
            *i = j;
            digits(i, |c| c.is_ascii_digit());
        }
    }
    if float {
        eat_suffix(src, i, FLOAT_SUFFIXES);
        return TokenKind::Float;
    }
    if eat_suffix(src, i, DURATION_UNITS) {
        return TokenKind::Duration;
    }
    if !eat_suffix(src, i, INT_SUFFIXES) && eat_suffix(src, i, FLOAT_SUFFIXES) {
        return TokenKind::Float;
    }
    TokenKind::Int
}

/// Consume a suffix if it is followed by a non-identifier character.
fn eat_suffix(src: &str, i: &mut usize, suffixes: &[&str]) -> bool {
    let rest = &src[*i..];
    // longest first so `us` does not shadow `usize`, `s` does not shadow `ms`
    let mut best: Option<&str> = None;
    for s in suffixes {
        if rest.starts_with(s) {
            let after = rest.as_bytes().get(s.len());
            let boundary = after.is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'));
            if boundary && best.is_none_or(|b| s.len() > b.len()) {
                best = Some(s);
            }
        }
    }
    if let Some(s) = best {
        *i += s.len();
        true
    } else {
        false
    }
}

/// A significant token plus its line information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigToken {
    pub kind: TokenKind,
    pub span: Span,
    /// Column (in bytes) of the first significant token of this token's logical line.
    pub line_indent: u32,
    /// True if this is the first significant token of a logical line.
    pub line_start: bool,
}

/// Turn raw tokens into the parser's significant stream.
///
/// `Newline` tokens are kept only when they terminate a non-empty logical line
/// at bracket depth zero and the next line does not start with `|>`.
/// The stream always ends with a `Newline` followed by `Eof`.
pub fn logical_lines(toks: &[Token], end: u32) -> Vec<SigToken> {
    let mut out: Vec<SigToken> = Vec::new();
    let mut depth: i32 = 0;
    let mut line_has_content = false;
    let mut col: u32 = 0; // column of the next token on the current physical line
    let mut current_indent: u32 = 0;
    let mut physical_line_start = true;

    // index of next significant token, used to detect `|>` continuations
    let next_sig = |from: usize| toks[from..].iter().find(|t| !t.kind.is_trivia()).map(|t| t.kind);

    for (idx, t) in toks.iter().enumerate() {
        match t.kind {
            TokenKind::Whitespace => {
                col += t.span.len();
            }
            TokenKind::Comment => {}
            TokenKind::Newline => {
                col = 0;
                physical_line_start = true;
                if depth > 0 || !line_has_content {
                    continue;
                }
                if next_sig(idx + 1) == Some(TokenKind::PipeGt) {
                    continue;
                }
                out.push(SigToken {
                    kind: TokenKind::Newline,
                    span: t.span,
                    line_indent: current_indent,
                    line_start: false,
                });
                line_has_content = false;
            }
            kind => {
                let line_start = !line_has_content;
                if line_start {
                    current_indent = if physical_line_start { col } else { current_indent };
                }
                physical_line_start = false;
                col += t.span.len();
                match kind {
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                        depth = (depth - 1).max(0)
                    }
                    _ => {}
                }
                out.push(SigToken { kind, span: t.span, line_indent: current_indent, line_start });
                line_has_content = true;
            }
        }
    }
    let end_span = Span::new(end, end);
    if line_has_content {
        out.push(SigToken {
            kind: TokenKind::Newline,
            span: end_span,
            line_indent: current_indent,
            line_start: false,
        });
    }
    out.push(SigToken { kind: TokenKind::Eof, span: end_span, line_indent: 0, line_start: true });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (t, e) = lex(src, 0);
        assert!(e.is_empty(), "{e:?}");
        t.into_iter().filter(|t| !t.kind.is_trivia()).map(|t| t.kind).collect()
    }

    #[test]
    fn lossless() {
        let src = "def f(x: int) -> str:  # c\n    ensures result == \"a\\\"b\"\n";
        let (t, _) = lex(src, 0);
        let joined: String = t.iter().map(|t| &src[t.span.range()]).collect();
        assert_eq!(joined, src);
    }

    #[test]
    fn numbers_and_durations() {
        use TokenKind::*;
        assert_eq!(kinds("1_000 0xFF 3.14 1e-3 1u8 16ms 50us 5s 2f32"), vec![
            Int, Int, Float, Float, Int, Duration, Duration, Duration, Float
        ]);
    }

    #[test]
    fn continuation_and_brackets() {
        let src = "flow f(x: A) -> B:\n    x\n    |> g\n    |> h(\n        1)\n";
        let (t, _) = lex(src, 0);
        let sig = logical_lines(&t, src.len() as u32);
        let newlines = sig.iter().filter(|t| t.kind == TokenKind::Newline).count();
        assert_eq!(newlines, 2);
    }
}
