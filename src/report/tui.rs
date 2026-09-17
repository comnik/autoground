use std::io::{stdout, IsTerminal, Write};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use super::step::format_step;
use crate::navigator::trace::GroundStep;

/// Interactive pager: left/right (or h/l, p/n) flip through steps. q quits.
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
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    terminal::enable_raw_mode()?;
    let result = page_loop(&mut stdout, steps, repo_name);
    terminal::disable_raw_mode()?;
    execute!(stdout, LeaveAlternateScreen, cursor::Show)?;
    result
}

fn page_loop(out: &mut impl Write, steps: &[GroundStep], repo_name: &str) -> std::io::Result<()> {
    let n = steps.len();
    let mut i = 0usize;
    loop {
        draw(out, &steps[i], i, n, repo_name)?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('p') => {
                    i = i.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') | KeyCode::Char(' ') => {
                    if i + 1 < n {
                        i += 1;
                    }
                }
                KeyCode::Home => i = 0,
                KeyCode::End => i = n - 1,
                KeyCode::Char('q') | KeyCode::Esc => break,
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

fn draw(
    out: &mut impl Write,
    step: &GroundStep,
    i: usize,
    n: usize,
    repo_name: &str,
) -> std::io::Result<()> {
    queue!(out, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let body = format_step(step, i, n, repo_name);
    for line in body.lines() {
        color_line(out, line)?;
        queue!(out, Print("\r\n"))?;
    }
    queue!(
        out,
        Print("\r\n"),
        SetForegroundColor(Color::DarkGrey),
        Print("  ← →  flip steps    home/end    q  quit"),
        ResetColor,
    )?;
    out.flush()?;
    Ok(())
}

fn color_line(out: &mut impl Write, line: &str) -> std::io::Result<()> {
    let color = if line.contains(" * whole") {
        Some(Color::Green)
    } else if line.contains(" + part") {
        Some(Color::Yellow)
    } else if line.contains(" > descend") {
        Some(Color::Cyan)
    } else if line.contains("noise") {
        Some(Color::DarkGrey)
    } else if line.contains("verdict     true") || line.contains("found") {
        Some(Color::Green)
    } else if line.contains("verdict     false") || line.contains("absent") {
        Some(Color::Red)
    } else if line.contains("verdict     unknown")
        || line.contains("unknown") && line.trim_start().starts_with("unknown")
    {
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
