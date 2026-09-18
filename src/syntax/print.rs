use super::ast::{Form, Formula, Program, Term};
use super::template::quote_const;

/// Pretty-print a program. Comments are not reconstructed.
pub fn print_program(program: &Program) -> String {
    let mut out = String::new();
    for (i, form) in program.forms.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        print_form(form, 0, &mut out);
        out.push('\n');
    }
    out
}

/// Pretty-print a `require` form, matching `print_program`.
pub fn print_require(f: &Formula) -> String {
    let mut out = String::from("(require\n");
    write_indent(1, &mut out);
    write_formula(f, 1, &mut out);
    out.push(')');
    out
}

/// Pretty-print a formula at indent 0.
pub fn print_formula(f: &Formula) -> String {
    let mut out = String::new();
    write_formula(f, 0, &mut out);
    out
}

/// `(pred "a" "b")` for a fully ground EDB atom.
pub fn print_ground_atom(pred: &str, args: &[String]) -> String {
    let mut out = String::from("(");
    out.push_str(pred);
    for a in args {
        out.push(' ');
        out.push_str(&quote_const(a));
    }
    out.push(')');
    out
}

fn print_form(form: &Form, indent: usize, out: &mut String) {
    match form {
        Form::Require(f) => {
            out.push_str("(require");
            out.push('\n');
            write_indent(indent + 1, out);
            write_formula(f, indent + 1, out);
            out.push(')');
        }
        Form::Def { name, vars, body } => {
            out.push_str("(def ");
            out.push_str(name);
            out.push('\n');
            write_indent(indent + 1, out);
            out.push('[');
            for (i, v) in vars.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(v);
            }
            out.push(']');
            out.push('\n');
            write_indent(indent + 1, out);
            write_formula(body, indent + 1, out);
            out.push(')');
        }
        Form::Declare(d) => {
            out.push_str("(declare ");
            out.push_str(&d.name);
            out.push(' ');
            out.push_str(&quote_const(&d.template));
            out.push(')');
        }
    }
}

fn write_formula(f: &Formula, indent: usize, out: &mut String) {
    match f {
        Formula::And(xs) => print_nary("and", xs, indent, out),
        Formula::Or(xs) => print_nary("or", xs, indent, out),
        Formula::Not(x) => {
            out.push_str("(not ");
            write_formula(x, indent, out);
            out.push(')');
        }
        Formula::Implies(a, b) => {
            out.push_str("(implies");
            out.push('\n');
            write_indent(indent + 1, out);
            write_formula(a, indent + 1, out);
            out.push('\n');
            write_indent(indent + 1, out);
            write_formula(b, indent + 1, out);
            out.push(')');
        }
        Formula::Exists(vars, body) => print_quant("exists", vars, body, indent, out),
        Formula::Forall(vars, body) => print_quant("forall", vars, body, indent, out),
        Formula::Atom { pred, args } => {
            out.push('(');
            out.push_str(pred);
            for arg in args {
                out.push(' ');
                print_term(arg, out);
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

fn print_quant(head: &str, vars: &[String], body: &Formula, indent: usize, out: &mut String) {
    out.push('(');
    out.push_str(head);
    out.push_str(" [");
    for (i, v) in vars.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(v);
    }
    out.push(']');
    out.push('\n');
    write_indent(indent + 1, out);
    write_formula(body, indent + 1, out);
    out.push(')');
}

fn print_term(t: &Term, out: &mut String) {
    match t {
        Term::Var(v) => out.push_str(v),
        Term::Const(c) => out.push_str(&quote_const(c)),
        Term::Typed { ty, value } => {
            out.push('(');
            out.push_str(ty);
            out.push(' ');
            out.push_str(&quote_const(value));
            out.push(')');
        }
    }
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
    fn round_trip_simple() {
        let src = "(require (and (component \"Frontend\") (component \"Backend\")))\n";
        let p = parse(src).unwrap();
        let printed = print_program(&p);
        let p2 = parse(&printed).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn round_trip_declare() {
        let src = "(declare Component \"{String} is a component in the codebase\")\n";
        let p = parse(src).unwrap();
        let printed = print_program(&p);
        let p2 = parse(&printed).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn print_require_matches_program_form() {
        let src = "(require (and (component \"Frontend\") (component \"Backend\")))\n";
        let p = parse(src).unwrap();
        let f = p.requires().next().expect("require");
        assert_eq!(print_require(f), print_program(&p).trim_end());
        assert_eq!(
            print_ground_atom("includes", &["Frontend".into(), "ui/app.py".into()]),
            "(includes \"Frontend\" \"ui/app.py\")"
        );
    }
}
