//! Markdown context for a run: validated claims and the files that settled them.
//!
//! Written beside the trace as `context-{nanos}-{pid}-{n}.md`. False and
//! unknown claims are omitted. A citation lists settlement files whose verdict
//! was entailed or contradicted, with no confidence and no search ranking.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::plan::{Expr, Plan};
use crate::read::{Stance, Verdict};
use crate::value::Value;

/// `trace-….jsonl` → `context-….md` in the same directory.
pub fn context_path(trace: &Path) -> PathBuf {
    let stem = trace
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("trace");
    let name = match stem.strip_prefix("trace") {
        Some(rest) => format!("context{rest}.md"),
        None => format!("{stem}.md"),
    };
    match trace.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Write [`render`] to [`context_path`]. An empty validated set still creates the file.
pub fn write_context(
    trace: &Path,
    plan: &Plan,
    claim_values: &[Value],
    stances: &[Vec<Stance>],
) -> io::Result<PathBuf> {
    let path = context_path(trace);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&path, render(plan, claim_values, stances))?;
    Ok(path)
}

/// One block per validated claim. Blocks are separated by a blank line.
pub fn render(plan: &Plan, claim_values: &[Value], stances: &[Vec<Stance>]) -> String {
    let mut blocks = Vec::new();
    for (claim, value) in plan.claims.iter().zip(claim_values) {
        if *value != Value::True {
            continue;
        }
        let mut lines = render_expr(&claim.expr, "", &plan.atoms, stances);
        let files = cite(&claim.expr, stances);
        if !files.is_empty() {
            lines.push(format!("(see {})", files.join(", ")));
        }
        if !lines.is_empty() {
            blocks.push(lines.join("\n"));
        }
    }
    if blocks.is_empty() {
        String::new()
    } else {
        let mut out = blocks.join("\n\n");
        out.push('\n');
        out
    }
}

fn render_expr(
    expr: &Expr,
    prefix: &str,
    atoms: &[String],
    stances: &[Vec<Stance>],
) -> Vec<String> {
    match expr {
        Expr::Atom(index) => {
            let sentence = atoms.get(*index).map(String::as_str).unwrap_or("");
            vec![format!("{prefix}{sentence}")]
        }
        Expr::Not(inner) => render_expr(inner, &format!("{prefix}NOT: "), atoms, stances),
        Expr::And(xs) => render_siblings(xs, prefix, "AND: ", atoms, stances),
        Expr::Or(xs) => render_siblings(xs, prefix, "OR: ", atoms, stances),
    }
}

fn render_siblings(
    exprs: &[Expr],
    prefix: &str,
    op: &str,
    atoms: &[String],
    stances: &[Vec<Stance>],
) -> Vec<String> {
    let Some((first, rest)) = exprs.split_first() else {
        return Vec::new();
    };
    let mut lines = render_expr(first, prefix, atoms, stances);
    for expr in rest {
        lines.extend(render_expr(expr, &format!("{prefix}{op}"), atoms, stances));
    }
    lines
}

fn cite(expr: &Expr, stances: &[Vec<Stance>]) -> Vec<String> {
    let mut files = BTreeSet::new();
    collect(expr, stances, &mut files);
    files.into_iter().collect()
}

fn collect(expr: &Expr, stances: &[Vec<Stance>], files: &mut BTreeSet<String>) {
    match expr {
        Expr::Atom(index) => {
            let Some(rows) = stances.get(*index) else {
                return;
            };
            for stance in rows {
                if matches!(stance.verdict, Verdict::Entailed | Verdict::Contradicted) {
                    files.insert(stance.id.clone());
                }
            }
        }
        Expr::Not(inner) => collect(inner, stances, files),
        Expr::And(xs) | Expr::Or(xs) => {
            for expr in xs {
                collect(expr, stances, files);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;
    use crate::{compile, Plan};

    fn planned(src: &str) -> Plan {
        Plan::of(&compile(&parse(src).unwrap()).unwrap())
    }

    fn stance(id: &str, verdict: Verdict) -> Stance {
        Stance {
            id: id.to_string(),
            verdict,
            confidence: 0.91,
        }
    }

    #[test]
    fn context_path_keeps_the_trace_identity() {
        assert_eq!(
            context_path(Path::new("/tmp/autoground/trace-1790-51022-0.jsonl")),
            PathBuf::from("/tmp/autoground/context-1790-51022-0.md")
        );
    }

    #[test]
    fn a_true_sentence_cites_its_entailed_file() {
        let plan = planned(r#"("Hello")"#);
        let stances = vec![vec![
            stance("src/a.rs", Verdict::Entailed),
            stance("src/noise.rs", Verdict::Neutral),
        ]];
        assert_eq!(
            render(&plan, &[Value::True], &stances),
            "Hello\n(see src/a.rs)\n"
        );
    }

    #[test]
    fn and_and_or_prefix_later_siblings() {
        let plan = planned(
            r#"
            (and ("A") ("B") ("C"))
            (or ("A") ("B"))
            "#,
        );
        let stances = vec![
            vec![stance("b.rs", Verdict::Entailed)],
            vec![stance("a.rs", Verdict::Entailed)],
            vec![stance("c.rs", Verdict::Contradicted)],
        ];
        assert_eq!(
            render(&plan, &[Value::True, Value::True], &stances),
            "\
A
AND: B
AND: C
(see a.rs, b.rs, c.rs)

A
OR: B
(see a.rs, b.rs)
"
        );
    }

    #[test]
    fn not_and_a_negated_conjunct_keep_their_prefixes() {
        let plan = planned(
            r#"
            (not ("gone"))
            (and ("kept") (not ("gone")))
            "#,
        );
        let stances = vec![
            vec![stance("src/log.rs", Verdict::Contradicted)],
            vec![stance("src/kept.rs", Verdict::Entailed)],
        ];
        assert_eq!(
            render(&plan, &[Value::True, Value::True], &stances),
            "\
NOT: gone
(see src/log.rs)

kept
AND: NOT: gone
(see src/kept.rs, src/log.rs)
"
        );
    }

    #[test]
    fn citations_drop_neutrals_and_list_each_path_once_sorted() {
        let plan = planned(r#"(and ("A") ("B"))"#);
        let stances = vec![
            vec![
                stance("src/z.rs", Verdict::Entailed),
                stance("src/m.rs", Verdict::Neutral),
            ],
            vec![
                stance("src/a.rs", Verdict::Entailed),
                stance("src/z.rs", Verdict::Contradicted),
            ],
        ];
        let text = render(&plan, &[Value::True], &stances);
        assert_eq!(
            text,
            "\
A
AND: B
(see src/a.rs, src/z.rs)
"
        );
        assert!(!text.contains("0.91"));
        assert!(!text.contains("src/m.rs"));
    }

    #[test]
    fn a_true_not_with_no_settling_file_has_no_citation() {
        let plan = planned(r#"(not ("there is a GPU compiler"))"#);
        assert_eq!(
            render(&plan, &[Value::True], &[Vec::new()]),
            "NOT: there is a GPU compiler\n"
        );
    }

    #[test]
    fn false_and_unknown_claims_are_omitted() {
        let plan = planned(
            r#"
            ("nope")
            ("unsure")
            "#,
        );
        let stances = vec![
            vec![stance("src/secret.rs", Verdict::Entailed)],
            vec![stance("src/other.rs", Verdict::Contradicted)],
        ];
        assert_eq!(render(&plan, &[Value::False, Value::Unknown], &stances), "");
    }

    #[test]
    fn two_validated_claims_are_separated_by_a_blank_line() {
        let plan = planned(
            r#"
            ("one")
            ("hidden")
            ("two")
            "#,
        );
        let stances = vec![
            vec![stance("a.rs", Verdict::Entailed)],
            vec![stance("secret.rs", Verdict::Entailed)],
            vec![stance("b.rs", Verdict::Entailed)],
        ];
        assert_eq!(
            render(&plan, &[Value::True, Value::False, Value::True], &stances),
            "\
one
(see a.rs)

two
(see b.rs)
"
        );
    }
}
