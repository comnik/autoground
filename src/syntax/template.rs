/// Builtin sort. Arguments of type `String` print as the value itself.
pub const STRING: &str = "String";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplatePart {
    Literal(String),
    Slot(String),
}

pub fn is_builtin_sort(name: &str) -> bool {
    name == STRING
}

/// Parse `{Type}` slots out of a verbalization template.
pub fn parse_template(src: &str) -> Result<Vec<TemplatePart>, String> {
    let mut parts = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut lit = String::new();
    while i < chars.len() {
        if chars[i] == '{' {
            if !lit.is_empty() {
                parts.push(TemplatePart::Literal(std::mem::take(&mut lit)));
            }
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '}' {
                if chars[i] == '{' {
                    return Err("nested '{' in verbalization template".into());
                }
                i += 1;
            }
            if i >= chars.len() {
                return Err("unmatched '{' in verbalization template".into());
            }
            let name: String = chars[start..i].iter().collect();
            if name.is_empty() {
                return Err("empty `{}` in verbalization template".into());
            }
            if !is_type_name(&name) {
                return Err(format!(
                    "invalid type name `{name}` in verbalization template"
                ));
            }
            parts.push(TemplatePart::Slot(name));
            i += 1;
        } else if chars[i] == '}' {
            return Err("unmatched '}' in verbalization template".into());
        } else {
            lit.push(chars[i]);
            i += 1;
        }
    }
    if !lit.is_empty() {
        parts.push(TemplatePart::Literal(lit));
    }
    Ok(parts)
}

fn is_type_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn template_slots(parts: &[TemplatePart]) -> Vec<String> {
    parts
        .iter()
        .filter_map(|p| match p {
            TemplatePart::Slot(s) => Some(s.clone()),
            TemplatePart::Literal(_) => None,
        })
        .collect()
}

/// Quote a constant the way the printer does.
pub fn quote_const(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}
