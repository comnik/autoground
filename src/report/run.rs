//! Live view of `autoground check` on a terminal.
//!
//! While requests are in flight, one status line names the sentence being
//! judged. The working line stops short of the last column and leaves the
//! last row free. When the check finishes, the view is the brief, printed
//! once: each judged clause carries its own mark, and an undischarged clause
//! carries its failure comment. Piped output never draws this.

use std::io::{self, stdout, Write};
use std::ops::Range;
use std::sync::{Arc, Mutex};

use crossterm::{
    cursor, execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use serde_json::Value as Json;

use crate::check::{Failure, Node, Report};
use crate::syntax::ClauseSpan;
use crate::value::Value;

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Rule,
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

enum Mode {
    Checking,
    Done,
}

struct CheckView {
    mode: Mode,
    sentence: String,
    tick: usize,
    done: Vec<Row>,
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

fn row_one(text: impl Into<String>, tone: Tone) -> Row {
    Row {
        pieces: vec![piece(text, tone)],
    }
}

impl CheckView {
    fn checking() -> Self {
        Self {
            mode: Mode::Checking,
            sentence: String::new(),
            tick: 0,
            done: Vec::new(),
        }
    }

    fn on_event(&mut self, event: &Json) {
        if !matches!(self.mode, Mode::Checking) {
            return;
        }
        if event.get("kind").and_then(|kind| kind.as_str()) != Some("exchange") {
            return;
        }
        let sentence = sentence_of(event);
        if sentence.is_empty() {
            return;
        }
        self.sentence = sentence;
        self.tick = self.tick.wrapping_add(1);
    }

    fn finish(&mut self, source: &str, clauses: &[ClauseSpan], report: &Report) {
        self.mode = Mode::Done;
        self.sentence.clear();
        let paints = paints(clauses, &report.claims);
        let notes = notes(clauses, &report.claims);
        self.done = source_rows(source, &paints, &notes);
    }

    #[cfg(test)]
    fn rows(&self, width: usize) -> Vec<Row> {
        self.frame(width.max(20), usize::MAX)
    }

    /// `max_rows` bounds the working line. The finished brief ignores it and
    /// is printed once, so a long file may scroll.
    fn frame(&self, width: usize, _max_rows: usize) -> Vec<Row> {
        let width = width.max(1);
        match self.mode {
            Mode::Checking => vec![status_row(&self.sentence, self.tick, width)],
            Mode::Done => self.done.clone(),
        }
    }
}

/// Columns and rows a working frame may occupy. The finished brief is not
/// limited: it is drawn once.
fn viewport(cols: usize, rows: usize, done: bool) -> (usize, usize) {
    let width = cols.saturating_sub(1).max(1);
    let max_rows = if done {
        usize::MAX
    } else {
        rows.saturating_sub(1).max(1)
    };
    (width, max_rows)
}

fn status_row(sentence: &str, tick: usize, width: usize) -> Row {
    let width = width.max(1);
    let spin = SPIN[tick % SPIN.len()];
    let label = format!(" {spin} checking ");
    let label_len = label.chars().count();
    if label_len >= width {
        return row_one(fit(&label, width), Tone::Rule);
    }
    let mut tail = if sentence.is_empty() {
        String::new()
    } else {
        format!(" {sentence} ")
    };
    let budget = width - label_len;
    if tail.chars().count() > budget {
        tail = fit(&tail, budget);
    }
    let fill = budget - tail.chars().count();
    row_one(format!("{label}{}{tail}", "─".repeat(fill)), Tone::Rule)
}

fn sentence_of(event: &Json) -> String {
    let Some(questions) = event
        .get("request")
        .and_then(|request| request.get("questions"))
        .and_then(|questions| questions.as_object())
    else {
        return String::new();
    };
    let mut sentences = Vec::new();
    for question in questions.values() {
        let Some(text) = question
            .get("instructions")
            .and_then(|instructions| instructions.get("proposition"))
            .and_then(|proposition| proposition.as_str())
        else {
            continue;
        };
        if !text.is_empty() && !sentences.iter().any(|seen| seen == text) {
            sentences.push(text.to_string());
        }
    }
    sentences.join(" · ")
}

fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

fn tone_color(tone: Tone) -> Option<Color> {
    Some(match tone {
        Tone::Rule => Color::DarkGrey,
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

struct Body {
    view: CheckView,
    drawn: u16,
}

impl Body {
    fn draw(&mut self) -> io::Result<()> {
        let (cols, term_rows) = terminal::size()
            .map(|(w, h)| (w as usize, h as usize))
            .unwrap_or((80, 24));
        let done = matches!(self.view.mode, Mode::Done);
        let (width, max_rows) = viewport(cols, term_rows, done);
        let rows = self.view.frame(width, max_rows);
        let mut out = stdout();
        if self.drawn > 0 {
            queue!(out, cursor::MoveToColumn(0), cursor::MoveUp(self.drawn))?;
        }
        for row in &rows {
            queue!(out, Clear(ClearType::CurrentLine))?;
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
            queue!(out, Print("\r\n"))?;
        }
        let extra = (self.drawn as usize).saturating_sub(rows.len());
        for _ in 0..extra {
            queue!(out, Clear(ClearType::CurrentLine), Print("\r\n"))?;
        }
        if extra > 0 {
            queue!(out, cursor::MoveUp(extra as u16))?;
        }
        out.flush()?;
        self.drawn = rows.len() as u16;
        Ok(())
    }
}

/// In-place status line. Dropping it shows the cursor again.
#[derive(Clone)]
pub struct Live {
    body: Arc<Mutex<Body>>,
    hidden: Arc<Mutex<bool>>,
}

impl Live {
    pub fn open() -> io::Result<Self> {
        execute!(stdout(), cursor::Hide)?;
        Ok(Self {
            body: Arc::new(Mutex::new(Body {
                view: CheckView::checking(),
                drawn: 0,
            })),
            hidden: Arc::new(Mutex::new(true)),
        })
    }

    pub fn show_checking(&self) -> io::Result<()> {
        let mut body = self.lock();
        body.view = CheckView::checking();
        body.draw()
    }

    /// Apply one shared-log event and redraw. Draw errors are ignored so a
    /// display glitch does not fail the check.
    pub fn on_event(&self, event: &Json) {
        if event.get("kind").and_then(|kind| kind.as_str()) != Some("exchange") {
            return;
        }
        let mut body = self.lock();
        body.view.on_event(event);
        let _ = body.draw();
    }

    pub fn finish(&self, source: &str, clauses: &[ClauseSpan], report: &Report) -> io::Result<()> {
        let mut body = self.lock();
        body.view.finish(source, clauses, report);
        body.draw()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Body> {
        self.body
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // The log hook holds one clone. Only the last clone restores the cursor,
        // so clearing the hook mid-run does not show it early.
        if Arc::strong_count(&self.body) > 1 {
            return;
        }
        let mut hidden = self
            .hidden
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *hidden {
            let _ = execute!(stdout(), cursor::Show);
            *hidden = false;
        }
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
        let mut view = CheckView::checking();
        view.finish(src, &program.clauses, report);
        view.rows(160)
    }

    #[test]
    fn checking_names_the_sentence() {
        let mut view = CheckView::checking();
        let idle = view.rows(40);
        assert_eq!(idle.len(), 1);
        assert!(idle[0].plain().contains("checking"), "{}", idle[0].plain());
        assert!(idle[0].plain().contains('─'), "{}", idle[0].plain());

        view.on_event(&serde_json::json!({
            "kind": "search_step",
            "proposition": "ignored"
        }));
        assert!(view.rows(80)[0].plain().contains("checking"));
        assert!(!view.rows(80)[0].plain().contains("ignored"));

        view.on_event(&serde_json::json!({
            "kind": "exchange",
            "request": {
                "questions": {
                    "q1": {"instructions": {"proposition": "Search calls Oracle"}},
                    "q2": {"instructions": {"proposition": "Search calls Oracle"}}
                }
            }
        }));
        let named = view.rows(80)[0].plain();
        assert!(named.contains('⠙') || named.contains('⠹'), "{named}");
        assert!(named.contains("Search calls Oracle"), "{named}");
        assert!(!named.contains(" · "), "{named}");
    }

    #[test]
    fn a_working_line_stays_inside_the_terminal() {
        assert_eq!(viewport(120, 40, false), (119, 39));
        assert_eq!(viewport(120, 40, true), (119, usize::MAX));
        assert_eq!(viewport(1, 1, false), (1, 1));

        let mut view = CheckView::checking();
        view.on_event(&serde_json::json!({
            "kind": "exchange",
            "request": {
                "questions": {
                    "q1": {"instructions": {"proposition": "Search calls Oracle and the sentence is long"}}
                }
            }
        }));
        let row = view.frame(24, 8);
        assert_eq!(row.len(), 1);
        assert!(row[0].plain().chars().count() <= 24, "{}", row[0].plain());
        assert!(row[0].plain().contains("checking"), "{}", row[0].plain());
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
