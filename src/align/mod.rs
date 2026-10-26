//! Whether two briefs state the same names, senses, and sentences.
//!
//! A witness path is a file, so it is not a sentence. A name or sense that
//! arrives through `:refer` is declared in the other file. `and` and `or`
//! contribute their sentences.

mod mermaid;

use std::collections::BTreeSet;

use crate::compiler::Compiled;
use crate::syntax::template::quote_const;
use crate::syntax::{Form, Program, Proposition, Support};

pub use mermaid::{lower_mermaid, LowerError};

/// One side of an alignment, after the brief has compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    names: BTreeSet<String>,
    senses: BTreeSet<(String, String)>,
    /// Nullary senses used as terms. A diagram node of the same spelling
    /// resolves to one of these, so the picture keeps the bare label.
    terms: BTreeSet<(String, String)>,
    statements: BTreeSet<Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Statement {
    sentence: String,
    negated: bool,
}

impl Statement {
    fn show(&self) -> String {
        if self.negated {
            format!("not {}", self.sentence)
        } else {
            self.sentence.clone()
        }
    }
}

/// Names, senses, and statements declared by `program`. Sentences are the
/// filled readings in `compiled`.
pub fn model_of(program: &Program, compiled: &Compiled) -> Model {
    let mut names = BTreeSet::new();
    let mut senses = BTreeSet::new();
    let mut terms = BTreeSet::new();
    declarations(
        &program.forms,
        &compiled.terms,
        &mut names,
        &mut senses,
        &mut terms,
    );
    let mut statements = BTreeSet::new();
    for claim in &compiled.claims {
        walk(claim, false, &mut statements);
    }
    Model {
        names,
        senses,
        terms,
        statements,
    }
}

fn declarations(
    forms: &[Form],
    term_senses: &BTreeSet<(String, String)>,
    names: &mut BTreeSet<String>,
    senses: &mut BTreeSet<(String, String)>,
    terms: &mut BTreeSet<(String, String)>,
) {
    for form in forms {
        match form {
            Form::Name(name) => {
                names.insert(name.clone());
            }
            Form::Sense(sense) => {
                let pair = (sense.name.clone(), sense.template.clone());
                if term_senses.contains(&pair) {
                    terms.insert(pair);
                } else {
                    senses.insert(pair);
                }
            }
            Form::Detail(body) => declarations(body, term_senses, names, senses, terms),
            Form::Assume(_) | Form::Formula(_) => {}
        }
    }
}

fn walk(prop: &Proposition, negated: bool, out: &mut BTreeSet<Statement>) {
    match prop {
        Proposition::And(xs) | Proposition::Or(xs) => {
            for prop in xs {
                walk(prop, negated, out);
            }
        }
        Proposition::Not(inner) => walk(inner, !negated, out),
        Proposition::Sentence(sentence) | Proposition::Assume(sentence) => {
            out.insert(stated(&sentence.text, negated));
        }
        Proposition::Claim { sentence, support } => {
            out.insert(stated(&sentence.text, negated));
            if let Support::Sentence(prop) = support {
                walk(prop, negated, out);
            }
        }
        Proposition::Scope { body, .. } => {
            for prop in body {
                walk(prop, negated, out);
            }
        }
    }
}

fn stated(sentence: &str, negated: bool) -> Statement {
    Statement {
        sentence: sentence.to_string(),
        negated,
    }
}

/// The lines to print when the two models differ. [`None`] when they align.
pub fn mismatch(left: &Model, right: &Model) -> Option<String> {
    let mut sections = Vec::new();
    if let Some(text) = section(
        "names",
        &names_only(left, right),
        &names_only(right, left),
        String::clone,
    ) {
        sections.push(text);
    }
    if let Some(text) = section(
        "senses",
        &senses_only(left, right),
        &senses_only(right, left),
        |(name, template)| format!("{name} {}", quote_const(template)),
    ) {
        sections.push(text);
    }
    if let Some(text) = section(
        "statements",
        &only(&left.statements, &right.statements),
        &only(&right.statements, &left.statements),
        Statement::show,
    ) {
        sections.push(text);
    }
    if sections.is_empty() {
        None
    } else {
        Some(format!("{}\n", sections.join("\n")))
    }
}

fn only<T: Clone + Ord>(left: &BTreeSet<T>, right: &BTreeSet<T>) -> Vec<T> {
    left.difference(right).cloned().collect()
}

/// A name is covered when the other side declares it, or uses a term of that spelling.
fn names_only(left: &Model, right: &Model) -> Vec<String> {
    left.names
        .iter()
        .filter(|name| !right.names.contains(*name) && !has_term(right, name))
        .cloned()
        .collect()
}

/// A sense template differs, and a term differs unless the other side names that spelling.
fn senses_only(left: &Model, right: &Model) -> Vec<(String, String)> {
    let mut out = only(&left.senses, &right.senses);
    out.extend(
        left.terms
            .iter()
            .filter(|term| !term_covered(term, right))
            .cloned(),
    );
    out.sort();
    out
}

fn has_term(model: &Model, spelling: &str) -> bool {
    model.terms.iter().any(|(name, _)| name == spelling)
}

fn term_covered(term: &(String, String), other: &Model) -> bool {
    other.terms.contains(term) || other.names.contains(&term.0)
}

fn section<T>(title: &str, left: &[T], right: &[T], show: impl Fn(&T) -> String) -> Option<String> {
    if left.is_empty() && right.is_empty() {
        return None;
    }
    let mut out = String::from(title);
    out.push('\n');
    side("only in left", left, &show, &mut out);
    side("only in right", right, &show, &mut out);
    Some(out.trim_end().to_string())
}

fn side<T>(label: &str, items: &[T], show: &impl Fn(&T) -> String, out: &mut String) {
    if items.is_empty() {
        return;
    }
    out.push_str("  ");
    out.push_str(label);
    out.push('\n');
    for item in items {
        out.push_str("    ");
        out.push_str(&show(item));
        out.push('\n');
    }
}

#[cfg(test)]
pub(crate) fn model_from(src: &str) -> Model {
    let program = crate::parse(src).unwrap_or_else(|err| panic!("{err}\n{src}"));
    let compiled = crate::compile(&program).unwrap_or_else(|err| panic!("{err}\n{src}"));
    model_of(&program, &compiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRONT: &str = r#"
(name Frontend)
(name Backend)
(sense depends_on "{X} depends on {Y}")
(assume (depends_on Frontend Backend))
"#;

    #[test]
    fn a_brief_aligns_with_itself() {
        let model = model_from(FRONT);
        assert!(mismatch(&model, &model).is_none());
    }

    #[test]
    fn a_markdown_fence_aligns_with_its_brief() {
        let markdown = "\
See the model.

```brief
(name Frontend)
(assume \"Frontend serves pages\")
```

The rest is prose.
";
        let src = crate::brief_source(markdown).unwrap();
        let left = model_from("(name Frontend)\n(assume \"Frontend serves pages\")\n");
        assert!(mismatch(&left, &model_from(&src)).is_none());
    }

    #[test]
    fn a_difference_is_named_in_its_direction() {
        let left = model_from(
            r#"
(name Frontend)
(sense depends_on "{X} depends on {Y}")
(claim "Frontend serves pages" (witness "web.rs"))
"#,
        );
        let right = model_from(
            r#"
(name Frontend)
(name Cache)
(sense depends_on "{X} calls {Y}")
(claim "Cache stores pages" (witness "cache.rs"))
"#,
        );
        assert_eq!(
            mismatch(&left, &right).unwrap(),
            "\
names
  only in right
    Cache
senses
  only in left
    depends_on \"{X} depends on {Y}\"
  only in right
    depends_on \"{X} calls {Y}\"
statements
  only in left
    Frontend serves pages
  only in right
    Cache stores pages
"
        );
    }

    #[test]
    fn a_witness_path_is_not_a_statement() {
        let left = model_from("(claim \"alpha\" (witness \"note.txt\"))\n");
        let right = model_from("(claim \"alpha\" (witness \"other.txt\"))\n");
        assert!(mismatch(&left, &right).is_none());
    }

    #[test]
    fn and_and_or_contribute_the_same_sentences() {
        let and = model_from("(claim \"h\" (and \"alpha\" \"beta\"))\n");
        let or = model_from("(claim \"h\" (or \"alpha\" \"beta\"))\n");
        assert!(mismatch(&and, &or).is_none());
    }

    #[test]
    fn a_second_not_restores_the_sentence() {
        let once = model_from("(claim \"h\" (not \"alpha\"))\n");
        let twice = model_from("(claim \"h\" (not (not \"alpha\")))\n");
        let positive = model_from("(claim \"h\" \"alpha\")\n");
        assert!(mismatch(&twice, &positive).is_none());
        let diff = mismatch(&once, &positive).unwrap();
        assert!(diff.contains("only in left\n    not alpha\n"), "{diff}");
        assert!(diff.contains("only in right\n    alpha\n"), "{diff}");
    }

    #[test]
    fn a_name_declared_in_a_detail_counts() {
        let left = model_from(
            r#"
(name Frontend)
(detail
  (name Cache)
  (assume "Cache stores pages"))
"#,
        );
        let right = model_from("(name Frontend)\n(assume \"Cache stores pages\")\n");
        let diff = mismatch(&left, &right).unwrap();
        assert_eq!(
            diff,
            "\
names
  only in left
    Cache
"
        );
    }

    #[test]
    fn an_imported_name_is_not_a_declaration_of_this_file() {
        let lib =
            crate::parse("(ns lib)\n(name Shared)\n(sense knows \"{X} knows {Y}\")\n").unwrap();
        let entry = crate::parse(
            "(ns app (:require [lib :refer :all]))\n(name Local)\n(assume (knows Local Shared))\n",
        )
        .unwrap();
        let files = vec![crate::compiler::Loaded {
            path: "lib.brief".to_string(),
            program: lib,
        }];
        let (compiled, _) = crate::compiler::link_parts(&entry, Some("app.brief"), &files).unwrap();
        let model = model_of(&entry, &compiled);
        assert!(model.names.contains("Local"));
        assert!(!model.names.contains("Shared"));
        assert!(model.senses.is_empty());
        assert!(model
            .statements
            .iter()
            .any(|statement| statement.sentence == "Local knows Shared"));
    }

    #[test]
    fn two_term_templates_do_not_align() {
        let left = model_from(
            r#"
(name R)
(sense T "the tables named in output_table_names")
(sense creates "{a} creates {b}")
(assume (creates R (T)))
"#,
        );
        let right = model_from(
            r#"
(name R)
(sense T "the destination tables")
(sense creates "{a} creates {b}")
(assume (creates R (T)))
"#,
        );
        let diff = mismatch(&left, &right).unwrap();
        assert!(
            diff.contains("the tables named in output_table_names"),
            "{diff}"
        );
        assert!(diff.contains("the destination tables"), "{diff}");
        assert!(!diff.contains("names\n"), "{diff}");
    }
}
