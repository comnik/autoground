use std::io::{stdout, IsTerminal, Write};
use std::time::{Duration, SystemTime};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use super::step::{format_jev, format_step};
use super::{format_duration, format_usd, jev_request_cost_usd, jev_request_intervals, RunTiming};
use crate::navigator::confidence::{accept_resolve, confident_look, RESOLVE};
use crate::navigator::judge::{Consistency, DocumentVerdict, ResolveValue};
use crate::navigator::trace::GroundStep;

const JEV_BAR: Color = Color::Rgb {
    r: 135,
    g: 206,
    b: 250,
};
const REST_BAR: Color = Color::DarkGrey;

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

/// Print the run summary. On a TTY, a concurrency timeline follows the
/// header (one row per in-flight Jev count), and left/right open the
/// grounding debug pager.
pub fn present_run(
    header: &str,
    body: &str,
    steps: &[GroundStep],
    repo_name: &str,
    timing: RunTiming,
) -> std::io::Result<()> {
    print!("{header}");
    stdout().flush()?;
    if stdout().is_terminal() {
        draw_timing_bar(&mut stdout(), timing, steps)?;
    }
    print!("{body}");
    stdout().flush()?;
    if !stdout().is_terminal() {
        return Ok(());
    }
    if steps.is_empty() {
        return Ok(());
    }

    println!("  ← →  debug    q  quit");
    stdout().flush()?;

    let _raw = RawMode::enter()?;
    let mut out = stdout();
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if is_quit_key(&key) {
                    break;
                }
                if is_debug_key(&key) {
                    debug_view(&mut out, steps, repo_name)?;
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

fn draw_timing_bar(
    out: &mut impl Write,
    timing: RunTiming,
    steps: &[GroundStep],
) -> std::io::Result<()> {
    let cols = terminal::size().map(|(c, _)| c as usize).unwrap_or(80);
    let elapsed = timing.elapsed();
    let mut suffix = format!("  {}", format_duration(elapsed));
    if let Some(cost) = jev_request_cost_usd(steps) {
        suffix.push(' ');
        suffix.push_str(&format_usd(cost));
    }
    let intervals = jev_request_intervals(steps);
    let width_guess = cols.saturating_sub(suffix.len()).saturating_sub(4).max(8);
    let peek = concurrency_cells(width_guess, timing.started_at, timing.ended_at, &intervals);
    let max_n = peek.iter().copied().max().unwrap_or(0);
    if max_n == 0 {
        let bar_width = cols.saturating_sub(suffix.len()).saturating_sub(1).max(8);
        queue!(
            out,
            SetForegroundColor(REST_BAR),
            Print(&"▀".repeat(bar_width)),
            ResetColor,
            Print(&suffix),
            Print("\n")
        )?;
        return out.flush();
    }
    let label_w = max_n.to_string().len().max(1);
    let bar_width = cols
        .saturating_sub(label_w + 1 + suffix.len())
        .saturating_sub(1)
        .max(8);
    let cells = concurrency_cells(bar_width, timing.started_at, timing.ended_at, &intervals);
    let levels: Vec<usize> = (1..=max_n).rev().filter(|&n| cells.contains(&n)).collect();
    for (i, n) in levels.iter().enumerate() {
        let label = format!("{n:>label_w$} ");
        queue!(out, Print(&label))?;
        write_level_bar(out, &cells, *n)?;
        if i + 1 == levels.len() {
            queue!(out, Print(&suffix), Print("\n"))?;
        } else {
            queue!(out, Print("\n"))?;
        }
    }
    out.flush()
}

fn write_level_bar(out: &mut impl Write, cells: &[usize], n: usize) -> std::io::Result<()> {
    let mut i = 0;
    while i < cells.len() {
        let hit = cells[i] == n;
        let mut j = i + 1;
        while j < cells.len() && (cells[j] == n) == hit {
            j += 1;
        }
        if hit {
            queue!(
                out,
                SetForegroundColor(JEV_BAR),
                Print(&"▀".repeat(j - i)),
                ResetColor
            )?;
        } else {
            queue!(out, Print(&" ".repeat(j - i)))?;
        }
        i = j;
    }
    Ok(())
}

/// Peak in-flight Jev count for each column of the run timeline.
fn concurrency_cells(
    width: usize,
    run_start: SystemTime,
    run_end: SystemTime,
    intervals: &[(SystemTime, SystemTime)],
) -> Vec<usize> {
    let mut cells = vec![0usize; width];
    let total = match run_end.duration_since(run_start) {
        Ok(d) if d > Duration::ZERO => d.as_secs_f64(),
        _ => return cells,
    };
    let mut events: Vec<(f64, i32)> = Vec::with_capacity(intervals.len() * 2);
    for &(start, end) in intervals {
        let s = start
            .duration_since(run_start)
            .unwrap_or(Duration::ZERO)
            .as_secs_f64()
            .clamp(0.0, total);
        let e = end
            .duration_since(run_start)
            .unwrap_or(Duration::ZERO)
            .as_secs_f64()
            .clamp(0.0, total);
        if e <= s {
            continue;
        }
        events.push((s, 1));
        events.push((e, -1));
    }
    // Ends before starts at the same instant so back-to-back calls stay at n=1.
    events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut n = 0i32;
    let mut last = 0.0;
    let mut i = 0;
    while i < events.len() {
        let t = events[i].0;
        if t > last && n > 0 {
            paint_level(&mut cells, width, total, last, t, n as usize);
        }
        while i < events.len() && events[i].0 == t {
            n += events[i].1;
            i += 1;
        }
        last = t;
    }
    cells
}

fn paint_level(cells: &mut [usize], width: usize, total: f64, s: f64, e: f64, n: usize) {
    let mut a = ((s / total) * width as f64).floor() as usize;
    let mut b = ((e / total) * width as f64).ceil() as usize;
    a = a.min(width);
    b = b.min(width);
    if a == b && a < width {
        b = a + 1;
    }
    for cell in cells.iter_mut().take(b).skip(a) {
        *cell = (*cell).max(n);
    }
}

fn is_debug_key(key: &event::KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l')
            | KeyCode::Char('p')
            | KeyCode::Char('n')
            | KeyCode::Char(' ')
    )
}

fn is_quit_key(key: &event::KeyEvent) -> bool {
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

fn debug_view(out: &mut impl Write, steps: &[GroundStep], repo_name: &str) -> std::io::Result<()> {
    let _alt = AlternateScreen::enter(out)?;
    page_loop(out, steps, repo_name)
}

/// Interactive pager: left/right (or h/l, p/n) flip through steps. q quits.
/// `v` toggles the Jev request/response under the tree.
pub fn page_trace(steps: &[GroundStep], repo_name: &str) -> std::io::Result<()> {
    if steps.is_empty() {
        println!("no grounding steps");
        return Ok(());
    }
    if !std::io::stdout().is_terminal() {
        print!("{}", super::step::format_trace(steps, repo_name));
        return Ok(());
    }

    let mut stdout = stdout();
    let _raw = RawMode::enter()?;
    debug_view(&mut stdout, steps, repo_name)
}

fn page_loop(out: &mut impl Write, steps: &[GroundStep], repo_name: &str) -> std::io::Result<()> {
    let n = steps.len();
    let mut i = 0usize;
    let mut verbose = false;
    let mut scroll = 0usize;
    loop {
        draw(out, &steps[i], i, n, repo_name, verbose, &mut scroll)?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('p') => {
                    i = i.saturating_sub(1);
                    scroll = 0;
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') | KeyCode::Char(' ') => {
                    if i + 1 < n {
                        i += 1;
                        scroll = 0;
                    }
                }
                KeyCode::Home => {
                    i = 0;
                    scroll = 0;
                }
                KeyCode::End => {
                    i = n - 1;
                    scroll = 0;
                }
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    verbose = !verbose;
                    scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(page_rows());
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(page_rows());
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

fn page_rows() -> usize {
    terminal::size()
        .map(|(_, r)| (r as usize).saturating_sub(1))
        .unwrap_or(24)
        .max(1)
}

fn draw(
    out: &mut impl Write,
    step: &GroundStep,
    i: usize,
    n: usize,
    repo_name: &str,
    verbose: bool,
    scroll: &mut usize,
) -> std::io::Result<()> {
    let (cols, rows) = terminal::size()?;
    let width = (cols as usize).saturating_sub(1).max(1);
    let page = (rows as usize).saturating_sub(1).max(1);
    let mut body = format_step(step, i, n, repo_name);
    if verbose {
        body.push_str(&format_jev(step));
    }
    let lines = wrap_lines(&body, width);
    let max_scroll = lines.len().saturating_sub(page);
    *scroll = (*scroll).min(max_scroll);

    queue!(out, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let mut in_payload = false;
    for (idx, line) in lines.iter().enumerate() {
        if line.contains("Jev request")
            || line.contains("Jev response")
            || line.contains("no Jev request")
        {
            in_payload = true;
        }
        if idx < *scroll || idx >= *scroll + page {
            continue;
        }
        queue!(out, cursor::MoveTo(0, (idx - *scroll) as u16))?;
        if in_payload {
            queue!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(line),
                ResetColor
            )?;
        } else {
            color_line(out, line, step)?;
        }
    }
    let more = if *scroll + page < lines.len() {
        "    ↓ more"
    } else {
        ""
    };
    let v_hint = if verbose { "v  tree" } else { "v  jev" };
    let help = format!("  ← →  flip steps    {v_hint}    ↑↓  scroll    q  quit{more}");
    queue!(
        out,
        cursor::MoveTo(0, rows.saturating_sub(1)),
        SetForegroundColor(Color::DarkGrey),
        Print(&help),
        Clear(ClearType::UntilNewLine),
        ResetColor,
    )?;
    out.flush()?;
    Ok(())
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

fn color_line(out: &mut impl Write, line: &str, step: &GroundStep) -> std::io::Result<()> {
    let color = if line.contains(" * investigate") {
        Some(Color::Green)
    } else if let Some(c) = document_line_color(line, step) {
        Some(c)
    } else if line.contains("found") {
        Some(Color::Green)
    } else if line.contains("absent") {
        Some(Color::Red)
    } else if line.trim_start().starts_with("unknown") {
        Some(Color::Yellow)
    } else {
        None
    };
    if let Some(c) = color {
        queue!(out, SetForegroundColor(c), Print(line), ResetColor)?;
    } else {
        queue!(out, Print(line))?;
    }
    Ok(())
}

fn document_line_color(line: &str, step: &GroundStep) -> Option<Color> {
    if let GroundStep::Conflict {
        value,
        consistency,
        value_confidence,
        ..
    } = step
    {
        if line.contains("consistency") {
            return Some(match consistency {
                Consistency::Conflicting => Color::Yellow,
                Consistency::Consistent => Color::Green,
            });
        }
        if line.contains("value") || line.contains("confidence") {
            let accepted = accept_resolve(*value, *value_confidence);
            if accepted == ResolveValue::Unknown && *value != ResolveValue::Unknown {
                return Some(Color::Yellow);
            }
            if line.contains("value") {
                return Some(match accepted {
                    ResolveValue::True => Color::Green,
                    ResolveValue::False => Color::Red,
                    ResolveValue::Unknown => Color::Yellow,
                });
            }
            if *value_confidence < RESOLVE {
                return Some(Color::Yellow);
            }
        }
        return None;
    }
    let GroundStep::Document { judgement, .. } = step else {
        return None;
    };
    let actionable = match judgement.verdict {
        DocumentVerdict::Entailed => judgement.is_entailed(),
        DocumentVerdict::Contradicted => judgement.is_contradicted(),
        DocumentVerdict::Neutral => confident_look(judgement.confidence),
    };
    if line.contains("verdict") {
        if !actionable {
            return None;
        }
        return Some(match judgement.verdict {
            DocumentVerdict::Entailed => Color::Green,
            DocumentVerdict::Contradicted => Color::Red,
            DocumentVerdict::Neutral => Color::Yellow,
        });
    }
    if line.contains("confidence") && !actionable {
        return Some(Color::Yellow);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn t(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn concurrency_cells_sequential_requests() {
        let cells = concurrency_cells(
            10,
            t(1000),
            t(2000),
            &[(t(1000), t(1500)), (t(1800), t(2000))],
        );
        assert_eq!(cells, vec![1, 1, 1, 1, 1, 0, 0, 0, 1, 1]);
    }

    #[test]
    fn concurrency_cells_overlapping_requests() {
        let cells = concurrency_cells(10, t(0), t(1000), &[(t(0), t(600)), (t(400), t(800))]);
        assert_eq!(cells, vec![1, 1, 1, 1, 2, 2, 1, 1, 0, 0]);
    }

    #[test]
    fn concurrency_cells_abutting_requests_stay_at_one() {
        let cells = concurrency_cells(10, t(0), t(1000), &[(t(0), t(500)), (t(500), t(1000))]);
        assert_eq!(cells, vec![1; 10]);
    }

    #[test]
    fn concurrency_cells_empty_when_no_jev() {
        let cells = concurrency_cells(8, t(0), t(1000), &[]);
        assert_eq!(cells, vec![0; 8]);
    }

    #[test]
    fn concurrency_cells_zero_duration_run_is_empty() {
        let cells = concurrency_cells(8, t(5), t(5), &[(t(5), t(5))]);
        assert_eq!(cells, vec![0; 8]);
    }
}
