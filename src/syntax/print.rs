use super::ast::{Form, Formula, Program, Reading};
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
        Formula::Apply { reading, args } => {
            out.push('(');
            match reading {
                Reading::Sense(name) => out.push_str(name),
                Reading::Inline { template, holes: _ } => out.push_str(&quote_const(template)),
            }
            for arg in args {
                out.push(' ');
                out.push_str(arg);
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
}
