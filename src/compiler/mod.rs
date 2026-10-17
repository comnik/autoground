use std::collections::{HashMap, HashSet};

use crate::syntax::template::{apply, holes_word, names_word};
use crate::syntax::{Arg, Form, Formula, Ident, Program, Proposition, Reading, Sense};

mod link;
mod scan;

pub use link::{link, link_parts};
pub use scan::{display_path, scan_namespaces, LoadError, Loaded};

/// Names and senses visible through `:as` and `:refer`.
pub(crate) struct Opened {
    names: HashSet<String>,
    senses: HashMap<String, Sense>,
}

pub(crate) struct Imports {
    names: HashSet<String>,
    senses: HashMap<String, Sense>,
    /// Alias or required namespace name, mapped to that namespace.
    qualifiers: HashMap<String, String>,
    opened: HashMap<String, Opened>,
}

impl Imports {
    pub(crate) fn none() -> Self {
        Self {
            names: HashSet::new(),
            senses: HashMap::new(),
            qualifiers: HashMap::new(),
            opened: HashMap::new(),
        }
    }
}

/// Where a resolved name was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameOrigin {
    Declared,
    Imported,
}

/// Names and senses a file can see through `:as` and `:refer`.
///
/// Local declarations stay with the file. This is only what other namespaces
/// contribute, plus enough of each required namespace to resolve a qualifier.
#[derive(Debug, Clone)]
pub struct Vocabulary {
    names: HashSet<String>,
    senses: HashMap<String, Sense>,
    qualifiers: HashMap<String, String>,
    opened_names: HashMap<String, HashSet<String>>,
    opened_senses: HashMap<String, HashMap<String, Sense>>,
}

impl Vocabulary {
    pub fn none() -> Self {
        Self {
            names: HashSet::new(),
            senses: HashMap::new(),
            qualifiers: HashMap::new(),
            opened_names: HashMap::new(),
            opened_senses: HashMap::new(),
        }
    }

    pub(crate) fn from_imports(imports: &Imports) -> Self {
        Self {
            names: imports.names.clone(),
            senses: imports.senses.clone(),
            qualifiers: imports.qualifiers.clone(),
            opened_names: imports
                .opened
                .iter()
                .map(|(ns, opened)| (ns.clone(), opened.names.clone()))
                .collect(),
            opened_senses: imports
                .opened
                .iter()
                .map(|(ns, opened)| (ns.clone(), opened.senses.clone()))
                .collect(),
        }
    }

    pub fn imported_names(&self) -> &HashSet<String> {
        &self.names
    }

    pub fn imported_senses(&self) -> &HashMap<String, Sense> {
        &self.senses
    }

    /// `declared` is the names this file can see, not the imported ones.
    pub fn resolve_name(
        &self,
        ident: &Ident,
        declared: &HashSet<String>,
    ) -> Result<(String, NameOrigin), CompileError> {
        if let Some(qualifier) = &ident.qualifier {
            let namespace = self.required(qualifier)?;
            if !self
                .opened_names
                .get(&namespace)
                .is_some_and(|names| names.contains(&ident.name))
            {
                return Err(CompileError {
                    message: format!("`{}` is not a name in `{namespace}`", ident.name),
                });
            }
            return Ok((ident.name.clone(), NameOrigin::Imported));
        }
        if declared.contains(&ident.name) {
            return Ok((ident.name.clone(), NameOrigin::Declared));
        }
        if self.names.contains(&ident.name) {
            return Ok((ident.name.clone(), NameOrigin::Imported));
        }
        Err(CompileError {
            message: format!("`{}` is not a declared name", ident.name),
        })
    }

    /// `declared` is the senses this file can see, not the imported ones.
    pub fn resolve_sense(
        &self,
        ident: &Ident,
        declared: &HashMap<String, Sense>,
    ) -> Result<Sense, CompileError> {
        if let Some(qualifier) = &ident.qualifier {
            let namespace = self.required(qualifier)?;
            let Some(sense) = self
                .opened_senses
                .get(&namespace)
                .and_then(|senses| senses.get(&ident.name))
            else {
                return Err(CompileError {
                    message: format!("`{}` is not a sense in `{namespace}`", ident.name),
                });
            };
            return Ok(sense.clone());
        }
        if let Some(sense) = declared.get(&ident.name) {
            return Ok(sense.clone());
        }
        self.senses
            .get(&ident.name)
            .cloned()
            .ok_or_else(|| CompileError {
                message: format!("`{}` is not a declared sense", ident.name),
            })
    }

    fn required(&self, qualifier: &str) -> Result<String, CompileError> {
        self.qualifiers
            .get(qualifier)
            .cloned()
            .ok_or_else(|| CompileError {
                message: format!("`{qualifier}` is not a required namespace"),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub message: String,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CompileError {}

/// A model whose claims are sentences under `and`, `or`, `not`, and `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub names: Vec<String>,
    pub senses: Vec<Sense>,
    pub claims: Vec<Proposition>,
}

/// Resolve senses and check that every claim is a saturated ground sentence.
///
/// A program with `:require` clauses needs [`link`], which supplies the
/// imported names. On its own, `compile` reports the first required namespace
/// as missing.
pub fn compile(program: &Program) -> Result<Compiled, CompileError> {
    if let Some(ns) = &program.namespace {
        if let Some(require) = ns.requires.first() {
            return Err(CompileError {
                message: format!("namespace `{}` is not declared", require.namespace),
            });
        }
    }
    compile_with(program, &Imports::none())
}

pub(crate) fn compile_with(program: &Program, imports: &Imports) -> Result<Compiled, CompileError> {
    let mut names = Vec::new();
    let mut name_seen = HashSet::new();
    let mut senses = Vec::new();
    let mut sense_seen = HashSet::new();

    for form in &program.forms {
        match form {
            Form::Name(name) => {
                if !name_seen.insert(name.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate name `{name}`"),
                    });
                }
                names.push(name.clone());
            }
            Form::Sense(sense) => {
                if !sense_seen.insert(sense.name.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate sense `{}`", sense.name),
                    });
                }
                senses.push(sense.clone());
            }
            Form::Claim(_) => {}
        }
    }

    for name in &names {
        if sense_seen.contains(name) {
            return Err(CompileError {
                message: format!("`{name}` is declared as both a name and a sense"),
            });
        }
    }

    let mut sense_by_name: HashMap<String, Sense> = senses
        .iter()
        .map(|sense| (sense.name.clone(), sense.clone()))
        .collect();
    for (name, sense) in &imports.senses {
        sense_by_name
            .entry(name.clone())
            .or_insert_with(|| sense.clone());
    }
    let mut declared: HashSet<String> = names.iter().cloned().collect();
    declared.extend(imports.names.iter().cloned());

    let mut claims = Vec::new();
    for formula in program.claims() {
        claims.push(resolve(formula, &declared, &sense_by_name, imports)?);
    }
    check_details(&claims)?;

    Ok(Compiled {
        names,
        senses,
        claims,
    })
}

/// One detail defines a sentence. A cycle would ask that sentence to establish
/// itself.
fn check_details(claims: &[Proposition]) -> Result<(), CompileError> {
    let mut defs: HashMap<String, Vec<Proposition>> = HashMap::new();
    for claim in claims {
        collect_detail(claim, &mut defs)?;
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for head in defs.keys() {
        if let Some(sentence) = detail_cycle(head, &defs, &mut visiting, &mut visited) {
            return Err(CompileError {
                message: format!("`{sentence}` is detailed in terms of itself"),
            });
        }
    }
    Ok(())
}

fn collect_detail(
    proposition: &Proposition,
    defs: &mut HashMap<String, Vec<Proposition>>,
) -> Result<(), CompileError> {
    match proposition {
        Proposition::And(xs) | Proposition::Or(xs) => {
            for proposition in xs {
                collect_detail(proposition, defs)?;
            }
        }
        Proposition::Not(inner) => collect_detail(inner, defs)?,
        Proposition::Sentence(_) | Proposition::Witness { .. } => {}
        Proposition::Detail { head, body } => {
            if defs.contains_key(head) {
                return Err(CompileError {
                    message: format!("duplicate detail of `{head}`"),
                });
            }
            defs.insert(head.clone(), body.clone());
            for proposition in body {
                collect_detail(proposition, defs)?;
            }
        }
    }
    Ok(())
}

fn detail_cycle(
    head: &str,
    defs: &HashMap<String, Vec<Proposition>>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) -> Option<String> {
    if visited.contains(head) {
        return None;
    }
    if !visiting.insert(head.to_string()) {
        return Some(head.to_string());
    }
    if let Some(body) = defs.get(head) {
        for dep in detail_deps(body, defs) {
            if let Some(sentence) = detail_cycle(&dep, defs, visiting, visited) {
                return Some(sentence);
            }
        }
    }
    visiting.remove(head);
    visited.insert(head.to_string());
    None
}

fn detail_deps(body: &[Proposition], defs: &HashMap<String, Vec<Proposition>>) -> Vec<String> {
    let mut deps = Vec::new();
    for proposition in body {
        push_detail_deps(proposition, defs, &mut deps);
    }
    deps
}

fn push_detail_deps(
    proposition: &Proposition,
    defs: &HashMap<String, Vec<Proposition>>,
    deps: &mut Vec<String>,
) {
    match proposition {
        Proposition::Sentence(sentence) | Proposition::Witness { sentence, .. } => {
            if defs.contains_key(sentence) {
                deps.push(sentence.clone());
            }
        }
        Proposition::And(xs) | Proposition::Or(xs) => {
            for proposition in xs {
                push_detail_deps(proposition, defs, deps);
            }
        }
        Proposition::Not(inner) => push_detail_deps(inner, defs, deps),
        Proposition::Detail { head, body } => {
            if defs.contains_key(head) {
                deps.push(head.clone());
            }
            for proposition in body {
                push_detail_deps(proposition, defs, deps);
            }
        }
    }
}

fn resolve(
    formula: &Formula,
    names: &HashSet<String>,
    senses: &HashMap<String, Sense>,
    imports: &Imports,
) -> Result<Proposition, CompileError> {
    match formula {
        Formula::And(xs) => Ok(Proposition::And(resolve_all(xs, names, senses, imports)?)),
        Formula::Or(xs) => Ok(Proposition::Or(resolve_all(xs, names, senses, imports)?)),
        Formula::Not(x) => Ok(Proposition::Not(Box::new(resolve(
            x, names, senses, imports,
        )?))),
        Formula::Detail { head, body } => resolve_detail(head, body, names, senses, imports),
        Formula::Witness { fact, paths } => resolve_witness(fact, paths, names, senses, imports),
        Formula::Apply { reading, args } => {
            let mut spellings = Vec::with_capacity(args.len());
            for arg in args {
                spellings.push(resolve_arg(arg, names, imports)?);
            }
            let (what, template, holes) = match reading {
                Reading::Sense(ident) => {
                    let sense = resolve_sense(ident, senses, imports)?;
                    (
                        format!("`{}`", ident.display()),
                        sense.template,
                        sense.holes,
                    )
                }
                Reading::Inline { template, holes } => {
                    (format!("\"{template}\""), template.clone(), holes.clone())
                }
            };
            if spellings.len() != holes.len() {
                return Err(arity_error(&what, &holes, spellings.len()));
            }
            let sentence =
                apply(&template, &spellings).map_err(|message| CompileError { message })?;
            Ok(Proposition::Sentence(sentence))
        }
    }
}

/// Names and senses declared in `body` extend the enclosing scope for the head
/// and for the body's claims. They are not visible outside this detail.
fn resolve_detail(
    head: &Formula,
    body: &[Form],
    names: &HashSet<String>,
    senses: &HashMap<String, Sense>,
    imports: &Imports,
) -> Result<Proposition, CompileError> {
    let (names, senses) = extend_scope(body, names, senses)?;
    let Proposition::Sentence(head) = resolve(head, &names, &senses, imports)? else {
        return Err(CompileError {
            message: "`detail` heads a sentence".into(),
        });
    };
    let mut claims = Vec::new();
    for form in body {
        if let Form::Claim(formula) = form {
            claims.push(resolve(formula, &names, &senses, imports)?);
        }
    }
    if claims.is_empty() {
        return Err(CompileError {
            message: "a detail's body has no claim".into(),
        });
    }
    Ok(Proposition::Detail { head, body: claims })
}

fn resolve_witness(
    fact: &Formula,
    paths: &[String],
    names: &HashSet<String>,
    senses: &HashMap<String, Sense>,
    imports: &Imports,
) -> Result<Proposition, CompileError> {
    let Proposition::Sentence(sentence) = resolve(fact, names, senses, imports)? else {
        return Err(CompileError {
            message: "`witness` cites a sentence".into(),
        });
    };
    let mut seen = HashSet::new();
    for path in paths {
        if !is_brief_path(path) {
            return Err(CompileError {
                message: format!("`{path}` is not a path relative to the brief"),
            });
        }
        if !seen.insert(path.clone()) {
            return Err(CompileError {
                message: format!("witness cites `{path}` twice"),
            });
        }
    }
    Ok(Proposition::Witness {
        sentence,
        paths: paths.to_vec(),
    })
}

/// A witness path is relative to the brief file. `..` is a parent of that file.
fn is_brief_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return false;
    }
    let path = std::path::Path::new(path);
    path.is_relative()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_)
                    | std::path::Component::CurDir
                    | std::path::Component::ParentDir
            )
        })
}

/// Names and senses declared in `body`, added to the enclosing scope.
pub fn extend_scope(
    body: &[Form],
    names: &HashSet<String>,
    senses: &HashMap<String, Sense>,
) -> Result<(HashSet<String>, HashMap<String, Sense>), CompileError> {
    let mut names = names.clone();
    let mut senses = senses.clone();
    for form in body {
        match form {
            Form::Name(name) => {
                if !names.insert(name.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate name `{name}`"),
                    });
                }
                if senses.contains_key(name) {
                    return Err(CompileError {
                        message: format!("`{name}` is declared as both a name and a sense"),
                    });
                }
            }
            Form::Sense(sense) => {
                if senses.contains_key(&sense.name) {
                    return Err(CompileError {
                        message: format!("duplicate sense `{}`", sense.name),
                    });
                }
                if names.contains(&sense.name) {
                    return Err(CompileError {
                        message: format!("`{}` is declared as both a name and a sense", sense.name),
                    });
                }
                senses.insert(sense.name.clone(), sense.clone());
            }
            Form::Claim(_) => {}
        }
    }
    Ok((names, senses))
}

fn resolve_all(
    formulas: &[Formula],
    names: &HashSet<String>,
    senses: &HashMap<String, Sense>,
    imports: &Imports,
) -> Result<Vec<Proposition>, CompileError> {
    formulas
        .iter()
        .map(|formula| resolve(formula, names, senses, imports))
        .collect()
}

fn resolve_arg(
    arg: &Arg,
    names: &HashSet<String>,
    imports: &Imports,
) -> Result<String, CompileError> {
    match arg {
        Arg::Literal(text) => Ok(text.clone()),
        Arg::Name(ident) => resolve_name(ident, names, imports),
    }
}

fn resolve_name(
    ident: &Ident,
    names: &HashSet<String>,
    imports: &Imports,
) -> Result<String, CompileError> {
    if let Some(qualifier) = &ident.qualifier {
        let namespace = required(qualifier, imports)?;
        if !imports.opened[&namespace].names.contains(&ident.name) {
            return Err(CompileError {
                message: format!("`{}` is not a name in `{namespace}`", ident.name),
            });
        }
        return Ok(ident.name.clone());
    }
    if names.contains(&ident.name) {
        return Ok(ident.name.clone());
    }
    Err(CompileError {
        message: format!("`{}` is not a declared name", ident.name),
    })
}

fn resolve_sense(
    ident: &Ident,
    senses: &HashMap<String, Sense>,
    imports: &Imports,
) -> Result<Sense, CompileError> {
    if let Some(qualifier) = &ident.qualifier {
        let namespace = required(qualifier, imports)?;
        let Some(sense) = imports.opened[&namespace].senses.get(&ident.name) else {
            return Err(CompileError {
                message: format!("`{}` is not a sense in `{namespace}`", ident.name),
            });
        };
        return Ok(sense.clone());
    }
    senses
        .get(&ident.name)
        .cloned()
        .ok_or_else(|| CompileError {
            message: format!("`{}` is not a declared sense", ident.name),
        })
}

fn required(qualifier: &str, imports: &Imports) -> Result<String, CompileError> {
    imports
        .qualifiers
        .get(qualifier)
        .cloned()
        .ok_or_else(|| CompileError {
            message: format!("`{qualifier}` is not a required namespace"),
        })
}

fn arity_error(what: &str, holes: &[String], nargs: usize) -> CompileError {
    let listed = if holes.is_empty() {
        "0 holes".to_string()
    } else {
        let labels = holes
            .iter()
            .map(|hole| format!("{{{hole}}}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} {} ({labels})", holes.len(), holes_word(holes.len()))
    };
    CompileError {
        message: format!(
            "{what} has {listed} but was applied to {nargs} {}",
            names_word(nargs)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    fn compiled(src: &str) -> Compiled {
        compile(&parse(src).unwrap()).unwrap()
    }

    #[test]
    fn named_and_inline_applications_meet() {
        let c = compiled(
            r#"
(name X)
(name Y)
(sense part_of "{Name} is a part of {Name}")
(part_of X Y)
("{Name} is a part of {Name}" X Y)
"#,
        );
        assert_eq!(
            c.claims,
            vec![
                Proposition::Sentence("X is a part of Y".into()),
                Proposition::Sentence("X is a part of Y".into()),
            ]
        );
    }

    #[test]
    fn keeps_boolean_structure() {
        let c = compiled(
            r#"
(name Search)
(name Oracle)
(name Universe)
(sense calls "{caller} calls {callee}")
(and (calls Search Oracle) (not (calls Universe Oracle)))
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::And(vec![
                Proposition::Sentence("Search calls Oracle".into()),
                Proposition::Not(Box::new(Proposition::Sentence(
                    "Universe calls Oracle".into()
                ))),
            ])]
        );
    }

    #[test]
    fn declarations_may_follow_uses() {
        let c = compiled(
            r#"
(depends_on Frontend Backend)
(name Frontend)
(name Backend)
(sense depends_on "{X} depends on {Y}")
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Sentence("Frontend depends on Backend".into())]
        );
    }

    #[test]
    fn a_string_argument_is_its_own_spelling() {
        let c = compiled(
            r#"
(name Universe)
(sense source "{component} is implemented at {path}")
(source Universe "src/universe")
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Sentence(
                "Universe is implemented at src/universe".into()
            )]
        );
    }

    #[test]
    fn a_detail_keeps_its_locals_inside_the_body() {
        let c = compiled(
            r#"
(name Concept)
(detail (input Concept LogicBlock)
  (name LogicBlock)
  (name Relationship)
  (sense input "{X} is an input into {Y}")
  (and
    ("Concept is closed over by a LogicBlock")
    (input Relationship LogicBlock)))
"#,
        );
        assert_eq!(c.names, vec!["Concept".to_string()]);
        assert!(c.senses.is_empty());
        assert_eq!(
            c.claims,
            vec![Proposition::Detail {
                head: "Concept is an input into LogicBlock".into(),
                body: vec![Proposition::And(vec![
                    Proposition::Sentence("Concept is closed over by a LogicBlock".into()),
                    Proposition::Sentence("Relationship is an input into LogicBlock".into()),
                ])],
            }]
        );

        let itself = compile(
            &parse(
                r#"
(name Concept)
(name LogicBlock)
(sense input "{X} is an input into {Y}")
(detail (input Concept LogicBlock)
  (input Concept LogicBlock))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            itself
                .message
                .contains("`Concept is an input into LogicBlock` is detailed in terms of itself"),
            "{itself}"
        );

        let leaked = compile(
            &parse(
                r#"
(detail ("h")
  (name Local)
  ("Local holds"))
("{Name} holds" Local)
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            leaked.message.contains("`Local` is not a declared name"),
            "{leaked}"
        );

        let hidden = compile(
            &parse(
                r#"
(detail ("outer")
  (detail ("inner")
    (name Hidden)
    ("Hidden holds"))
  ("{Name} holds" Hidden))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            hidden.message.contains("`Hidden` is not a declared name"),
            "{hidden}"
        );

        let again = compile(
            &parse(
                r#"
(name Concept)
(detail ("h")
  (name Concept)
  ("Concept holds"))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            again.message.contains("duplicate name `Concept`"),
            "{again}"
        );
    }

    #[test]
    fn a_sentence_is_detailed_once_and_not_in_terms_of_itself() {
        let twice = compile(
            &parse(
                r#"
(detail ("arrow") ("one"))
(detail ("arrow") ("two"))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            twice.message.contains("duplicate detail of `arrow`"),
            "{twice}"
        );

        let cycle = compile(
            &parse(
                r#"
(detail ("arrow") ("leaf"))
(detail ("leaf") ("arrow"))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            cycle
                .message
                .contains("`arrow` is detailed in terms of itself")
                || cycle
                    .message
                    .contains("`leaf` is detailed in terms of itself"),
            "{cycle}"
        );
    }

    #[test]
    fn a_witness_keeps_its_sentence_and_its_paths() {
        let c = compiled(
            r#"
(name Search)
(name Oracle)
(sense calls "{caller} calls {callee}")
(witness (calls Search Oracle) "src/search/mod.rs" "src/oracle/jev.rs")
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Witness {
                sentence: "Search calls Oracle".into(),
                paths: vec!["src/search/mod.rs".into(), "src/oracle/jev.rs".into()],
            }]
        );
    }

    #[test]
    fn a_witness_path_is_relative_to_the_brief() {
        for (src, path) in [
            ("(witness (\"a\") \"../notes.txt\")", "../notes.txt"),
            (
                "(witness (\"a\") \"examples/sqrt2.brief\")",
                "examples/sqrt2.brief",
            ),
        ] {
            let compiled = compiled(src);
            assert_eq!(
                compiled.claims,
                vec![Proposition::Witness {
                    sentence: "a".into(),
                    paths: vec![path.into()],
                }]
            );
        }
        for (src, needle) in [
            (
                "(witness (\"a\") \"/etc/passwd\")",
                "`/etc/passwd` is not a path relative to the brief",
            ),
            (
                "(witness (\"a\") \"src/a.rs\" \"src/a.rs\")",
                "witness cites `src/a.rs` twice",
            ),
        ] {
            let err = compile(&parse(src).unwrap()).unwrap_err();
            assert!(err.message.contains(needle), "{src}: {err}");
        }
    }

    #[test]
    fn a_detail_is_not_witnessed_by_its_own_head() {
        let err = compile(
            &parse(
                r#"
(detail ("arrow")
  (witness ("arrow") "src/a.rs"))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            err.message
                .contains("`arrow` is detailed in terms of itself"),
            "{err}"
        );
    }

    #[test]
    fn a_nullary_sense_is_its_reading() {
        let compiled = compile(
            &parse("(sense lemma-1 \"The square root of two is irrational.\")\n(lemma-1)").unwrap(),
        )
        .unwrap();
        assert_eq!(
            compiled.claims,
            vec![Proposition::Sentence(
                "The square root of two is irrational.".into()
            )]
        );
    }

    #[test]
    fn rejects_an_undeclared_name() {
        let err = compile(
            &parse("(sense component \"{Name} is a part of the system\") (component Frontend)")
                .unwrap(),
        )
        .unwrap_err();
        assert!(
            err.message.contains("`Frontend` is not a declared name"),
            "{err}"
        );
    }

    #[test]
    fn rejects_an_unknown_sense_and_an_arity_mismatch() {
        let unknown = compile(&parse("(name Frontend) (component Frontend)").unwrap()).unwrap_err();
        assert!(
            unknown.message.contains("not a declared sense"),
            "{unknown}"
        );

        let arity = compile(
            &parse(
                r#"
(name Ada)
(sense borrowed_from "{borrower} borrows from {lender}")
(borrowed_from Ada)
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            arity.message.contains("`borrowed_from`")
                && arity.message.contains("{borrower}")
                && arity.message.contains("{lender}")
                && arity.message.contains("1 name"),
            "{arity}"
        );
    }

    #[test]
    fn rejects_duplicates_and_a_shared_spelling() {
        let dup_name = compile(&parse("(name Oracle) (name Oracle)").unwrap()).unwrap_err();
        assert!(dup_name.message.contains("duplicate name"), "{dup_name}");

        let dup_sense = compile(
            &parse("(sense calls \"{caller} calls {callee}\") (sense calls \"{X} calls {Y}\")")
                .unwrap(),
        )
        .unwrap_err();
        assert!(dup_sense.message.contains("duplicate sense"), "{dup_sense}");

        let both =
            compile(&parse("(name calls) (sense calls \"{X} calls {Y}\")").unwrap()).unwrap_err();
        assert!(both.message.contains("both a name and a sense"), "{both}");
    }
}
