use std::ops::Range;

use super::ast::{
    Arg, ClauseSpan, Form, Formula, Ident, Ns, Program, Reading, Refer, Require, Sense,
};
use super::template::hole_names;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    LParen,
    RParen,
    LBrack,
    RBrack,
    Symbol(String),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    /// Byte offset of the token in the source.
    start: usize,
    end: usize,
    line: usize,
    col: usize,
}

struct Lexer<'a> {
    src: &'a [u8],
    i: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src: src.as_bytes(),
            i: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n' | b',')) {
                self.bump();
            }
            if self.peek() == Some(b';') && self.src.get(self.i + 1) == Some(&b';') {
                while let Some(c) = self.peek() {
                    self.bump();
                    if c == b'\n' {
                        break;
                    }
                }
                continue;
            }
            break;
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, ParseError> {
        self.skip_ws_and_comments();
        let line = self.line;
        let col = self.col;
        let start = self.i;
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        let kind = match c {
            b'(' => {
                self.bump();
                TokenKind::LParen
            }
            b')' => {
                self.bump();
                TokenKind::RParen
            }
            b'[' => {
                self.bump();
                TokenKind::LBrack
            }
            b']' => {
                self.bump();
                TokenKind::RBrack
            }
            b'"' => TokenKind::String(self.lex_string(line, col)?),
            _ => TokenKind::Symbol(self.lex_symbol(line, col)?),
        };
        Ok(Some(Token {
            kind,
            start,
            end: self.i,
            line,
            col,
        }))
    }

    fn lex_string(&mut self, line: usize, col: usize) -> Result<String, ParseError> {
        self.bump();
        let mut out = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(ParseError {
                        line,
                        col,
                        message: "unterminated string".into(),
                    });
                }
                Some(b'"') => return Ok(out),
                Some(b'\\') => match self.bump() {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(b'n') => out.push('\n'),
                    Some(c) => out.push(c as char),
                    None => {
                        return Err(ParseError {
                            line,
                            col,
                            message: "unterminated string escape".into(),
                        });
                    }
                },
                Some(c) => out.push(c as char),
            }
        }
    }

    fn lex_symbol(&mut self, line: usize, col: usize) -> Result<String, ParseError> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if matches!(
                c,
                b'(' | b')' | b'[' | b']' | b'"' | b' ' | b'\t' | b'\r' | b'\n' | b','
            ) {
                break;
            }
            if c == b';' && self.src.get(self.i + 1) == Some(&b';') {
                break;
            }
            out.push(self.bump().unwrap() as char);
        }
        if out.is_empty() {
            return Err(ParseError {
                line,
                col,
                message: format!("unexpected character {:?}", self.peek().map(|c| c as char)),
            });
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sexp {
    span: Range<usize>,
    line: usize,
    col: usize,
    kind: SexpKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SexpKind {
    List(Vec<Sexp>),
    Vector(Vec<Sexp>),
    Symbol(String),
    String(String),
}

struct Parser {
    tokens: Vec<Token>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.i)
    }

    fn bump(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.i).cloned()?;
        self.i += 1;
        Some(t)
    }

    fn sexp(&mut self) -> Result<Sexp, ParseError> {
        let tok = self
            .bump()
            .ok_or_else(|| err(1, 1, "unexpected end of input"))?;
        let kind = match tok.kind {
            TokenKind::LParen => {
                let mut xs = Vec::new();
                loop {
                    match self.peek() {
                        None => return Err(err(tok.line, tok.col, "unclosed '('")),
                        Some(t) if matches!(t.kind, TokenKind::RParen) => {
                            self.bump();
                            break;
                        }
                        Some(_) => xs.push(self.sexp()?),
                    }
                }
                SexpKind::List(xs)
            }
            TokenKind::RParen => return Err(err(tok.line, tok.col, "unexpected ')'")),
            TokenKind::LBrack => {
                let mut xs = Vec::new();
                loop {
                    match self.peek() {
                        None => return Err(err(tok.line, tok.col, "unclosed '['")),
                        Some(t) if matches!(t.kind, TokenKind::RBrack) => {
                            self.bump();
                            break;
                        }
                        Some(_) => xs.push(self.sexp()?),
                    }
                }
                SexpKind::Vector(xs)
            }
            TokenKind::RBrack => return Err(err(tok.line, tok.col, "unexpected ']'")),
            TokenKind::Symbol(s) => SexpKind::Symbol(s),
            TokenKind::String(s) => SexpKind::String(s),
        };
        let end = self.tokens[self.i - 1].end;
        Ok(Sexp {
            span: tok.start..end,
            line: tok.line,
            col: tok.col,
            kind,
        })
    }
}

fn tokenize(src: &str) -> Result<Vec<Token>, ParseError> {
    let mut lexer = Lexer::new(src);
    let mut tokens = Vec::new();
    while let Some(t) = lexer.next_token()? {
        tokens.push(t);
    }
    Ok(tokens)
}

/// Parse a model: an optional `ns`, then `name` and `sense` declarations
/// and claims built with `and`, `or`, `not`, and relation application.
pub fn parse(src: &str) -> Result<Program, ParseError> {
    let tokens = tokenize(src)?;
    if tokens.is_empty() {
        return Ok(Program {
            namespace: None,
            forms: Vec::new(),
            spans: Vec::new(),
            clauses: Vec::new(),
        });
    }
    let mut p = Parser { tokens, i: 0 };
    let mut namespace = None;
    let mut forms = Vec::new();
    let mut spans = Vec::new();
    let mut clauses = Vec::new();
    while p.peek().is_some() {
        let start = p.peek().map(|token| token.start).unwrap_or(0);
        let s = p.sexp()?;
        let end = p.tokens[p.i - 1].end;
        if is_ns(&s) {
            if namespace.is_some() || !forms.is_empty() {
                return Err(err_at(&s, "`ns` must be the first form"));
            }
            namespace = Some(lower_ns(&s)?);
            continue;
        }
        let (form, clause) = lower_form(&s)?;
        forms.push(form);
        spans.push(start..end);
        if let Some(clause) = clause {
            clauses.push(clause);
        }
    }
    Ok(Program {
        namespace,
        forms,
        spans,
        clauses,
    })
}

/// Whether a top-level form is `(ns ...)`, including one that fails to parse.
///
/// A scanned file with no namespace is not part of the index. A file that
/// tries to declare one and does not parse is an error instead.
pub fn mentions_namespace(src: &str) -> bool {
    match tokenize(src) {
        Ok(tokens) => top_level_ns(&tokens),
        Err(_) => raw_mentions_ns(src),
    }
}

fn top_level_ns(tokens: &[Token]) -> bool {
    let mut depth = 0i32;
    let mut expect_head = false;
    for token in tokens {
        match &token.kind {
            TokenKind::LParen => {
                if depth == 0 {
                    expect_head = true;
                }
                depth += 1;
            }
            TokenKind::RParen => {
                depth -= 1;
                expect_head = false;
            }
            TokenKind::Symbol(name) if expect_head && name == "ns" => return true,
            _ => expect_head = false,
        }
    }
    false
}

fn raw_mentions_ns(src: &str) -> bool {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b';' if bytes.get(i + 1) == Some(&b';') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'(' => {
                i += 1;
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b',') {
                    i += 1;
                }
                if bytes[i..].starts_with(b"ns")
                    && bytes
                        .get(i + 2)
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_' && *c != b'-')
                {
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    false
}

fn is_ns(s: &Sexp) -> bool {
    let SexpKind::List(xs) = &s.kind else {
        return false;
    };
    matches!(xs.first().map(|item| &item.kind), Some(SexpKind::Symbol(head)) if head == "ns")
}

fn lower_ns(s: &Sexp) -> Result<Ns, ParseError> {
    let xs = list(s)?;
    if xs.len() < 2 {
        return Err(err_at(s, "`ns` takes a name"));
    }
    let name = declared_symbol(&xs[1], "namespace")?;
    validate_namespace(&name).map_err(|message| err_at(&xs[1], message))?;
    let mut requires = Vec::new();
    for clause in &xs[2..] {
        requires.extend(lower_require_clause(clause)?);
    }
    Ok(Ns { name, requires })
}

fn lower_require_clause(s: &Sexp) -> Result<Vec<Require>, ParseError> {
    let xs = list(s)?;
    if xs.is_empty() {
        return Err(err_at(s, "`ns` accepts `(:require ...)` clauses"));
    }
    let head = expect_symbol(&xs[0], "`ns` accepts `(:require ...)` clauses")?;
    if head != ":require" {
        return Err(err_at(&xs[0], "`ns` accepts `(:require ...)` clauses"));
    }
    if xs.len() < 2 {
        return Err(err_at(s, "`:require` takes a libspec"));
    }
    xs[1..].iter().map(lower_libspec).collect()
}

fn lower_libspec(s: &Sexp) -> Result<Require, ParseError> {
    let xs = vector(s)?;
    if xs.is_empty() {
        return Err(err_at(
            s,
            "a libspec starts with a namespace, e.g. [com.acme.product :as product]",
        ));
    }
    let namespace = declared_symbol(&xs[0], "namespace")?;
    validate_namespace(&namespace).map_err(|message| err_at(&xs[0], message))?;
    let mut alias = None;
    let mut refer = None;
    let mut i = 1;
    while i < xs.len() {
        let key = expect_symbol(&xs[i], "a libspec uses `:as` and `:refer`")?;
        match key.as_str() {
            ":as" => {
                if alias.is_some() {
                    return Err(err_at(&xs[i], "`:as` is given once"));
                }
                let Some(spec) = xs.get(i + 1) else {
                    return Err(err_at(&xs[i], "`:as` takes an alias"));
                };
                let name = declared_symbol(spec, "alias")?;
                if name.starts_with(':') {
                    return Err(err_at(spec, "an alias is a name"));
                }
                alias = Some(name);
                i += 2;
            }
            ":refer" => {
                if refer.is_some() {
                    return Err(err_at(&xs[i], "`:refer` is given once"));
                }
                let Some(spec) = xs.get(i + 1) else {
                    return Err(err_at(&xs[i], "`:refer` takes `:all` or a vector of names"));
                };
                refer = Some(lower_refer(spec)?);
                i += 2;
            }
            _ => return Err(err_at(&xs[i], "a libspec uses `:as` and `:refer`")),
        }
    }
    Ok(Require {
        namespace,
        alias,
        refer: refer.unwrap_or(Refer::Nothing),
    })
}

fn lower_refer(s: &Sexp) -> Result<Refer, ParseError> {
    match &s.kind {
        SexpKind::Symbol(sym) if sym == ":all" => Ok(Refer::All),
        SexpKind::Vector(xs) => {
            let mut names = Vec::new();
            for item in xs {
                let name = declared_symbol(item, "referred name")?;
                if name.starts_with(':') {
                    return Err(err_at(item, "a referred name is a name or a sense"));
                }
                if names.iter().any(|seen| seen == &name) {
                    return Err(err_at(item, format!("`{name}` is referred twice")));
                }
                names.push(name);
            }
            Ok(Refer::Only(names))
        }
        _ => Err(err_at(s, "`:refer` takes `:all` or a vector of names")),
    }
}

fn validate_namespace(name: &str) -> Result<(), String> {
    if name.contains('/') || name.split('.').any(|segment| segment.is_empty()) {
        Err("a namespace is dot-separated segments, for example `com.acme.product`".into())
    } else {
        Ok(())
    }
}

fn lower_form(s: &Sexp) -> Result<(Form, Option<ClauseSpan>), ParseError> {
    let xs = list(s)?;
    match xs.first().map(|item| &item.kind) {
        Some(SexpKind::Symbol(head)) if head == "name" => {
            Ok((Form::Name(lower_name_decl(xs)?), None))
        }
        Some(SexpKind::Symbol(head)) if head == "sense" => {
            Ok((Form::Sense(lower_sense(xs)?), None))
        }
        _ => {
            let (formula, clause) = lower_formula(s)?;
            Ok((Form::Claim(formula), Some(clause)))
        }
    }
}

fn lower_name_decl(xs: &[Sexp]) -> Result<String, ParseError> {
    if xs.len() != 2 {
        return Err(err_at(&xs[0], "`name` takes one name"));
    }
    declared_symbol(&xs[1], "name")
}

fn lower_sense(xs: &[Sexp]) -> Result<Sense, ParseError> {
    if xs.len() != 3 {
        return Err(err(
            1,
            1,
            "`sense` takes a name and a reading, e.g. (sense calls \"{caller} calls {callee}\")",
        ));
    }
    let name = declared_symbol(&xs[1], "sense name")?;
    let template = match &xs[2].kind {
        SexpKind::String(template) => template.clone(),
        _ => {
            return Err(err(
                1,
                1,
                "`sense` reading must be a string, e.g. \"{borrower} borrows from {lender}\"",
            ))
        }
    };
    let holes = hole_names(&template).map_err(|message| err(1, 1, message))?;
    Ok(Sense {
        name,
        template,
        holes,
    })
}

fn lower_formula(s: &Sexp) -> Result<(Formula, ClauseSpan), ParseError> {
    let xs = list(s)?;
    if xs.is_empty() {
        return Err(err(1, 1, "empty list is not a formula"));
    }
    let clause = |children: Vec<ClauseSpan>| ClauseSpan {
        span: s.span.clone(),
        children,
    };
    match &xs[0].kind {
        SexpKind::Symbol(head) if head == "name" || head == "sense" || head == "ns" => Err(err_at(
            &xs[0],
            format!("`{head}` is a declaration and cannot appear inside a formula"),
        )),
        SexpKind::Symbol(head) if head == "and" => {
            let (forms, children) = lower_formulas(&xs[1..])?;
            Ok((Formula::And(forms), clause(children)))
        }
        SexpKind::Symbol(head) if head == "or" => {
            let (forms, children) = lower_formulas(&xs[1..])?;
            Ok((Formula::Or(forms), clause(children)))
        }
        SexpKind::Symbol(head) if head == "not" => {
            if xs.len() != 2 {
                return Err(err(1, 1, "`not` takes one formula"));
            }
            let (inner, child) = lower_formula(&xs[1])?;
            Ok((Formula::Not(Box::new(inner)), clause(vec![child])))
        }
        SexpKind::Symbol(head) if is_retired(head) => Err(retired(head)),
        SexpKind::Symbol(head) => Ok((
            Formula::Apply {
                reading: Reading::Sense(use_symbol(&xs[0], head)?),
                args: lower_args(&xs[1..])?,
            },
            clause(Vec::new()),
        )),
        SexpKind::String(template) => {
            let holes = hole_names(template).map_err(|message| err(1, 1, message))?;
            Ok((
                Formula::Apply {
                    reading: Reading::Inline {
                        template: template.clone(),
                        holes,
                    },
                    args: lower_args(&xs[1..])?,
                },
                clause(Vec::new()),
            ))
        }
        SexpKind::List(_) | SexpKind::Vector(_) => Err(err_at(
            &xs[0],
            "a formula starts with `and`, `or`, `not`, a sense name, or a reading",
        )),
    }
}

fn lower_formulas(xs: &[Sexp]) -> Result<(Vec<Formula>, Vec<ClauseSpan>), ParseError> {
    let mut forms = Vec::with_capacity(xs.len());
    let mut spans = Vec::with_capacity(xs.len());
    for sexp in xs {
        let (form, span) = lower_formula(sexp)?;
        forms.push(form);
        spans.push(span);
    }
    Ok((forms, spans))
}

fn lower_args(xs: &[Sexp]) -> Result<Vec<Arg>, ParseError> {
    xs.iter().map(lower_arg).collect()
}

fn lower_arg(s: &Sexp) -> Result<Arg, ParseError> {
    match &s.kind {
        SexpKind::Symbol(spelling) => Ok(Arg::Name(use_symbol(s, spelling)?)),
        SexpKind::String(text) => Ok(Arg::Literal(text.clone())),
        _ => Err(err_at(s, "an argument must be a name or a string")),
    }
}

fn declared_symbol(s: &Sexp, what: &str) -> Result<String, ParseError> {
    let spelling = expect_symbol(s, &format!("a {what} must be a symbol"))?;
    if spelling.contains('/') {
        return Err(err_at(s, format!("a {what} takes no qualifier")));
    }
    reject_reserved(s, &spelling)?;
    Ok(spelling)
}

fn use_symbol(s: &Sexp, spelling: &str) -> Result<Ident, ParseError> {
    let slashes = spelling.matches('/').count();
    if slashes == 0 {
        return Ok(Ident {
            qualifier: None,
            name: spelling.to_string(),
        });
    }
    if slashes > 1 {
        return Err(err_at(s, "a name has at most one `/`"));
    }
    let (qualifier, name) = spelling.split_once('/').unwrap();
    if qualifier.is_empty() || name.is_empty() {
        return Err(err_at(s, "a `/` needs a qualifier and a name"));
    }
    Ok(Ident {
        qualifier: Some(qualifier.to_string()),
        name: name.to_string(),
    })
}

fn list(s: &Sexp) -> Result<&[Sexp], ParseError> {
    match &s.kind {
        SexpKind::List(xs) => Ok(xs),
        _ => Err(err_at(s, "a form must be a list")),
    }
}

fn vector(s: &Sexp) -> Result<&[Sexp], ParseError> {
    match &s.kind {
        SexpKind::Vector(xs) => Ok(xs),
        _ => Err(err_at(
            s,
            "a libspec must be a vector, e.g. [com.acme.product :as product]",
        )),
    }
}

fn expect_symbol(s: &Sexp, message: &str) -> Result<String, ParseError> {
    match &s.kind {
        SexpKind::Symbol(name) => Ok(name.clone()),
        _ => Err(err_at(s, message)),
    }
}

fn reject_reserved(s: &Sexp, spelling: &str) -> Result<(), ParseError> {
    if matches!(spelling, "name" | "sense" | "ns" | "and" | "or" | "not") {
        Err(err_at(
            s,
            format!("`{spelling}` is reserved and cannot be declared"),
        ))
    } else {
        Ok(())
    }
}

fn is_retired(head: &str) -> bool {
    matches!(
        head,
        "require" | "def" | "declare" | "forall" | "exists" | "implies"
    )
}

fn retired(head: &str) -> ParseError {
    err(
        1,
        1,
        format!(
            "`{head}` is not in this language; a model is names, senses, and `and` / `or` / `not`"
        ),
    )
}

fn err(line: usize, col: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        line,
        col,
        message: message.into(),
    }
}

fn err_at(s: &Sexp, message: impl Into<String>) -> ParseError {
    err(s.line, s.col, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{Arg, Formula, Reading, Refer};

    #[test]
    fn parses_declarations_and_applications() {
        let src = r#"
;; vocabulary
(name aaa)
(name bbb)
(sense depends_on "{X} depends on {Y}")
(sense borrowed_from "{borrower} borrows from {lender}")

("{X} depends on {Y}" aaa bbb)
("{X} is deprecated" aaa)
("Oracle calls the TypeSafe API")
(depends_on aaa bbb)
(and (depends_on aaa bbb) (not (borrowed_from aaa bbb)))
"#;
        let program = parse(src).unwrap();
        assert_eq!(program.names().collect::<Vec<_>>(), vec!["aaa", "bbb"]);
        let senses: Vec<_> = program.senses().map(|s| s.name.as_str()).collect();
        assert_eq!(senses, vec!["depends_on", "borrowed_from"]);
        assert_eq!(
            program.senses().nth(1).unwrap().holes,
            vec!["borrower", "lender"]
        );
        assert_eq!(program.claims().count(), 5);
    }

    #[test]
    fn repeated_hole_labels_stay_in_order() {
        let program = parse(r#"(sense part_of "{Name} is a part of {Name}")"#).unwrap();
        assert_eq!(program.senses().next().unwrap().holes, vec!["Name", "Name"]);
    }

    #[test]
    fn rejects_retired_forms() {
        for src in [
            "(require (component \"Frontend\"))",
            "(def depends_on [x y] (component x))",
            "(declare component \"{String} is a component\")",
            "(forall [x] (component x))",
            "(exists [f] (file f))",
            "(implies (component \"A\") (component \"B\"))",
        ] {
            let err = parse(src).unwrap_err();
            assert!(err.message.contains("not in this language"), "{src}: {err}");
        }
    }

    #[test]
    fn rejects_a_declaration_inside_a_formula() {
        let err = parse("(and (name aaa))").unwrap_err();
        assert!(err.message.contains("declaration"));
        let err = parse("(and (ns com.acme.product))").unwrap_err();
        assert!(err.message.contains("declaration"), "{err}");
    }

    #[test]
    fn parses_namespace_requires() {
        let src = r#"
(ns com.acme.client
  (:require [com.acme.product :as product]
            [com.acme.names :refer [Frontend, Backend]]
            [com.acme.api :as api :refer :all]
            [com.acme.bare]
            [com.acme.other :refer [part_of] :as other]))
(name Local)
(product/part_of product/Frontend Local)
"#;
        let program = parse(src).unwrap();
        let ns = program.namespace.clone().unwrap();
        assert_eq!(ns.name, "com.acme.client");
        assert_eq!(ns.requires.len(), 5);
        assert_eq!(ns.requires[0].namespace, "com.acme.product");
        assert_eq!(ns.requires[0].alias.as_deref(), Some("product"));
        assert_eq!(ns.requires[0].refer, Refer::Nothing);
        assert_eq!(
            ns.requires[1].refer,
            Refer::Only(vec!["Frontend".into(), "Backend".into()])
        );
        assert_eq!(ns.requires[2].alias.as_deref(), Some("api"));
        assert_eq!(ns.requires[2].refer, Refer::All);
        assert_eq!(ns.requires[3].alias, None);
        assert_eq!(ns.requires[3].refer, Refer::Nothing);
        assert_eq!(ns.requires[4].alias.as_deref(), Some("other"));
        assert_eq!(ns.requires[4].refer, Refer::Only(vec!["part_of".into()]));
        assert_eq!(program.names().collect::<Vec<_>>(), vec!["Local"]);
        let Formula::Apply { reading, args } = program.claims().next().unwrap() else {
            panic!("claim");
        };
        let Reading::Sense(head) = reading else {
            panic!("sense");
        };
        assert_eq!(head.display(), "product/part_of");
        let Arg::Name(qualified) = &args[0] else {
            panic!("qualified name");
        };
        assert_eq!(qualified.display(), "product/Frontend");
        let Arg::Name(local) = &args[1] else {
            panic!("local name");
        };
        assert_eq!(local.name, "Local");
    }

    #[test]
    fn ns_must_be_first_and_a_file_has_one() {
        for src in [
            "(name A)\n(ns com.acme.product)",
            "(ns com.acme.a)\n(ns com.acme.b)",
        ] {
            let err = parse(src).unwrap_err();
            assert!(err.message.contains("first"), "{src}: {err}");
        }
    }

    #[test]
    fn rejects_qualified_declarations_and_bad_namespaces() {
        let name = parse("(name product/Frontend)").unwrap_err();
        assert!(name.message.contains("no qualifier"), "{name}");
        let sense = parse("(sense product/part_of \"{X} is part of {Y}\")").unwrap_err();
        assert!(sense.message.contains("no qualifier"), "{sense}");
        for src in [
            "(ns foo/bar)",
            "(ns .foo)",
            "(ns foo.)",
            "(ns foo..bar)",
            "(ns)",
        ] {
            let err = parse(src).unwrap_err();
            assert!(
                err.message.contains("namespace") || err.message.contains("`ns` takes a name"),
                "{src}: {err}"
            );
        }
        let slash = parse("(name A)\n(sense s \"{X}\")\n(s a/b/c)").unwrap_err();
        assert!(slash.message.contains("at most one"), "{slash}");
    }

    #[test]
    fn require_at_top_level_stays_retired() {
        let err = parse("(require (component \"Frontend\"))").unwrap_err();
        assert!(err.message.contains("not in this language"), "{err}");
    }

    #[test]
    fn skips_comments() {
        let program = parse(";; hi\n(\"Oracle calls the TypeSafe API\")\n").unwrap();
        assert_eq!(program.forms.len(), 1);
    }

    #[test]
    fn claim_spans_are_the_source_text() {
        let src = include_str!("../../examples/diagram.lect");
        let program = parse(src).unwrap();
        let spans = program.claim_spans();
        assert_eq!(spans.len(), 4);
        assert_eq!(&src[spans[0].clone()], "(depends_on Frontend Backend)");
        assert_eq!(
            &src[spans[1].clone()],
            "(not (depends_on Backend Frontend))"
        );
        assert!(src[spans[2].clone()].starts_with("(or\n"));
        assert!(src[spans[2].clone()].contains("\"Oracle calls the TypeSafe API\""));
        assert!(src[spans[3].clone()].starts_with("(and\n"));
        assert!(src[spans[3].clone()].contains("\"{Name} is a component of the system\" Backend"));
        assert!(program.spans.len() == program.forms.len());
        assert_eq!(program.clauses.len(), spans.len());
        assert_eq!(program.clauses[3].children.len(), 3);
        assert_eq!(
            &src[program.clauses[3].children[0].span.clone()],
            "(\"Frontend is a component of the system\")"
        );
        assert_eq!(
            &src[program.clauses[3].children[2].span.clone()],
            "(\"{caller} calls {callee}\" Frontend Backend)"
        );
    }
}
