//! Live report of `autoground sweep` on a terminal.
//!
//! While files are still being judged, the frame is the report so far and one
//! status line. The report keeps the latest rows that fit. When the sweep
//! finishes, the frame is the whole report, printed once. Piped output never
//! draws this.

use std::io::{self, stdout, Write};

use crossterm::{
    cursor, execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Plain,
    ErrorLead,
    Error,
    Status,
}

struct Row {
    text: String,
    tone: Tone,
}

impl Row {
    #[cfg(test)]
    fn plain(&self) -> &str {
        &self.text
    }
}

/// In-place report. Dropping it shows the cursor again.
pub struct Live {
    tick: usize,
    drawn: u16,
    hidden: bool,
}

impl Live {
    pub fn open() -> io::Result<Self> {
        execute!(stdout(), cursor::Hide)?;
        Ok(Self {
            tick: 0,
            drawn: 0,
            hidden: true,
        })
    }

    pub fn show(&mut self, report: &str, status: &str) -> io::Result<()> {
        let (cols, term_rows) = terminal::size()
            .map(|(width, height)| (width as usize, height as usize))
            .unwrap_or((80, 24));
        let width = cols.saturating_sub(1).max(1);
        let max_rows = term_rows.saturating_sub(1).max(1);
        let rows = working_rows(report, status, self.tick, width, max_rows);
        self.tick = self.tick.wrapping_add(1);
        self.draw(&rows)
    }

    pub fn finish(&mut self, report: &str) -> io::Result<()> {
        self.draw(&finished_rows(report))
    }

    fn draw(&mut self, rows: &[Row]) -> io::Result<()> {
        let mut out = stdout();
        if self.drawn > 0 {
            queue!(out, cursor::MoveToColumn(0), cursor::MoveUp(self.drawn))?;
        }
        for row in rows {
            queue!(out, Clear(ClearType::CurrentLine))?;
            if matches!(row.tone, Tone::ErrorLead) {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if let Some(color) = tone_color(row.tone) {
                queue!(out, SetForegroundColor(color))?;
            }
            queue!(
                out,
                Print(&row.text),
                ResetColor,
                SetAttribute(Attribute::Reset),
                Print("\r\n"),
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

impl Drop for Live {
    fn drop(&mut self) {
        if self.hidden {
            let _ = execute!(stdout(), cursor::Show);
            self.hidden = false;
        }
    }
}

fn working_rows(
    report: &str,
    status: &str,
    tick: usize,
    width: usize,
    max_rows: usize,
) -> Vec<Row> {
    let width = width.max(1);
    let max_rows = max_rows.max(1);
    let status = status_row(status, tick, width);
    if max_rows == 1 {
        return vec![status];
    }
    let mut lines = report_rows(report, width);
    let room = max_rows - 1;
    if lines.len() > room {
        lines = lines.split_off(lines.len() - room);
    }
    lines.push(status);
    lines
}

fn finished_rows(report: &str) -> Vec<Row> {
    report_rows(report, usize::MAX)
}

fn report_rows(report: &str, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut detail = false;
    for line in report.lines() {
        let tone = if line.starts_with("Error:") {
            detail = true;
            Tone::ErrorLead
        } else if detail && line.starts_with("  ") {
            Tone::Error
        } else {
            detail = false;
            Tone::Plain
        };
        rows.push(Row {
            text: fit(line, width),
            tone,
        });
    }
    rows
}

fn status_row(status: &str, tick: usize, width: usize) -> Row {
    let width = width.max(1);
    let spin = SPIN[tick % SPIN.len()];
    let label = format!(" {spin} {status} ");
    let label_len = label.chars().count();
    let text = if label_len >= width {
        fit(&label, width)
    } else {
        format!("{label}{}", "─".repeat(width - label_len))
    };
    Row {
        text,
        tone: Tone::Status,
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

fn tone_color(tone: Tone) -> Option<Color> {
    match tone {
        Tone::Plain => None,
        Tone::ErrorLead | Tone::Error => Some(Color::Red),
        Tone::Status => Some(Color::DarkGrey),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_frame_keeps_the_latest_rows_and_a_status_line() {
        let mut report = String::new();
        for n in 0..10 {
            report.push_str(&format!("/file-{n}\n  claim\n"));
        }
        let rows = working_rows(&report, "judging 10/10 files", 1, 40, 5);
        assert_eq!(rows.len(), 5);
        let status = rows.last().unwrap().plain();
        assert!(status.contains('⠙'), "{status}");
        assert!(status.contains("judging 10/10 files"), "{status}");
        assert!(status.chars().count() <= 40, "{status}");
        assert!(rows[0].plain().contains("/file-8"), "{}", rows[0].plain());
        assert!(!rows.iter().any(|row| row.plain().contains("/file-0")));
    }

    #[test]
    fn the_finished_report_lists_every_line() {
        let report = "/b.py\n  alpha\nError: /a.py\n  HTTP 400: too big\n";
        let rows = finished_rows(report);
        assert_eq!(
            rows.iter().map(|row| row.plain()).collect::<Vec<_>>(),
            vec!["/b.py", "  alpha", "Error: /a.py", "  HTTP 400: too big",]
        );
        assert_eq!(rows[0].tone, Tone::Plain);
        assert_eq!(rows[1].tone, Tone::Plain);
        assert_eq!(rows[2].tone, Tone::ErrorLead);
        assert_eq!(rows[3].tone, Tone::Error);
    }

    #[test]
    fn a_working_line_stays_inside_the_width() {
        let report =
            "Error: /a/very/long/path/that/does/not/fit\n  HTTP 400: also quite long indeed\n";
        let rows = working_rows(report, "judging 1/1 files", 0, 24, 6);
        assert!(rows.iter().all(|row| row.plain().chars().count() <= 24));
        assert!(rows[0].plain().ends_with('…'), "{}", rows[0].plain());
        let narrow = working_rows("", "judging 1/1 files", 0, 10, 3);
        assert!(
            narrow[0].plain().chars().count() <= 10,
            "{}",
            narrow[0].plain()
        );
    }
}
