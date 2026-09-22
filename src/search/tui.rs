use std::io::{stdout, IsTerminal, Write};
use std::path::Path;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use super::trace::{format_exchange, format_steps, format_summary, SearchStep, SearchTraceFile};
use super::{Applied, Decision};

struct RawMode;

impl RawMode {
    fn enter() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

struct AlternateScreen;

impl AlternateScreen {
    fn enter(out: &mut impl Write) -> std::io::Result<Self> {
        execute!(out, EnterAlternateScreen, cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for AlternateScreen {
    fn drop(&mut self) {
        let _ = execute!(stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

/// Print the verdict, usage, trace path, and true/false evidence.
/// On a TTY, left/right opens the step pager.
pub fn present_search(path: &Path, file: &SearchTraceFile, full: bool) -> std::io::Result<()> {
    print!("{}", format_summary(path, file));
    if full && !file.steps.is_empty() {
        println!();
        print!("{}", format_steps(&file.steps));
    }
    stdout().flush()?;
    if !stdout().is_terminal() || file.steps.is_empty() {
        return Ok(());
    }
    println!("  ← →  steps    q  quit");
    stdout().flush()?;

    let _raw = RawMode::enter()?;
    let mut out = stdout();
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if is_quit(&key) {
                    break;
                }
                if is_open(&key) {
                    page(&mut out, &file.steps)?;
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

fn is_open(key: &event::KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l')
    )
}

fn is_quit(key: &event::KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => true,
        KeyCode::Char('c') | KeyCode::Char('C')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            true
        }
        _ => false,
    }
}

struct Pane {
    step: usize,
    sel: usize,
    list_scroll: usize,
    ctx_scroll: usize,
    raw: bool,
    /// Jump the context pane to the selected object on the next draw.
    follow: bool,
}

fn page(out: &mut impl Write, steps: &[SearchStep]) -> std::io::Result<()> {
    let _alt = AlternateScreen::enter(out)?;
    let mut pane = Pane {
        step: 0,
        sel: 0,
        list_scroll: 0,
        ctx_scroll: 0,
        raw: false,
        follow: true,
    };
    loop {
        draw(out, steps, &mut pane)?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('p') => {
                    pane.step = pane.step.saturating_sub(1);
                    pane.sel = 0;
                    pane.list_scroll = 0;
                    pane.ctx_scroll = 0;
                    pane.follow = true;
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') | KeyCode::Char(' ') => {
                    if pane.step + 1 < steps.len() {
                        pane.step += 1;
                        pane.sel = 0;
                        pane.list_scroll = 0;
                        pane.ctx_scroll = 0;
                        pane.follow = true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    pane.sel = pane.sel.saturating_sub(1);
                    pane.follow = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = steps[pane.step].classifications.len();
                    if pane.sel + 1 < n {
                        pane.sel += 1;
                        pane.follow = true;
                    }
                }
                KeyCode::PageUp => {
                    pane.follow = false;
                    pane.ctx_scroll = pane.ctx_scroll.saturating_sub(body_rows());
                }
                KeyCode::PageDown => {
                    pane.follow = false;
                    pane.ctx_scroll = pane.ctx_scroll.saturating_add(body_rows());
                }
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    pane.raw = !pane.raw;
                    pane.ctx_scroll = 0;
                    pane.follow = !pane.raw;
                }
                KeyCode::Char('q') | KeyCode::Esc => break,
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

fn body_rows() -> usize {
    terminal::size()
        .map(|(_, rows)| (rows as usize).saturating_sub(2))
        .unwrap_or(22)
        .max(1)
}

fn draw(out: &mut impl Write, steps: &[SearchStep], pane: &mut Pane) -> std::io::Result<()> {
    let (cols, rows) = terminal::size()?;
    let cols = cols as usize;
    let rows = rows as usize;
    let body = rows.saturating_sub(2).max(1);
    let step = &steps[pane.step];
    let n = step.classifications.len();
    if n == 0 {
        pane.sel = 0;
    } else if pane.sel >= n {
        pane.sel = n - 1;
    }
    if pane.sel < pane.list_scroll {
        pane.list_scroll = pane.sel;
    } else if pane.sel >= pane.list_scroll + body {
        pane.list_scroll = pane.sel + 1 - body;
    }

    let col1 = column_width(step, cols);
    let col2 = cols.saturating_sub(col1 + 3);
    let right = right_lines(step, pane.sel, pane.raw, col2);
    let max_ctx = right.len().saturating_sub(body);
    if pane.follow && !pane.raw {
        pane.ctx_scroll = context_anchor(step, pane.sel, &right)
            .saturating_sub(2)
            .min(max_ctx);
        pane.follow = false;
    } else {
        pane.ctx_scroll = pane.ctx_scroll.min(max_ctx);
    }

    queue!(out, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    queue!(
        out,
        Print(format!("  step {}/{}", pane.step + 1, steps.len())),
        Clear(ClearType::UntilNewLine)
    )?;

    for row in 0..body {
        queue!(out, cursor::MoveTo(0, (row + 1) as u16))?;
        let idx = pane.list_scroll + row;
        if let Some(decision) = step.classifications.get(idx) {
            let selected = idx == pane.sel;
            paint_entry(out, decision, col1, selected)?;
        } else {
            queue!(out, Print(" ".repeat(col1)))?;
        }
        queue!(
            out,
            SetForegroundColor(Color::DarkGrey),
            Print(" │ "),
            ResetColor
        )?;
        let line = right_row(&right, pane.ctx_scroll, row);
        let shown = fit(&line, col2);
        if pane.raw {
            queue!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(&shown),
                ResetColor
            )?;
        } else if row == 0 {
            queue!(out, Print(&shown))?;
        } else {
            queue!(out, Print(&shown))?;
        }
        queue!(out, Clear(ClearType::UntilNewLine))?;
    }

    let v_hint = if pane.raw { "v  context" } else { "v  request" };
    let help = format!("  ← →  steps    ↑↓  select    {v_hint}    pgup/pgdn  context    q  back");
    queue!(
        out,
        cursor::MoveTo(0, rows.saturating_sub(1) as u16),
        SetForegroundColor(Color::DarkGrey),
        Print(fit(&help, cols.saturating_sub(1))),
        Clear(ClearType::UntilNewLine),
        ResetColor,
    )?;
    out.flush()
}

fn column_width(step: &SearchStep, cols: usize) -> usize {
    let longest = step
        .classifications
        .iter()
        .map(|d| entry_name(d).chars().count())
        .max()
        .unwrap_or(12);
    let room = cols.saturating_sub(24).max(12);
    longest.clamp(12, room.min(40))
}

fn paint_entry(
    out: &mut impl Write,
    decision: &Decision,
    width: usize,
    selected: bool,
) -> std::io::Result<()> {
    let name = fit(&entry_name(decision), width);
    let color = match decision.applied {
        Applied::True => Some(Color::Green),
        Applied::False => Some(Color::Red),
        Applied::Prune => Some(Color::DarkGrey),
        Applied::Refine | Applied::Unknown => None,
    };
    if selected {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    if let Some(color) = color {
        queue!(out, SetForegroundColor(color), Print(name), ResetColor)?;
    } else {
        queue!(out, Print(name))?;
    }
    if selected {
        queue!(out, SetAttribute(Attribute::Reset))?;
    }
    Ok(())
}

fn right_lines(step: &SearchStep, sel: usize, raw: bool, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(decision) = step.classifications.get(sel) {
        lines.push(format!("{}  {:.2}", decision.raw, decision.confidence));
    } else {
        lines.push(String::new());
    }
    lines.push(String::new());
    let body = if raw {
        format_exchange(step)
    } else {
        context_text(step)
    };
    lines.extend(wrap_lines(body.trim_start_matches('\n'), width.max(1)));
    lines
}

fn right_row(lines: &[String], scroll: usize, row: usize) -> String {
    // The verdict line stays put. The rest of the pane scrolls under it.
    if row == 0 {
        return lines.first().cloned().unwrap_or_default();
    }
    if row == 1 {
        return lines.get(1).cloned().unwrap_or_default();
    }
    lines.get(scroll + row).cloned().unwrap_or_default()
}

fn context_text(step: &SearchStep) -> String {
    if let Some(view) = step
        .exchange
        .as_ref()
        .and_then(|ex| ex.request.get("state"))
        .and_then(|state| state.get("view"))
        .and_then(|view| view.as_str())
    {
        return view.to_string();
    }
    let mut out = String::new();
    for obj in &step.view {
        if obj.leaf {
            out.push_str(&format!("{}  ({} bytes)\n", obj.id, obj.bytes));
        } else if obj.listing.is_empty() {
            out.push_str(&entry_name_id(&obj.id, false));
            out.push('\n');
        } else {
            out.push_str(&obj.listing);
            if !obj.listing.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    if out.is_empty() {
        out.push_str("(no context recorded)\n");
    }
    out
}

fn context_anchor(step: &SearchStep, sel: usize, lines: &[String]) -> usize {
    let Some(decision) = step.classifications.get(sel) else {
        return 0;
    };
    let needle = if decision.leaf {
        format!("=== {} ===", decision.id)
    } else if decision.id.is_empty() {
        "/".to_string()
    } else {
        format!("{}/", decision.id)
    };
    lines
        .iter()
        .position(|line| line.contains(&needle))
        .unwrap_or(0)
}

fn entry_name(decision: &Decision) -> String {
    entry_name_id(&decision.id, decision.leaf)
}

fn entry_name_id(id: &str, leaf: bool) -> String {
    if id.is_empty() {
        "/".to_string()
    } else if leaf {
        id.to_string()
    } else {
        format!("{id}/")
    }
}

fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= width {
        let pad = " ".repeat(width - count);
        let mut out = text.to_string();
        out.push_str(&pad);
        return out;
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

fn wrap_lines(body: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut rest = line;
        while rest.len() > width {
            let mut end = width;
            while end > 0 && !rest.is_char_boundary(end) {
                end -= 1;
            }
            if end == 0 {
                end = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
        out.push(rest.to_string());
    }
    out
}
