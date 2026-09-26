use std::collections::{HashMap, HashSet};

use crate::syntax::template::{apply, holes_word, names_word};
use crate::syntax::{Form, Formula, Program, Proposition, Reading, Sense};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub message: String,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CompileError {}

/// A model whose claims are sentences under `and`, `or`, and `not`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub names: Vec<String>,
    pub senses: Vec<Sense>,
    pub claims: Vec<Proposition>,
}

/// Resolve senses and check that every claim is a saturated ground sentence.
pub fn compile(program: &Program) -> Result<Compiled, CompileError> {
    let mut names = Vec::new();
    let mut name_seen = HashSet::new();
    let mut senses = Vec::new();
    let mut sense_seen = HashSet::new();

    for form in &program.forms {
        match form {
            Form::Name(name) => {
                if !name_seen.insert(name.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate name `{name}`"),
                    });
                }
                names.push(name.clone());
            }
            Form::Sense(sense) => {
                if !sense_seen.insert(sense.name.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate sense `{}`", sense.name),
                    });
                }
                senses.push(sense.clone());
            }
            Form::Claim(_) => {}
        }
    }

    for name in &names {
        if sense_seen.contains(name) {
            return Err(CompileError {
                message: format!("`{name}` is declared as both a name and a sense"),
            });
        }
    }

    let sense_by_name: HashMap<&str, &Sense> = senses
        .iter()
        .map(|sense| (sense.name.as_str(), sense))
        .collect();
    let declared: HashSet<&str> = names.iter().map(String::as_str).collect();

    let mut claims = Vec::new();
    for formula in program.claims() {
        claims.push(resolve(formula, &declared, &sense_by_name)?);
    }

    Ok(Compiled {
        names,
        senses,
        claims,
    })
}

fn resolve(
    formula: &Formula,
    names: &HashSet<&str>,
    senses: &HashMap<&str, &Sense>,
) -> Result<Proposition, CompileError> {
    match formula {
        Formula::And(xs) => Ok(Proposition::And(resolve_all(xs, names, senses)?)),
        Formula::Or(xs) => Ok(Proposition::Or(resolve_all(xs, names, senses)?)),
        Formula::Not(x) => Ok(Proposition::Not(Box::new(resolve(x, names, senses)?))),
        Formula::Apply { reading, args } => {
            for arg in args {
                if !names.contains(arg.as_str()) {
                    return Err(CompileError {
                        message: format!("`{arg}` is not a declared name"),
                    });
                }
            }
            let (what, template, holes) = match reading {
                Reading::Sense(name) => {
                    let Some(sense) = senses.get(name.as_str()) else {
                        return Err(CompileError {
                            message: format!("`{name}` is not a declared sense"),
                        });
                    };
                    (
                        format!("`{name}`"),
                        sense.template.as_str(),
                        sense.holes.as_slice(),
                    )
                }
                Reading::Inline { template, holes } => (
                    format!("\"{template}\""),
                    template.as_str(),
                    holes.as_slice(),
                ),
            };
            if args.len() != holes.len() {
                return Err(arity_error(&what, holes, args.len()));
            }
            let sentence = apply(template, args).map_err(|message| CompileError { message })?;
            Ok(Proposition::Sentence(sentence))
        }
    }
}

fn resolve_all(
    formulas: &[Formula],
    names: &HashSet<&str>,
    senses: &HashMap<&str, &Sense>,
) -> Result<Vec<Proposition>, CompileError> {
    formulas
        .iter()
        .map(|formula| resolve(formula, names, senses))
        .collect()
}

fn arity_error(what: &str, holes: &[String], nargs: usize) -> CompileError {
    let listed = if holes.is_empty() {
        "0 holes".to_string()
    } else {
        let labels = holes
            .iter()
            .map(|hole| format!("{{{hole}}}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} {} ({labels})", holes.len(), holes_word(holes.len()))
    };
    CompileError {
        message: format!(
            "{what} has {listed} but was applied to {nargs} {}",
            names_word(nargs)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    fn compiled(src: &str) -> Compiled {
        compile(&parse(src).unwrap()).unwrap()
    }

    #[test]
    fn named_and_inline_applications_meet() {
        let c = compiled(
            r#"
(name X)
(name Y)
(sense part_of "{Name} is a part of {Name}")
(part_of X Y)
("{Name} is a part of {Name}" X Y)
"#,
        );
        assert_eq!(
            c.claims,
            vec![
                Proposition::Sentence("X is a part of Y".into()),
                Proposition::Sentence("X is a part of Y".into()),
            ]
        );
    }

    #[test]
    fn keeps_boolean_structure() {
        let c = compiled(
            r#"
(name Search)
(name Oracle)
(name Universe)
(sense calls "{caller} calls {callee}")
(and (calls Search Oracle) (not (calls Universe Oracle)))
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::And(vec![
                Proposition::Sentence("Search calls Oracle".into()),
                Proposition::Not(Box::new(Proposition::Sentence(
                    "Universe calls Oracle".into()
                ))),
            ])]
        );
    }

    #[test]
    fn declarations_may_follow_uses() {
        let c = compiled(
            r#"
(depends_on Frontend Backend)
(name Frontend)
(name Backend)
(sense depends_on "{X} depends on {Y}")
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Sentence("Frontend depends on Backend".into())]
        );
    }

    #[test]
    fn rejects_an_undeclared_name() {
        let err = compile(
            &parse("(sense component \"{Name} is a part of the system\") (component Frontend)")
                .unwrap(),
        )
        .unwrap_err();
        assert!(
            err.message.contains("`Frontend` is not a declared name"),
            "{err}"
        );
    }

    #[test]
    fn rejects_an_unknown_sense_and_an_arity_mismatch() {
        let unknown = compile(&parse("(name Frontend) (component Frontend)").unwrap()).unwrap_err();
        assert!(
            unknown.message.contains("not a declared sense"),
            "{unknown}"
        );

        let arity = compile(
            &parse(
                r#"
(name Ada)
(sense borrowed_from "{borrower} borrows from {lender}")
(borrowed_from Ada)
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            arity.message.contains("`borrowed_from`")
                && arity.message.contains("{borrower}")
                && arity.message.contains("{lender}")
                && arity.message.contains("1 name"),
            "{arity}"
        );
    }

    #[test]
    fn rejects_duplicates_and_a_shared_spelling() {
        let dup_name = compile(&parse("(name Oracle) (name Oracle)").unwrap()).unwrap_err();
        assert!(dup_name.message.contains("duplicate name"), "{dup_name}");

        let dup_sense = compile(
            &parse("(sense calls \"{caller} calls {callee}\") (sense calls \"{X} calls {Y}\")")
                .unwrap(),
        )
        .unwrap_err();
        assert!(dup_sense.message.contains("duplicate sense"), "{dup_sense}");

        let both =
            compile(&parse("(name calls) (sense calls \"{X} calls {Y}\")").unwrap()).unwrap_err();
        assert!(both.message.contains("both a name and a sense"), "{both}");
    }
}
