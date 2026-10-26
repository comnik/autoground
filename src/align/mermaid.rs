//! A flowchart lowered to brief source.
//!
//! Node labels that can be declared become names. An edge label that can be
//! declared is that sense applied to the two endpoints and assumed. A leading
//! `not ` is refused. Any other label is the wording between the two names.
//! A `%%` line whose body starts with `(` is brief source.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::syntax::parse::is_declared_name;
use crate::syntax::template::quote_const;

#[derive(Debug)]
pub struct LowerError {
    line: usize,
    message: String,
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for LowerError {}

struct Diagram {
    forms: Vec<String>,
    order: Vec<String>,
    labels: HashMap<String, String>,
    edges: Vec<RawEdge>,
}

struct RawEdge {
    from: String,
    to: String,
    label: String,
}

/// Brief source for `src`, which is a mermaid flowchart.
pub fn lower_mermaid(src: &str) -> Result<String, LowerError> {
    let mut diagram = Diagram {
        forms: Vec::new(),
        order: Vec::new(),
        labels: HashMap::new(),
        edges: Vec::new(),
    };
    let mut header = false;
    let mut depth = 0usize;
    for (idx, line) in src.lines().enumerate() {
        let number = idx + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("%%") {
            let body = rest.trim();
            if body.starts_with('(') {
                diagram.forms.push(body.to_string());
            }
            continue;
        }
        let words: Vec<&str> = trimmed.split_whitespace().collect();
        if words.len() == 2 && (words[0] == "flowchart" || words[0] == "graph") {
            if header {
                return Err(fail(number, trimmed, "a diagram has two flowcharts"));
            }
            if !is_direction(words[1]) {
                return Err(fail(number, trimmed, "unrecognized diagram line"));
            }
            header = true;
            continue;
        }
        if !header {
            return Err(fail(number, trimmed, "a diagram needs a flowchart"));
        }
        if words.first() == Some(&"subgraph") {
            depth += 1;
            continue;
        }
        if words == ["end"] {
            if depth == 0 {
                return Err(fail(number, trimmed, "end is outside a subgraph"));
            }
            depth -= 1;
            continue;
        }
        if matches!(
            words.first().copied(),
            Some("classDef" | "style" | "linkStyle" | "class")
        ) {
            continue;
        }
        let chain = parse_chain(trimmed).map_err(|message| fail(number, trimmed, &message))?;
        for node in chain.nodes {
            diagram.note(node);
        }
        for edge in chain.edges {
            if split_not(&edge.label).0 {
                return Err(fail(number, trimmed, "a negated edge cannot be assumed"));
            }
            diagram.edges.push(edge);
        }
    }
    if !header {
        return Err(LowerError {
            line: 0,
            message: "a diagram needs a flowchart".to_string(),
        });
    }
    if depth != 0 {
        return Err(LowerError {
            line: 0,
            message: "a subgraph is still open".to_string(),
        });
    }
    Ok(diagram.render())
}

fn fail(line: usize, source: &str, message: &str) -> LowerError {
    LowerError {
        line,
        message: format!("{message}: {source}"),
    }
}

fn is_direction(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "TB" | "TD" | "BT" | "RL" | "LR"
    )
}

impl Diagram {
    /// A later bare mention keeps the label a shape already gave this id.
    fn note(&mut self, node: Node) {
        if let Some(label) = self.labels.get_mut(&node.id) {
            if node.shaped {
                *label = node.label;
            }
            return;
        }
        self.order.push(node.id.clone());
        self.labels.insert(node.id, node.label);
    }

    fn spelling<'a>(&'a self, id: &'a str) -> &'a str {
        self.labels.get(id).map(String::as_str).unwrap_or(id)
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for form in &self.forms {
            out.push_str(form);
            out.push('\n');
        }
        let mut seen = HashSet::new();
        for id in &self.order {
            let label = self.spelling(id);
            if is_declared_name(label) && seen.insert(label.to_string()) {
                out.push_str("(name ");
                out.push_str(label);
                out.push_str(")\n");
            }
        }
        for edge in &self.edges {
            let text = edge.label.trim();
            let from = self.spelling(&edge.from);
            let to = self.spelling(&edge.to);
            let form = if is_declared_name(text) {
                format!("({text} {} {})", arg(from), arg(to))
            } else {
                let mut template = String::from("{X} ");
                template.push_str(text);
                template.push_str(" {Y}");
                format!("({} {} {})", quote_const(&template), arg(from), arg(to))
            };
            out.push_str("(assume ");
            out.push_str(&form);
            out.push_str(")\n");
        }
        out
    }
}

fn arg(spelling: &str) -> String {
    if is_declared_name(spelling) {
        spelling.to_string()
    } else {
        quote_const(spelling)
    }
}

/// Whether a label starts with `not `, which an edge cannot assume.
fn split_not(label: &str) -> (bool, &str) {
    let label = label.trim();
    if let Some(rest) = label.strip_prefix("not ") {
        let rest = rest.trim();
        if !rest.is_empty() {
            return (true, rest);
        }
    }
    (false, label)
}

struct Node {
    id: String,
    label: String,
    /// The label came from a shape, so it replaces a bare id seen earlier.
    shaped: bool,
}

struct Chain {
    nodes: Vec<Node>,
    edges: Vec<RawEdge>,
}

struct ParsedEdge {
    reversed: bool,
    label: String,
}

fn parse_chain(line: &str) -> Result<Chain, String> {
    let mut scan = Scan { s: line, i: 0 };
    scan.skip_ws();
    let mut nodes = vec![scan.parse_node()?];
    let mut edges = Vec::new();
    loop {
        scan.skip_ws();
        if scan.eof() {
            break;
        }
        if !scan.at_edge() {
            return Err("unrecognized diagram line".to_string());
        }
        let parsed = scan.parse_edge()?;
        scan.skip_ws();
        let node = scan.parse_node()?;
        let from = nodes.len() - 1;
        nodes.push(node);
        let to = nodes.len() - 1;
        let (from, to) = if parsed.reversed {
            (to, from)
        } else {
            (from, to)
        };
        edges.push(RawEdge {
            from: nodes[from].id.clone(),
            to: nodes[to].id.clone(),
            label: parsed.label,
        });
    }
    Ok(Chain { nodes, edges })
}

struct Scan<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Scan<'a> {
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }

    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn at_stroke(&self) -> bool {
        let rest = self.rest();
        rest.starts_with("--")
            || rest.starts_with("==")
            || rest.starts_with("-.")
            || rest.starts_with("..")
    }

    fn at_edge(&self) -> bool {
        if self.at_stroke() {
            return true;
        }
        let rest = self.rest();
        rest.starts_with('<') && {
            let after = &rest[1..];
            after.starts_with("--")
                || after.starts_with("==")
                || after.starts_with("-.")
                || after.starts_with("..")
        }
    }

    fn consume_stroke(&mut self) {
        while self.peek().is_some_and(|c| matches!(c, '-' | '.' | '=')) {
            self.bump();
        }
    }

    fn parse_node(&mut self) -> Result<Node, String> {
        if self.eof() {
            return Err("a node is missing".to_string());
        }
        let id = if self.peek() == Some('"') {
            self.read_quoted()?
        } else {
            let id = self.read_id();
            if id.is_empty() {
                return Err("unrecognized diagram line".to_string());
            }
            id
        };
        let (label, shaped) = if self.opens_shape() {
            (self.read_shape()?, true)
        } else {
            (id.clone(), false)
        };
        if self.rest().starts_with(":::") {
            self.i += 3;
            if self.read_class().is_empty() {
                return Err("a class is missing its name".to_string());
            }
        }
        Ok(Node { id, label, shaped })
    }

    fn read_id(&mut self) -> String {
        let start = self.i;
        while let Some(c) = self.peek() {
            if self.at_stroke() || !is_id_char(c) {
                break;
            }
            self.bump();
        }
        self.s[start..self.i].to_string()
    }

    fn read_class(&mut self) -> String {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            self.bump();
        }
        self.s[start..self.i].to_string()
    }

    fn read_quoted(&mut self) -> Result<String, String> {
        self.bump();
        let mut out = String::new();
        while let Some(c) = self.bump() {
            if c == '\\' {
                if let Some(next) = self.bump() {
                    out.push(next);
                }
                continue;
            }
            if c == '"' {
                return Ok(out);
            }
            out.push(c);
        }
        Err("a quote is missing its closer".to_string())
    }

    fn opens_shape(&self) -> bool {
        matches!(self.peek(), Some('[' | '(' | '{'))
    }

    fn read_shape(&mut self) -> Result<String, String> {
        let (open, close) = if self.rest().starts_with("((") {
            (2, "))")
        } else if self.rest().starts_with("([") {
            (2, "])")
        } else if self.rest().starts_with("[(") {
            (2, ")]")
        } else if self.rest().starts_with("[[") {
            (2, "]]")
        } else if self.rest().starts_with("{{") {
            (2, "}}")
        } else if self.rest().starts_with('[') {
            (1, "]")
        } else if self.rest().starts_with('(') {
            (1, ")")
        } else if self.rest().starts_with('{') {
            (1, "}")
        } else {
            return Err("unrecognized diagram line".to_string());
        };
        self.i += open;
        self.read_until(close)
    }

    fn read_until(&mut self, closer: &str) -> Result<String, String> {
        let start = self.i;
        let mut quoted = false;
        while !self.eof() {
            if self.peek() == Some('"') {
                quoted = !quoted;
                self.bump();
                continue;
            }
            if !quoted && self.rest().starts_with(closer) {
                let text = self.s[start..self.i].to_string();
                self.i += closer.len();
                return Ok(unquote_label(&text));
            }
            self.bump();
        }
        Err(format!("a node label is missing its closing {closer}"))
    }

    fn parse_edge(&mut self) -> Result<ParsedEdge, String> {
        let reversed = if self.rest().starts_with('<') && self.at_edge() {
            self.bump();
            true
        } else {
            false
        };
        if !self.at_stroke() {
            return Err("unrecognized diagram line".to_string());
        }
        self.consume_stroke();
        let mut forward = false;
        if self.peek() == Some('>') {
            self.bump();
            forward = true;
        }
        if reversed && forward {
            return Err("an edge points both ways".to_string());
        }
        self.skip_ws();
        let label = if self.peek() == Some('|') {
            self.read_bar_label()?
        } else if !forward && !self.eof() && !self.at_edge() {
            let start = self.i;
            while !self.eof() && !self.at_edge() {
                self.bump();
            }
            if !self.at_edge() {
                return Err("an edge has no direction".to_string());
            }
            let text = self.s[start..self.i].trim().to_string();
            if self.rest().starts_with('<') {
                return Err("an edge points both ways".to_string());
            }
            self.consume_stroke();
            if self.peek() != Some('>') {
                return Err("an edge has no direction".to_string());
            }
            self.bump();
            forward = true;
            text
        } else {
            String::new()
        };
        let label = unquote_label(&label);
        if label.is_empty() {
            return Err("an edge has no label".to_string());
        }
        if !forward && !reversed {
            return Err("an edge has no direction".to_string());
        }
        Ok(ParsedEdge { reversed, label })
    }

    fn read_bar_label(&mut self) -> Result<String, String> {
        self.bump();
        let start = self.i;
        while let Some(c) = self.peek() {
            if c == '|' {
                let text = self.s[start..self.i].to_string();
                self.bump();
                return Ok(text);
            }
            self.bump();
        }
        Err("an edge label is missing its closing |".to_string())
    }
}

fn is_id_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '[' | ']' | '(' | ')' | '{' | '}' | '|' | '"' | ';' | ',' | ':'
        )
}

fn unquote_label(text: &str) -> String {
    let text = text.trim();
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text[1..text.len() - 1]
            .replace("\\\"", "\"")
            .replace("#quot;", "\"")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align::{mismatch, model_from};

    const BRIEF: &str = r#"
(name Frontend)
(name Backend)
(sense depends_on "{X} depends on {Y}")
(assume (depends_on Frontend Backend))
"#;

    const PICTURE: &str = r#"
%% a sketch
%% (sense depends_on "{X} depends on {Y}")
flowchart LR
  classDef box fill:#fff
  subgraph group ["group"]
    n1["Frontend"]:::box -->|depends_on| n2["Backend"]
  end
  linkStyle 0 stroke:#333
  class n1 box
"#;

    #[test]
    fn a_flowchart_aligns_with_the_brief_it_draws() {
        let picture = model_from(&lower_mermaid(PICTURE).unwrap());
        assert!(mismatch(&model_from(BRIEF), &picture).is_none());
    }

    #[test]
    fn a_reading_on_the_arrow_states_the_sentence_and_not_the_sense() {
        let picture = "\
flowchart LR
  Frontend -->|depends on| Backend
";
        let diff = mismatch(
            &model_from(BRIEF),
            &model_from(&lower_mermaid(picture).unwrap()),
        )
        .unwrap();
        assert_eq!(
            diff,
            "\
senses
  only in left
    depends_on \"{X} depends on {Y}\"
"
        );
    }

    #[test]
    fn a_sense_name_on_an_arrow_needs_its_declaration() {
        let picture = "flowchart LR\n  Frontend -->|depends_on| Backend\n";
        let lowered = lower_mermaid(picture).unwrap();
        let err = crate::compile(&crate::parse(&lowered).unwrap()).unwrap_err();
        assert!(err.to_string().contains("depends_on"), "{err}\n{lowered}");
    }

    #[test]
    fn an_extra_node_is_a_name_only_the_picture_declares() {
        let picture = "\
%% (sense depends_on \"{X} depends on {Y}\")
flowchart LR
  Frontend -->|depends_on| Backend
  Cache
";
        let diff = mismatch(
            &model_from(BRIEF),
            &model_from(&lower_mermaid(picture).unwrap()),
        )
        .unwrap();
        assert_eq!(
            diff,
            "\
names
  only in right
    Cache
"
        );
    }

    #[test]
    fn an_unknown_line_is_refused() {
        let err = lower_mermaid("flowchart LR\n  click Frontend cb\n").unwrap_err();
        assert!(err.to_string().contains("click Frontend cb"), "{err}");
    }

    #[test]
    fn an_unlabeled_edge_is_refused() {
        let err = lower_mermaid("flowchart LR\n  Frontend --> Backend\n").unwrap_err();
        assert!(err.to_string().contains("no label"), "{err}");
    }

    #[test]
    fn a_negated_edge_is_refused() {
        let err =
            lower_mermaid("flowchart LR\n  Backend -->|not depends_on| Frontend\n").unwrap_err();
        assert!(err.to_string().contains("cannot be assumed"), "{err}");
    }

    #[test]
    fn a_node_label_resolves_to_a_term_sense() {
        let brief = r#"
(name LogicReasoner)
(sense LogicOutputTables "the tables named in a logic block's output_table_names")
(sense creates "{reasoner} plans deploy units that create {object}")
(assume (creates LogicReasoner (LogicOutputTables)))
"#;
        let picture = r#"
%% (sense creates "{reasoner} plans deploy units that create {object}")
flowchart LR
  LogicReasoner -->|creates| LogicOutputTables
"#;
        let diff = mismatch(
            &model_from(brief),
            &model_from(&lower_mermaid(picture).unwrap()),
        );
        assert!(diff.is_none(), "{diff:?}");
    }

    #[test]
    fn a_sentence_sense_does_not_resolve_a_node_label() {
        let brief = "(sense lemma \"The square root of two is irrational.\")\n(assume (lemma))\n";
        let picture = "flowchart LR\n  lemma\n";
        let diff = mismatch(
            &model_from(brief),
            &model_from(&lower_mermaid(picture).unwrap()),
        )
        .unwrap();
        assert!(diff.contains("only in right\n    lemma\n"), "{diff}");
        assert!(
            diff.contains("lemma \"The square root of two is irrational.\""),
            "{diff}"
        );
    }
}
