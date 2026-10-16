use super::ast::{Arg, Form, Formula, Ident, Ns, Program, Reading, Refer};
use super::template::quote_const;

/// Pretty-print a program. Comments are not reconstructed.
pub fn print_program(program: &Program) -> String {
    let mut out = String::new();
    if let Some(ns) = &program.namespace {
        print_ns(ns, &mut out);
        if !program.forms.is_empty() {
            out.push('\n');
        }
    }
    for (i, form) in program.forms.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        print_form(form, 0, &mut out);
        out.push('\n');
    }
    out
}

fn print_ns(ns: &Ns, out: &mut String) {
    out.push_str("(ns ");
    out.push_str(&ns.name);
    if ns.requires.is_empty() {
        out.push(')');
        out.push('\n');
        return;
    }
    out.push('\n');
    out.push_str("  (:require");
    for require in &ns.requires {
        out.push('\n');
        out.push_str("    [");
        out.push_str(&require.namespace);
        if let Some(alias) = &require.alias {
            out.push_str(" :as ");
            out.push_str(alias);
        }
        match &require.refer {
            Refer::Nothing => {}
            Refer::All => out.push_str(" :refer :all"),
            Refer::Only(names) => {
                out.push_str(" :refer [");
                out.push_str(&names.join(" "));
                out.push(']');
            }
        }
        out.push(']');
    }
    out.push_str("))\n");
}

/// Pretty-print a formula at indent 0.
pub fn print_formula(formula: &Formula) -> String {
    let mut out = String::new();
    write_formula(formula, 0, &mut out);
    out
}

fn print_form(form: &Form, indent: usize, out: &mut String) {
    match form {
        Form::Name(name) => {
            out.push_str("(name ");
            out.push_str(name);
            out.push(')');
        }
        Form::Sense(sense) => {
            out.push_str("(sense ");
            out.push_str(&sense.name);
            out.push(' ');
            out.push_str(&quote_const(&sense.template));
            out.push(')');
        }
        Form::Claim(formula) => write_formula(formula, indent, out),
    }
}

fn write_formula(formula: &Formula, indent: usize, out: &mut String) {
    match formula {
        Formula::And(xs) => print_nary("and", xs, indent, out),
        Formula::Or(xs) => print_nary("or", xs, indent, out),
        Formula::Not(x) => {
            out.push_str("(not ");
            write_formula(x, indent, out);
            out.push(')');
        }
        Formula::Detail { head, body } => {
            out.push_str("(detail ");
            write_formula(head, indent, out);
            if body.len() == 1 {
                out.push(' ');
                print_form(&body[0], indent, out);
                out.push(')');
                return;
            }
            for form in body {
                out.push('\n');
                write_indent(indent + 1, out);
                print_form(form, indent + 1, out);
            }
            out.push(')');
        }
        Formula::Witness { fact, paths } => {
            out.push_str("(witness ");
            write_formula(fact, indent, out);
            for path in paths {
                out.push(' ');
                out.push_str(&quote_const(path));
            }
            out.push(')');
        }
        Formula::Apply { reading, args } => {
            out.push('(');
            match reading {
                Reading::Sense(name) => push_ident(name, out),
                Reading::Inline { template, holes: _ } => out.push_str(&quote_const(template)),
            }
            for arg in args {
                out.push(' ');
                match arg {
                    Arg::Name(ident) => push_ident(ident, out),
                    Arg::Literal(text) => out.push_str(&quote_const(text)),
                }
            }
            out.push(')');
        }
    }
}

fn print_nary(head: &str, xs: &[Formula], indent: usize, out: &mut String) {
    out.push('(');
    out.push_str(head);
    if xs.is_empty() {
        out.push(')');
        return;
    }
    if xs.len() == 1 {
        out.push(' ');
        write_formula(&xs[0], indent, out);
        out.push(')');
        return;
    }
    for x in xs {
        out.push('\n');
        write_indent(indent + 1, out);
        write_formula(x, indent + 1, out);
    }
    out.push(')');
}

fn push_ident(ident: &Ident, out: &mut String) {
    if let Some(qualifier) = &ident.qualifier {
        out.push_str(qualifier);
        out.push('/');
    }
    out.push_str(&ident.name);
}

fn write_indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse::parse;

    #[test]
    fn round_trip() {
        let src = r#"(name aaa)
(name bbb)
(sense depends_on "{X} depends on {Y}")
(sense borrowed_from "{borrower} borrows from {lender}")
("{X} depends on {Y}" aaa bbb)
("{X} is deprecated" aaa)
("Oracle calls the TypeSafe API")
(depends_on aaa bbb)
(and
  (depends_on aaa bbb)
  (not (borrowed_from aaa bbb)))
"#;
        let program = parse(src).unwrap();
        let printed = print_program(&program);
        let again = parse(&printed).unwrap();
        assert_eq!(program, again);
    }

    #[test]
    fn round_trip_keeps_the_namespace() {
        let src = r#"(ns com.acme.client
  (:require
    [com.acme.product :as product]
    [com.acme.names :refer [Frontend]]
    [com.acme.api :refer :all]))
(name Backend)
(product/part_of product/Frontend Backend)
"#;
        let program = parse(src).unwrap();
        let printed = print_program(&program);
        let again = parse(&printed).unwrap();
        assert_eq!(program, again);
    }

    #[test]
    fn round_trip_keeps_a_detail() {
        let src = r#"(detail ("broad")
  (name Concept)
  (sense input "{X} is an input into {Y}")
  (and
    (input Concept LogicBlock)
    ("Concept is named")))
"#;
        let program = parse(src).unwrap();
        let again = parse(&print_program(&program)).unwrap();
        assert_eq!(program, again);
    }

    #[test]
    fn round_trip_keeps_a_string_argument() {
        let src = "(source Universe \"src/universe\")\n";
        let program = parse(src).unwrap();
        let again = parse(&print_program(&program)).unwrap();
        assert_eq!(program, again);
    }
}
