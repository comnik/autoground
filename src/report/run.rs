//! Annotated brief printed when `autoground check` finishes on a terminal.
//!
//! Each judged clause carries its own mark, and an undischarged clause
//! carries its failure comment. A pipe prints the indented tree instead.

use std::io::{self, stdout, Write};
use std::ops::Range;

use crossterm::{
    queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
};

use crate::check::{Failure, Node, Report};
use crate::syntax::ClauseSpan;
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Source,
    TrueMark,
    FalseMark,
    FalseText,
    UnknownMark,
    UnknownText,
    /// A `;;` comment on a false clause. Red, but not the bold violation mark.
    FalseNote,
    /// A `;; assumed` comment. The sentence was not checked against a file.
    Assumed,
}

#[derive(Clone)]
struct Piece {
    text: String,
    tone: Tone,
}

#[derive(Clone)]
struct Row {
    pieces: Vec<Piece>,
}

impl Row {
    #[cfg(test)]
    fn plain(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect()
    }
}

fn piece(text: impl Into<String>, tone: Tone) -> Piece {
    Piece {
        text: text.into(),
        tone,
    }
}

/// Print the brief. Each judged clause carries its own mark.
pub fn write_brief(source: &str, clauses: &[ClauseSpan], report: &Report) -> io::Result<()> {
    let mut out = stdout();
    for row in brief_rows(source, clauses, report) {
        for piece in &row.pieces {
            if tone_is_bold(piece.tone) {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if let Some(color) = tone_color(piece.tone) {
                queue!(out, SetForegroundColor(color))?;
            }
            queue!(
                out,
                Print(&piece.text),
                ResetColor,
                SetAttribute(Attribute::Reset)
            )?;
        }
        queue!(out, Print("\n"))?;
    }
    out.flush()
}

fn brief_rows(source: &str, clauses: &[ClauseSpan], report: &Report) -> Vec<Row> {
    let paints = paints(clauses, &report.claims);
    let notes = notes(clauses, &report.claims);
    source_rows(source, &paints, &notes)
}

fn tone_color(tone: Tone) -> Option<Color> {
    Some(match tone {
        Tone::TrueMark => Color::DarkGreen,
        Tone::FalseMark | Tone::FalseText | Tone::FalseNote => Color::Red,
        Tone::UnknownMark | Tone::UnknownText | Tone::Assumed => Color::Yellow,
        Tone::Source => return None,
    })
}

fn tone_is_bold(tone: Tone) -> bool {
    matches!(tone, Tone::FalseMark | Tone::FalseText)
}

struct Mark {
    span: Range<usize>,
    value: Value,
}

struct ClaimPaint {
    /// One glyph per clause, on the line that clause opens.
    openers: Vec<Mark>,
    /// False and unknown clauses. A true clause is absent, so it stays plain.
    regions: Vec<Mark>,
}

fn paints(clauses: &[ClauseSpan], claims: &[Node]) -> Vec<ClaimPaint> {
    clauses
        .iter()
        .zip(claims)
        .map(|(clause, node)| {
            let mut openers = Vec::new();
            let mut regions = Vec::new();
            openers_of(node, clause, &mut openers);
            regions_of(node, clause, &mut regions);
            ClaimPaint { openers, regions }
        })
        .collect()
}

/// Every judged clause opens with its own resolution, including the body of a detail.
fn openers_of(node: &Node, clause: &ClauseSpan, out: &mut Vec<Mark>) {
    out.push(Mark {
        span: clause.span.clone(),
        value: node.value,
    });
    if node.children.len() == clause.children.len() && !node.children.is_empty() {
        for (child, span) in node.children.iter().zip(&clause.children) {
            openers_of(child, span, out);
        }
    }
}

/// A compound that fails uniformly is one region, connective included.
/// A mix keeps only the clauses that are false or unknown.
///
/// A witness's fact span is not a check child, so the witness is one region.
fn regions_of(node: &Node, clause: &ClauseSpan, out: &mut Vec<Mark>) {
    if node.value == Value::True {
        return;
    }
    if node.children.len() == clause.children.len() && !node.children.is_empty() {
        let uniform = node.children.iter().all(|child| child.value == node.value);
        if uniform {
            out.push(Mark {
                span: clause.span.clone(),
                value: node.value,
            });
            return;
        }
        for (child, span) in node.children.iter().zip(&clause.children) {
            regions_of(child, span, out);
        }
        return;
    }
    out.push(Mark {
        span: clause.span.clone(),
        value: node.value,
    });
}

struct Note {
    /// Byte offset of the clause this comment sits above.
    at: usize,
    text: String,
    tone: Tone,
}

fn notes(clauses: &[ClauseSpan], claims: &[Node]) -> Vec<Note> {
    let mut out = Vec::new();
    for (clause, node) in clauses.iter().zip(claims) {
        notes_of(node, clause, false, &mut out);
    }
    out
}

/// `parent_holds` means an ancestor is already true, so a false clause inside
/// it is not why the claim failed.
fn notes_of(node: &Node, clause: &ClauseSpan, parent_holds: bool, out: &mut Vec<Note>) {
    if node.assumed {
        out.push(Note {
            at: clause.span.start,
            text: ";; assumed".to_string(),
            tone: Tone::Assumed,
        });
    } else if node.value != Value::True && !parent_holds {
        if let Some(failure) = failure_comment(node) {
            out.push(Note {
                at: clause.span.start,
                text: format!(";; {}", failure.label()),
                tone: note_tone(node.value),
            });
        }
    }
    if node.children.len() == clause.children.len() && !node.children.is_empty() {
        let holds = parent_holds || node.value == Value::True;
        for (child, span) in node.children.iter().zip(&clause.children) {
            notes_of(child, span, holds, out);
        }
    }
}

fn failure_comment(node: &Node) -> Option<&Failure> {
    node.failure.as_ref()
}

fn note_tone(value: Value) -> Tone {
    if value == Value::False {
        Tone::FalseNote
    } else {
        Tone::UnknownMark
    }
}

fn source_rows(source: &str, paints: &[ClaimPaint], notes: &[Note]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (lo, hi) in line_ranges(source) {
        for note in notes.iter().filter(|note| (lo..hi).contains(&note.at)) {
            rows.push(comment_row(source, lo, note));
        }
        rows.push(source_line(source, lo, hi, paints));
    }
    rows
}

fn comment_row(source: &str, line_lo: usize, note: &Note) -> Row {
    let prefix = &source[line_lo..note.at];
    let indent = if prefix.chars().all(|ch| ch.is_whitespace()) {
        prefix.to_string()
    } else {
        " ".repeat(prefix.chars().count())
    };
    Row {
        pieces: vec![
            piece("  ", Tone::Source),
            piece(format!("{indent}{}", note.text), note.tone),
        ],
    }
}

fn line_ranges(source: &str) -> Vec<(usize, usize)> {
    if source.is_empty() {
        return Vec::new();
    }
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let mut end = index;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            ranges.push((start, end));
            start = index + 1;
        }
    }
    if start < source.len() {
        ranges.push((start, source.len()));
    }
    ranges
}

fn source_line(source: &str, lo: usize, hi: usize, paints: &[ClaimPaint]) -> Row {
    let opener = paints
        .iter()
        .flat_map(|paint| paint.openers.iter())
        .filter(|mark| mark.span.start >= lo && mark.span.start < hi)
        .min_by_key(|mark| mark.span.start)
        .map(|mark| mark.value);
    let regions = overlapping(paints, lo, hi);
    let mut pieces = vec![gutter(opener, &regions)];
    pieces.extend(content_pieces(source, lo, hi, &regions));
    Row { pieces }
}

fn overlapping<'a>(paints: &'a [ClaimPaint], lo: usize, hi: usize) -> Vec<&'a Mark> {
    paints
        .iter()
        .flat_map(|paint| paint.regions.iter())
        .filter(|mark| mark.span.start < hi && mark.span.end > lo)
        .collect()
}

fn rank(value: Value) -> u8 {
    match value {
        Value::False => 2,
        Value::Unknown => 1,
        Value::True => 0,
    }
}

/// The clause's own glyph opens its first line. A detail's body does too.
/// Later lines are barred only where a false or unknown clause sits, so a
/// true clause stays plain.
fn gutter(opener: Option<Value>, regions: &[&Mark]) -> Piece {
    if let Some(value) = opener {
        let (mark, tone) = match value {
            Value::False => ('✗', Tone::FalseMark),
            Value::Unknown => ('?', Tone::UnknownMark),
            Value::True => ('✓', Tone::TrueMark),
        };
        return piece(format!("{mark} "), tone);
    }
    let strongest = regions
        .iter()
        .map(|mark| mark.value)
        .max_by_key(|value| rank(*value));
    match strongest {
        Some(Value::False) => piece("▌ ", Tone::FalseMark),
        Some(Value::Unknown) => piece("· ", Tone::UnknownMark),
        _ => piece("  ", Tone::Source),
    }
}

fn content_pieces(source: &str, lo: usize, hi: usize, marks: &[&Mark]) -> Vec<Piece> {
    if lo >= hi {
        return Vec::new();
    }
    let mut cuts = vec![lo, hi];
    for mark in marks {
        if (lo..hi).contains(&mark.span.start) && source.is_char_boundary(mark.span.start) {
            cuts.push(mark.span.start);
        }
        if (lo..hi).contains(&mark.span.end) && source.is_char_boundary(mark.span.end) {
            cuts.push(mark.span.end);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut pieces = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            continue;
        }
        let text = &source[start..end];
        if text.is_empty() {
            continue;
        }
        let tone = marks
            .iter()
            .filter(|mark| mark.span.start <= start && mark.span.end >= end)
            .map(|mark| mark.value)
            .max_by_key(|value| rank(*value))
            .map(text_tone)
            .unwrap_or(Tone::Source);
        pieces.push(piece(text, tone));
    }
    pieces
}

fn text_tone(value: Value) -> Tone {
    match value {
        Value::False => Tone::FalseText,
        Value::Unknown => Tone::UnknownText,
        Value::True => Tone::Source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    fn leaf(text: &str, value: Value, failure: Option<Failure>, cites: &[&str]) -> Node {
        Node {
            text: text.to_string(),
            value,
            failure,
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

    fn detail(head: &str, value: Value, body: Vec<Node>, step: Option<Failure>) -> Node {
        Node {
            text: head.to_string(),
            value,
            failure: step.clone(),
            cites: Vec::new(),
            children: body,
            implication: Some(Box::new(leaf(
                "sentences entail this claim",
                if step.is_none() {
                    Value::True
                } else {
                    Value::Unknown
                },
                step,
                &[],
            ))),
            assumed: false,
        }
    }

    fn finished(src: &str, report: &Report) -> Vec<Row> {
        let program = parse(src).unwrap();
        brief_rows(src, &program.clauses, report)
    }

    #[test]
    fn finished_view_marks_the_brief() {
        let src = "\
(claim (\"search finds the window\") (witness \"src/search.rs\"))
(\"bare sentence\")
(or (\"kept\") (\"dropped\"))
";
        let report = Report {
            claims: vec![
                leaf(
                    "search finds the window",
                    Value::True,
                    None,
                    &["src/search.rs"],
                ),
                leaf(
                    "bare sentence",
                    Value::Unknown,
                    Some(Failure::Unpinned),
                    &[],
                ),
                connective(
                    "or",
                    Value::True,
                    vec![
                        leaf("kept", Value::True, None, &[]),
                        leaf(
                            "dropped",
                            Value::False,
                            Some(Failure::Contradicted),
                            &["gone.rs"],
                        ),
                    ],
                ),
            ],
        };
        let rows = finished(src, &report);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "✓ (claim (\"search finds the window\") (witness \"src/search.rs\"))".to_string(),
                "  ;; unpinned".to_string(),
                "? (\"bare sentence\")".to_string(),
                "✓ (or (\"kept\") (\"dropped\"))".to_string(),
            ]
        );
        assert!(rows[2]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::UnknownText));
        assert!(rows[0]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::TrueMark));
        assert!(text.iter().all(|line| !line.contains("contradicted")));
    }

    #[test]
    fn a_contradicted_witness_is_the_red_clause() {
        let src = "(and (claim (\"nope\") (witness \"gone.rs\")) (claim (\"search finds the window\") (witness \"src/search.rs\")))\n";
        let report = Report {
            claims: vec![connective(
                "and",
                Value::False,
                vec![
                    leaf(
                        "nope",
                        Value::False,
                        Some(Failure::Contradicted),
                        &["gone.rs"],
                    ),
                    leaf(
                        "search finds the window",
                        Value::True,
                        None,
                        &["src/search.rs"],
                    ),
                ],
            )],
        };
        let rows = finished(src, &report);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert!(
            text.iter().any(|line| line.contains(";; contradicted")),
            "{text:?}"
        );
        let claim = rows.last().expect("claim line");
        assert!(claim
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::FalseText && piece.text.contains("nope")));
        assert!(claim.pieces.iter().any(|piece| {
            piece.tone == Tone::Source && piece.text.contains("search finds the window")
        }));
    }

    #[test]
    fn an_implication_failure_comments_the_detail() {
        let src = "\
(claim (\"the head\")
  (claim (\"search finds the window\") (witness \"src/search.rs\")))
";
        let report = Report {
            claims: vec![detail(
                "the head",
                Value::Unknown,
                vec![leaf(
                    "search finds the window",
                    Value::True,
                    None,
                    &["src/search.rs"],
                )],
                Some(Failure::NotEntailed),
            )],
        };
        let rows = finished(src, &report);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(text[0], "  ;; not entailed");
        assert!(text[1].starts_with("? (claim (\"the head\")"), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.starts_with('✓') && line.contains("(witness")),
            "{text:?}"
        );
    }

    #[test]
    fn a_detail_marks_each_body_witness() {
        let src = "\
(claim (\"the head\")
  (and
    (claim (\"search finds the window\") (witness \"src/search.rs\"))
    (claim (\"nope\") (witness \"gone.rs\"))))
";
        let report = Report {
            claims: vec![detail(
                "the head",
                Value::Unknown,
                vec![connective(
                    "and",
                    Value::Unknown,
                    vec![
                        leaf(
                            "search finds the window",
                            Value::True,
                            None,
                            &["src/search.rs"],
                        ),
                        leaf(
                            "nope",
                            Value::Unknown,
                            Some(Failure::NotEntailed),
                            &["gone.rs"],
                        ),
                    ],
                )],
                None,
            )],
        };
        let rows = finished(src, &report);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "? (claim (\"the head\")".to_string(),
                "?   (and".to_string(),
                "✓     (claim (\"search finds the window\") (witness \"src/search.rs\"))"
                    .to_string(),
                "      ;; not entailed".to_string(),
                "?     (claim (\"nope\") (witness \"gone.rs\"))))".to_string(),
            ]
        );
        assert!(rows[4]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::UnknownText && piece.text.contains("nope")));
        assert!(rows[2]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::TrueMark));
    }

    #[test]
    fn an_assumption_is_marked_assumed() {
        let src = "(assume (\"the premise holds\"))\n";
        let report = Report {
            claims: vec![Node {
                text: "the premise holds".to_string(),
                value: Value::True,
                failure: None,
                cites: Vec::new(),
                children: Vec::new(),
                implication: None,
                assumed: true,
            }],
        };
        let rows = finished(src, &report);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert!(
            text.iter().any(|line| line.contains(";; assumed")),
            "{text:?}"
        );
        assert!(rows.iter().any(|row| {
            row.pieces
                .iter()
                .any(|piece| piece.tone == Tone::Assumed && piece.text.contains("assumed"))
        }));
    }
}
