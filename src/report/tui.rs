use std::io::{stdout, IsTerminal, Write};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use super::step::{format_jev, format_step};
use crate::navigator::judge::{DocumentVerdict, TAU_CONFIDENCE};
use crate::navigator::trace::GroundStep;

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

/// Print the run summary. On a TTY, left/right open the grounding debug pager.
pub fn present_run(summary: &str, steps: &[GroundStep], repo_name: &str) -> std::io::Result<()> {
    print!("{summary}");
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
    } else if line.contains("noise") {
        Some(Color::DarkGrey)
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
    let GroundStep::Document { judgement, .. } = step else {
        return None;
    };
    let high = judgement.confidence >= TAU_CONFIDENCE;
    if line.contains("verdict") {
        if !high {
            return None;
        }
        return Some(match judgement.verdict {
            DocumentVerdict::True => Color::Green,
            DocumentVerdict::False => Color::Red,
            DocumentVerdict::Unknown => Color::Yellow,
        });
    }
    if line.contains("confidence") && !high {
        return Some(Color::Yellow);
    }
    None
}
