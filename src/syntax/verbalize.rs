use super::ast::Proposition;
use super::parse::{parse, ParseError};
use super::template::quote_const;
use crate::compiler::{compile, CompileError};

#[derive(Debug)]
pub enum VerbalizeError {
    Parse(ParseError),
    Compile(CompileError),
}

impl std::fmt::Display for VerbalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerbalizeError::Parse(err) => write!(f, "{err}"),
            VerbalizeError::Compile(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for VerbalizeError {}

/// Render a proposition as source. A sentence is a ground reading `("...")`.
/// `and`, `or`, `not`, and `detail` stay in place for the planner. A detail's
/// local declarations are resolved away; its body is the claims that remain.
pub fn verbalize_proposition(proposition: &Proposition) -> String {
    match proposition {
        Proposition::Sentence(sentence) => format!("({})", quote_const(sentence)),
        Proposition::Not(inner) => format!("(not {})", verbalize_proposition(inner)),
        Proposition::And(xs) => verbalize_nary("and", xs),
        Proposition::Or(xs) => verbalize_nary("or", xs),
        Proposition::Detail { head, body } => {
            let mut out = format!("(detail ({})", quote_const(head));
            for proposition in body {
                out.push(' ');
                out.push_str(&verbalize_proposition(proposition));
            }
            out.push(')');
            out
        }
    }
}

fn verbalize_nary(head: &str, xs: &[Proposition]) -> String {
    let mut out = format!("({head}");
    for proposition in xs {
        out.push(' ');
        out.push_str(&verbalize_proposition(proposition));
    }
    out.push(')');
    out
}

/// Parse and compile `src`, then verbalize each claim.
pub fn verbalize_source(src: &str) -> Result<Vec<String>, VerbalizeError> {
    let program = parse(src).map_err(VerbalizeError::Parse)?;
    let compiled = compile(&program).map_err(VerbalizeError::Compile)?;
    Ok(compiled.claims.iter().map(verbalize_proposition).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
(name aaa)
(name bbb)
(sense depends_on "{X} depends on {Y}")
(sense deprecated "{X} is deprecated")
(sense borrowed_from "{borrower} borrows from {lender}")

("{X} depends on {Y}" aaa bbb)
("{X} is deprecated" aaa)
("Oracle calls the TypeSafe API")
(depends_on aaa bbb)
(borrowed_from aaa bbb)
(and (depends_on aaa bbb) (not (deprecated aaa)))
(or (depends_on aaa bbb) (depends_on bbb aaa))
"#;

    #[test]
    fn applications_reduce_to_sentences() {
        let lines = verbalize_source(SRC).unwrap();
        assert_eq!(
            lines,
            vec![
                "(\"aaa depends on bbb\")".to_string(),
                "(\"aaa is deprecated\")".to_string(),
                "(\"Oracle calls the TypeSafe API\")".to_string(),
                "(\"aaa depends on bbb\")".to_string(),
                "(\"aaa borrows from bbb\")".to_string(),
                "(and (\"aaa depends on bbb\") (not (\"aaa is deprecated\")))".to_string(),
                "(or (\"aaa depends on bbb\") (\"bbb depends on aaa\"))".to_string(),
            ]
        );
    }

    #[test]
    fn a_verbalized_claim_parses_as_a_ground_sentence() {
        let lines = verbalize_source(SRC).unwrap();
        let again = verbalize_source(&lines[0]).unwrap();
        assert_eq!(again, vec![lines[0].clone()]);
    }
}
