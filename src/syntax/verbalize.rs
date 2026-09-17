use std::collections::HashMap;

use super::ast::{Declare, Term};
use super::parse::{parse_verbalize_input, ParseError};
use super::template::{is_builtin_sort, parse_template, quote_const, TemplatePart};

/// Verbalize a ground or open atom using `declare` templates.
///
/// * `{String}` (builtin) prints as-is: `"Frontend"` or `x`.
/// * A unary predicate type prints as `File "ui/app.py"` / `File f`.
/// * A declared predicate at the top level fills its template.
pub fn verbalize_atom(pred: &str, args: &[Term], decls: &HashMap<String, Declare>) -> String {
    if let Some(d) = decls.get(pred) {
        return fill_template(d, args);
    }
    fallback(pred, args)
}

fn fill_template(d: &Declare, args: &[Term]) -> String {
    let parts = match parse_template(&d.template) {
        Ok(p) => p,
        Err(_) => return fallback(&d.name, args),
    };
    let mut slot_i = 0;
    let mut out = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(s) => out.push_str(&s),
            TemplatePart::Slot(ty) => {
                let arg = args.get(slot_i);
                slot_i += 1;
                match arg {
                    Some(term) => out.push_str(&verbalize_arg(&ty, term)),
                    None => out.push('?'),
                }
            }
        }
    }
    out
}

fn verbalize_arg(slot_ty: &str, arg: &Term) -> String {
    let (print_ty, value, is_var) = match arg {
        Term::Var(v) => (slot_ty, v.as_str(), true),
        Term::Const(c) => (slot_ty, c.as_str(), false),
        Term::Typed { ty, value } => (ty.as_str(), value.as_str(), false),
    };
    if is_builtin_sort(print_ty) {
        if is_var {
            value.to_string()
        } else {
            quote_const(value)
        }
    } else if is_var {
        format!("{print_ty} {value}")
    } else {
        format!("{print_ty} {}", quote_const(value))
    }
}

fn fallback(pred: &str, args: &[Term]) -> String {
    if args.len() <= 1 {
        match args.first() {
            None => pred.to_string(),
            Some(Term::Var(v)) => format!("{pred} {v}"),
            Some(Term::Const(c)) => format!("{pred} {}", quote_const(c)),
            Some(Term::Typed { ty, value }) => {
                format!("{pred} {ty} {}", quote_const(value))
            }
        }
    } else {
        let mut out = format!("({pred}");
        for a in args {
            out.push(' ');
            match a {
                Term::Var(v) => out.push_str(v),
                Term::Const(c) => out.push_str(&quote_const(c)),
                Term::Typed { ty, value } => {
                    out.push_str(ty);
                    out.push(' ');
                    out.push_str(&quote_const(value));
                }
            }
        }
        out.push(')');
        out
    }
}

/// Parse source (a file body or an atom string) and verbalize every atom.
pub fn verbalize_source(src: &str) -> Result<Vec<String>, ParseError> {
    let input = parse_verbalize_input(src)?;
    let mut seen = HashMap::new();
    for d in &input.declares {
        if seen.insert(d.name.clone(), d.clone()).is_some() {
            return Err(ParseError {
                line: 1,
                col: 1,
                message: format!("duplicate declaration of `{}`", d.name),
            });
        }
    }
    Ok(input
        .atoms
        .iter()
        .map(|(pred, args)| verbalize_atom(pred, args, &seen))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse::parse;

    fn catalog(src: &str) -> HashMap<String, Declare> {
        let p = parse(src).unwrap();
        p.declares().cloned().map(|d| (d.name.clone(), d)).collect()
    }

    #[test]
    fn user_examples() {
        let decls = catalog(
            r#"
(declare Component "{String} is a component in the codebase")
(declare File "{String} is a file in the codebase")
(declare defines "{File} (partially) defines {Component}")
"#,
        );
        assert_eq!(
            verbalize_atom("Component", &[Term::Const("Frontend".into())], &decls),
            "\"Frontend\" is a component in the codebase"
        );
        assert_eq!(
            verbalize_atom(
                "defines",
                &[
                    Term::Typed {
                        ty: "File".into(),
                        value: "ui/app.py".into()
                    },
                    Term::Typed {
                        ty: "Component".into(),
                        value: "Frontend".into()
                    }
                ],
                &decls
            ),
            "File \"ui/app.py\" (partially) defines Component \"Frontend\""
        );
    }

    #[test]
    fn untyped_constants_use_slot_types() {
        let decls = catalog(
            r#"
(declare Component "{String} is a component in the codebase")
(declare File "{String} is a file in the codebase")
(declare defines "{File} (partially) defines {Component}")
"#,
        );
        assert_eq!(
            verbalize_atom(
                "defines",
                &[
                    Term::Const("ui/app.py".into()),
                    Term::Const("Frontend".into())
                ],
                &decls
            ),
            "File \"ui/app.py\" (partially) defines Component \"Frontend\""
        );
    }

    #[test]
    fn verbalize_source_mixes_declares_and_atoms() {
        let lines = verbalize_source(
            r#"
(declare Component "{String} is a component in the codebase")
(declare File "{String} is a file in the codebase")
(declare defines "{File} (partially) defines {Component}")
(Component "Frontend")
(defines (File "ui/app.py") (Component "Frontend"))
"#,
        )
        .unwrap();
        assert_eq!(
            lines,
            vec![
                "\"Frontend\" is a component in the codebase".to_string(),
                "File \"ui/app.py\" (partially) defines Component \"Frontend\"".to_string(),
            ]
        );
    }

    #[test]
    fn undeclared_unary_prints_as_name_and_value() {
        let lines = verbalize_source(r#"(Component "Frontend")"#).unwrap();
        assert_eq!(lines, vec!["Component \"Frontend\"".to_string()]);
    }

    #[test]
    fn variables_are_unquoted() {
        let decls = catalog(r#"(declare Component "{String} is a component in the codebase")"#);
        assert_eq!(
            verbalize_atom("Component", &[Term::Var("x".into())], &decls),
            "x is a component in the codebase"
        );
    }

    #[test]
    fn rejects_bad_template() {
        let err = crate::syntax::template::parse_template("{String is broken").unwrap_err();
        assert!(err.contains("unmatched"));
    }
}
