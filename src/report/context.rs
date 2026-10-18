//! Markdown context for a check: the green proof.
//!
//! Written beside the trace as `context.md`. A claim that is not discharged
//! is omitted. A detail is the intent it declares, and the claims it
//! discharged are nested under that line. Witness paths stay on the claim
//! that cited them. A detail that discharged nothing is omitted.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::check::{Node, Report};
use crate::value::Value;

/// `…/trace.jsonl` → `…/context.md`.
pub fn context_path(trace: &Path) -> PathBuf {
    match trace.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("context.md"),
        _ => PathBuf::from("context.md"),
    }
}

/// Write [`render`] to [`context_path`]. An empty validated set still creates the file.
pub fn write_context(trace: &Path, report: &Report) -> io::Result<PathBuf> {
    let path = context_path(trace);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&path, render(report))?;
    Ok(path)
}

/// One block per discharged claim. Blocks are separated by a blank line.
pub fn render(report: &Report) -> String {
    let mut blocks = Vec::new();
    for claim in &report.claims {
        if let Some(block) = block_of(claim) {
            blocks.push(block);
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

fn block_of(node: &Node) -> Option<String> {
    if node.assumed || node.text == "sentences entail this claim" {
        return None;
    }
    if is_scope(node) {
        return scope_block(node);
    }
    if node.implication.is_some() {
        return idb_block(node);
    }
    if node.value != Value::True {
        return None;
    }
    let mut lines = render_node(node, "");
    let files = cites(node);
    if !files.is_empty() {
        lines.push(format!("(see {})", files.join(", ")));
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// A detail. Check names it with its intent, or `detail` when it has none.
fn is_scope(node: &Node) -> bool {
    node.implication.is_none()
        && !node.children.is_empty()
        && !matches!(node.text.as_str(), "and" | "or" | "not")
}

fn scope_block(node: &Node) -> Option<String> {
    let mut parts = Vec::new();
    for child in &node.children {
        if let Some(part) = block_of(child) {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return None;
    }
    let body = parts.join("\n\n");
    if node.text == "detail" {
        Some(body)
    } else {
        Some(format!("{}\n{}", node.text, indent(&body)))
    }
}

/// The claim sentence, then the support it discharged. A use of a lemma is
/// not copied here; the lemma's own block carries it.
fn idb_block(node: &Node) -> Option<String> {
    let mut nested = Vec::new();
    for child in &node.children {
        if is_bare_use(child) {
            continue;
        }
        if let Some(part) = block_of(child) {
            nested.push(part);
        }
    }
    if node.value != Value::True {
        if nested.is_empty() {
            None
        } else {
            Some(nested.join("\n\n"))
        }
    } else if nested.is_empty() {
        let mut lines = vec![node.text.clone()];
        let files = cites(node);
        if !files.is_empty() {
            lines.push(format!("(see {})", files.join(", ")));
        }
        Some(lines.join("\n"))
    } else {
        Some(format!("{}\n{}", node.text, indent(&nested.join("\n\n"))))
    }
}

fn is_bare_use(node: &Node) -> bool {
    node.value == Value::True
        && node.cites.is_empty()
        && node.children.is_empty()
        && node.implication.is_none()
        && !node.assumed
        && !matches!(node.text.as_str(), "and" | "or" | "not")
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_node(node: &Node, prefix: &str) -> Vec<String> {
    if node.text == "and" && !node.children.is_empty() {
        return render_siblings(&node.children, prefix, "AND: ");
    }
    if node.text == "or" && !node.children.is_empty() {
        return render_siblings(&node.children, prefix, "OR: ");
    }
    if node.text == "not" {
        let Some(inner) = node.children.first() else {
            return vec![format!("{prefix}NOT:")];
        };
        return render_node(inner, &format!("{prefix}NOT: "));
    }
    // A detail is its head. The body stays in the cites.
    vec![format!("{prefix}{}", node.text)]
}

fn render_siblings(nodes: &[Node], prefix: &str, op: &str) -> Vec<String> {
    let Some((first, rest)) = nodes.split_first() else {
        return Vec::new();
    };
    let mut lines = render_node(first, prefix);
    for node in rest {
        lines.extend(render_node(node, &format!("{prefix}{op}")));
    }
    lines
}

fn cites(node: &Node) -> Vec<String> {
    let mut files = BTreeSet::new();
    collect_cites(node, &mut files);
    files.into_iter().collect()
}

fn collect_cites(node: &Node, files: &mut BTreeSet<String>) {
    for cite in &node.cites {
        files.insert(cite.clone());
    }
    for child in &node.children {
        collect_cites(child, files);
    }
    if let Some(step) = &node.implication {
        collect_cites(step, files);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Failure;

    fn leaf(text: &str, value: Value, cites: &[&str]) -> Node {
        Node {
            text: text.to_string(),
            value,
            failure: None,
            cites: cites.iter().map(|cite| (*cite).to_string()).collect(),
            children: Vec::new(),
            implication: None,
            assumed: false,
        }
    }

    fn connective(text: &str, value: Value, children: Vec<Node>) -> Node {
        Node {
            text: text.to_string(),
            value,
            failure: None,
            cites: Vec::new(),
            children,
            implication: None,
            assumed: false,
        }
    }

    fn scope(intent: &str, value: Value, body: Vec<Node>) -> Node {
        Node {
            text: intent.to_string(),
            value,
            failure: None,
            cites: Vec::new(),
            children: body,
            implication: None,
            assumed: false,
        }
    }

    #[test]
    fn context_sits_beside_the_trace() {
        assert_eq!(
            context_path(Path::new("/tmp/autoground/1790-51022-0/trace.jsonl")),
            PathBuf::from("/tmp/autoground/1790-51022-0/context.md")
        );
    }

    #[test]
    fn a_true_claim_nests_under_the_detail_intent() {
        let report = Report {
            claims: vec![
                scope(
                    "why it holds",
                    Value::False,
                    vec![
                        leaf("Local holds", Value::True, &["src/a.rs"]),
                        leaf("nope", Value::False, &["src/secret.rs"]),
                    ],
                ),
                scope(
                    "why the middle holds",
                    Value::True,
                    vec![scope(
                        "inner",
                        Value::True,
                        vec![leaf("leaf", Value::True, &["src/b.rs"])],
                    )],
                ),
                scope(
                    "empty",
                    Value::True,
                    vec![leaf("hidden", Value::False, &[])],
                ),
                leaf("other", Value::True, &["src/c.rs"]),
            ],
        };
        assert_eq!(
            render(&report),
            "\
why it holds
  Local holds
  (see src/a.rs)

why the middle holds
  inner
    leaf
    (see src/b.rs)

other
(see src/c.rs)
"
        );
    }

    #[test]
    fn a_true_and_records_a_lemma_as_its_head() {
        let report = Report {
            claims: vec![
                connective(
                    "and",
                    Value::True,
                    vec![
                        leaf("other", Value::True, &["src/other.py"]),
                        leaf("arrow", Value::True, &[]),
                    ],
                ),
                leaf(
                    "arrow",
                    Value::True,
                    &["reasoners/prescriptive_reasoner.py"],
                ),
            ],
        };
        assert_eq!(
            render(&report),
            "\
other
AND: arrow
(see src/other.py)

arrow
(see reasoners/prescriptive_reasoner.py)
"
        );
    }

    #[test]
    fn a_true_sentence_cites_its_file() {
        let report = Report {
            claims: vec![leaf("Hello", Value::True, &["src/a.rs"])],
        };
        assert_eq!(render(&report), "Hello\n(see src/a.rs)\n");
    }

    #[test]
    fn and_and_or_prefix_later_siblings() {
        let report = Report {
            claims: vec![
                connective(
                    "and",
                    Value::True,
                    vec![
                        leaf("A", Value::True, &["b.rs"]),
                        leaf("B", Value::True, &["a.rs"]),
                        leaf("C", Value::True, &["c.rs"]),
                    ],
                ),
                connective(
                    "or",
                    Value::True,
                    vec![
                        leaf("A", Value::True, &["b.rs"]),
                        leaf("B", Value::True, &["a.rs"]),
                    ],
                ),
            ],
        };
        assert_eq!(
            render(&report),
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
        let report = Report {
            claims: vec![
                connective(
                    "not",
                    Value::True,
                    vec![leaf("gone", Value::False, &["src/log.rs"])],
                ),
                connective(
                    "and",
                    Value::True,
                    vec![
                        leaf("kept", Value::True, &["src/kept.rs"]),
                        connective(
                            "not",
                            Value::True,
                            vec![leaf("gone", Value::False, &["src/log.rs"])],
                        ),
                    ],
                ),
            ],
        };
        assert_eq!(
            render(&report),
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
    fn citations_list_each_path_once_sorted() {
        let report = Report {
            claims: vec![connective(
                "and",
                Value::True,
                vec![
                    leaf("A", Value::True, &["src/z.rs"]),
                    leaf("B", Value::True, &["src/a.rs", "src/z.rs"]),
                ],
            )],
        };
        assert_eq!(
            render(&report),
            "\
A
AND: B
(see src/a.rs, src/z.rs)
"
        );
    }

    #[test]
    fn a_true_not_with_no_file_has_no_citation() {
        let report = Report {
            claims: vec![connective(
                "not",
                Value::True,
                vec![leaf("there is a GPU compiler", Value::False, &[])],
            )],
        };
        assert_eq!(render(&report), "NOT: there is a GPU compiler\n");
    }

    #[test]
    fn false_and_unknown_claims_are_omitted() {
        let mut nope = leaf("nope", Value::False, &["src/secret.rs"]);
        nope.failure = Some(Failure::Contradicted);
        let mut unsure = leaf("unsure", Value::Unknown, &["src/other.rs"]);
        unsure.failure = Some(Failure::NotEntailed);
        let report = Report {
            claims: vec![nope, unsure],
        };
        assert_eq!(render(&report), "");
    }

    #[test]
    fn two_validated_claims_are_separated_by_a_blank_line() {
        let report = Report {
            claims: vec![
                leaf("one", Value::True, &["a.rs"]),
                leaf("hidden", Value::False, &["secret.rs"]),
                leaf("two", Value::True, &["b.rs"]),
            ],
        };
        assert_eq!(
            render(&report),
            "\
one
(see a.rs)

two
(see b.rs)
"
        );
    }
}
