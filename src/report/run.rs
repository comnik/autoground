//! Live view of `autoground run` on a terminal.
//!
//! The view watches the shared log. Search steps and read steps move each
//! claim between planning, search, and read. Piped output never draws this.

use std::collections::HashMap;
use std::io::{self, stdout, Write};
use std::sync::{Arc, Mutex};

use crossterm::{
    cursor, execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use serde_json::Value as Json;

use crate::plan::{Expr, Plan};
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
    True,
    False,
    Unknown,
    Rule,
}

struct ClaimWatch {
    verbalized: String,
    phase: Phase,
    detail: String,
    tick: usize,
    active: bool,
    value: Option<Value>,
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
}

struct Row {
    text: String,
    tone: Tone,
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
                value: None,
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

    fn finish(&mut self, values: &[Value]) {
        self.phase = Phase::Done;
        self.detail.clear();
        self.mode = Mode::Done;
        for (claim, value) in self.claims.iter_mut().zip(values) {
            claim.value = Some(*value);
            claim.phase = Phase::Done;
            claim.active = false;
        }
    }

    fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(20);
        match self.mode {
            Mode::Banner(phase) => vec![status_row(phase, "", width)],
            Mode::Working => {
                let mut rows: Vec<_> = self
                    .claims
                    .iter()
                    .map(|claim| claim_row(claim, width))
                    .collect();
                rows.push(status_row(self.phase, &self.detail, width));
                rows
            }
            Mode::Done => self
                .claims
                .iter()
                .map(|claim| claim_row(claim, width))
                .collect(),
        }
    }
}

fn claim_row(claim: &ClaimWatch, width: usize) -> Row {
    if let Some(value) = claim.value {
        // The final frame is not redrawn, so a long claim may wrap.
        return Row {
            text: format!("{:<7}  {}", value.to_string(), claim.verbalized),
            tone: tone_of_value(value),
        };
    }
    let mark = if claim.active {
        SPIN[claim.tick % SPIN.len()]
    } else {
        "·"
    };
    let phase = claim.phase.label();
    let left = format!("  {mark}  {phase:<8}  ");
    let room = width.saturating_sub(left.chars().count());
    let detail = claim.detail.as_str();
    let text = if detail.is_empty() || room <= detail.chars().count() + 2 {
        format!("{left}{}", fit(&claim.verbalized, room))
    } else {
        let claim_room = room - detail.chars().count() - 2;
        let claim_text = fit(&claim.verbalized, claim_room);
        let pad = room - claim_text.chars().count() - detail.chars().count();
        format!("{left}{claim_text}{}{detail}", " ".repeat(pad))
    };
    Row {
        text,
        tone: tone_of_phase(claim.phase),
    }
}

fn status_row(phase: Phase, detail: &str, width: usize) -> Row {
    let label = format!(" {} ", phase.label());
    let mut tail = if detail.is_empty() {
        String::new()
    } else {
        format!(" {detail} ")
    };
    let label_len = label.chars().count();
    if label_len + tail.chars().count() > width {
        tail = fit(&tail, width.saturating_sub(label_len));
    }
    let fill = width.saturating_sub(label_len + tail.chars().count());
    Row {
        text: format!("{label}{}{tail}", "─".repeat(fill)),
        tone: Tone::Rule,
    }
}

fn tone_of_phase(phase: Phase) -> Tone {
    match phase {
        Phase::Planning => Tone::Planning,
        Phase::Search => Tone::Search,
        Phase::Read => Tone::Read,
        Phase::Done => Tone::Rule,
    }
}

fn tone_of_value(value: Value) -> Tone {
    match value {
        Value::True => Tone::True,
        Value::False => Tone::False,
        Value::Unknown => Tone::Unknown,
    }
}

fn tone_color(tone: Tone) -> Color {
    match tone {
        Tone::Planning => Color::DarkGrey,
        Tone::Search => Color::Cyan,
        Tone::Read => Color::Blue,
        Tone::True => Color::Green,
        Tone::False => Color::Red,
        Tone::Unknown => Color::Yellow,
        Tone::Rule => Color::DarkGrey,
    }
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
        Expr::And(xs) | Expr::Or(xs) => {
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

struct Body {
    view: RunView,
    drawn: u16,
}

impl Body {
    fn draw(&mut self) -> io::Result<()> {
        let width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
        let rows = self.view.rows(width);
        let mut out = stdout();
        if self.drawn > 0 {
            queue!(out, cursor::MoveToColumn(0), cursor::MoveUp(self.drawn))?;
        }
        for row in &rows {
            queue!(
                out,
                Clear(ClearType::CurrentLine),
                SetForegroundColor(tone_color(row.tone)),
                Print(&row.text),
                ResetColor,
                Print("\r\n")
            )?;
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
        let mut body = self.lock();
        body.view.on_event(event);
        let _ = body.draw();
    }

    pub fn finish(&self, values: &[Value]) -> io::Result<()> {
        let mut body = self.lock();
        body.view.finish(values);
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

    fn sample() -> Plan {
        Plan::of(
            &compile(
                &parse(
                    r#"
(and ("Frontend is part of the system") (not ("there is a GPU compiler")))
(not ("there is a GPU compiler"))
"#,
                )
                .unwrap(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn planning_is_a_status_bar() {
        let view = RunView::planning();
        let lines = view
            .rows(40)
            .into_iter()
            .map(|row| row.text)
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
        let searching: Vec<_> = view.rows(80).into_iter().map(|row| row.text).collect();
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
        let searched: Vec<_> = view.rows(80).into_iter().map(|row| row.text).collect();
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
        let reading: Vec<_> = view.rows(100).into_iter().map(|row| row.text).collect();
        assert!(reading[0].contains("read"), "{}", reading[0]);
        assert!(reading[0].contains("decoy/noise.py"), "{}", reading[0]);
        assert!(reading[1].contains("read"), "{}", reading[1]);
        assert!(reading[1].contains("decoy/noise.py"), "{}", reading[1]);
        assert!(reading[2].contains("read"), "{}", reading[2]);
        assert!(reading[2].contains("decoy/noise.py"), "{}", reading[2]);

        view.finish(&[Value::False, Value::True]);
        let done: Vec<_> = view.rows(160).into_iter().map(|row| row.text).collect();
        assert_eq!(
            done,
            vec![
                "false    (and (\"Frontend is part of the system\") (not (\"there is a GPU compiler\")))",
                "true     (not (\"there is a GPU compiler\"))",
            ]
        );
    }
}
