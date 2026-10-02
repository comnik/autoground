//! Live view of `autoground run` on a terminal.
//!
//! The view watches the shared log. Search steps and read steps move each
//! claim between planning, search, and read. The working frame stays inside
//! the terminal: each line stops short of the last column, and the list keeps
//! the last row free, following the claim being read when the model is taller
//! than the screen. A frame that fills the width wraps, and a frame that fills
//! the height scrolls; the next move-up then starts below the previous frame,
//! so every step prints another copy. When the run finishes, the view is the
//! model file, printed once: a true claim carries a quiet mark, and a false
//! claim carries a red mark on the clauses that failed. Just above a false
//! clause is a comment naming the file that contradicted it, or noting that
//! no file supported it. `--explain all` adds the same kind of comment to
//! true clauses. Piped output never draws this.

use std::collections::HashMap;
use std::io::{self, stdout, Write};
use std::ops::Range;
use std::sync::{Arc, Mutex};

use crossterm::{
    cursor, execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use serde_json::Value as Json;

use crate::plan::{Expr, Plan};
use crate::syntax::ClauseSpan;
use crate::value::Value;

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Planning,
    Search,
    Read,
    Done,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Planning => "planning",
            Phase::Search => "search",
            Phase::Read => "read",
            Phase::Done => "done",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Planning,
    Search,
    Read,
    Rule,
    Source,
    TrueMark,
    FalseMark,
    FalseText,
    UnknownMark,
    UnknownText,
    /// A `;;` comment on a false clause. Red, but not the bold violation mark.
    FalseNote,
    /// A `;;` comment on a true clause.
    TrueNote,
}

struct ClaimWatch {
    verbalized: String,
    phase: Phase,
    detail: String,
    tick: usize,
    active: bool,
}

enum Mode {
    Banner(Phase),
    Working,
    Done,
}

struct RunView {
    mode: Mode,
    phase: Phase,
    detail: String,
    tick: usize,
    claims: Vec<ClaimWatch>,
    /// Sentence → claims that mention it.
    atom_claims: HashMap<String, Vec<usize>>,
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

impl RunView {
    fn planning() -> Self {
        Self {
            mode: Mode::Banner(Phase::Planning),
            phase: Phase::Planning,
            detail: String::new(),
            tick: 0,
            claims: Vec::new(),
            atom_claims: HashMap::new(),
            done: Vec::new(),
        }
    }

    fn show_plan(&mut self, plan: &Plan) {
        self.claims = plan
            .claims
            .iter()
            .map(|claim| ClaimWatch {
                verbalized: claim.verbalized.clone(),
                phase: Phase::Search,
                detail: String::new(),
                tick: 0,
                active: true,
            })
            .collect();
        self.atom_claims = index_atoms(plan);
        self.mode = Mode::Working;
        self.phase = Phase::Search;
        self.detail.clear();
    }

    fn on_event(&mut self, event: &Json) {
        if self.phase == Phase::Done {
            return;
        }
        match event.get("kind").and_then(|kind| kind.as_str()) {
            Some("search_step") => self.on_search(event),
            Some("read_step") => self.on_read(event),
            _ => {}
        }
    }

    fn on_search(&mut self, event: &Json) {
        self.phase = Phase::Search;
        self.detail = search_detail(event);
        self.tick = self.tick.wrapping_add(1);
        for claim in &mut self.claims {
            if claim.phase == Phase::Planning || claim.phase == Phase::Search {
                claim.phase = Phase::Search;
                claim.active = true;
                claim.tick = self.tick;
            }
        }
        if !matches!(self.mode, Mode::Working) {
            self.mode = Mode::Working;
        }
    }

    fn on_read(&mut self, event: &Json) {
        self.phase = Phase::Read;
        let path = event
            .get("path")
            .and_then(|path| path.as_str())
            .unwrap_or("");
        self.detail = if path.is_empty() {
            "escalation".to_string()
        } else {
            path.to_string()
        };
        self.tick = self.tick.wrapping_add(1);
        for claim in &mut self.claims {
            if claim.phase != Phase::Done {
                claim.phase = Phase::Read;
                claim.active = false;
            }
        }
        let proposition = event
            .get("proposition")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if let Some(indexes) = self.atom_claims.get(proposition) {
            for &index in indexes {
                let claim = &mut self.claims[index];
                claim.detail = self.detail.clone();
                claim.active = true;
                claim.tick = self.tick;
            }
        }
        self.mode = Mode::Working;
    }

    fn finish(
        &mut self,
        source: &str,
        clauses: &[ClauseSpan],
        plan: &Plan,
        atom_values: &[Value],
        grounds: &[Ground],
        explain: Explain,
    ) {
        self.phase = Phase::Done;
        self.detail.clear();
        self.mode = Mode::Done;
        let paints = paints(clauses, plan, atom_values);
        let notes = notes(clauses, plan, atom_values, grounds, explain);
        self.done = source_rows(source, &paints, &notes);
    }

    #[cfg(test)]
    fn rows(&self, width: usize) -> Vec<Row> {
        self.frame(width.max(20), usize::MAX)
    }

    /// `max_rows` bounds the working list. The finished model ignores it and
    /// is printed once, so a long file may scroll.
    fn frame(&self, width: usize, max_rows: usize) -> Vec<Row> {
        let width = width.max(1);
        match self.mode {
            Mode::Banner(phase) => vec![status_row(phase, "", "", width)],
            Mode::Working => self.working_rows(width, max_rows.max(1)),
            Mode::Done => self.done.clone(),
        }
    }

    fn focus(&self) -> usize {
        if self.phase == Phase::Read {
            self.claims
                .iter()
                .position(|claim| claim.active)
                .unwrap_or(0)
        } else {
            0
        }
    }

    fn working_rows(&self, width: usize, max_rows: usize) -> Vec<Row> {
        let total = self.claims.len();
        let slots = max_rows.saturating_sub(1);
        let (start, shown) = if slots == 0 || total == 0 {
            (0, 0)
        } else {
            window_bounds(total, slots, self.focus())
        };
        let place = place_label(start, shown, total);
        let mut rows: Vec<_> = self.claims[start..start + shown]
            .iter()
            .map(|claim| claim_row(claim, width))
            .collect();
        rows.push(status_row(self.phase, &self.detail, &place, width));
        rows
    }
}

/// Columns and rows a working frame may occupy. The finished model is not
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

fn window_bounds(total: usize, slots: usize, focus: usize) -> (usize, usize) {
    if total <= slots {
        return (0, total);
    }
    let focus = focus.min(total - 1);
    let start = focus.saturating_sub(slots / 2).min(total - slots);
    (start, slots)
}

fn place_label(start: usize, shown: usize, total: usize) -> String {
    if shown >= total {
        String::new()
    } else if shown == 0 {
        format!("0/{total}")
    } else {
        format!("{}-{}/{}", start + 1, start + shown, total)
    }
}

fn claim_row(claim: &ClaimWatch, width: usize) -> Row {
    let mark = if claim.active {
        SPIN[claim.tick % SPIN.len()]
    } else {
        "·"
    };
    let phase = claim.phase.label();
    let left = format!("  {mark}  {phase:<8}  ");
    let left_len = left.chars().count();
    let text = if width <= left_len {
        fit(&left, width)
    } else {
        let room = width - left_len;
        let detail = claim.detail.as_str();
        if detail.is_empty() || room <= detail.chars().count() + 2 {
            format!("{left}{}", fit(&claim.verbalized, room))
        } else {
            let claim_room = room - detail.chars().count() - 2;
            let claim_text = fit(&claim.verbalized, claim_room);
            let pad = room - claim_text.chars().count() - detail.chars().count();
            format!("{left}{claim_text}{}{detail}", " ".repeat(pad))
        }
    };
    row_one(text, tone_of_phase(claim.phase))
}

fn status_row(phase: Phase, detail: &str, place: &str, width: usize) -> Row {
    let width = width.max(1);
    let label = format!(" {} ", phase.label());
    let label_len = label.chars().count();
    if label_len >= width {
        return row_one(fit(&label, width), Tone::Rule);
    }
    let mut place = if place.is_empty() {
        String::new()
    } else {
        format!(" {place} ")
    };
    if label_len + place.chars().count() > width {
        place = fit(&place, width - label_len);
    }
    let place_len = place.chars().count();
    let mut tail = if detail.is_empty() {
        String::new()
    } else {
        format!(" {detail} ")
    };
    let budget = width - label_len - place_len;
    if tail.chars().count() > budget {
        tail = fit(&tail, budget);
    }
    let fill = budget - tail.chars().count();
    row_one(
        format!("{label}{}{tail}{place}", "─".repeat(fill)),
        Tone::Rule,
    )
}

fn tone_of_phase(phase: Phase) -> Tone {
    match phase {
        Phase::Planning => Tone::Planning,
        Phase::Search => Tone::Search,
        Phase::Read => Tone::Read,
        Phase::Done => Tone::Rule,
    }
}

fn tone_color(tone: Tone) -> Option<Color> {
    Some(match tone {
        Tone::Planning | Tone::Rule => Color::DarkGrey,
        Tone::Search => Color::Cyan,
        Tone::Read => Color::Blue,
        Tone::TrueMark => Color::DarkGreen,
        Tone::FalseMark | Tone::FalseText | Tone::FalseNote => Color::Red,
        Tone::TrueNote => Color::DarkGreen,
        Tone::UnknownMark | Tone::UnknownText => Color::Yellow,
        Tone::Source => return None,
    })
}

fn tone_is_bold(tone: Tone) -> bool {
    matches!(tone, Tone::FalseMark | Tone::FalseText)
}

/// How much of a finished claim to annotate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Explain {
    /// Comment false clauses only.
    Contradictions,
    /// Also comment true clauses with the file that supported them.
    All,
}

/// What the reader found for one atomic sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ground {
    Entailed(Vec<String>),
    Contradicted(Vec<String>),
    /// False because the window held no witness.
    Unsupported,
    Unknown,
}

fn search_detail(event: &Json) -> String {
    event
        .get("classifications")
        .and_then(|value| value.as_array())
        .and_then(|items| {
            items.iter().rev().find_map(|item| {
                let id = item.get("id")?.as_str()?;
                if id.is_empty() {
                    None
                } else {
                    Some(id.to_string())
                }
            })
        })
        .unwrap_or_default()
}

fn index_atoms(plan: &Plan) -> HashMap<String, Vec<usize>> {
    let mut map: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, claim) in plan.claims.iter().enumerate() {
        let mut atoms = Vec::new();
        collect(&claim.expr, &mut atoms);
        for atom in atoms {
            let Some(sentence) = plan.atoms.get(atom) else {
                continue;
            };
            let claims = map.entry(sentence.clone()).or_default();
            if !claims.contains(&index) {
                claims.push(index);
            }
        }
    }
    map
}

fn collect(expr: &Expr, atoms: &mut Vec<usize>) {
    match expr {
        Expr::Atom(index) => atoms.push(*index),
        Expr::And(xs) | Expr::Or(xs) | Expr::Detail { body: xs, .. } => {
            for expr in xs {
                collect(expr, atoms);
            }
        }
        Expr::Not(inner) => collect(inner, atoms),
    }
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

struct Mark {
    span: Range<usize>,
    value: Value,
}

struct ClaimPaint {
    span: Range<usize>,
    value: Value,
    /// False and unknown clauses. A true clause is absent, so it stays plain.
    regions: Vec<Mark>,
}

fn paints(clauses: &[ClauseSpan], plan: &Plan, atoms: &[Value]) -> Vec<ClaimPaint> {
    clauses
        .iter()
        .zip(plan.claims.iter())
        .map(|(clause, claim)| {
            let value = claim.expr.value(atoms);
            let mut regions = Vec::new();
            regions_of(&claim.expr, clause, atoms, &mut regions);
            if regions.is_empty() && value != Value::True {
                regions.push(Mark {
                    span: clause.span.clone(),
                    value,
                });
            }
            ClaimPaint {
                span: clause.span.clone(),
                value,
                regions,
            }
        })
        .collect()
}

/// A compound that fails uniformly is one region, connective included.
/// A mix keeps only the clauses that are false or unknown.
fn regions_of(expr: &Expr, clause: &ClauseSpan, atoms: &[Value], out: &mut Vec<Mark>) {
    let value = expr.value(atoms);
    if value == Value::True {
        return;
    }
    match expr {
        Expr::Atom(_) | Expr::Not(_) => out.push(Mark {
            span: clause.span.clone(),
            value,
        }),
        Expr::And(xs) | Expr::Or(xs) | Expr::Detail { body: xs, .. } => {
            if xs.len() != clause.children.len() {
                out.push(Mark {
                    span: clause.span.clone(),
                    value,
                });
                return;
            }
            let uniform = xs.iter().all(|child| child.value(atoms) == value);
            if uniform {
                out.push(Mark {
                    span: clause.span.clone(),
                    value,
                });
                return;
            }
            for (child, span) in xs.iter().zip(&clause.children) {
                regions_of(child, span, atoms, out);
            }
        }
    }
}

struct Note {
    /// Byte offset of the clause this comment sits above.
    at: usize,
    text: String,
    tone: Tone,
}

fn notes(
    clauses: &[ClauseSpan],
    plan: &Plan,
    atoms: &[Value],
    grounds: &[Ground],
    explain: Explain,
) -> Vec<Note> {
    if grounds.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (clause, claim) in clauses.iter().zip(plan.claims.iter()) {
        notes_of(
            &claim.expr,
            clause,
            atoms,
            grounds,
            explain,
            false,
            &mut out,
        );
    }
    out
}

/// `parent_holds` means an ancestor compound is already true, so a false
/// clause inside it is not why the claim failed.
fn notes_of(
    expr: &Expr,
    clause: &ClauseSpan,
    atoms: &[Value],
    grounds: &[Ground],
    explain: Explain,
    parent_holds: bool,
    out: &mut Vec<Note>,
) {
    let value = expr.value(atoms);
    match expr {
        Expr::And(xs) | Expr::Or(xs) => {
            if xs.len() != clause.children.len() {
                return;
            }
            let holds = parent_holds || value == Value::True;
            for (child, span) in xs.iter().zip(&clause.children) {
                notes_of(child, span, atoms, grounds, explain, holds, out);
            }
        }
        Expr::Detail { body, .. } => {
            if body.len() == clause.children.len() {
                let holds = parent_holds || value == Value::True;
                for (child, span) in body.iter().zip(&clause.children) {
                    notes_of(child, span, atoms, grounds, explain, holds, out);
                }
                return;
            }
            // A use of the sentence. The file evidence stays on the detail's body.
            if parent_holds && value == Value::False {
                return;
            }
            if !shown(value, explain) {
                return;
            }
            let text = match value {
                Value::True => ";; entailed via detail",
                Value::False => ";; fails via detail",
                Value::Unknown => return,
            };
            out.push(Note {
                at: clause.span.start,
                text: text.to_string(),
                tone: note_tone(value),
            });
        }
        Expr::Not(inner) => {
            if parent_holds && value == Value::False {
                return;
            }
            if !shown(value, explain) {
                return;
            }
            if let Some(text) = not_text(value, inner, grounds) {
                out.push(Note {
                    at: clause.span.start,
                    text,
                    tone: note_tone(value),
                });
            }
        }
        Expr::Atom(index) => {
            if parent_holds && value == Value::False {
                return;
            }
            let ground = grounds.get(*index);
            if !shown(value, explain) {
                return;
            }
            if let Some(text) = atom_text(value, ground) {
                out.push(Note {
                    at: clause.span.start,
                    text,
                    tone: note_tone(value),
                });
            }
        }
    }
}

fn shown(value: Value, explain: Explain) -> bool {
    match explain {
        Explain::Contradictions => value == Value::False,
        Explain::All => value == Value::True || value == Value::False,
    }
}

fn note_tone(value: Value) -> Tone {
    if value == Value::False {
        Tone::FalseNote
    } else {
        Tone::TrueNote
    }
}

fn atom_text(value: Value, ground: Option<&Ground>) -> Option<String> {
    match (value, ground) {
        (Value::False, Some(Ground::Contradicted(paths))) if !paths.is_empty() => {
            Some(format!(";; contradicted in {}", join_paths(paths)))
        }
        (Value::False, _) => Some(";; no supporting file".to_string()),
        (Value::True, Some(Ground::Entailed(paths))) if !paths.is_empty() => {
            Some(format!(";; entailed in {}", join_paths(paths)))
        }
        _ => None,
    }
}

fn not_text(value: Value, inner: &Expr, grounds: &[Ground]) -> Option<String> {
    let mut entailed = Vec::new();
    let mut contradicted = Vec::new();
    let mut unsupported = false;
    gather(
        inner,
        grounds,
        &mut entailed,
        &mut contradicted,
        &mut unsupported,
    );
    match value {
        Value::False if !entailed.is_empty() => {
            Some(format!(";; entailed in {}", join_paths(&entailed)))
        }
        Value::False => Some(";; no supporting file".to_string()),
        Value::True if !contradicted.is_empty() => {
            Some(format!(";; contradicted in {}", join_paths(&contradicted)))
        }
        Value::True if unsupported => Some(";; no supporting file".to_string()),
        _ => None,
    }
}

fn gather(
    expr: &Expr,
    grounds: &[Ground],
    entailed: &mut Vec<String>,
    contradicted: &mut Vec<String>,
    unsupported: &mut bool,
) {
    match expr {
        Expr::Atom(index) => match grounds.get(*index) {
            Some(Ground::Entailed(paths)) => entailed.extend(paths.clone()),
            Some(Ground::Contradicted(paths)) => contradicted.extend(paths.clone()),
            Some(Ground::Unsupported) => *unsupported = true,
            Some(Ground::Unknown) | None => {}
        },
        Expr::Not(inner) => gather(inner, grounds, entailed, contradicted, unsupported),
        Expr::And(xs) | Expr::Or(xs) | Expr::Detail { body: xs, .. } => {
            for child in xs {
                gather(child, grounds, entailed, contradicted, unsupported);
            }
        }
    }
}

fn join_paths(paths: &[String]) -> String {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.contains(path) {
            unique.push(path.clone());
        }
    }
    unique.join(", ")
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
        .filter(|paint| paint.span.start >= lo && paint.span.start < hi)
        .max_by_key(|paint| rank(paint.value))
        .map(|paint| paint.value);
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

/// The claim's own glyph opens its first line. Later lines are barred only
/// where a false or unknown clause sits, so a true clause stays plain.
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
    view: RunView,
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

/// In-place claim list. Dropping it shows the cursor again.
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
                view: RunView::planning(),
                drawn: 0,
            })),
            hidden: Arc::new(Mutex::new(true)),
        })
    }

    pub fn show_planning(&self) -> io::Result<()> {
        let mut body = self.lock();
        body.view = RunView::planning();
        body.draw()
    }

    pub fn show_plan(&self, plan: &Plan) -> io::Result<()> {
        let mut body = self.lock();
        body.view.show_plan(plan);
        body.draw()
    }

    /// Apply one shared-log event and redraw. Draw errors are ignored so a
    /// display glitch does not fail the run.
    pub fn on_event(&self, event: &Json) {
        let kind = event.get("kind").and_then(|kind| kind.as_str());
        if !matches!(kind, Some("search_step" | "read_step")) {
            return;
        }
        let mut body = self.lock();
        body.view.on_event(event);
        let _ = body.draw();
    }

    pub fn finish(
        &self,
        source: &str,
        clauses: &[ClauseSpan],
        plan: &Plan,
        atom_values: &[Value],
        grounds: &[Ground],
        explain: Explain,
    ) -> io::Result<()> {
        let mut body = self.lock();
        body.view
            .finish(source, clauses, plan, atom_values, grounds, explain);
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
    use crate::compile;
    use crate::syntax::parse;

    const SAMPLE: &str = r#"
(and ("Frontend is part of the system") (not ("there is a GPU compiler")))
(not ("there is a GPU compiler"))
"#;

    fn sample() -> Plan {
        Plan::of(&compile(&parse(SAMPLE).unwrap()).unwrap())
    }

    #[test]
    fn planning_is_a_status_bar() {
        let view = RunView::planning();
        let lines = view
            .rows(40)
            .into_iter()
            .map(|row| row.plain())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("planning"), "{}", lines[0]);
        assert!(lines[0].contains('─'), "{}", lines[0]);
    }

    #[test]
    fn search_and_read_events_move_each_claim() {
        let plan = sample();
        let mut view = RunView::planning();
        view.show_plan(&plan);
        let searching: Vec<_> = view.rows(80).into_iter().map(|row| row.plain()).collect();
        assert!(searching[0].contains("search"), "{}", searching[0]);
        assert!(
            searching[0].contains("Frontend is part of the system"),
            "{}",
            searching[0]
        );
        assert!(
            searching[1].contains("there is a GPU compiler"),
            "{}",
            searching[1]
        );
        assert!(searching[2].contains("search"), "{}", searching[2]);

        view.on_event(&serde_json::json!({
            "kind": "search_step",
            "classifications": [{"id": "frontend", "signal": 0.9, "applied": "keep"}]
        }));
        let searched: Vec<_> = view.rows(80).into_iter().map(|row| row.plain()).collect();
        assert!(
            searched[0].contains('⠋') || searched[0].contains('⠙'),
            "{}",
            searched[0]
        );
        assert!(
            searched.last().unwrap().contains("frontend"),
            "{searched:?}"
        );

        view.on_event(&serde_json::json!({
            "kind": "read_step",
            "proposition": "there is a GPU compiler",
            "path": "decoy/noise.py",
            "verdict": "neutral"
        }));
        let reading: Vec<_> = view.rows(100).into_iter().map(|row| row.plain()).collect();
        assert!(reading[0].contains("read"), "{}", reading[0]);
        assert!(reading[0].contains("decoy/noise.py"), "{}", reading[0]);
        assert!(reading[1].contains("read"), "{}", reading[1]);
        assert!(reading[1].contains("decoy/noise.py"), "{}", reading[1]);
        assert!(reading[2].contains("read"), "{}", reading[2]);
        assert!(reading[2].contains("decoy/noise.py"), "{}", reading[2]);

        let program = parse(SAMPLE).unwrap();
        let plan = Plan::of(&compile(&program).unwrap());
        // The sentence is false, so the inner `not` holds and the `and` fails on its first clause.
        view.finish(
            SAMPLE,
            &program.clauses,
            &plan,
            &[Value::False, Value::False],
            &[],
            Explain::Contradictions,
        );
        let done = view.rows(160);
        let text: Vec<_> = done.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "  ".to_string(),
                "✗ (and (\"Frontend is part of the system\") (not (\"there is a GPU compiler\")))"
                    .to_string(),
                "✓ (not (\"there is a GPU compiler\"))".to_string(),
            ]
        );
        assert!(done[1]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::FalseText));
        assert!(done[2]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::TrueMark));
        assert!(done[2]
            .pieces
            .iter()
            .all(|piece| piece.tone != Tone::FalseText));
    }

    #[test]
    fn a_working_frame_stays_inside_the_terminal() {
        assert_eq!(viewport(120, 40, false), (119, 39));
        assert_eq!(viewport(120, 40, true), (119, usize::MAX));
        assert_eq!(viewport(1, 1, false), (1, 1));

        let src: String = (0..30)
            .map(|i| format!("(\"{i} is recorded\")\n"))
            .collect();
        let program = parse(&src).unwrap();
        let plan = Plan::of(&compile(&program).unwrap());
        let mut view = RunView::planning();
        view.show_plan(&plan);

        let full = view.rows(80);
        assert_eq!(full.len(), 31);
        assert!(!full.last().unwrap().plain().contains('/'));

        view.on_event(&serde_json::json!({
            "kind": "search_step",
            "classifications": [{"id": "src", "signal": 0.9, "applied": "keep"}]
        }));
        let searching = view.frame(60, 8);
        assert_eq!(searching.len(), 8);
        assert!(searching[0].plain().contains("\"0 is recorded\""));
        assert!(searching[6].plain().contains("\"6 is recorded\""));
        assert!(searching[7].plain().contains("1-7/30"));
        assert!(searching
            .iter()
            .all(|row| row.plain().chars().count() <= 60));

        view.on_event(&serde_json::json!({
            "kind": "read_step",
            "proposition": "20 is recorded",
            "path": "src/store.py",
            "verdict": "entailed"
        }));
        let reading = view.frame(40, 8);
        assert_eq!(reading.len(), 8);
        assert!(
            reading
                .iter()
                .any(|row| row.plain().contains("\"20 is") && row.plain().contains("src/store.py")),
            "{:?}",
            reading.iter().map(|row| row.plain()).collect::<Vec<_>>()
        );
        assert!(
            reading[7].plain().contains("18-24/30"),
            "{}",
            reading[7].plain()
        );
        assert!(reading.iter().all(|row| row.plain().chars().count() <= 40));

        let narrow = view.frame(8, 3);
        assert!(narrow.iter().all(|row| row.plain().chars().count() <= 8));

        view.finish(
            &src,
            &program.clauses,
            &plan,
            &vec![Value::True; 30],
            &[],
            Explain::Contradictions,
        );
        assert!(view.frame(40, 8).len() > 8);
    }

    #[test]
    fn finished_view_is_the_source_file() {
        let src = include_str!("../../examples/diagram.lect");
        let rows = finished(
            src,
            &[
                Value::True,
                Value::True,
                Value::True,
                Value::False,
                Value::False,
                Value::False,
                Value::False,
            ],
        );
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert!(text.iter().any(|line| line == "  (name Frontend)"));
        assert!(text
            .iter()
            .any(|line| line == "✓ (depends_on Frontend Backend)"));
        assert!(text
            .iter()
            .any(|line| line == "✗ (not (depends_on Backend Frontend))"));
        assert!(text.iter().any(|line| line == "✓ (or"));
        assert!(text
            .iter()
            .any(|line| line == "    (calls Oracle TypeSafe)"));
        let and_at = text
            .iter()
            .position(|line| line == "✗ (and")
            .expect("and claim");
        assert_eq!(
            text[and_at + 1],
            "▌   (\"Frontend is a component of the system\")"
        );
        assert!(text[and_at + 2].starts_with("▌   "));
        assert!(text[and_at + 2].contains("\"{Name} is a component of the system\" Backend"));
        assert!(text.iter().all(|line| !line.contains("true     ")));
        assert!(rows[and_at]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::FalseText));
        let depends = rows
            .iter()
            .find(|row| row.plain().contains("depends_on Frontend Backend"))
            .unwrap();
        assert!(depends
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::TrueMark));
        assert!(depends
            .pieces
            .iter()
            .all(|piece| piece.tone != Tone::FalseText));
    }

    #[test]
    fn a_false_claim_marks_only_the_clauses_that_failed() {
        let src = r#"(name Oracle)
(name Universe)
(name Search)
(name Log)
(sense calls "{caller} calls {callee}")

(and
  ("All major components record structured events in the unified log.")
  (calls Oracle Log)
  (calls Universe Log)
  (calls Search Log))
"#;
        let rows = finished(
            src,
            &[Value::False, Value::False, Value::False, Value::True],
        );
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        let and_at = text.iter().position(|line| line == "✗ (and").unwrap();
        assert_eq!(
            text[and_at + 1],
            "▌   (\"All major components record structured events in the unified log.\")"
        );
        assert_eq!(text[and_at + 2], "▌   (calls Oracle Log)");
        assert_eq!(text[and_at + 3], "▌   (calls Universe Log)");
        assert_eq!(text[and_at + 4], "    (calls Search Log))");
        assert!(rows[and_at]
            .pieces
            .iter()
            .all(|piece| piece.tone != Tone::FalseText));
        assert!(rows[and_at + 2]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::FalseText));
        assert!(rows[and_at + 4]
            .pieces
            .iter()
            .all(|piece| piece.tone != Tone::FalseText && piece.tone != Tone::FalseMark));

        let nested = "\
(and
  (not (\"Search calls Log\"))
  (\"kept\"))
";
        let rows = finished(nested, &[Value::True, Value::True]);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "✗ (and".to_string(),
                "▌   (not (\"Search calls Log\"))".to_string(),
                "    (\"kept\"))".to_string(),
            ]
        );
        assert!(rows[1]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::FalseText));
        assert!(rows[2]
            .pieces
            .iter()
            .all(|piece| piece.tone != Tone::FalseText));

        let mixed = "\
(and
  (\"a\")
  (\"b\"))
";
        let rows = finished(mixed, &[Value::False, Value::Unknown]);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "✗ (and".to_string(),
                "▌   (\"a\")".to_string(),
                "·   (\"b\"))".to_string(),
            ]
        );
        assert!(rows[2]
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::UnknownText));
        assert!(rows[2]
            .pieces
            .iter()
            .all(|piece| piece.tone != Tone::FalseText));

        let held = "\
(or
  (\"a\")
  (\"b\"))
";
        let rows = finished(held, &[Value::False, Value::True]);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "✓ (or".to_string(),
                "    (\"a\")".to_string(),
                "    (\"b\"))".to_string(),
            ]
        );
        assert!(rows
            .iter()
            .all(|row| row.pieces.iter().all(|piece| piece.tone != Tone::FalseText)));
    }

    #[test]
    fn a_use_of_a_detailed_sentence_cites_the_body() {
        let src = "\
(and
  (\"broad\")
  (\"arrow\"))
(detail (\"arrow\")
  (\"leaf\"))
";
        let rows = explained(
            src,
            &[Value::False, Value::True],
            &[
                Ground::Contradicted(vec!["multi_reasoner_validation.py".into()]),
                Ground::Entailed(vec!["reasoners/prescriptive_reasoner.py".into()]),
            ],
            Explain::All,
        );
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "✗ (and".to_string(),
                "    ;; contradicted in multi_reasoner_validation.py".to_string(),
                "▌   (\"broad\")".to_string(),
                "    ;; entailed via detail".to_string(),
                "    (\"arrow\"))".to_string(),
                "✓ (detail (\"arrow\")".to_string(),
                "    ;; entailed in reasoners/prescriptive_reasoner.py".to_string(),
                "    (\"leaf\"))".to_string(),
            ]
        );
        assert!(text.iter().all(|line| !line.contains("no supporting file")));
    }

    fn finished(src: &str, atoms: &[Value]) -> Vec<Row> {
        explained(src, atoms, &[], Explain::Contradictions)
    }

    fn explained(src: &str, atoms: &[Value], grounds: &[Ground], explain: Explain) -> Vec<Row> {
        let program = parse(src).unwrap();
        let plan = Plan::of(&compile(&program).unwrap());
        let painted = paints(&program.clauses, &plan, atoms);
        let notes = notes(&program.clauses, &plan, atoms, grounds, explain);
        source_rows(src, &painted, &notes)
    }

    #[test]
    fn a_false_clause_cites_the_file_that_contradicted_it() {
        let src = "\
(and
  (\"All major components record structured events in the unified log.\")
  (calls Oracle Log)
  (calls Universe Log)
  (calls Search Log))
";
        let src = &format!(
        "(name Oracle)\n(name Universe)\n(name Search)\n(name Log)\n(sense calls \"{{caller}} calls {{callee}}\")\n\n{src}"
    );
        let grounds = [
            Ground::Entailed(vec!["README.md".into()]),
            Ground::Entailed(vec!["src/oracle/mod.rs".into()]),
            Ground::Contradicted(vec!["src/universe/mod.rs".into()]),
            Ground::Entailed(vec!["src/main.rs".into()]),
        ];
        let values = [Value::True, Value::True, Value::False, Value::True];
        let rows = explained(src, &values, &grounds, Explain::Contradictions);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        let and_at = text.iter().position(|line| line == "✗ (and").unwrap();
        assert_eq!(
            text[and_at..],
            [
                "✗ (and".to_string(),
                "    (\"All major components record structured events in the unified log.\")"
                    .to_string(),
                "    (calls Oracle Log)".to_string(),
                "    ;; contradicted in src/universe/mod.rs".to_string(),
                "▌   (calls Universe Log)".to_string(),
                "    (calls Search Log))".to_string(),
            ]
        );
        let comment = &rows[and_at + 3];
        assert!(comment
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::FalseNote));
        assert!(comment
            .pieces
            .iter()
            .all(|piece| piece.tone != Tone::FalseText && !tone_is_bold(piece.tone)));

        let rows = explained(src, &values, &grounds, Explain::All);
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert!(text
            .iter()
            .any(|line| line == "    ;; entailed in src/main.rs"));
        assert!(text
            .iter()
            .any(|line| line == "    ;; contradicted in src/universe/mod.rs"));
        let support = rows
            .iter()
            .find(|row| row.plain().contains("entailed in src/main.rs"))
            .unwrap();
        assert!(support
            .pieces
            .iter()
            .any(|piece| piece.tone == Tone::TrueNote));

        let held = "\
(or
  (\"a\")
  (\"b\"))
";
        let rows = explained(
            held,
            &[Value::False, Value::True],
            &[
                Ground::Contradicted(vec!["decoy.rs".into()]),
                Ground::Entailed(vec!["src/main.rs".into()]),
            ],
            Explain::Contradictions,
        );
        assert!(rows.iter().all(|row| !row.plain().contains(";;")));
        let rows = explained(
            held,
            &[Value::False, Value::True],
            &[
                Ground::Contradicted(vec!["decoy.rs".into()]),
                Ground::Entailed(vec!["src/main.rs".into()]),
            ],
            Explain::All,
        );
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "✓ (or".to_string(),
                "    (\"a\")".to_string(),
                "    ;; entailed in src/main.rs".to_string(),
                "    (\"b\"))".to_string(),
            ]
        );

        let negated = "(not (\"Search calls Log\"))\n";
        let rows = explained(
            negated,
            &[Value::True],
            &[Ground::Entailed(vec!["src/main.rs".into()])],
            Explain::Contradictions,
        );
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "  ;; entailed in src/main.rs".to_string(),
                "✗ (not (\"Search calls Log\"))".to_string(),
            ]
        );

        let detailed = "\
(detail (\"broad\")
  (\"nope\")
  (\"yep\"))
";
        let rows = explained(
            detailed,
            &[Value::False, Value::True],
            &[
                Ground::Unsupported,
                Ground::Entailed(vec!["src/yep.rs".into()]),
            ],
            Explain::Contradictions,
        );
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "✗ (detail (\"broad\")".to_string(),
                "    ;; no supporting file".to_string(),
                "▌   (\"nope\")".to_string(),
                "    (\"yep\"))".to_string(),
            ]
        );

        let absent = "(\"nope\")\n";
        let rows = explained(
            absent,
            &[Value::False],
            &[Ground::Unsupported],
            Explain::Contradictions,
        );
        let text: Vec<_> = rows.iter().map(|row| row.plain()).collect();
        assert_eq!(
            text,
            vec![
                "  ;; no supporting file".to_string(),
                "✗ (\"nope\")".to_string(),
            ]
        );
    }
}
