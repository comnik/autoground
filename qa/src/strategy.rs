use std::collections::HashMap;
use std::fs;
use std::path::Path;

use autoground::check::strategy::{
    DefaultStrategy, Statement, Strategy, Verdict, DOCUMENT_QUESTION, IMPLICATION_QUESTION,
    JOINT_QUESTION, PART_QUESTION,
};
use autoground::confidence::SUPPORT;
use autoground::oracle::{Answer, Classification, QuestionKind, State};
use autoground::syntax::template::{parse_template, TemplatePart};
use serde::Deserialize;
use serde_json::{json, Map, Value as Json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleLayout {
    /// Production behavior: filled string only, no role properties.
    GroundedOnly,
    /// Unfilled template in `proposition`, role assignments in `instructions.roles`.
    Nested,
    /// Unfilled template in `proposition`, each role as a top-level property of `instructions`.
    Flat,
    /// Filled string in `proposition`, plus `template` and `roles` attached alongside.
    Augmented,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HoleSyntax {
    /// Keep `{hole}` placeholders as written in the template.
    Braces,
    /// Render each hole as bare backticked key name (`` `hole` ``), matching Jev's field-reference style.
    BacktickRef,
    /// Strip all literal backticks and render ONLY holes as `` `hole` ``.
    BacktickRefClean,
    /// Strip all literal backticks and render ONLY holes as `` `roles.hole` ``.
    BacktickRolesPath,
}

#[derive(Debug, Clone)]
pub struct ConfigurableStrategy {
    pub name: String,
    pub role_layout: RoleLayout,
    pub hole_syntax: HoleSyntax,
    pub strip_backticks: bool,
    pub threshold: f64,
    pub document_question: String,
    pub part_question: String,
    pub joint_question: String,
    pub implication_question: String,
    pub document_criteria: Json,
    pub joint_criteria: Json,
    pub implication_criteria: Json,
    /// When true and `role_layout != GroundedOnly`, implication premises with roles
    /// are also serialized as structured objects in `state.sentences`.
    pub structured_premises: bool,
}

impl ConfigurableStrategy {
    pub fn preset(name: &str) -> Option<Self> {
        let mut base = Self {
            name: "default".into(),
            role_layout: RoleLayout::GroundedOnly,
            hole_syntax: HoleSyntax::Braces,
            strip_backticks: false,
            threshold: SUPPORT,
            document_question: DOCUMENT_QUESTION.into(),
            part_question: PART_QUESTION.into(),
            joint_question: JOINT_QUESTION.into(),
            implication_question: IMPLICATION_QUESTION.into(),
            document_criteria: DefaultStrategy::document_criteria(),
            joint_criteria: DefaultStrategy::document_criteria(),
            implication_criteria: DefaultStrategy::implication_criteria(),
            structured_premises: false,
        };
        match name {
            "default" => Some(base),
            "default-nobackticks" => {
                base.name = "default-nobackticks".into();
                base.strip_backticks = true;
                Some(base)
            }
            "roles-nested" => {
                base.name = "roles-nested".into();
                base.role_layout = RoleLayout::Nested;
                Some(base)
            }
            "roles-nested-nobackticks" => {
                base.name = "roles-nested-nobackticks".into();
                base.role_layout = RoleLayout::Nested;
                base.strip_backticks = true;
                Some(base)
            }
            "roles-nested-ref" => {
                base.name = "roles-nested-ref".into();
                base.role_layout = RoleLayout::Nested;
                base.hole_syntax = HoleSyntax::BacktickRef;
                Some(base)
            }
            "roles-nested-ref-clean" => {
                base.name = "roles-nested-ref-clean".into();
                base.role_layout = RoleLayout::Nested;
                base.hole_syntax = HoleSyntax::BacktickRefClean;
                Some(base)
            }
            "roles-nested-dotted" => {
                base.name = "roles-nested-dotted".into();
                base.role_layout = RoleLayout::Nested;
                base.hole_syntax = HoleSyntax::BacktickRolesPath;
                Some(base)
            }
            "roles-flat" => {
                base.name = "roles-flat".into();
                base.role_layout = RoleLayout::Flat;
                Some(base)
            }
            "roles-flat-nobackticks" => {
                base.name = "roles-flat-nobackticks".into();
                base.role_layout = RoleLayout::Flat;
                base.strip_backticks = true;
                Some(base)
            }
            "roles-flat-ref" => {
                base.name = "roles-flat-ref".into();
                base.role_layout = RoleLayout::Flat;
                base.hole_syntax = HoleSyntax::BacktickRef;
                Some(base)
            }
            "roles-augmented" => {
                base.name = "roles-augmented".into();
                base.role_layout = RoleLayout::Augmented;
                Some(base)
            }
            "roles-nested-premises" => {
                base.name = "roles-nested-premises".into();
                base.role_layout = RoleLayout::Nested;
                base.structured_premises = true;
                Some(base)
            }
            _ => None,
        }
    }

    pub fn builtins() -> &'static [(&'static str, &'static str)] {
        &[
            ("default", "exact production phrasing from autoground check"),
            (
                "default-nobackticks",
                "default phrasing with backticks stripped from `proposition`",
            ),
            (
                "roles-nested",
                "template in `proposition` + role assignments in `instructions.roles`",
            ),
            (
                "roles-nested-nobackticks",
                "roles-nested with backticks stripped from `proposition`",
            ),
            (
                "roles-nested-ref",
                "roles-nested with `{hole}` rendered as bare `` `hole` `` references",
            ),
            (
                "roles-nested-ref-clean",
                "roles-nested with literal backticks stripped so only `` `hole` `` refs have backticks",
            ),
            (
                "roles-nested-dotted",
                "roles-nested with literal backticks stripped and holes rendered as `` `roles.hole` ``",
            ),
            (
                "roles-flat",
                "template in `proposition` + each role as a top-level property of `instructions`",
            ),
            (
                "roles-flat-nobackticks",
                "roles-flat with backticks stripped from `proposition`",
            ),
            (
                "roles-flat-ref",
                "roles-flat with `{hole}` rendered as bare `` `hole` `` references",
            ),
            (
                "roles-augmented",
                "filled `proposition` + `template` and `roles` attached alongside",
            ),
            (
                "roles-nested-premises",
                "roles-nested, plus structured `{template, roles}` premises in implication state",
            ),
        ]
    }

    pub fn load(spec: &str) -> Result<Box<dyn Strategy>, String> {
        if spec == "default" {
            return Ok(Box::new(DefaultStrategy));
        }
        if let Some(preset) = Self::preset(spec) {
            return Ok(Box::new(preset));
        }
        let path = Path::new(spec);
        if path.is_file() {
            let text = fs::read_to_string(path)
                .map_err(|err| format!("failed to read strategy {}: {err}", path.display()))?;
            let overlay: StrategyOverlay = toml::from_str(&text)
                .map_err(|err| format!("failed to parse strategy {}: {err}", path.display()))?;
            return Ok(Box::new(Self::from_overlay(overlay)?));
        }
        Err(format!(
            "unknown strategy `{spec}` (not a built-in preset or readable TOML file)"
        ))
    }

    fn from_overlay(overlay: StrategyOverlay) -> Result<Self, String> {
        let base_name = overlay.base.as_deref().unwrap_or("default");
        let mut s = Self::preset(base_name)
            .ok_or_else(|| format!("unknown base strategy `{base_name}` in overlay"))?;
        if let Some(name) = overlay.name {
            s.name = name;
        }
        if let Some(threshold) = overlay.threshold {
            s.threshold = threshold;
        }
        if let Some(q) = overlay.document_question {
            s.document_question = q;
        }
        if let Some(q) = overlay.part_question {
            s.part_question = q;
        }
        if let Some(q) = overlay.joint_question {
            s.joint_question = q;
        }
        if let Some(q) = overlay.implication_question {
            s.implication_question = q;
        }
        if let Some(c) = overlay.document_criteria {
            s.document_criteria = c.into_json();
        }
        if let Some(c) = overlay.joint_criteria {
            s.joint_criteria = c.into_json();
        }
        if let Some(c) = overlay.implication_criteria {
            s.implication_criteria = c.into_json();
        }
        if let Some(sp) = overlay.structured_premises {
            s.structured_premises = sp;
        }
        if let Some(sb) = overlay.strip_backticks {
            s.strip_backticks = sb;
        }
        if let Some(hs) = overlay.hole_syntax {
            s.hole_syntax = hs;
        }
        Ok(s)
    }

    fn format_template(&self, template: &str) -> String {
        let rendered = match self.hole_syntax {
            HoleSyntax::Braces => template.to_string(),
            HoleSyntax::BacktickRef => render_backtick_refs(template, false, ""),
            HoleSyntax::BacktickRefClean => render_backtick_refs(template, true, ""),
            HoleSyntax::BacktickRolesPath => render_backtick_refs(template, true, "roles."),
        };
        if self.strip_backticks {
            rendered.replace('`', "")
        } else {
            rendered
        }
    }

    fn build_instructions(&self, statement: &Statement, question: &str) -> Json {
        let mut map = Map::new();
        map.insert("question".into(), json!(question));

        let grounded = if self.strip_backticks {
            statement.grounded.replace('`', "")
        } else {
            statement.grounded.clone()
        };

        let has_roles = statement.template.is_some() && !statement.roles.is_empty();
        if !has_roles || self.role_layout == RoleLayout::GroundedOnly {
            map.insert("proposition".into(), json!(grounded));
            return Json::Object(map);
        }

        let raw_template = statement.template.as_deref().unwrap();
        let (template, unique_roles) = disambiguate_roles(raw_template, &statement.roles);
        let formatted_template = self.format_template(&template);

        match self.role_layout {
            RoleLayout::GroundedOnly => unreachable!(),
            RoleLayout::Nested => {
                map.insert("proposition".into(), json!(formatted_template));
                let mut roles_obj = Map::new();
                for (k, v) in unique_roles {
                    roles_obj.insert(k, json!(v));
                }
                map.insert("roles".into(), Json::Object(roles_obj));
            }
            RoleLayout::Flat => {
                let mut overflow_roles = Map::new();
                for (k, v) in unique_roles {
                    if k == "proposition" || k == "question" || k == "roles" {
                        overflow_roles.insert(k, json!(v));
                    } else {
                        map.insert(k, json!(v));
                    }
                }
                if !overflow_roles.is_empty() {
                    map.insert("roles".into(), Json::Object(overflow_roles));
                }
                map.insert("proposition".into(), json!(formatted_template));
            }
            RoleLayout::Augmented => {
                map.insert("proposition".into(), json!(grounded));
                map.insert("template".into(), json!(formatted_template));
                let mut roles_obj = Map::new();
                for (k, v) in unique_roles {
                    roles_obj.insert(k, json!(v));
                }
                map.insert("roles".into(), Json::Object(roles_obj));
            }
        }
        Json::Object(map)
    }
}

/// Render `{hole}` as bare `` `hole` ``, avoiding double backticks when the
/// template already wrapped the hole in backticks (`` `{hole}` `` -> `` `hole` ``).
/// When `strip_literal_backticks` is true, backticks are removed from literal
/// spans so only role references carry backticks.
fn render_backtick_refs(template: &str, strip_literal_backticks: bool, prefix: &str) -> String {
    let Ok(parts) = parse_template(template) else {
        return template.to_string();
    };
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        match part {
            TemplatePart::Literal(lit) => {
                if strip_literal_backticks {
                    out.push_str(&lit.replace('`', ""));
                } else {
                    let mut s = lit.as_str();
                    if i > 0 && matches!(parts[i - 1], TemplatePart::Hole(_)) {
                        s = s.strip_prefix('`').unwrap_or(s);
                    }
                    if i + 1 < parts.len() && matches!(parts[i + 1], TemplatePart::Hole(_)) {
                        s = s.strip_suffix('`').unwrap_or(s);
                    }
                    out.push_str(s);
                }
            }
            TemplatePart::Hole(hole) => {
                out.push('`');
                out.push_str(prefix);
                out.push_str(hole);
                out.push('`');
            }
        }
    }
    out
}

/// Give repeated hole labels distinct property names (`Name_1`, `Name_2`) and
/// rewrite the template placeholders to match, while leaving unique labels as-is.
fn disambiguate_roles(template: &str, roles: &[(String, String)]) -> (String, Vec<(String, String)>) {
    let mut totals: HashMap<&str, usize> = HashMap::new();
    for (k, _) in roles {
        *totals.entry(k.as_str()).or_insert(0) += 1;
    }
    if totals.values().all(|&count| count == 1) {
        return (template.to_string(), roles.to_vec());
    }
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let unique_roles: Vec<(String, String)> = roles
        .iter()
        .map(|(k, v)| {
            if totals[k.as_str()] > 1 {
                let n = seen.entry(k.as_str()).or_insert(0);
                *n += 1;
                (format!("{k}_{n}"), v.clone())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();

    let Ok(parts) = parse_template(template) else {
        return (template.to_string(), unique_roles);
    };
    let mut out = String::new();
    let mut idx = 0;
    for part in parts {
        match part {
            TemplatePart::Literal(lit) => out.push_str(&lit),
            TemplatePart::Hole(hole) => {
                let name = unique_roles
                    .get(idx)
                    .map(|(k, _)| k.as_str())
                    .unwrap_or(hole.as_str());
                out.push('{');
                out.push_str(name);
                out.push('}');
                idx += 1;
            }
        }
    }
    (out, unique_roles)
}

impl Strategy for ConfigurableStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn document(
        &self,
        statement: &Statement,
        path: &str,
        body: &str,
        part: Option<(u32, u32)>,
    ) -> Vec<Classification> {
        let state = match part {
            Some((index, total)) => State::new(json!({
                "path": path,
                "body": body,
                "part": index,
                "parts": total,
            })),
            None => State::new(json!({
                "path": path,
                "body": body,
            })),
        };
        let q = if part.is_some() {
            &self.part_question
        } else {
            &self.document_question
        };
        vec![Classification {
            state,
            kind: QuestionKind::Choice,
            instructions: self.build_instructions(statement, q),
            criteria: self.document_criteria.clone(),
        }]
    }

    fn joint(&self, statement: &Statement, files: &[(String, String)]) -> Vec<Classification> {
        let bodies: Vec<Json> = files
            .iter()
            .map(|(path, body)| json!({ "path": path, "body": body }))
            .collect();
        vec![Classification {
            state: State::new(json!({ "files": bodies })),
            kind: QuestionKind::Choice,
            instructions: self.build_instructions(statement, &self.joint_question),
            criteria: self.joint_criteria.clone(),
        }]
    }

    fn implication(&self, head: &Statement, premises: &[Statement]) -> Vec<Classification> {
        let sentences: Vec<Json> = premises
            .iter()
            .map(|p| {
                if self.structured_premises && p.template.is_some() && !p.roles.is_empty() {
                    let (tmpl, unique) =
                        disambiguate_roles(p.template.as_deref().unwrap(), &p.roles);
                    let mut roles_obj = Map::new();
                    for (k, v) in unique {
                        roles_obj.insert(k, json!(v));
                    }
                    json!({ "template": tmpl, "roles": roles_obj })
                } else {
                    json!(p.grounded)
                }
            })
            .collect();
        vec![Classification {
            state: State::new(json!({ "sentences": sentences })),
            kind: QuestionKind::Choice,
            instructions: self.build_instructions(head, &self.implication_question),
            criteria: self.implication_criteria.clone(),
        }]
    }

    fn settle(&self, answers: &[Answer]) -> Verdict {
        let Some(answer) = answers.first() else {
            return Verdict {
                kind: autoground::check::strategy::VerdictKind::Neutral,
                confidence: 0.0,
                probabilities: None,
                settled: None,
            };
        };
        DefaultStrategy::settle_choice(answer, self.threshold)
    }
}

#[derive(Debug, Deserialize)]
struct StrategyOverlay {
    name: Option<String>,
    base: Option<String>,
    threshold: Option<f64>,
    document_question: Option<String>,
    part_question: Option<String>,
    joint_question: Option<String>,
    implication_question: Option<String>,
    document_criteria: Option<CriteriaOverlay>,
    joint_criteria: Option<CriteriaOverlay>,
    implication_criteria: Option<CriteriaOverlay>,
    structured_premises: Option<bool>,
    strip_backticks: Option<bool>,
    hole_syntax: Option<HoleSyntax>,
}

#[derive(Debug, Deserialize)]
struct CriteriaOverlay {
    entailed: String,
    contradicted: String,
    neutral: String,
}

impl CriteriaOverlay {
    fn into_json(self) -> Json {
        json!({
            "entailed": self.entailed,
            "contradicted": self.contradicted,
            "neutral": self.neutral,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_nested_and_flat_emit_structured_role_properties() {
        let stmt = Statement::from_template(
            "the `{interface}` trait is implemented by `{implementation}`",
            vec![
                ("interface".into(), "Oracle".into()),
                ("implementation".into(), "JevOracle".into()),
            ],
        )
        .unwrap();

        let nested = ConfigurableStrategy::preset("roles-nested").unwrap();
        let req = &nested.document(&stmt, "src/oracle/jev.rs", "body", None)[0];
        assert_eq!(
            req.instructions["proposition"],
            "the `{interface}` trait is implemented by `{implementation}`"
        );
        assert_eq!(req.instructions["roles"]["interface"], "Oracle");
        assert_eq!(req.instructions["roles"]["implementation"], "JevOracle");

        let flat = ConfigurableStrategy::preset("roles-flat").unwrap();
        let req = &flat.document(&stmt, "src/oracle/jev.rs", "body", None)[0];
        assert_eq!(
            req.instructions["proposition"],
            "the `{interface}` trait is implemented by `{implementation}`"
        );
        assert_eq!(req.instructions["interface"], "Oracle");
        assert_eq!(req.instructions["implementation"], "JevOracle");
    }

    #[test]
    fn repeated_holes_are_disambiguated() {
        let stmt = Statement::from_template(
            "{Name} is a part of {Name}",
            vec![("Name".into(), "A".into()), ("Name".into(), "B".into())],
        )
        .unwrap();
        let nested = ConfigurableStrategy::preset("roles-nested").unwrap();
        let req = &nested.document(&stmt, "a.rs", "body", None)[0];
        assert_eq!(req.instructions["proposition"], "{Name_1} is a part of {Name_2}");
        assert_eq!(req.instructions["roles"]["Name_1"], "A");
        assert_eq!(req.instructions["roles"]["Name_2"], "B");
    }
}
