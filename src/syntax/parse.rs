use super::ast::{Form, Formula, Program, Term};

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
        Ok(Some(Token { kind, line, col }))
    }

    fn lex_string(&mut self, line: usize, col: usize) -> Result<String, ParseError> {
        self.bump(); // opening quote
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
enum Sexp {
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
        let tok = self.bump().ok_or_else(|| ParseError {
            line: 1,
            col: 1,
            message: "unexpected end of input".into(),
        })?;
        match tok.kind {
            TokenKind::LParen => self.list(true, tok.line, tok.col),
            TokenKind::LBrack => self.list(false, tok.line, tok.col),
            TokenKind::RParen => Err(ParseError {
                line: tok.line,
                col: tok.col,
                message: "unexpected ')'".into(),
            }),
            TokenKind::RBrack => Err(ParseError {
                line: tok.line,
                col: tok.col,
                message: "unexpected ']'".into(),
            }),
            TokenKind::Symbol(s) => Ok(Sexp::Symbol(s)),
            TokenKind::String(s) => Ok(Sexp::String(s)),
        }
    }

    fn list(&mut self, paren: bool, line: usize, col: usize) -> Result<Sexp, ParseError> {
        let mut xs = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError {
                        line,
                        col,
                        message: if paren {
                            "unterminated '('".into()
                        } else {
                            "unterminated '['".into()
                        },
                    });
                }
                Some(t)
                    if (paren && t.kind == TokenKind::RParen)
                        || (!paren && t.kind == TokenKind::RBrack) =>
                {
                    self.bump();
                    break;
                }
                Some(_) => xs.push(self.sexp()?),
            }
        }
        if paren {
            Ok(Sexp::List(xs))
        } else {
            Ok(Sexp::Vector(xs))
        }
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

/// Parse a single atom, as used by `autoground ground`.
/// Accepts `(component "Frontend")`, `component "Frontend"`, and
/// `(component ?x)` / `(component x)`.
pub fn parse_atom(src: &str) -> Result<(String, Vec<Term>), ParseError> {
    let trimmed = src.trim();
    let wrapped = if trimmed.starts_with('(') {
        trimmed.to_string()
    } else {
        format!("({trimmed})")
    };
    let program = parse(&format!("(require {wrapped})"))?;
    match program.forms.into_iter().next() {
        Some(Form::Require(Formula::Atom { pred, args })) => {
            let args = args
                .into_iter()
                .map(|t| match t {
                    Term::Var(v) => Term::Var(v.strip_prefix('?').unwrap_or(&v).to_string()),
                    other => other,
                })
                .collect();
            Ok((pred, args))
        }
        Some(Form::Require(_)) => Err(err(
            1,
            1,
            "expected a single atom like (component \"Frontend\") or (component ?x)",
        )),
        _ => Err(err(1, 1, "expected a single atom")),
    }
}

/// Parse a complete model from source text.
pub fn parse(src: &str) -> Result<Program, ParseError> {
    let tokens = tokenize(src)?;
    if tokens.is_empty() {
        return Ok(Program { forms: Vec::new() });
    }
    let mut p = Parser { tokens, i: 0 };
    let mut forms = Vec::new();
    while p.peek().is_some() {
        let s = p.sexp()?;
        forms.push(lower_form(&s)?);
    }
    Ok(Program { forms })
}

fn lower_form(s: &Sexp) -> Result<Form, ParseError> {
    let Sexp::List(xs) = s else {
        return Err(err(1, 1, "top-level form must be a list"));
    };
    match xs.first() {
        Some(Sexp::Symbol(head)) if head == "require" => {
            if xs.len() != 2 {
                return Err(err(1, 1, "`require` takes one formula"));
            }
            Ok(Form::Require(lower_formula(&xs[1])?))
        }
        Some(Sexp::Symbol(head)) if head == "def" => {
            if xs.len() != 4 {
                return Err(err(1, 1, "`def` takes a name, a variable list, and a body"));
            }
            let name = match &xs[1] {
                Sexp::Symbol(n) => n.clone(),
                _ => return Err(err(1, 1, "`def` name must be a symbol")),
            };
            let vars = lower_varlist(&xs[2])?;
            let body = lower_formula(&xs[3])?;
            Ok(Form::Def { name, vars, body })
        }
        Some(Sexp::Symbol(head)) => Err(err(
            1,
            1,
            format!("unknown top-level form `{head}`; expected `require` or `def`"),
        )),
        _ => Err(err(
            1,
            1,
            "top-level form must start with `require` or `def`",
        )),
    }
}

fn lower_varlist(s: &Sexp) -> Result<Vec<String>, ParseError> {
    let Sexp::Vector(xs) = s else {
        return Err(err(
            1,
            1,
            "variable list must be written in square brackets",
        ));
    };
    let mut vars = Vec::new();
    for x in xs {
        match x {
            Sexp::Symbol(v) => vars.push(v.clone()),
            _ => return Err(err(1, 1, "variable list entries must be symbols")),
        }
    }
    Ok(vars)
}

fn lower_formula(s: &Sexp) -> Result<Formula, ParseError> {
    let Sexp::List(xs) = s else {
        return Err(err(
            1,
            1,
            "a formula must be a list `(and ...)`, `(not ...)`, or `(pred args...)`",
        ));
    };
    if xs.is_empty() {
        return Err(err(1, 1, "empty list is not a formula"));
    }
    let Sexp::Symbol(head) = &xs[0] else {
        return Err(err(1, 1, "formula head must be a symbol"));
    };
    match head.as_str() {
        "and" => Ok(Formula::And(lower_formulas(&xs[1..])?)),
        "or" => Ok(Formula::Or(lower_formulas(&xs[1..])?)),
        "not" => {
            if xs.len() != 2 {
                return Err(err(1, 1, "`not` takes one formula"));
            }
            Ok(Formula::Not(Box::new(lower_formula(&xs[1])?)))
        }
        "implies" => {
            if xs.len() != 3 {
                return Err(err(1, 1, "`implies` takes two formulas"));
            }
            Ok(Formula::Implies(
                Box::new(lower_formula(&xs[1])?),
                Box::new(lower_formula(&xs[2])?),
            ))
        }
        "exists" | "forall" => {
            if xs.len() != 3 {
                return Err(err(
                    1,
                    1,
                    format!("`{head}` takes a variable list and a formula"),
                ));
            }
            let vars = lower_varlist(&xs[1])?;
            let body = Box::new(lower_formula(&xs[2])?);
            if head == "exists" {
                Ok(Formula::Exists(vars, body))
            } else {
                Ok(Formula::Forall(vars, body))
            }
        }
        _ => {
            let mut args = Vec::new();
            for a in &xs[1..] {
                args.push(lower_term(a)?);
            }
            Ok(Formula::Atom {
                pred: head.clone(),
                args,
            })
        }
    }
}

fn lower_formulas(xs: &[Sexp]) -> Result<Vec<Formula>, ParseError> {
    xs.iter().map(lower_formula).collect()
}

fn lower_term(s: &Sexp) -> Result<Term, ParseError> {
    match s {
        Sexp::Symbol(v) => Ok(Term::Var(v.clone())),
        Sexp::String(c) => Ok(Term::Const(c.clone())),
        _ => Err(err(
            1,
            1,
            "a term must be a variable (symbol) or a constant (string)",
        )),
    }
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
    fn rejects_unknown_toplevel() {
        let err = parse("(foo 1)").unwrap_err();
        assert!(err.message.contains("unknown top-level form"));
    }

    #[test]
    fn skips_comments() {
        let p = parse(";; hi\n(require (ok \"x\"))\n").unwrap();
        assert_eq!(p.forms.len(), 1);
    }

    #[test]
    fn parse_atom_const_and_var() {
        let (pred, args) = parse_atom("(component \"Frontend\")").unwrap();
        assert_eq!(pred, "component");
        assert_eq!(args, vec![Term::Const("Frontend".into())]);
        let (pred, args) = parse_atom("(component ?x)").unwrap();
        assert_eq!(pred, "component");
        assert_eq!(args, vec![Term::Var("x".into())]);
        let (pred, args) = parse_atom("component x").unwrap();
        assert_eq!(pred, "component");
        assert_eq!(args, vec![Term::Var("x".into())]);
    }
}
