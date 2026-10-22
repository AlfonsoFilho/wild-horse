//! Recursive-descent / Pratt parser for the contents of one `wh` block.
//!
//! Indentation is handled structurally: every significant token knows the
//! indentation of its logical line, and [`Parser::block`] collects the lines
//! indented deeper than the header that opened the block. Errors are
//! recovered at line granularity, so one broken clause never hides the rest
//! of the document.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::lexer::{SigToken, TokenKind, logical_lines, lex};
use crate::span::Span;

pub const HARD_KEYWORDS: &[&str] = &[
    "import", "from", "as", "type", "alias", "struct", "enum", "capability", "adapter",
    "implements", "def", "async", "flow", "machine", "app", "test", "extern", "pure", "uses",
    "raises", "requires", "ensures", "invariant", "example", "property", "where", "forall",
    "exists", "in", "is", "and", "or", "not", "if", "else", "lambda", "return", "own", "mut",
    "True", "False", "None",
];

pub fn is_hard_keyword(s: &str) -> bool {
    HARD_KEYWORDS.contains(&s)
}

/// Parse the code of a `wh` block located at `content` within `doc`.
pub fn parse_block(doc: &str, content: Span) -> (Vec<Item>, Vec<Diagnostic>) {
    let src = &doc[content.range()];
    let (raw, lex_errors) = lex(src, content.start);
    let toks = logical_lines(&raw, content.end);
    let mut p = Parser { doc, toks, pos: 0, diags: Vec::new() };
    p.diags.extend(lex_errors.into_iter().map(|e| Diagnostic::error(e.message, e.span)));
    let items = p.items();
    (items, p.diags)
}

/// Parse a standalone expression (used by edit operations and the inspector).
pub fn parse_expr(src: &str) -> (Option<Expr>, Vec<Diagnostic>) {
    let (raw, lex_errors) = lex(src, 0);
    let toks = logical_lines(&raw, src.len() as u32);
    let mut p = Parser { doc: src, toks, pos: 0, diags: Vec::new() };
    p.diags.extend(lex_errors.into_iter().map(|e| Diagnostic::error(e.message, e.span)));
    let e = p.expr();
    if e.is_some() && !p.at_line_end() {
        let t = p.peek();
        p.error("unexpected trailing tokens", t.span);
    }
    (e, p.diags)
}

/// Parse a standalone type expression.
pub fn parse_type(src: &str) -> (Option<TypeExpr>, Vec<Diagnostic>) {
    let (raw, lex_errors) = lex(src, 0);
    let toks = logical_lines(&raw, src.len() as u32);
    let mut p = Parser { doc: src, toks, pos: 0, diags: Vec::new() };
    p.diags.extend(lex_errors.into_iter().map(|e| Diagnostic::error(e.message, e.span)));
    let t = p.ty();
    if t.is_some() && !p.at_line_end() {
        let tok = p.peek();
        p.error("unexpected trailing tokens", tok.span);
    }
    (t, p.diags)
}

struct Parser<'a> {
    doc: &'a str,
    toks: Vec<SigToken>,
    pos: usize,
    diags: Vec<Diagnostic>,
}

type P<T> = Option<T>;

impl<'a> Parser<'a> {
    // ------------------------------------------------------------ token helpers

    fn peek(&self) -> SigToken {
        self.toks[self.pos.min(self.toks.len() - 1)]
    }

    fn nth(&self, n: usize) -> SigToken {
        self.toks[(self.pos + n).min(self.toks.len() - 1)]
    }

    fn text(&self, t: SigToken) -> &'a str {
        &self.doc[t.span.range()]
    }

    fn at(&self, k: TokenKind) -> bool {
        self.peek().kind == k
    }

    fn at_kw(&self, kw: &str) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Ident && self.text(t) == kw
    }

    fn nth_is_kw(&self, n: usize, kw: &str) -> bool {
        let t = self.nth(n);
        t.kind == TokenKind::Ident && self.text(t) == kw
    }

    fn at_line_end(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Newline | TokenKind::Eof)
    }

    fn bump(&mut self) -> SigToken {
        let t = self.peek();
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        t
    }

    /// End of the last consumed token, not counting line terminators, so
    /// spans never swallow the newline that ends their last line.
    fn prev_end(&self) -> u32 {
        let mut i = self.pos;
        while i > 0 {
            let t = self.toks[i - 1];
            if t.kind != TokenKind::Newline {
                return t.span.end;
            }
            i -= 1;
        }
        self.peek().span.start
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        // one error per position keeps cascades readable
        if self.diags.last().is_some_and(|d| d.span.start == span.start) {
            return;
        }
        self.diags.push(Diagnostic::error(msg, span));
    }

    fn describe(&self, t: SigToken) -> String {
        match t.kind {
            TokenKind::Newline => "end of line".into(),
            TokenKind::Eof => "end of block".into(),
            _ => format!("`{}`", self.text(t)),
        }
    }

    fn expect(&mut self, k: TokenKind, what: &str) -> P<SigToken> {
        if self.at(k) {
            Some(self.bump())
        } else {
            let t = self.peek();
            self.error(format!("expected {what}, found {}", self.describe(t)), t.span);
            None
        }
    }

    fn eat(&mut self, k: TokenKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &str) -> P<SigToken> {
        if self.at_kw(kw) {
            Some(self.bump())
        } else {
            let t = self.peek();
            self.error(format!("expected `{kw}`, found {}", self.describe(t)), t.span);
            None
        }
    }

    fn name(&mut self, what: &str) -> P<Ident> {
        let t = self.peek();
        if t.kind == TokenKind::Ident && !is_hard_keyword(self.text(t)) {
            self.bump();
            Some(Ident { name: self.text(t).to_string(), span: t.span })
        } else {
            self.error(format!("expected {what}, found {}", self.describe(t)), t.span);
            None
        }
    }


    fn string(&mut self, what: &str) -> P<(String, Span)> {
        let t = self.peek();
        match t.kind {
            TokenKind::Str | TokenKind::RawStr | TokenKind::TripleStr => {
                self.bump();
                Some((string_value(self.text(t), t.kind), t.span))
            }
            _ => {
                self.error(format!("expected {what}, found {}", self.describe(t)), t.span);
                None
            }
        }
    }

    // ------------------------------------------------------------ line structure

    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.bump();
        }
    }

    /// Skip tokens until the start of a line indented at most `indent`.
    fn recover_to_indent(&mut self, indent: u32) {
        loop {
            let t = self.peek();
            if t.kind == TokenKind::Eof {
                return;
            }
            if t.line_start && t.line_indent <= indent && self.pos > 0 {
                // only stop at a line start we haven't consumed part of
                let prev = self.toks[self.pos - 1];
                if prev.kind == TokenKind::Newline {
                    return;
                }
            }
            self.bump();
        }
    }

    /// After a line entry: consume the end of line or complain about leftovers.
    fn finish_line(&mut self) {
        match self.peek().kind {
            TokenKind::Newline => {
                self.bump();
            }
            TokenKind::Eof => {}
            _ if self.peek().line_start => {} // a nested block already ended the line
            _ => {
                let t = self.peek();
                self.error(format!("unexpected {}", self.describe(t)), t.span);
                while !self.at_line_end() {
                    self.bump();
                }
                self.eat(TokenKind::Newline);
            }
        }
    }

    /// Parse the body after a `:`. Calls `entry` once per line of the
    /// indented block (or once for an inline body on the same line).
    /// Returns the span covered by the body.
    fn block(&mut self, header_indent: u32, mut entry: impl FnMut(&mut Self, u32) -> P<()>) -> Span {
        let start = self.peek().span.start;
        if !self.at_line_end() {
            // inline body: `case X: stmt`
            if entry(self, header_indent + 1).is_none() {
                while !self.at_line_end() {
                    self.bump();
                }
            }
            return Span::new(start, self.prev_end());
        }
        self.eat(TokenKind::Newline);
        let first = self.peek();
        if first.kind == TokenKind::Eof || first.line_indent <= header_indent {
            self.error("expected an indented block", first.span);
            return Span::new(start, start);
        }
        let body_start = first.span.start;
        let indent = first.line_indent;
        loop {
            let t = self.peek();
            if t.kind == TokenKind::Eof || t.line_indent <= header_indent {
                break;
            }
            if t.line_indent != indent {
                self.error("unexpected indentation", t.span);
                self.bump();
                self.recover_to_indent(indent);
                continue;
            }
            let before = self.pos;
            match entry(self, indent) {
                Some(()) => self.finish_line(),
                None => {
                    if self.pos == before {
                        self.bump();
                    }
                    self.recover_to_indent(indent);
                }
            }
        }
        Span::new(body_start, self.prev_end())
    }

    // ------------------------------------------------------------ items

    fn items(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            let t = self.peek();
            if t.kind == TokenKind::Eof {
                break;
            }
            if t.line_indent != 0 {
                self.error("unexpected indentation at top level", t.span);
                self.bump();
                self.recover_to_indent(0);
                continue;
            }
            let before = self.pos;
            match self.item() {
                Some(item) => {
                    items.push(item);
                    self.finish_line();
                }
                None => {
                    if self.pos == before {
                        self.bump();
                    }
                    self.recover_to_indent(0);
                    items.push(Item {
                        decorators: vec![],
                        kind: ItemKind::Error,
                        span: Span::new(t.span.start, self.prev_end()),
                    });
                }
            }
        }
        items
    }

    fn item(&mut self) -> P<Item> {
        let start = self.peek().span.start;
        let mut decorators = Vec::new();
        while self.at(TokenKind::At) {
            let at = self.bump();
            let name = self.name("decorator name")?;
            let args = if self.eat(TokenKind::LParen) { self.call_args()? } else { vec![] };
            decorators.push(Decorator { name, args, span: Span::new(at.span.start, self.prev_end()) });
            self.expect(TokenKind::Newline, "end of line after decorator")?;
        }
        let t = self.peek();
        let kw = if t.kind == TokenKind::Ident { self.text(t) } else { "" };
        let kind = match kw {
            "from" | "import" => ItemKind::Import(self.import()?),
            "depends" => ItemKind::Depends(self.depends()?),
            "alias" => ItemKind::Alias(self.alias()?),
            "type" => ItemKind::TypeDef(self.typedef()?),
            "struct" => ItemKind::Struct(self.struct_()?),
            "enum" => ItemKind::Enum(self.enum_()?),
            "capability" => ItemKind::Capability(self.capability()?),
            "adapter" => ItemKind::Adapter(self.adapter()?),
            "def" | "async" | "extern" => ItemKind::Def(self.def(0)?),
            "flow" => ItemKind::Flow(self.flow(0)?),
            "machine" => ItemKind::Machine(self.machine()?),
            "app" => ItemKind::App(self.app()?),
            "test" => ItemKind::Test(self.test()?),
            _ => {
                self.error(
                    format!(
                        "expected a definition (`def`, `flow`, `struct`, `enum`, `type`, …), found {}",
                        self.describe(t)
                    ),
                    t.span,
                );
                return None;
            }
        };
        Some(Item { decorators, kind, span: Span::new(start, self.prev_end()) })
    }

    fn dotted(&mut self) -> P<Vec<Ident>> {
        let mut path = vec![self.name("module name")?];
        while self.eat(TokenKind::Dot) {
            path.push(self.name("module name")?);
        }
        Some(path)
    }

    fn import(&mut self) -> P<Import> {
        if self.eat_kw("from") {
            let module = self.dotted()?;
            self.expect_kw("import")?;
            let mut names = vec![self.name("imported name")?];
            while self.eat(TokenKind::Comma) {
                names.push(self.name("imported name")?);
            }
            Some(Import::From { module, names })
        } else {
            self.expect_kw("import")?;
            let module = self.dotted()?;
            let alias = if self.eat_kw("as") { Some(self.name("alias")?) } else { None };
            Some(Import::Module { module, alias })
        }
    }

    fn depends(&mut self) -> P<Depends> {
        let start = self.expect_kw("depends")?.span.start;
        let crate_name = self.name("crate name")?;
        self.expect(TokenKind::Eq, "`=`")?;
        let (version, _) = self.string("version string")?;
        let mut features = Vec::new();
        if self.eat_kw("features") {
            self.expect(TokenKind::LBracket, "`[`")?;
            while !self.at(TokenKind::RBracket) {
                features.push(self.string("feature name")?.0);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket, "`]`")?;
        }
        Some(Depends { crate_name, version, features, span: Span::new(start, self.prev_end()) })
    }

    fn generics(&mut self) -> P<Vec<Generic>> {
        let mut out = Vec::new();
        if !self.eat(TokenKind::LBracket) {
            return Some(out);
        }
        loop {
            let name = self.name("type parameter")?;
            let mut bounds = Vec::new();
            if self.eat(TokenKind::Colon) {
                bounds.push(self.name("trait bound")?);
                while self.eat(TokenKind::Plus) {
                    bounds.push(self.name("trait bound")?);
                }
            }
            out.push(Generic { name, bounds });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBracket, "`]`")?;
        Some(out)
    }

    fn alias(&mut self) -> P<Alias> {
        self.expect_kw("alias")?;
        let name = self.name("type name")?;
        let generics = self.generics()?;
        self.expect(TokenKind::Eq, "`=`")?;
        let ty = self.ty()?;
        Some(Alias { name, generics, ty })
    }

    fn typedef(&mut self) -> P<TypeDef> {
        self.expect_kw("type")?;
        let name = self.name("type name")?;
        let generics = self.generics()?;
        self.expect(TokenKind::Eq, "`=`")?;
        let base = self.ty()?;
        let refinement = if self.eat_kw("where") { Some(self.expr()?) } else { None };
        Some(TypeDef { name, generics, base, refinement })
    }

    fn field(&mut self) -> P<Field> {
        let start = self.peek().span.start;
        let mut is_key = false;
        let mut is_sensitive = false;
        while self.nth(1).kind == TokenKind::Ident && (self.at_kw("key") || self.at_kw("sensitive")) {
            let modifier = self.bump();
            if self.text(modifier) == "key" {
                is_key = true;
            } else {
                is_sensitive = true;
            }
        }
        let name = self.name("field name")?;
        self.expect(TokenKind::Colon, "`:` and a type")?;
        let ty = self.ty()?;
        let default = if self.eat(TokenKind::Eq) { Some(self.expr()?) } else { None };
        Some(Field { is_key, is_sensitive, name, ty, default, span: Span::new(start, self.prev_end()) })
    }

    fn docstring(&mut self) -> Option<String> {
        if self.at(TokenKind::TripleStr) {
            let t = self.bump();
            Some(string_value(self.text(t), t.kind))
        } else {
            None
        }
    }

    fn struct_(&mut self) -> P<Struct> {
        self.expect_kw("struct")?;
        let name = self.name("struct name")?;
        let generics = self.generics()?;
        self.expect(TokenKind::Colon, "`:`")?;
        let mut doc = None;
        let mut fields = Vec::new();
        let mut invariants = Vec::new();
        let body_span = self.block(0, |p, _| {
            if let Some(d) = p.docstring() {
                doc = Some(d);
            } else if p.eat_kw("invariant") {
                invariants.push(p.expr()?);
            } else {
                fields.push(p.field()?);
            }
            Some(())
        });
        Some(Struct { name, generics, doc, fields, invariants, body_span })
    }

    fn enum_(&mut self) -> P<Enum> {
        self.expect_kw("enum")?;
        let name = self.name("enum name")?;
        let generics = self.generics()?;
        self.expect(TokenKind::Colon, "`:`")?;
        let mut variants = Vec::new();
        let body_span = self.block(0, |p, _| {
            let start = p.peek().span.start;
            let name = p.name("variant name")?;
            let mut items = Vec::new();
            if p.eat(TokenKind::LParen) {
                while !p.at(TokenKind::RParen) {
                    let is_from = p.eat_kw("from");
                    let field_name = if p.at(TokenKind::Ident) && p.nth(1).kind == TokenKind::Colon {
                        let n = p.name("field name")?;
                        p.bump();
                        Some(n)
                    } else {
                        None
                    };
                    let ty = p.ty()?;
                    items.push(VariantItem { is_from, name: field_name, ty });
                    if !p.eat(TokenKind::Comma) {
                        break;
                    }
                }
                p.expect(TokenKind::RParen, "`)`")?;
            }
            variants.push(Variant { name, items, span: Span::new(start, p.prev_end()) });
            Some(())
        });
        Some(Enum { name, generics, variants, body_span })
    }

    fn capability(&mut self) -> P<Capability> {
        self.expect_kw("capability")?;
        let name = self.name("capability name")?;
        let generics = self.generics()?;
        self.expect(TokenKind::Colon, "`:`")?;
        let mut methods = Vec::new();
        let mut doc = None;
        self.block(0, |p, indent| {
            if let Some(d) = p.docstring() {
                doc = Some(d);
            } else {
                methods.push(p.def(indent)?);
            }
            Some(())
        });
        Some(Capability { name, doc, generics, methods })
    }

    fn adapter(&mut self) -> P<Adapter> {
        self.expect_kw("adapter")?;
        let name = self.name("adapter name")?;
        self.expect_kw("implements")?;
        let mut implements = vec![self.ty()?];
        while self.eat(TokenKind::Comma) {
            implements.push(self.ty()?);
        }
        self.expect(TokenKind::Colon, "`:`")?;
        let mut fields = Vec::new();
        let mut clauses = Vec::new();
        let mut doc = None;
        self.block(0, |p, indent| {
            if let Some(d) = p.docstring() {
                doc = Some(d);
            } else if p.at(TokenKind::Ident) && p.nth(1).kind == TokenKind::Colon {
                fields.push(p.field()?);
            } else {
                clauses.push(p.clause(indent)?);
            }
            Some(())
        });
        Some(Adapter { name, doc, implements, fields, clauses })
    }

    fn param(&mut self) -> P<Param> {
        let start = self.peek().span.start;
        // `sensitive` is a modifier only when a name follows (a parameter may be called `sensitive`)
        let sensitive_kw = |p: &Self| p.at_kw("sensitive") && p.nth(1).kind == TokenKind::Ident;
        let mut is_sensitive = false;
        if sensitive_kw(self) {
            self.bump();
            is_sensitive = true;
        }
        let mode = if self.eat_kw("own") {
            ParamMode::Own
        } else if self.eat_kw("mut") {
            ParamMode::Mut
        } else {
            ParamMode::Borrow
        };
        if !is_sensitive && sensitive_kw(self) {
            self.bump();
            is_sensitive = true;
        }
        let name = self.name("parameter name")?;
        self.expect(TokenKind::Colon, "`:` and a type")?;
        let ty = self.ty()?;
        let default = if self.eat(TokenKind::Eq) { Some(self.expr()?) } else { None };
        Some(Param { mode, is_sensitive, name, ty, default, span: Span::new(start, self.prev_end()) })
    }

    fn params(&mut self) -> P<Vec<Param>> {
        self.expect(TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) {
            params.push(self.param()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "`)`")?;
        Some(params)
    }

    fn sig(&mut self) -> P<Signature> {
        let start = self.peek().span.start;
        let name = self.name("function name")?;
        let generics = self.generics()?;
        let params = self.params()?;
        let ret = if self.eat(TokenKind::Arrow) { Some(self.ty()?) } else { None };
        let raises = if self.eat_kw("raises") { Some(self.ty()?) } else { None };
        let mut uses = Vec::new();
        if self.eat_kw("uses") {
            uses.push(self.name("capability name")?);
            while self.eat(TokenKind::Comma) {
                uses.push(self.name("capability name")?);
            }
        }
        Some(Signature { name, generics, params, ret, raises, uses, span: Span::new(start, self.prev_end()) })
    }

    fn def(&mut self, indent: u32) -> P<Def> {
        let is_extern = self.eat_kw("extern");
        let is_async = self.eat_kw("async");
        self.expect_kw("def")?;
        let sig = self.sig()?;
        let extern_path = if is_extern {
            self.expect_kw("from")?;
            Some(self.string("Rust path string")?.0)
        } else {
            None
        };
        let mut doc = None;
        let mut clauses = Vec::new();
        let mut body_span = None;
        if self.eat(TokenKind::Colon) {
            let mut first = true;
            body_span = Some(self.block(indent, |p, ind| {
                if first {
                    first = false;
                    if let Some(d) = p.docstring() {
                        doc = Some(d);
                        return Some(());
                    }
                }
                clauses.push(p.clause(ind)?);
                Some(())
            }));
        }
        Some(Def { is_async, extern_path, sig, doc, clauses, body_span })
    }

    fn clause(&mut self, indent: u32) -> P<Clause> {
        let start = self.peek().span.start;
        let t = self.peek();
        let kw = if t.kind == TokenKind::Ident { self.text(t) } else { "" };
        let kind = match kw {
            "pure" => {
                self.bump();
                ClauseKind::Pure
            }
            "requires" => {
                self.bump();
                ClauseKind::Requires(self.expr()?)
            }
            "ensures" => {
                self.bump();
                ClauseKind::Ensures(self.expr()?)
            }
            "raises" => {
                self.bump();
                let error = self.ty()?;
                self.expect_kw("when")?;
                ClauseKind::RaisesWhen { error, when: self.expr()? }
            }
            "example" => {
                self.bump();
                let expr = self.expr()?;
                let raises = if self.eat_kw("raises") { Some(self.ty()?) } else { None };
                ClauseKind::Example { expr, raises }
            }
            "property" => {
                self.bump();
                let name = self.name("property name")?;
                let params = self.params()?;
                self.expect(TokenKind::Colon, "`:`")?;
                let mut given = Vec::new();
                let mut bodies: Vec<Expr> = Vec::new();
                self.block(indent, |p, _| {
                    if p.eat_kw("given") {
                        let n = p.name("parameter name")?;
                        p.expect_kw("from")?;
                        given.push((n, p.expr()?));
                    } else {
                        bodies.push(p.expr()?);
                    }
                    Some(())
                });
                let body = bodies.into_iter().reduce(|a, b| {
                    let span = a.span.to(b.span);
                    Expr { kind: ExprKind::Binary { op: BinOp::And, lhs: Box::new(a), rhs: Box::new(b) }, span }
                });
                let Some(body) = body else {
                    self.error("property needs a body expression", name.span);
                    return None;
                };
                ClauseKind::Property { name, params, given, body }
            }
            "bench" => {
                self.bump();
                let name = self.name("bench name")?;
                let params = self.params()?;
                self.expect(TokenKind::Colon, "`:`")?;
                ClauseKind::Bench { name, params, body: self.expr()? }
            }
            "mutate" => {
                self.bump();
                ClauseKind::Mutate(self.expr()?)
            }
            "complexity" => {
                self.bump();
                ClauseKind::Complexity(self.expr()?)
            }
            "pattern" => {
                self.bump();
                ClauseKind::Pattern(self.ty()?)
            }
            "note" => {
                self.bump();
                ClauseKind::Note(self.string("note string")?.0)
            }
            "optimize" => {
                self.bump();
                ClauseKind::Optimize(self.name("optimization target")?)
            }
            "budget" => {
                self.bump();
                let time = self.expr()?;
                self.expect_kw("for")?;
                let var = self.name("size variable")?;
                self.expect(TokenKind::Eq, "`=`")?;
                ClauseKind::Budget { time, var, size: self.expr()? }
            }
            "alloc" => {
                self.bump();
                if self.eat_kw("none") {
                    ClauseKind::Alloc(AllocSpec::None)
                } else {
                    let e = self.expr()?;
                    match e.kind {
                        ExprKind::Call { func, mut args }
                            if func.callee_name() == Some("bounded") && args.len() == 1 =>
                        {
                            ClauseKind::Alloc(AllocSpec::Bounded(args.remove(0).value))
                        }
                        _ => {
                            self.error("expected `none` or `bounded(n)`", e.span);
                            return None;
                        }
                    }
                }
            }
            "unsafe" => {
                self.bump();
                if self.eat_kw("allow") {
                    ClauseKind::Unsafe { allow: true }
                } else {
                    self.expect_kw("forbid")?;
                    ClauseKind::Unsafe { allow: false }
                }
            }
            "no_panic" => {
                self.bump();
                ClauseKind::NoPanic
            }
            "trace" => {
                self.bump();
                ClauseKind::Trace(self.name("trace level")?)
            }
            _ => {
                self.error(format!("unknown clause {}", self.describe(t)), t.span);
                return None;
            }
        };
        Some(Clause { kind, span: Span::new(start, self.prev_end()) })
    }

    // ------------------------------------------------------------ flows

    fn flow(&mut self, indent: u32) -> P<Flow> {
        self.expect_kw("flow")?;
        let sig = self.sig()?;
        self.expect(TokenKind::Colon, "`:`")?;
        let (doc, trace, body, body_span) = self.flow_body(indent);
        Some(Flow { sig, doc, trace, body, body_span })
    }

    fn flow_body(&mut self, indent: u32) -> (Option<String>, Option<Ident>, Vec<FlowStmt>, Span) {
        let mut doc = None;
        let mut trace = None;
        let mut body = Vec::new();
        let span = self.block(indent, |p, ind| {
            if body.is_empty() {
                if let Some(d) = p.docstring() {
                    doc = Some(d);
                    return Some(());
                }
                // `trace <level>` header line (before any statement)
                if p.at_kw("trace") && p.nth(1).kind == TokenKind::Ident && matches!(p.nth(2).kind, TokenKind::Newline | TokenKind::Eof) {
                    p.bump();
                    trace = Some(p.name("trace level")?);
                    return Some(());
                }
            }
            body.push(p.flow_stmt(ind)?);
            Some(())
        });
        (doc, trace, body, span)
    }

    fn flow_stmt(&mut self, indent: u32) -> P<FlowStmt> {
        let start = self.peek().span.start;
        let kind = if self.eat_kw("return") {
            if self.at_line_end() { FlowStmtKind::Return(None) } else { FlowStmtKind::Return(Some(self.flow_expr()?)) }
        } else if self.at_kw("match") && !matches!(self.nth(1).kind, TokenKind::Eq | TokenKind::Comma) {
            self.bump();
            let scrutinee = self.expr()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let mut arms = Vec::new();
            self.block(indent, |p, ind| {
                let arm_start = p.peek().span.start;
                p.expect_kw("case")?;
                let pattern = p.postfix()?;
                p.expect(TokenKind::Colon, "`:`")?;
                let mut arm_body = Vec::new();
                p.block(ind, |p, ind2| {
                    arm_body.push(p.flow_stmt(ind2)?);
                    Some(())
                });
                arms.push(MatchArm { pattern, body: arm_body, span: Span::new(arm_start, p.prev_end()) });
                Some(())
            });
            FlowStmtKind::Match { scrutinee, arms }
        } else if let Some(n) = self.binding_lookahead() {
            let mut targets = Vec::new();
            for i in 0..n {
                targets.push(self.name("binding name")?);
                if i + 1 < n {
                    self.expect(TokenKind::Comma, "`,`")?;
                }
            }
            self.expect(TokenKind::Eq, "`=`")?;
            FlowStmtKind::Bind { targets, value: self.flow_expr()? }
        } else {
            FlowStmtKind::Expr(self.flow_expr()?)
        };
        Some(FlowStmt { kind, span: Span::new(start, self.prev_end()) })
    }

    /// `a = ...` or `a, b = ...` → number of targets.
    fn binding_lookahead(&self) -> Option<usize> {
        let mut i = 0;
        let mut n = 0;
        loop {
            if self.nth(i).kind != TokenKind::Ident {
                return None;
            }
            n += 1;
            match self.nth(i + 1).kind {
                TokenKind::Eq => return Some(n),
                TokenKind::Comma => i += 2,
                _ => return None,
            }
        }
    }

    fn flow_expr(&mut self) -> P<FlowExpr> {
        let head = self.expr()?;
        let mut steps = Vec::new();
        while self.eat(TokenKind::PipeGt) {
            steps.push(self.expr()?);
        }
        let span = Span::new(head.span.start, self.prev_end());
        Some(FlowExpr { head, steps, span })
    }

    // ------------------------------------------------------------ machine, app, test

    fn machine(&mut self) -> P<Machine> {
        self.expect_kw("machine")?;
        let name = self.name("machine name")?;
        self.expect(TokenKind::LParen, "`(`")?;
        let param = self.param()?;
        self.expect(TokenKind::RParen, "`)`")?;
        let ret = if self.eat(TokenKind::Arrow) { Some(self.ty()?) } else { None };
        self.expect(TokenKind::Colon, "`:`")?;
        let mut transitions = Vec::new();
        let mut initial = None;
        let mut ignores = Vec::new();
        let mut doc = None;
        let mut trace = None;
        let body_span = self.block(0, |p, _| {
            if let Some(d) = p.docstring() {
                doc = Some(d);
                return Some(());
            }
            let clause_head = p.nth(1).line_start == false && p.nth(1).kind == TokenKind::Ident;
            if p.at_kw("initial") && clause_head {
                p.bump();
                initial = Some(p.postfix()?);
                return Some(());
            }
            if p.at_kw("trace") && clause_head && matches!(p.nth(2).kind, TokenKind::Newline | TokenKind::Eof) {
                p.bump();
                trace = Some(p.name("trace level")?);
                return Some(());
            }
            let start = p.peek().span.start;
            if p.at_kw("ignore") && clause_head {
                p.bump();
                let state = p.postfix()?;
                p.expect_kw("on")?;
                let event = p.postfix()?;
                ignores.push(IgnoreRule { state, event, span: Span::new(start, p.prev_end()) });
                return Some(());
            }
            let from = p.postfix()?;
            p.expect_kw("on")?;
            let event = p.postfix()?;
            let guard = if p.eat_kw("if") { Some(p.or_expr()?) } else { None };
            p.expect(TokenKind::Arrow, "`->`")?;
            let to = p.postfix()?;
            let emit = if p.eat_kw("emit") { Some(p.expr()?) } else { None };
            transitions.push(Transition { from, event, guard, to, emit, span: Span::new(start, p.prev_end()) });
            Some(())
        });
        Some(Machine { name, param, ret, transitions, initial, ignores, doc, trace, body_span })
    }

    fn provide(&mut self) -> P<Provide> {
        let start = self.peek().span.start;
        let capability = self.name("capability name")?;
        self.expect(TokenKind::Eq, "`=`")?;
        let value = self.expr()?;
        Some(Provide { capability, value, span: Span::new(start, self.prev_end()) })
    }

    fn app(&mut self) -> P<App> {
        self.expect_kw("app")?;
        let name = self.name("app name")?;
        self.expect(TokenKind::Colon, "`:`")?;
        let mut targets = Vec::new();
        let mut log = Vec::new();
        let mut depends = Vec::new();
        let mut provides = Vec::new();
        let mut entry = None;
        let mut routes = Vec::new();
        let mut statuses = Vec::new();
        let mut listen = None;
        let mut exports = Vec::new();
        self.block(0, |p, indent| {
            let start = p.peek().span.start;
            if p.eat_kw("route") {
                let method = p.name("HTTP method, e.g. `POST`")?;
                let (path, path_span) = p.string("route path string, e.g. \"/signup\"")?;
                p.expect(TokenKind::Arrow, "`->`")?;
                let target = p.postfix()?;
                routes.push(Route { method, path, path_span, target, span: Span::new(start, p.prev_end()) });
            } else if p.eat_kw("status") {
                let error = p.ty()?;
                p.expect(TokenKind::Eq, "`=`")?;
                let code = p.expr()?;
                statuses.push(StatusRule { error, code, span: Span::new(start, p.prev_end()) });
            } else if p.eat_kw("listen") {
                listen = Some(p.expr()?);
            } else if p.eat_kw("export") {
                exports.push(p.postfix()?);
                while p.eat(TokenKind::Comma) {
                    exports.push(p.postfix()?);
                }
            } else if p.eat_kw("target") {
                targets.push(p.expr()?);
            } else if p.eat_kw("log") {
                while !p.at_line_end() {
                    let key = p.name("log setting")?;
                    p.expect(TokenKind::Eq, "`=`")?;
                    log.push((key, p.expr()?));
                }
            } else if p.at_kw("depends") {
                depends.push(p.depends()?);
            } else if p.eat_kw("provide") {
                provides.push(p.provide()?);
            } else if p.eat_kw("entry") {
                let sig = p.sig()?;
                p.expect(TokenKind::Colon, "`:`")?;
                let (doc, trace, body, body_span) = p.flow_body(indent);
                entry = Some(Flow { sig, doc, trace, body, body_span });
            } else {
                let t = p.peek();
                p.error(
                    format!("expected `target`, `log`, `depends`, `provide`, `entry`, `route`, `status`, `listen` or `export`, found {}", p.describe(t)),
                    t.span,
                );
                return None;
            }
            Some(())
        });
        Some(App { name, targets, log, depends, provides, entry, routes, statuses, listen, exports })
    }

    fn test(&mut self) -> P<Test> {
        self.expect_kw("test")?;
        let (name, name_span) = self.string("test name string")?;
        self.expect(TokenKind::Colon, "`:`")?;
        let mut givens = Vec::new();
        let mut expects = Vec::new();
        self.block(0, |p, _| {
            if p.eat_kw("given") {
                givens.push(p.provide()?);
            } else {
                let start = p.expect_kw("expect")?.span.start;
                let expr = p.expr()?;
                let raises = if p.eat_kw("raises") { Some(p.ty()?) } else { None };
                expects.push(Expect { expr, raises, span: Span::new(start, p.prev_end()) });
            }
            Some(())
        });
        Some(Test { name, name_span, givens, expects })
    }

    // ------------------------------------------------------------ types

    fn ty(&mut self) -> P<TypeExpr> {
        let base = self.ty_atom()?;
        if self.at(TokenKind::Pipe) {
            self.bump();
            let none = self.expect_kw("None")?;
            let span = base.span.to(none.span);
            return Some(TypeExpr { kind: TypeExprKind::Optional(Box::new(base)), span });
        }
        Some(base)
    }

    fn ty_atom(&mut self) -> P<TypeExpr> {
        let start = self.peek().span.start;
        if self.eat(TokenKind::LParen) {
            let mut params = Vec::new();
            while !self.at(TokenKind::RParen) {
                params.push(self.ty()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "`)`")?;
            self.expect(TokenKind::Arrow, "`->` in function type")?;
            let ret = self.ty()?;
            let span = Span::new(start, self.prev_end());
            return Some(TypeExpr { kind: TypeExprKind::Fn { params, ret: Box::new(ret) }, span });
        }
        let mut path = vec![self.name("type")?];
        while self.at(TokenKind::Dot) && self.nth(1).kind == TokenKind::Ident {
            self.bump();
            path.push(self.name("type")?);
        }
        let mut args = Vec::new();
        if self.eat(TokenKind::LBracket) {
            while !self.at(TokenKind::RBracket) {
                if matches!(self.peek().kind, TokenKind::Int) {
                    args.push(TypeArg::Const(self.atom()?));
                } else {
                    args.push(TypeArg::Type(self.ty()?));
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket, "`]`")?;
        }
        Some(TypeExpr { kind: TypeExprKind::Named { path, args }, span: Span::new(start, self.prev_end()) })
    }

    // ------------------------------------------------------------ expressions

    pub fn expr(&mut self) -> P<Expr> {
        let start = self.peek().span.start;
        if self.eat_kw("lambda") {
            let mut params = Vec::new();
            while !self.at(TokenKind::Colon) {
                params.push(self.name("lambda parameter")?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Colon, "`:`")?;
            let body = self.expr()?;
            return Some(Expr {
                kind: ExprKind::Lambda { params, body: Box::new(body) },
                span: Span::new(start, self.prev_end()),
            });
        }
        if self.at_kw("forall") || self.at_kw("exists") {
            let q_tok = self.bump();
            let q = if self.text(q_tok) == "forall" { Quantifier::Forall } else { Quantifier::Exists };
            let var = self.name("bound variable")?;
            self.expect_kw("in")?;
            let iter = self.or_expr()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let body = self.expr()?;
            return Some(Expr {
                kind: ExprKind::Quantified { q, var, iter: Box::new(iter), body: Box::new(body) },
                span: Span::new(start, self.prev_end()),
            });
        }
        let then = self.or_expr()?;
        if self.at_kw("if") {
            self.bump();
            let cond = self.or_expr()?;
            self.expect_kw("else")?;
            let otherwise = self.expr()?;
            return Some(Expr {
                kind: ExprKind::IfElse { then: Box::new(then), cond: Box::new(cond), otherwise: Box::new(otherwise) },
                span: Span::new(start, self.prev_end()),
            });
        }
        Some(then)
    }

    fn binary(&mut self, lhs: Expr, op: BinOp, rhs: Expr) -> Expr {
        let span = lhs.span.to(rhs.span);
        Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span }
    }

    fn or_expr(&mut self) -> P<Expr> {
        let mut lhs = self.and_expr()?;
        while self.eat_kw("or") {
            let rhs = self.and_expr()?;
            lhs = self.binary(lhs, BinOp::Or, rhs);
        }
        Some(lhs)
    }

    fn and_expr(&mut self) -> P<Expr> {
        let mut lhs = self.not_expr()?;
        while self.eat_kw("and") {
            let rhs = self.not_expr()?;
            lhs = self.binary(lhs, BinOp::And, rhs);
        }
        Some(lhs)
    }

    fn not_expr(&mut self) -> P<Expr> {
        let start = self.peek().span.start;
        if self.eat_kw("not") {
            let e = self.not_expr()?;
            return Some(Expr {
                kind: ExprKind::Unary { op: UnaryOp::Not, expr: Box::new(e) },
                span: Span::new(start, self.prev_end()),
            });
        }
        self.comparison()
    }

    fn cmp_op(&mut self) -> Option<CmpOp> {
        let t = self.peek();
        let op = match t.kind {
            TokenKind::EqEq => CmpOp::Eq,
            TokenKind::NotEq => CmpOp::NotEq,
            TokenKind::Lt => CmpOp::Lt,
            TokenKind::Le => CmpOp::Le,
            TokenKind::Gt => CmpOp::Gt,
            TokenKind::Ge => CmpOp::Ge,
            TokenKind::Ident => match self.text(t) {
                "in" => CmpOp::In,
                "not" if self.nth_is_kw(1, "in") => {
                    self.bump();
                    CmpOp::NotIn
                }
                "is" if self.nth_is_kw(1, "not") => {
                    self.bump();
                    CmpOp::IsNot
                }
                "is" => CmpOp::Is,
                _ => return None,
            },
            _ => return None,
        };
        self.bump();
        Some(op)
    }

    fn comparison(&mut self) -> P<Expr> {
        let first = self.arith()?;
        let mut rest = Vec::new();
        while let Some(op) = self.cmp_op() {
            rest.push((op, self.arith()?));
        }
        if rest.is_empty() {
            return Some(first);
        }
        let span = first.span.to(rest.last().unwrap().1.span);
        Some(Expr { kind: ExprKind::Compare { first: Box::new(first), rest }, span })
    }

    fn arith(&mut self) -> P<Expr> {
        let mut lhs = self.term()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.term()?;
            lhs = self.binary(lhs, op, rhs);
        }
        Some(lhs)
    }

    fn term(&mut self) -> P<Expr> {
        let mut lhs = self.factor()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::SlashSlash => BinOp::FloorDiv,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.bump();
            let rhs = self.factor()?;
            lhs = self.binary(lhs, op, rhs);
        }
        Some(lhs)
    }

    fn factor(&mut self) -> P<Expr> {
        let start = self.peek().span.start;
        if self.eat(TokenKind::Minus) {
            let e = self.factor()?;
            return Some(Expr {
                kind: ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(e) },
                span: Span::new(start, self.prev_end()),
            });
        }
        let base = self.postfix()?;
        if self.eat(TokenKind::StarStar) {
            let exp = self.factor()?;
            return Some(self.binary(base, BinOp::Pow, exp));
        }
        Some(base)
    }

    fn postfix(&mut self) -> P<Expr> {
        let mut e = self.atom()?;
        loop {
            let start = e.span.start;
            if self.at(TokenKind::Dot) {
                self.bump();
                let attr = self.name("attribute name")?;
                e = Expr { kind: ExprKind::Attr { base: Box::new(e), attr }, span: Span::new(start, self.prev_end()) };
            } else if self.at(TokenKind::LParen) {
                self.bump();
                let args = self.call_args()?;
                e = Expr { kind: ExprKind::Call { func: Box::new(e), args }, span: Span::new(start, self.prev_end()) };
            } else if self.at(TokenKind::LBracket) {
                self.bump();
                let lo = if self.at(TokenKind::Colon) { None } else { Some(self.expr()?) };
                if self.eat(TokenKind::Colon) {
                    let hi = if self.at(TokenKind::RBracket) { None } else { Some(Box::new(self.expr()?)) };
                    self.expect(TokenKind::RBracket, "`]`")?;
                    e = Expr {
                        kind: ExprKind::Slice { base: Box::new(e), lo: lo.map(Box::new), hi },
                        span: Span::new(start, self.prev_end()),
                    };
                } else {
                    self.expect(TokenKind::RBracket, "`]`")?;
                    e = Expr {
                        kind: ExprKind::Index { base: Box::new(e), index: Box::new(lo?) },
                        span: Span::new(start, self.prev_end()),
                    };
                }
            } else {
                break;
            }
        }
        Some(e)
    }

    /// Arguments after `(`; consumes the closing `)`.
    fn call_args(&mut self) -> P<Vec<Arg>> {
        let mut args = Vec::new();
        while !self.at(TokenKind::RParen) {
            let name = if self.at(TokenKind::Ident) && self.nth(1).kind == TokenKind::Eq {
                let n = self.name("argument name")?;
                self.bump();
                Some(n)
            } else {
                None
            };
            let mut value = self.expr()?;
            if name.is_none() && self.at_kw("for") {
                value = self.comprehension_tail(value, true)?;
            }
            args.push(Arg { name, value });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "`)`")?;
        Some(args)
    }

    fn comprehension_tail(&mut self, elt: Expr, generator: bool) -> P<Expr> {
        self.expect_kw("for")?;
        let var = self.name("loop variable")?;
        self.expect_kw("in")?;
        let iter = self.or_expr()?;
        let cond = if self.eat_kw("if") { Some(Box::new(self.or_expr()?)) } else { None };
        let span = Span::new(elt.span.start, self.prev_end());
        Some(Expr {
            kind: ExprKind::Comprehension { elt: Box::new(elt), var, iter: Box::new(iter), cond, generator },
            span,
        })
    }

    fn atom(&mut self) -> P<Expr> {
        let t = self.peek();
        let text = self.text(t);
        let kind = match t.kind {
            TokenKind::Ident => match text {
                "True" => ExprKind::Bool(true),
                "False" => ExprKind::Bool(false),
                "None" => ExprKind::None,
                // `result`, `old`, `self` are soft keywords and parse as names
                _ if is_hard_keyword(text) => {
                    self.error(format!("expected an expression, found keyword `{text}`"), t.span);
                    return None;
                }
                _ => ExprKind::Name(text.to_string()),
            },
            TokenKind::Int => ExprKind::Int(text.replace('_', "")),
            TokenKind::Float => ExprKind::Float(text.replace('_', "")),
            TokenKind::Duration => {
                let split = text.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(text.len());
                ExprKind::Duration { value: text[..split].replace('_', ""), unit: text[split..].to_string() }
            }
            TokenKind::Str | TokenKind::TripleStr => {
                ExprKind::Str { value: string_value(text, t.kind), kind: StrKind::Normal }
            }
            TokenKind::RawStr => ExprKind::Str { value: string_value(text, t.kind), kind: StrKind::Raw },
            TokenKind::Bytes => ExprKind::Str { value: string_value(text, t.kind), kind: StrKind::Bytes },
            TokenKind::LParen => {
                self.bump();
                let start = t.span.start;
                if self.eat(TokenKind::RParen) {
                    return Some(Expr { kind: ExprKind::Tuple(vec![]), span: Span::new(start, self.prev_end()) });
                }
                let first = self.expr()?;
                if self.at_kw("for") {
                    let c = self.comprehension_tail(first, true)?;
                    self.expect(TokenKind::RParen, "`)`")?;
                    return Some(Expr { span: Span::new(start, self.prev_end()), ..c });
                }
                if self.eat(TokenKind::Comma) {
                    let mut items = vec![first];
                    while !self.at(TokenKind::RParen) {
                        items.push(self.expr()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RParen, "`)`")?;
                    return Some(Expr { kind: ExprKind::Tuple(items), span: Span::new(start, self.prev_end()) });
                }
                self.expect(TokenKind::RParen, "`)`")?;
                // keep the parenthesized span so edits can replace the whole thing
                return Some(Expr { span: Span::new(start, self.prev_end()), ..first });
            }
            TokenKind::LBracket => {
                self.bump();
                let start = t.span.start;
                let mut items = Vec::new();
                if !self.at(TokenKind::RBracket) {
                    let first = self.expr()?;
                    if self.at_kw("for") {
                        let c = self.comprehension_tail(first, false)?;
                        self.expect(TokenKind::RBracket, "`]`")?;
                        return Some(Expr { span: Span::new(start, self.prev_end()), ..c });
                    }
                    items.push(first);
                    while self.eat(TokenKind::Comma) {
                        if self.at(TokenKind::RBracket) {
                            break;
                        }
                        items.push(self.expr()?);
                    }
                }
                self.expect(TokenKind::RBracket, "`]`")?;
                return Some(Expr { kind: ExprKind::List(items), span: Span::new(start, self.prev_end()) });
            }
            _ => {
                self.error(format!("expected an expression, found {}", self.describe(t)), t.span);
                return None;
            }
        };
        self.bump();
        Some(Expr { kind, span: t.span })
    }
}

/// Decode a string literal token into its value.
pub fn string_value(text: &str, kind: TokenKind) -> String {
    match kind {
        TokenKind::TripleStr => {
            let inner = text.strip_prefix("\"\"\"").and_then(|s| s.strip_suffix("\"\"\"")).unwrap_or(text);
            dedent(inner)
        }
        TokenKind::RawStr => text.get(2..text.len().saturating_sub(1)).unwrap_or("").to_string(),
        TokenKind::Bytes => unescape(text.get(2..text.len().saturating_sub(1)).unwrap_or("")),
        _ => unescape(text.get(1..text.len().saturating_sub(1)).unwrap_or("")),
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let min = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<String> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if i == 0 {
            out.push(l.trim().to_string());
        } else {
            out.push(l.get(min..).unwrap_or("").trim_end().to_string());
        }
    }
    out.join("\n").trim().to_string()
}
