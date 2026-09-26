use std::ops::Range;

use super::ast::{ClauseSpan, Form, Formula, Program, Reading, Sense};
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
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
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
                b'(' | b')' | b'[' | b']' | b'"' | b' ' | b'\t' | b'\r' | b'\n'
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
    kind: SexpKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SexpKind {
    List(Vec<Sexp>),
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
            TokenKind::LBrack | TokenKind::RBrack => {
                return Err(err(tok.line, tok.col, "brackets are not in this language"))
            }
            TokenKind::Symbol(s) => SexpKind::Symbol(s),
            TokenKind::String(s) => SexpKind::String(s),
        };
        let end = self.tokens[self.i - 1].end;
        Ok(Sexp {
            span: tok.start..end,
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

/// Parse a model: `name` and `sense` declarations, and claims built with
/// `and`, `or`, `not`, and relation application.
pub fn parse(src: &str) -> Result<Program, ParseError> {
    let tokens = tokenize(src)?;
    if tokens.is_empty() {
        return Ok(Program {
            forms: Vec::new(),
            spans: Vec::new(),
            clauses: Vec::new(),
        });
    }
    let mut p = Parser { tokens, i: 0 };
    let mut forms = Vec::new();
    let mut spans = Vec::new();
    let mut clauses = Vec::new();
    while p.peek().is_some() {
        let start = p.peek().map(|token| token.start).unwrap_or(0);
        let s = p.sexp()?;
        let end = p.tokens[p.i - 1].end;
        let (form, clause) = lower_form(&s)?;
        forms.push(form);
        spans.push(start..end);
        if let Some(clause) = clause {
            clauses.push(clause);
        }
    }
    Ok(Program {
        forms,
        spans,
        clauses,
    })
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
        return Err(err(1, 1, "`name` takes one name"));
    }
    let spelling = expect_symbol(&xs[1], "`name` takes a symbol")?;
    reject_reserved(&spelling)?;
    Ok(spelling)
}

fn lower_sense(xs: &[Sexp]) -> Result<Sense, ParseError> {
    if xs.len() != 3 {
        return Err(err(
            1,
            1,
            "`sense` takes a name and a reading, e.g. (sense calls \"{caller} calls {callee}\")",
        ));
    }
    let name = expect_symbol(&xs[1], "`sense` name must be a symbol")?;
    reject_reserved(&name)?;
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
        SexpKind::Symbol(head) if head == "name" || head == "sense" => Err(err(
            1,
            1,
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
                reading: Reading::Sense(head.clone()),
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
        SexpKind::List(_) => Err(err(
            1,
            1,
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

fn lower_args(xs: &[Sexp]) -> Result<Vec<String>, ParseError> {
    xs.iter()
        .map(|s| expect_symbol(s, "an argument must be a name"))
        .collect()
}

fn list(s: &Sexp) -> Result<&[Sexp], ParseError> {
    match &s.kind {
        SexpKind::List(xs) => Ok(xs),
        _ => Err(err(1, 1, "a form must be a list")),
    }
}

fn expect_symbol(s: &Sexp, message: &str) -> Result<String, ParseError> {
    match &s.kind {
        SexpKind::Symbol(name) => Ok(name.clone()),
        _ => Err(err(1, 1, message)),
    }
}

fn reject_reserved(spelling: &str) -> Result<(), ParseError> {
    if matches!(spelling, "name" | "sense" | "and" | "or" | "not") {
        Err(err(
            1,
            1,
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

#[cfg(test)]
mod tests {
    use super::*;

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
