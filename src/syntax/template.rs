#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplatePart {
    Literal(String),
    Hole(String),
}

/// Split a reading into literal text and `{hole}` labels, in order.
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
                    return Err("nested '{' in reading".into());
                }
                i += 1;
            }
            if i >= chars.len() {
                return Err("unmatched '{' in reading".into());
            }
            let name: String = chars[start..i].iter().collect();
            if name.is_empty() {
                return Err("empty `{}` in reading".into());
            }
            if !is_hole_name(&name) {
                return Err(format!("invalid hole name `{name}` in reading"));
            }
            parts.push(TemplatePart::Hole(name));
            i += 1;
        } else if chars[i] == '}' {
            return Err("unmatched '}' in reading".into());
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

pub fn hole_names(src: &str) -> Result<Vec<String>, String> {
    Ok(parse_template(src)?
        .into_iter()
        .filter_map(|part| match part {
            TemplatePart::Hole(name) => Some(name),
            TemplatePart::Literal(_) => None,
        })
        .collect())
}

/// Fill holes left to right. `args` must match the number of holes.
pub fn apply(template: &str, args: &[String]) -> Result<String, String> {
    let parts = parse_template(template)?;
    let holes = parts
        .iter()
        .filter(|part| matches!(part, TemplatePart::Hole(_)))
        .count();
    if holes != args.len() {
        return Err(format!(
            "reading has {holes} {} but was applied to {} {}",
            holes_word(holes),
            args.len(),
            names_word(args.len())
        ));
    }
    let mut arg = 0;
    let mut out = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(text) => out.push_str(&text),
            TemplatePart::Hole(_) => {
                out.push_str(&args[arg]);
                arg += 1;
            }
        }
    }
    Ok(out)
}

pub fn holes_word(n: usize) -> &'static str {
    if n == 1 {
        "hole"
    } else {
        "holes"
    }
}

pub fn names_word(n: usize) -> &'static str {
    if n == 1 {
        "name"
    } else {
        "names"
    }
}

fn is_hole_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Quote a string the way the printer does.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_holes_left_to_right() {
        let sentence = apply("{X} depends on {Y}", &["aaa".into(), "bbb".into()]).unwrap();
        assert_eq!(sentence, "aaa depends on bbb");
        let sentence = apply("{X} is deprecated", &["aaa".into()]).unwrap();
        assert_eq!(sentence, "aaa is deprecated");
        let sentence = apply("Oracle calls the TypeSafe API", &[]).unwrap();
        assert_eq!(sentence, "Oracle calls the TypeSafe API");
    }

    #[test]
    fn repeated_labels_are_positional() {
        let holes = hole_names("{Name} is a part of {Name}").unwrap();
        assert_eq!(holes, vec!["Name", "Name"]);
        let sentence = apply("{Name} is a part of {Name}", &["X".into(), "Y".into()]).unwrap();
        assert_eq!(sentence, "X is a part of Y");
    }

    #[test]
    fn role_labels_do_not_reorder_arguments() {
        let sentence = apply(
            "{borrower} borrows from {lender}",
            &["Ada".into(), "Grace".into()],
        )
        .unwrap();
        assert_eq!(sentence, "Ada borrows from Grace");
    }

    #[test]
    fn rejects_a_broken_hole() {
        let err = parse_template("{Name is broken").unwrap_err();
        assert!(err.contains("unmatched"));
    }
}
