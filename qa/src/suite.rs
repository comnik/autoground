use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use autoground::check::strategy::{Statement, VerdictKind};
use autoground::syntax::template::hole_names;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeKind {
    Document,
    Part,
    Joint,
    Implication,
}

impl ProbeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeKind::Document => "document",
            ProbeKind::Part => "part",
            ProbeKind::Joint => "joint",
            ProbeKind::Implication => "implication",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProbeState {
    Document {
        path: String,
        body: String,
    },
    Part {
        path: String,
        body: String,
        part: u32,
        parts: u32,
    },
    Joint {
        files: Vec<(String, String)>,
    },
    Implication {
        premises: Vec<Statement>,
    },
}

impl ProbeState {
    pub fn kind(&self) -> ProbeKind {
        match self {
            ProbeState::Document { .. } => ProbeKind::Document,
            ProbeState::Part { .. } => ProbeKind::Part,
            ProbeState::Joint { .. } => ProbeKind::Joint,
            ProbeState::Implication { .. } => ProbeKind::Implication,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Probe {
    pub id: String,
    pub group_id: String,
    pub state: ProbeState,
    pub statement: Statement,
    pub expected: VerdictKind,
    pub tags: Vec<String>,
    pub negation_of: Option<String>,
    pub paraphrase_of: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SuiteFile {
    #[serde(default)]
    group: Vec<GroupSpec>,
}

#[derive(Debug, Deserialize)]
struct GroupSpec {
    id: String,
    kind: ProbeKind,
    path: Option<String>,
    body: Option<String>,
    lines: Option<[usize; 2]>,
    part: Option<u32>,
    parts: Option<u32>,
    #[serde(default)]
    files: Vec<FileSpec>,
    #[serde(default)]
    premises: Vec<StatementSpec>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    probes: Vec<ProbeSpec>,
}

#[derive(Debug, Deserialize)]
struct FileSpec {
    path: String,
    body: Option<String>,
    lines: Option<[usize; 2]>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StatementSpec {
    Plain(String),
    Structured {
        template: String,
        roles: RolesSpec,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RolesSpec {
    Pairs(Vec<[String; 2]>),
    Map(BTreeMap<String, String>),
}

#[derive(Debug, Deserialize)]
struct ProbeSpec {
    id: String,
    proposition: Option<String>,
    template: Option<String>,
    roles: Option<RolesSpec>,
    expected: VerdictKind,
    #[serde(default)]
    tags: Vec<String>,
    negation_of: Option<String>,
    paraphrase_of: Option<String>,
    notes: Option<String>,
}

pub fn load_suites(paths: &[PathBuf], repo_root: &Path) -> Result<Vec<Probe>, String> {
    let mut probes = Vec::new();
    let mut seen_ids = HashSet::new();
    for path in paths {
        let text = fs::read_to_string(path)
            .map_err(|err| format!("failed to read suite {}: {err}", path.display()))?;
        let file: SuiteFile = toml::from_str(&text)
            .map_err(|err| format!("failed to parse suite {}: {err}", path.display()))?;
        for group in file.group {
            let state = build_state(&group, repo_root)?;
            for spec in group.probes {
                if !seen_ids.insert(spec.id.clone()) {
                    return Err(format!("duplicate probe id `{}`", spec.id));
                }
                let statement = build_probe_statement(&spec)?;
                let mut tags = group.tags.clone();
                for tag in spec.tags {
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
                probes.push(Probe {
                    id: spec.id,
                    group_id: group.id.clone(),
                    state: state.clone(),
                    statement,
                    expected: spec.expected,
                    tags,
                    negation_of: spec.negation_of,
                    paraphrase_of: spec.paraphrase_of,
                    notes: spec.notes,
                });
            }
        }
    }
    Ok(probes)
}

fn build_state(group: &GroupSpec, repo_root: &Path) -> Result<ProbeState, String> {
    match group.kind {
        ProbeKind::Document => {
            let path = group
                .path
                .clone()
                .ok_or_else(|| format!("group `{}` (document) needs `path`", group.id))?;
            let body = load_body(repo_root, &path, group.body.as_deref(), group.lines)?;
            Ok(ProbeState::Document { path, body })
        }
        ProbeKind::Part => {
            let path = group
                .path
                .clone()
                .ok_or_else(|| format!("group `{}` (part) needs `path`", group.id))?;
            let part = group
                .part
                .ok_or_else(|| format!("group `{}` (part) needs `part`", group.id))?;
            let parts = group
                .parts
                .ok_or_else(|| format!("group `{}` (part) needs `parts`", group.id))?;
            let body = load_body(repo_root, &path, group.body.as_deref(), group.lines)?;
            Ok(ProbeState::Part {
                path,
                body,
                part,
                parts,
            })
        }
        ProbeKind::Joint => {
            if group.files.is_empty() {
                return Err(format!("group `{}` (joint) needs `files`", group.id));
            }
            let mut files = Vec::with_capacity(group.files.len());
            for file in &group.files {
                let body = load_body(repo_root, &file.path, file.body.as_deref(), file.lines)?;
                files.push((file.path.clone(), body));
            }
            Ok(ProbeState::Joint { files })
        }
        ProbeKind::Implication => {
            if group.premises.is_empty() {
                return Err(format!(
                    "group `{}` (implication) needs `premises`",
                    group.id
                ));
            }
            let mut premises = Vec::with_capacity(group.premises.len());
            for spec in &group.premises {
                premises.push(build_statement_spec(spec)?);
            }
            Ok(ProbeState::Implication { premises })
        }
    }
}

fn load_body(
    repo_root: &Path,
    path: &str,
    inline: Option<&str>,
    lines: Option<[usize; 2]>,
) -> Result<String, String> {
    let raw = match inline {
        Some(body) => body.to_string(),
        None => {
            let full = repo_root.join(path);
            fs::read_to_string(&full)
                .map_err(|err| format!("failed to read witness `{path}` ({}): {err}", full.display()))?
        }
    };
    let Some([start, end]) = lines else {
        return Ok(raw);
    };
    if start == 0 || end < start {
        return Err(format!("invalid line range [{start}, {end}] for `{path}`"));
    }
    let selected: Vec<&str> = raw
        .lines()
        .skip(start - 1)
        .take(end - start + 1)
        .collect();
    if selected.is_empty() {
        return Err(format!(
            "line range [{start}, {end}] out of bounds for `{path}`"
        ));
    }
    let mut out = selected.join("\n");
    out.push('\n');
    Ok(out)
}

fn build_probe_statement(spec: &ProbeSpec) -> Result<Statement, String> {
    match (&spec.proposition, &spec.template, &spec.roles) {
        (Some(prop), None, None) => Ok(Statement::plain(prop.clone())),
        (None, Some(template), Some(roles)) => {
            let pairs = resolve_roles(template, roles)
                .map_err(|err| format!("probe `{}`: {err}", spec.id))?;
            Statement::from_template(template.clone(), pairs)
                .map_err(|err| format!("probe `{}`: {err}", spec.id))
        }
        (None, Some(template), None) => Statement::from_template(template.clone(), Vec::new())
            .map_err(|err| format!("probe `{}`: {err}", spec.id)),
        _ => Err(format!(
            "probe `{}` must set either `proposition` or `template` (with optional `roles`)",
            spec.id
        )),
    }
}

fn build_statement_spec(spec: &StatementSpec) -> Result<Statement, String> {
    match spec {
        StatementSpec::Plain(text) => Ok(Statement::plain(text.clone())),
        StatementSpec::Structured { template, roles } => {
            let pairs = resolve_roles(template, roles)?;
            Statement::from_template(template.clone(), pairs)
        }
    }
}

fn resolve_roles(template: &str, roles: &RolesSpec) -> Result<Vec<(String, String)>, String> {
    let holes = hole_names(template)?;
    match roles {
        RolesSpec::Pairs(pairs) => {
            if pairs.len() != holes.len() {
                return Err(format!(
                    "template has {} holes ({:?}) but {} role pairs were given",
                    holes.len(),
                    holes,
                    pairs.len()
                ));
            }
            let mut out = Vec::with_capacity(pairs.len());
            for (expected_hole, [given_hole, value]) in holes.iter().zip(pairs.iter()) {
                if expected_hole != given_hole {
                    return Err(format!(
                        "role `{given_hole}` does not match template hole `{expected_hole}`"
                    ));
                }
                out.push((given_hole.clone(), value.clone()));
            }
            Ok(out)
        }
        RolesSpec::Map(map) => {
            let mut seen = HashSet::new();
            for hole in &holes {
                if !seen.insert(hole) {
                    return Err(format!(
                        "duplicate hole `{hole}` requires positional `roles = [[hole, value], ...]`"
                    ));
                }
            }
            for key in map.keys() {
                if !seen.contains(key) {
                    return Err(format!("role `{key}` does not appear in template"));
                }
            }
            let mut out = Vec::with_capacity(holes.len());
            for hole in holes {
                let value = map
                    .get(&hole)
                    .ok_or_else(|| format!("missing role assignment for `{hole}`"))?;
                out.push((hole, value.clone()));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_map_roles_in_template_order() {
        let mut map = BTreeMap::new();
        map.insert("callee".into(), "Oracle".into());
        map.insert("caller".into(), "check".into());
        let pairs = resolve_roles("{caller} calls {callee}", &RolesSpec::Map(map)).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("caller".into(), "check".into()),
                ("callee".into(), "Oracle".into()),
            ]
        );
    }

    #[test]
    fn resolves_positional_roles_with_repeated_hole_names() {
        let pairs = resolve_roles(
            "{Name} is a part of {Name}",
            &RolesSpec::Pairs(vec![
                ["Name".into(), "A".into()],
                ["Name".into(), "B".into()],
            ]),
        )
        .unwrap();
        let stmt = Statement::from_template("{Name} is a part of {Name}", pairs).unwrap();
        assert_eq!(stmt.grounded, "A is a part of B");
    }
}
