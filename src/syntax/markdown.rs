/// The bodies of fenced `lect` blocks, in document order.
///
/// A fence is a line of at least three backticks or tildes whose info string
/// starts with `lect`. The body runs to the matching closer, or to the end of
/// the document when the closer is missing. Up to three leading spaces on the
/// fence are stripped from each body line.
pub fn extract_lects(markdown: &str) -> Vec<String> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(open) = opening_lect_fence(lines[i]) {
            i += 1;
            let start = i;
            while i < lines.len() && !closing_fence(lines[i], &open) {
                i += 1;
            }
            blocks.push(render_body(&lines[start..i], open.indent));
            if i < lines.len() {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    blocks
}

/// Concatenate [`extract_lects`].
pub fn lect_source(markdown: &str) -> Result<String, MarkdownError> {
    let blocks = extract_lects(markdown);
    if blocks.is_empty() {
        return Err(MarkdownError);
    }
    Ok(blocks.concat())
}

#[derive(Debug)]
pub struct MarkdownError;

impl std::fmt::Display for MarkdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no lect blocks")
    }
}

impl std::error::Error for MarkdownError {}

struct OpenFence {
    ch: u8,
    len: usize,
    indent: usize,
}

fn opening_lect_fence(line: &str) -> Option<OpenFence> {
    let indent = leading_spaces(line);
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let (ch, len) = fence_run(rest)?;
    if len < 3 {
        return None;
    }
    let info = &rest[len..];
    if ch == b'`' && info.contains('`') {
        return None;
    }
    let word = info.trim().split_whitespace().next().unwrap_or("");
    if !word.eq_ignore_ascii_case("lect") {
        return None;
    }
    Some(OpenFence { ch, len, indent })
}

fn closing_fence(line: &str, open: &OpenFence) -> bool {
    let indent = leading_spaces(line);
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let Some((ch, len)) = fence_run(rest) else {
        return false;
    };
    ch == open.ch && len >= open.len && rest[len..].chars().all(|c| c == ' ' || c == '\t')
}

fn fence_run(rest: &str) -> Option<(u8, usize)> {
    let ch = *rest.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = rest.bytes().take_while(|b| *b == ch).count();
    Some((ch, len))
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

fn render_body(lines: &[&str], indent: usize) -> String {
    let mut out = String::new();
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let drop = leading_spaces(line).min(indent);
        out.push_str(&line[drop..]);
    }
    if !lines.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_lect_fences_and_skips_other_blocks() {
        let md = "\
# notes

```rust
fn main() {}
```

```lect
(name Frontend)
(\"Frontend is a component\")
```

Prose with `lect` in it.

~~~lect title
(name Backend)
~~~

```lecture
(name Nope)
```
";
        assert_eq!(
            lect_source(md).unwrap(),
            "(name Frontend)\n(\"Frontend is a component\")\n(name Backend)\n"
        );
    }

    #[test]
    fn a_longer_fence_holds_a_shorter_one() {
        let md = "\
````lect
```
(name Frontend)
```
````
";
        assert_eq!(lect_source(md).unwrap(), "```\n(name Frontend)\n```\n");
    }

    #[test]
    fn fence_indent_is_stripped_from_the_body() {
        let md = "  ```lect\n  (name Frontend)\n  ```\n";
        assert_eq!(lect_source(md).unwrap(), "(name Frontend)\n");
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let md = "```lect\n(name Frontend)\n";
        assert_eq!(lect_source(md).unwrap(), "(name Frontend)\n");
    }

    #[test]
    fn prose_has_no_lect_blocks() {
        let err = lect_source("# hello\n\n```rust\nfn main() {}\n```\n").unwrap_err();
        assert_eq!(err.to_string(), "no lect blocks");
    }
}
