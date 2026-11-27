use std::path::Path;
use std::time::Duration;

use serde_json::{json, Map, Value as Json};

use crate::check::{Failure, Node, Report};
use crate::oracle::{Usage, UsageRecord};
use crate::syntax::Program;
use crate::Value;

pub fn verdict(claims: &[Node]) -> Value {
    claims
        .iter()
        .fold(Value::True, |acc, claim| acc.and(claim.value))
}

pub fn check_report(
    src: &str,
    program: &Program,
    report: &Report,
    local_claim_count: usize,
    assumptions: &[String],
    usage: &Usage,
    elapsed: Duration,
    trace: &Path,
) -> Json {
    let local_claims: Vec<Json> = report.claims[..local_claim_count]
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let source = program
                .clauses
                .get(i)
                .and_then(|clause| src.get(clause.span.clone()))
                .map(str::trim);
            node_json(node, source)
        })
        .collect();
    let extra_claims: Vec<Json> = report.claims[local_claim_count..]
        .iter()
        .map(|node| node_json(node, None))
        .collect();

    let mut out = Map::new();
    out.insert("verdict".into(), json!(verdict(&report.claims)));
    out.insert("claims".into(), Json::Array(local_claims));
    if !extra_claims.is_empty() {
        out.insert("extra_claims".into(), Json::Array(extra_claims));
    }
    out.insert("assumptions".into(), json!(assumptions));
    out.insert("trace".into(), json!(trace.display().to_string()));
    out.insert("duration_ms".into(), json!(elapsed.as_millis() as u64));
    out.insert("usage".into(), json!(UsageRecord::from_usage(usage)));
    Json::Object(out)
}

pub fn against_failure(missing_certifies: &[String], unaccepted_assumptions: &[String]) -> Json {
    json!({
        "verdict": Value::False,
        "missing_certifies": missing_certifies,
        "unaccepted_assumptions": unaccepted_assumptions,
    })
}

pub enum DirEntry {
    Checked {
        path: String,
        src: String,
        program: Program,
        report: Report,
        assumptions: Vec<String>,
    },
    Failed {
        path: String,
        error: String,
    },
}

pub fn dir_report(
    entries: &[DirEntry],
    usage: Option<&Usage>,
    elapsed: Duration,
    trace: Option<&Path>,
) -> Json {
    let mut has_error = false;
    let mut overall = Value::True;
    let files: Vec<Json> = entries
        .iter()
        .map(|entry| match entry {
            DirEntry::Failed { path, error } => {
                has_error = true;
                json!({
                    "path": path,
                    "error": error,
                })
            }
            DirEntry::Checked {
                path,
                src,
                program,
                report,
                assumptions,
            } => {
                let file_verdict = verdict(&report.claims);
                overall = overall.and(file_verdict);
                let claims: Vec<Json> = report
                    .claims
                    .iter()
                    .enumerate()
                    .map(|(i, node)| {
                        let source = program
                            .clauses
                            .get(i)
                            .and_then(|clause| src.get(clause.span.clone()))
                            .map(str::trim);
                        node_json(node, source)
                    })
                    .collect();
                json!({
                    "path": path,
                    "verdict": file_verdict,
                    "claims": claims,
                    "assumptions": assumptions,
                })
            }
        })
        .collect();

    let mut out = Map::new();
    out.insert(
        "verdict".into(),
        if has_error {
            json!("error")
        } else {
            json!(overall)
        },
    );
    out.insert("files".into(), Json::Array(files));
    if let Some(trace) = trace {
        out.insert("trace".into(), json!(trace.display().to_string()));
    }
    out.insert("duration_ms".into(), json!(elapsed.as_millis() as u64));
    if let Some(usage) = usage {
        out.insert("usage".into(), json!(UsageRecord::from_usage(usage)));
    }
    Json::Object(out)
}

fn node_json(node: &Node, source: Option<&str>) -> Json {
    let mut out = Map::new();
    out.insert("text".into(), json!(node.text));
    if let Some(source) = source {
        out.insert("source".into(), json!(source));
    }
    out.insert("value".into(), json!(node.value));
    if node.assumed {
        out.insert("assumed".into(), json!(true));
    }
    if let Some(failure) = &node.failure {
        out.insert("failure".into(), failure_json(failure));
    }
    if !node.cites.is_empty() {
        out.insert("cites".into(), json!(node.cites));
    }
    if !node.children.is_empty() {
        out.insert(
            "children".into(),
            Json::Array(node.children.iter().map(|c| node_json(c, None)).collect()),
        );
    }
    if let Some(implication) = &node.implication {
        out.insert("implication".into(), node_json(implication, None));
    }
    Json::Object(out)
}

fn failure_json(failure: &Failure) -> Json {
    match failure {
        Failure::Unpinned => json!({ "kind": "unpinned", "label": failure.label() }),
        Failure::MissingFile { path } => {
            json!({ "kind": "missing_file", "path": path, "label": failure.label() })
        }
        Failure::TooLarge => json!({ "kind": "too_large", "label": failure.label() }),
        Failure::NotEntailed => json!({ "kind": "not_entailed", "label": failure.label() }),
        Failure::Contradicted => json!({ "kind": "contradicted", "label": failure.label() }),
        Failure::BriefWitness => json!({ "kind": "brief_witness", "label": failure.label() }),
        Failure::UnsupportedImage => {
            json!({ "kind": "unsupported_image", "label": failure.label() })
        }
        Failure::ImageTooSmall => json!({ "kind": "image_too_small", "label": failure.label() }),
        Failure::ImageTooLarge => json!({ "kind": "image_too_large", "label": failure.label() }),
        Failure::CertificateFailed { cert } => {
            json!({ "kind": "certificate_failed", "cert": cert, "label": failure.label() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    #[test]
    fn check_report_serializes_verdict_failures_and_assumptions() {
        let src = "(claim \"a\" (witness \"f.txt\"))\n(claim \"b\" (witness \"g.txt\"))\n";
        let program = parse(src).unwrap();
        let report = Report {
            claims: vec![
                Node {
                    text: "a".into(),
                    value: Value::True,
                    failure: None,
                    cites: vec!["f.txt".into()],
                    children: Vec::new(),
                    implication: None,
                    assumed: false,
                },
                Node {
                    text: "b".into(),
                    value: Value::Unknown,
                    failure: Some(Failure::CertificateFailed {
                        cert: "certify:acme/pagerank".into(),
                    }),
                    cites: vec!["g.txt".into()],
                    children: Vec::new(),
                    implication: None,
                    assumed: false,
                },
            ],
        };
        let doc = check_report(
            src,
            &program,
            &report,
            2,
            &["run came from gather-context.py".into()],
            &Usage::default(),
            Duration::from_millis(42),
            Path::new("/tmp/autoground/1/trace.jsonl"),
        );
        assert_eq!(doc["verdict"], "unknown");
        assert_eq!(doc["duration_ms"], 42);
        assert_eq!(doc["trace"], "/tmp/autoground/1/trace.jsonl");
        assert_eq!(doc["assumptions"], json!(["run came from gather-context.py"]));
        assert_eq!(doc["claims"][0]["value"], "true");
        assert_eq!(doc["claims"][0]["source"], "(claim \"a\" (witness \"f.txt\"))");
        assert!(doc["claims"][0].get("failure").is_none());
        assert_eq!(doc["claims"][1]["failure"]["kind"], "certificate_failed");
        assert_eq!(doc["claims"][1]["failure"]["cert"], "certify:acme/pagerank");

        // False dominates Unknown under Kleene conjunction.
        let mut false_report = report;
        false_report.claims[0].value = Value::False;
        assert_eq!(verdict(&false_report.claims), Value::False);
    }

    #[test]
    fn against_failure_serializes_missing_and_unaccepted() {
        let doc = against_failure(&["hv".into()], &["shortcut".into()]);
        assert_eq!(doc["verdict"], "false");
        assert_eq!(doc["missing_certifies"], json!(["hv"]));
        assert_eq!(doc["unaccepted_assumptions"], json!(["shortcut"]));
    }
}
