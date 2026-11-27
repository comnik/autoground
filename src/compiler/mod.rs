use std::collections::{BTreeSet, HashMap, HashSet};

use crate::syntax::template::{apply, holes_word, names_word};
use crate::syntax::{
    parse_dollar_witness, Arg, At, Certify, CertifyHead, Define, Form, Formula, Ground, Ident,
    Program, Proposition, Reading, Sense, Sentence, Support,
};

mod compare;
mod link;
mod scan;

pub use compare::{compare_against, compare_against_with_bindings, AgainstReport};
pub use link::{link, link_parts, link_parts_with_bindings, link_with_bindings};
pub use scan::{display_path, scan_briefs, scan_namespaces, BriefFile, LoadError, Loaded};

/// Canonical identity of a sense across namespaces.
/// `owner` is the declaring namespace name, or `""` for an unnamespaced file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SenseKey {
    pub(crate) owner: String,
    pub(crate) name: String,
}

impl SenseKey {
    pub(crate) fn display(&self) -> String {
        if self.owner.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.owner, self.name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TemplateArg {
    Param(usize),
    Bound(String),
    Fixed { shown: String, grounded: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DefinedBody {
    And(Vec<DefinedBody>),
    Or(Vec<DefinedBody>),
    Not(Box<DefinedBody>),
    Implies {
        antecedent: Box<DefinedBody>,
        consequent: Box<DefinedBody>,
    },
    Exists {
        vars: Vec<String>,
        body: Box<DefinedBody>,
    },
    Forall {
        vars: Vec<String>,
        body: Box<DefinedBody>,
    },
    Apply {
        sense_key: Option<SenseKey>,
        template: String,
        args: Vec<TemplateArg>,
    },
    Spec {
        paths: Vec<String>,
        labels: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedDefine {
    pub(crate) sense: Sense,
    pub(crate) body: DefinedBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedConstraint {
    pub(crate) body: DefinedBody,
    pub(crate) sentence: Sentence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportedCertify {
    pub(crate) cert_id: String,
    pub(crate) certificate: Vec<Proposition>,
    pub(crate) assumptions: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CertifyRule {
    pub(crate) cert_id: String,
    /// Empty for a certify declared in the same file (its `Cert` is already in `claims`).
    pub(crate) certificate: Vec<Proposition>,
    pub(crate) assumptions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportedGround {
    pub(crate) cert_id: String,
    pub(crate) witness: Vec<String>,
    pub(crate) certificate: Vec<Proposition>,
    pub(crate) assumptions: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct GroundRule {
    pub(crate) cert_id: String,
    pub(crate) witness: Vec<String>,
    /// Empty for a ground declared in the same file (its `Cert` is already in `claims`).
    pub(crate) certificate: Vec<Proposition>,
    pub(crate) assumptions: Vec<String>,
}

pub(crate) struct CompiledModule {
    pub(crate) compiled: Compiled,
    pub(crate) defines: HashMap<SenseKey, ResolvedDefine>,
    pub(crate) define_order: Vec<SenseKey>,
    pub(crate) facts: HashMap<String, Sentence>,
    pub(crate) fact_order: Vec<String>,
    pub(crate) constraints: HashMap<SenseKey, ResolvedConstraint>,
    pub(crate) constraint_order: Vec<SenseKey>,
    pub(crate) certifies: HashMap<SenseKey, ExportedCertify>,
    pub(crate) certified_facts: HashMap<String, ExportedCertify>,
    pub(crate) grounds: HashMap<SenseKey, ExportedGround>,
}

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
    /// A referred sense spelling, mapped to the namespace that declares it.
    owners: HashMap<String, String>,
    /// Parameterized definitions exported by required namespaces, keyed by canonical sense.
    defines: HashMap<SenseKey, ResolvedDefine>,
    /// Ground definitions exported by required namespaces, keyed by sentence text.
    facts: HashMap<String, Sentence>,
    /// Named closed constraints exported by required namespaces, keyed by canonical key.
    constraints: HashMap<SenseKey, ResolvedConstraint>,
    /// Referred constraint names mapped to their canonical key.
    constraint_names: HashMap<String, SenseKey>,
    /// Implementation certifications exported by required namespaces, keyed by canonical sense.
    pub(crate) certifies: HashMap<SenseKey, ExportedCertify>,
    /// Certified ground facts exported by required namespaces, keyed by sentence text.
    pub(crate) certified_facts: HashMap<String, ExportedCertify>,
    /// Grounding rules exported by required namespaces, keyed by canonical sense.
    pub(crate) grounds: HashMap<SenseKey, ExportedGround>,
}

impl Imports {
    pub(crate) fn none() -> Self {
        Self {
            names: HashSet::new(),
            senses: HashMap::new(),
            qualifiers: HashMap::new(),
            opened: HashMap::new(),
            owners: HashMap::new(),
            defines: HashMap::new(),
            facts: HashMap::new(),
            constraints: HashMap::new(),
            constraint_names: HashMap::new(),
            certifies: HashMap::new(),
            certified_facts: HashMap::new(),
            grounds: HashMap::new(),
        }
    }
}

#[derive(Clone)]
struct Defs {
    rules: HashMap<SenseKey, ResolvedDefine>,
    facts: HashMap<String, Sentence>,
    constraints: HashMap<SenseKey, ResolvedConstraint>,
    constraint_names: HashMap<String, SenseKey>,
    certifies: HashMap<SenseKey, CertifyRule>,
    grounds: HashMap<SenseKey, GroundRule>,
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

/// A model whose claims are sentences under `and`, `or`, `not`, `claim`, and `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub names: Vec<String>,
    pub senses: Vec<Sense>,
    pub claims: Vec<Proposition>,
    /// Assumption sentences, in source order, including ones inside a detail.
    pub assumptions: Vec<String>,
    /// Nullary senses used as terms, including ones declared inside a detail.
    /// Each entry is the sense's name and its template.
    pub terms: BTreeSet<(String, String)>,
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

/// [`compile`], resolving every `(param ...)` declaration from `bindings`.
pub fn compile_with_bindings(
    program: &Program,
    bindings: &HashMap<String, String>,
) -> Result<Compiled, CompileError> {
    if let Some(ns) = &program.namespace {
        if let Some(require) = ns.requires.first() {
            return Err(CompileError {
                message: format!("namespace `{}` is not declared", require.namespace),
            });
        }
    }
    Ok(compile_module_with_bindings(program, &Imports::none(), Some(bindings))?.compiled)
}

pub(crate) fn compile_with(program: &Program, imports: &Imports) -> Result<Compiled, CompileError> {
    Ok(compile_module(program, imports)?.compiled)
}

pub(crate) fn compile_module(
    program: &Program,
    imports: &Imports,
) -> Result<CompiledModule, CompileError> {
    compile_module_with_bindings(program, imports, None)
}

pub(crate) fn compile_module_with_bindings(
    program: &Program,
    imports: &Imports,
    bindings: Option<&HashMap<String, String>>,
) -> Result<CompiledModule, CompileError> {
    let mut names = Vec::new();
    let mut name_seen = HashSet::new();
    let mut param_values = HashMap::new();
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
            Form::Param(param) => {
                if !name_seen.insert(param.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate name `{param}`"),
                    });
                }
                if let Some(map) = bindings {
                    let Some(value) = map.get(param) else {
                        return Err(CompileError {
                            message: format!("missing binding for parameter `{param}`"),
                        });
                    };
                    param_values.insert(param.clone(), value.clone());
                }
                names.push(param.clone());
            }
            Form::Sense(sense) => {
                if !sense_seen.insert(sense.name.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate sense `{}`", sense.name),
                    });
                }
                senses.push(sense.clone());
            }
            Form::Define(_)
            | Form::Certify(_)
            | Form::Ground(_)
            | Form::Assume(_)
            | Form::Detail(_)
            | Form::Formula(_) => {}
        }
    }

    for name in &names {
        if sense_seen.contains(name) {
            return Err(CompileError {
                message: format!("`{name}` is declared as both a name and a sense"),
            });
        }
    }

    let current_ns = program
        .namespace
        .as_ref()
        .map(|ns| ns.name.clone())
        .unwrap_or_default();

    let mut sense_by_name: HashMap<String, (SenseKey, Sense)> = senses
        .iter()
        .map(|sense| {
            let key = SenseKey {
                owner: current_ns.clone(),
                name: sense.name.clone(),
            };
            (sense.name.clone(), (key, sense.clone()))
        })
        .collect();
    for (name, sense) in &imports.senses {
        let owner = imports.owners.get(name).cloned().unwrap_or_default();
        sense_by_name.entry(name.clone()).or_insert_with(|| {
            (
                SenseKey {
                    owner,
                    name: name.clone(),
                },
                sense.clone(),
            )
        });
    }
    let mut declared: HashSet<String> = names.iter().cloned().collect();
    declared.extend(imports.names.iter().cloned());

    let mut ctx = Ctx {
        imports,
        param_values,
        expansions: HashMap::new(),
        scopes: vec![file_scope(&program.forms, imports)],
        terms: BTreeSet::new(),
        assumed: HashSet::new(),
        claimed: HashSet::new(),
        next_detail_id: 0,
        used_ground_assumptions: Vec::new(),
        certified_rules: Vec::new(),
        certified_facts: Vec::new(),
    };

    let mut defs = Defs {
        rules: imports.defines.clone(),
        facts: imports.facts.clone(),
        constraints: imports.constraints.clone(),
        constraint_names: imports.constraint_names.clone(),
        certifies: imports
            .certifies
            .iter()
            .map(|(key, exp)| {
                (
                    key.clone(),
                    CertifyRule {
                        cert_id: exp.cert_id.clone(),
                        certificate: exp.certificate.clone(),
                        assumptions: exp.assumptions.clone(),
                    },
                )
            })
            .collect(),
        grounds: imports
            .grounds
            .iter()
            .map(|(key, exp)| {
                (
                    key.clone(),
                    GroundRule {
                        cert_id: exp.cert_id.clone(),
                        witness: exp.witness.clone(),
                        certificate: exp.certificate.clone(),
                        assumptions: exp.assumptions.clone(),
                    },
                )
            })
            .collect(),
    };
    let local_defs = collect_defines(
        &program.forms,
        &declared,
        &sense_by_name,
        &current_ns,
        &mut defs,
        &mut ctx,
    )?;
    check_define_cycles(&defs.rules)?;

    let _local_certifies = collect_certifies(
        &program.forms,
        &sense_by_name,
        &defs.rules,
        &defs.constraints,
        &defs.constraint_names,
        &current_ns,
        &mut defs.certifies,
        &mut ctx,
    )?;

    let local_grounds = collect_grounds(
        &program.forms,
        &sense_by_name,
        &defs.rules,
        &current_ns,
        &mut defs.grounds,
        &mut ctx,
    )?;

    collect_local_heads(&program.forms, &declared, &sense_by_name, &defs.rules, &mut ctx)?;

    let mut assumptions = Vec::new();
    let claims = resolve_forms(
        &program.forms,
        &declared,
        &sense_by_name,
        &defs,
        &current_ns,
        &mut ctx,
        &mut assumptions,
    )?;
    let file = ctx.scopes.pop().expect("file scope");
    ctx.terms.extend(finish_scope(file)?);
    check_definitions(&claims)?;

    for assumption in ctx.used_ground_assumptions {
        if !assumptions.contains(&assumption) {
            assumptions.push(assumption);
        }
    }

    let exported_certifies = export_certifies(&ctx.certified_rules, &claims);
    let exported_certified_facts = export_fact_certifies(&ctx.certified_facts, &claims);
    let exported_grounds = export_grounds(&local_grounds, &claims);

    Ok(CompiledModule {
        compiled: Compiled {
            names,
            senses,
            claims,
            assumptions,
            terms: ctx.terms,
        },
        defines: local_defs.rules,
        define_order: local_defs.rule_order,
        facts: local_defs.facts,
        fact_order: local_defs.fact_order,
        constraints: local_defs.constraints,
        constraint_order: local_defs.constraint_order,
        certifies: exported_certifies,
        certified_facts: exported_certified_facts,
        grounds: exported_grounds,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DefKey {
    Sentence(String),
    Cert(String),
}

enum Def {
    Claim(Support),
    Assume,
}

/// One claim or assumption defines a sentence. A cycle would ask that sentence
/// to establish itself.
fn check_definitions(claims: &[Proposition]) -> Result<(), CompileError> {
    let mut defs: HashMap<DefKey, Def> = HashMap::new();
    for claim in claims {
        collect_def(claim, &mut defs)?;
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for head in defs.keys() {
        if let Some(key) = definition_cycle(head, &defs, &mut visiting, &mut visited) {
            let message = match key {
                DefKey::Sentence(sentence) => format!("`{sentence}` is claimed in terms of itself"),
                DefKey::Cert(cert_id) => format!("`{cert_id}` is grounded in terms of itself"),
            };
            return Err(CompileError { message });
        }
    }
    Ok(())
}

fn collect_def(
    proposition: &Proposition,
    defs: &mut HashMap<DefKey, Def>,
) -> Result<(), CompileError> {
    match proposition {
        Proposition::And(xs) | Proposition::Or(xs) => {
            for proposition in xs {
                collect_def(proposition, defs)?;
            }
        }
        Proposition::Not(inner) => collect_def(inner, defs)?,
        Proposition::Sentence(_) => {}
        Proposition::Defined { body, .. } => collect_def(body, defs)?,
        Proposition::Claim { sentence, support } => {
            collect_support(support, defs)?;
            define(sentence, Def::Claim(support.clone()), defs)?;
        }
        Proposition::Cert {
            cert_id, support, ..
        } => {
            collect_support(support, defs)?;
            defs.insert(DefKey::Cert(cert_id.clone()), Def::Claim(support.clone()));
        }
        Proposition::Grounded { .. } => {}
        Proposition::Assume(sentence) => define(sentence, Def::Assume, defs)?,
        Proposition::Scope { body, .. } => {
            for proposition in body {
                collect_def(proposition, defs)?;
            }
        }
    }
    Ok(())
}

fn collect_support(
    support: &Support,
    defs: &mut HashMap<DefKey, Def>,
) -> Result<(), CompileError> {
    match support {
        Support::Witness { .. } => Ok(()),
        Support::Sentence(proposition) => collect_def(proposition, defs),
    }
}

fn define(
    sentence: &Sentence,
    def: Def,
    defs: &mut HashMap<DefKey, Def>,
) -> Result<(), CompileError> {
    let key = DefKey::Sentence(sentence.text.clone());
    if defs.contains_key(&key) {
        return Err(CompileError {
            message: format!("duplicate definition of `{}`", sentence.text),
        });
    }
    defs.insert(key, def);
    Ok(())
}

fn definition_cycle(
    head: &DefKey,
    defs: &HashMap<DefKey, Def>,
    visiting: &mut HashSet<DefKey>,
    visited: &mut HashSet<DefKey>,
) -> Option<DefKey> {
    if visited.contains(head) {
        return None;
    }
    if !visiting.insert(head.clone()) {
        return Some(head.clone());
    }
    if let Some(def) = defs.get(head) {
        for dep in def_deps(def, defs) {
            if let Some(found) = definition_cycle(&dep, defs, visiting, visited) {
                return Some(found);
            }
        }
    }
    visiting.remove(head);
    visited.insert(head.clone());
    None
}

fn def_deps(def: &Def, defs: &HashMap<DefKey, Def>) -> Vec<DefKey> {
    match def {
        Def::Assume => Vec::new(),
        Def::Claim(support) => support_deps(support, defs),
    }
}

fn support_deps(support: &Support, defs: &HashMap<DefKey, Def>) -> Vec<DefKey> {
    match support {
        Support::Witness { .. } => Vec::new(),
        Support::Sentence(proposition) => prop_deps(proposition, defs),
    }
}

fn prop_deps(proposition: &Proposition, defs: &HashMap<DefKey, Def>) -> Vec<DefKey> {
    match proposition {
        Proposition::Sentence(sentence) | Proposition::Assume(sentence) => {
            let key = DefKey::Sentence(sentence.text.clone());
            if defs.contains_key(&key) {
                vec![key]
            } else {
                Vec::new()
            }
        }
        Proposition::Claim { sentence, .. } => {
            let key = DefKey::Sentence(sentence.text.clone());
            if defs.contains_key(&key) {
                vec![key]
            } else {
                Vec::new()
            }
        }
        Proposition::Defined { body, .. } => prop_deps(body, defs),
        Proposition::Cert { cert_id, .. } => {
            let key = DefKey::Cert(cert_id.clone());
            if defs.contains_key(&key) {
                vec![key]
            } else {
                Vec::new()
            }
        }
        Proposition::Grounded {
            cert_id,
            certificate,
            ..
        } => {
            if certificate.is_empty() {
                let key = DefKey::Cert(cert_id.clone());
                if defs.contains_key(&key) {
                    return vec![key];
                }
            }
            Vec::new()
        }
        Proposition::And(xs) | Proposition::Or(xs) => xs
            .iter()
            .flat_map(|proposition| prop_deps(proposition, defs))
            .collect(),
        Proposition::Not(inner) => prop_deps(inner, defs),
        Proposition::Scope { body, .. } => body
            .iter()
            .flat_map(|proposition| prop_deps(proposition, defs))
            .collect(),
    }
}

fn contains_claim_or_assume(forms: &[Form]) -> bool {
    forms.iter().any(form_has_claim_or_assume)
}

fn form_has_claim_or_assume(form: &Form) -> bool {
    match form {
        Form::Assume(_) | Form::Certify(_) | Form::Ground(_) => true,
        Form::Detail(body) => contains_claim_or_assume(body),
        Form::Formula(formula) => formula_has_claim(formula),
        Form::Name(_) | Form::Param(_) | Form::Sense(_) | Form::Define(_) => false,
    }
}

fn formula_has_claim(formula: &Formula) -> bool {
    match formula {
        Formula::Claim { .. } => true,
        Formula::And(xs) | Formula::Or(xs) => xs.iter().any(formula_has_claim),
        Formula::Not(inner) => formula_has_claim(inner),
        Formula::Implies {
            antecedent,
            consequent,
        } => formula_has_claim(antecedent) || formula_has_claim(consequent),
        Formula::Apply { .. }
        | Formula::Witness { .. }
        | Formula::Spec { .. }
        | Formula::Exists { .. }
        | Formula::Forall { .. } => false,
    }
}

/// Where one nullary sense was used. The earliest site of each kind is kept.
struct DeclUse {
    name: String,
    template: String,
    imported: bool,
    term: Option<(usize, usize)>,
    sentence: Option<(usize, usize)>,
}

enum Place {
    Term,
    Sentence,
}

#[derive(Clone)]
struct Spelling {
    shown: String,
    grounded: String,
}

struct Ctx<'a> {
    imports: &'a Imports,
    param_values: HashMap<String, String>,
    /// Shown sentence, then the sentence the judge would be asked.
    expansions: HashMap<String, String>,
    /// Scopes from the outside in. The file is index 0.
    scopes: Vec<HashMap<String, DeclUse>>,
    terms: BTreeSet<(String, String)>,
    assumed: HashSet<String>,
    claimed: HashSet<String>,
    next_detail_id: usize,
    used_ground_assumptions: Vec<String>,
    certified_rules: Vec<(SenseKey, String)>,
    certified_facts: Vec<String>,
}

fn file_scope(forms: &[Form], imports: &Imports) -> HashMap<String, DeclUse> {
    let mut scope = scope_decls(forms);
    for (namespace, opened) in &imports.opened {
        for (name, sense) in &opened.senses {
            if !sense.holes.is_empty() {
                continue;
            }
            scope.insert(
                format!("{namespace}/{name}"),
                DeclUse {
                    name: name.clone(),
                    template: sense.template.clone(),
                    imported: true,
                    term: None,
                    sentence: None,
                },
            );
        }
    }
    scope
}

fn scope_decls(forms: &[Form]) -> HashMap<String, DeclUse> {
    let mut scope = HashMap::new();
    for form in forms {
        if let Form::Sense(sense) = form {
            if sense.holes.is_empty() {
                scope.insert(
                    sense.name.clone(),
                    DeclUse {
                        name: sense.name.clone(),
                        template: sense.template.clone(),
                        imported: false,
                        term: None,
                        sentence: None,
                    },
                );
            }
        }
    }
    scope
}

fn finish_scope(
    scope: HashMap<String, DeclUse>,
) -> Result<BTreeSet<(String, String)>, CompileError> {
    let mut conflict: Option<(String, (usize, usize), (usize, usize))> = None;
    for decl in scope.values() {
        let (Some(term), Some(sentence)) = (decl.term, decl.sentence) else {
            continue;
        };
        let earliest = term.min(sentence);
        let replace = conflict
            .as_ref()
            .is_none_or(|(_, prev_term, prev_sentence)| {
                earliest < (*prev_term).min(*prev_sentence)
            });
        if replace {
            conflict = Some((decl.name.clone(), term, sentence));
        }
    }
    if let Some((name, term, sentence)) = conflict {
        return Err(CompileError {
            message: format!(
                "`{name}` is used as a term at {}:{} and as a sentence at {}:{}.\nA nullary sense reads either as a noun phrase or as a sentence, not both.",
                term.0, term.1, sentence.0, sentence.1
            ),
        });
    }
    let mut terms = BTreeSet::new();
    for decl in scope.values() {
        if !decl.imported && decl.term.is_some() {
            terms.insert((decl.name.clone(), decl.template.clone()));
        }
    }
    Ok(terms)
}

fn find_decl(ctx: &Ctx, ident: &Ident) -> Option<(usize, String)> {
    if let Some(qualifier) = &ident.qualifier {
        let namespace = ctx.imports.qualifiers.get(qualifier)?;
        let key = format!("{namespace}/{}", ident.name);
        if ctx.scopes[0].contains_key(&key) {
            return Some((0, key));
        }
        return None;
    }
    for (index, scope) in ctx.scopes.iter().enumerate().rev() {
        if scope.get(&ident.name).is_some_and(|decl| !decl.imported) {
            return Some((index, ident.name.clone()));
        }
    }
    let namespace = ctx.imports.owners.get(&ident.name)?;
    let key = format!("{namespace}/{}", ident.name);
    if ctx.scopes[0].contains_key(&key) {
        Some((0, key))
    } else {
        None
    }
}

fn note(ctx: &mut Ctx, ident: &Ident, place: Place, at: At) {
    let Some((index, key)) = find_decl(ctx, ident) else {
        return;
    };
    let Some(decl) = ctx.scopes[index].get_mut(&key) else {
        return;
    };
    let slot = match place {
        Place::Term => &mut decl.term,
        Place::Sentence => &mut decl.sentence,
    };
    let pos = (at.line, at.col);
    if slot.is_none_or(|prev| pos < prev) {
        *slot = Some(pos);
    }
}

fn remember(
    expansions: &mut HashMap<String, String>,
    sentence: &Sentence,
) -> Result<(), CompileError> {
    match expansions.get(&sentence.text) {
        Some(prev) if prev != &sentence.grounded => Err(CompileError {
            message: format!(
                "`{}` expands to `{prev}` and `{}`",
                sentence.text, sentence.grounded
            ),
        }),
        Some(_) => Ok(()),
        None => {
            expansions.insert(sentence.text.clone(), sentence.grounded.clone());
            Ok(())
        }
    }
}

struct LocalDefines {
    rules: HashMap<SenseKey, ResolvedDefine>,
    rule_order: Vec<SenseKey>,
    facts: HashMap<String, Sentence>,
    fact_order: Vec<String>,
    constraints: HashMap<SenseKey, ResolvedConstraint>,
    constraint_order: Vec<SenseKey>,
}

fn collect_defines(
    forms: &[Form],
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    owner: &str,
    defs: &mut Defs,
    ctx: &mut Ctx<'_>,
) -> Result<LocalDefines, CompileError> {
    let mut local_rules = HashMap::new();
    let mut rule_order = Vec::new();
    for form in forms {
        let Form::Define(Define::Rule {
            sense: sense_ident,
            params,
            body,
            at,
        }) = form
        else {
            continue;
        };
        let (key, sense) = resolve_sense_with_key(sense_ident, senses, ctx.imports)?;
        if params.len() != sense.holes.len() {
            return Err(arity_error(
                &format!("`{}`", sense_ident.display()),
                &sense.holes,
                params.len(),
            ));
        }
        if sense.holes.is_empty() {
            note(ctx, sense_ident, Place::Sentence, *at);
        }
        if local_rules.contains_key(&key) || defs.rules.contains_key(&key) {
            return Err(CompileError {
                message: format!("duplicate definition of sense `{}`", sense_ident.display()),
            });
        }
        let body = match body {
            Formula::Spec { paths } => {
                let paths = spec_paths(paths)?;
                DefinedBody::Spec {
                    labels: paths.clone(),
                    paths,
                }
            }
            other => resolve_define_body(other, params, &[], names, senses, ctx)?,
        };
        if matches!(&body, DefinedBody::And(xs) | DefinedBody::Or(xs) if xs.is_empty()) {
            return Err(CompileError {
                message: format!(
                    "`define` body for `{}` cannot be empty",
                    sense_ident.display()
                ),
            });
        }
        let resolved = ResolvedDefine { sense, body };
        defs.rules.insert(key.clone(), resolved.clone());
        rule_order.push(key.clone());
        local_rules.insert(key, resolved);
    }

    let mut local_constraints = HashMap::new();
    let mut constraint_order = Vec::new();
    for form in forms {
        let Form::Define(Define::Constraint { name, body, .. }) = form else {
            continue;
        };
        if names.contains(name) || senses.contains_key(name) {
            return Err(CompileError {
                message: format!("`{name}` is already declared as a name or sense"),
            });
        }
        let key = SenseKey {
            owner: owner.to_string(),
            name: name.clone(),
        };
        if local_constraints.contains_key(&key) || defs.constraints.contains_key(&key) {
            return Err(CompileError {
                message: format!("duplicate definition of `{name}`"),
            });
        }
        let (resolved_body, sentence) = match body {
            Formula::Spec { paths } => {
                let paths = spec_paths(paths)?;
                let sentence = Sentence::from(format!(
                    "The implementation satisfies the specification in {}",
                    format_witness_list(&paths)
                ));
                (
                    DefinedBody::Spec {
                        labels: paths.clone(),
                        paths,
                    },
                    sentence,
                )
            }
            other => {
                let resolved_body = resolve_define_body(other, &[], &[], names, senses, ctx)?;
                if matches!(&resolved_body, DefinedBody::And(xs) | DefinedBody::Or(xs) if xs.is_empty())
                {
                    return Err(CompileError {
                        message: format!("`define` body for `{name}` cannot be empty"),
                    });
                }
                let text = render_defined_body(&resolved_body, &[], false, false)?;
                let grounded = render_defined_body(&resolved_body, &[], true, false)?;
                (resolved_body, Sentence { text, grounded })
            }
        };
        remember(&mut ctx.expansions, &sentence)?;
        let resolved = ResolvedConstraint {
            body: resolved_body,
            sentence,
        };
        defs.constraints.insert(key.clone(), resolved.clone());
        defs.constraint_names.insert(name.clone(), key.clone());
        constraint_order.push(key.clone());
        local_constraints.insert(key, resolved);
    }

    let mut local_facts = HashMap::new();
    let mut fact_order = Vec::new();
    for form in forms {
        let Form::Define(Define::Fact { sentence, .. }) = form else {
            continue;
        };
        let (sentence, head_key) =
            require_raw_sentence_with_key(sentence, names, senses, ctx, "`define` takes a sentence")?;
        if let Some((ident_display, key)) = head_key {
            if defs.rules.contains_key(&key) {
                return Err(CompileError {
                    message: format!("`{ident_display}` is already defined by `define`"),
                });
            }
        }
        if local_facts.contains_key(&sentence.text) || defs.facts.contains_key(&sentence.text) {
            return Err(CompileError {
                message: format!("duplicate definition of `{}`", sentence.text),
            });
        }
        defs.facts.insert(sentence.text.clone(), sentence.clone());
        fact_order.push(sentence.text.clone());
        local_facts.insert(sentence.text.clone(), sentence);
    }
    Ok(LocalDefines {
        rules: local_rules,
        rule_order,
        facts: local_facts,
        fact_order,
        constraints: local_constraints,
        constraint_order,
    })
}

fn resolve_define_body(
    formula: &Formula,
    params: &[String],
    bound: &[String],
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    ctx: &mut Ctx<'_>,
) -> Result<DefinedBody, CompileError> {
    match formula {
        Formula::And(xs) => {
            let mut out = Vec::with_capacity(xs.len());
            for x in xs {
                out.push(resolve_define_body(x, params, bound, names, senses, ctx)?);
            }
            Ok(DefinedBody::And(out))
        }
        Formula::Or(xs) => {
            let mut out = Vec::with_capacity(xs.len());
            for x in xs {
                out.push(resolve_define_body(x, params, bound, names, senses, ctx)?);
            }
            Ok(DefinedBody::Or(out))
        }
        Formula::Not(x) => Ok(DefinedBody::Not(Box::new(resolve_define_body(
            x, params, bound, names, senses, ctx,
        )?))),
        Formula::Implies {
            antecedent,
            consequent,
        } => Ok(DefinedBody::Implies {
            antecedent: Box::new(resolve_define_body(
                antecedent, params, bound, names, senses, ctx,
            )?),
            consequent: Box::new(resolve_define_body(
                consequent, params, bound, names, senses, ctx,
            )?),
        }),
        Formula::Exists { vars, body } | Formula::Forall { vars, body } => {
            let universal = matches!(formula, Formula::Forall { .. });
            for var in vars {
                if params.iter().any(|p| p == var) || bound.iter().any(|b| b == var) {
                    return Err(CompileError {
                        message: format!("variable `{var}` is already bound in an enclosing scope"),
                    });
                }
            }
            let mut inner_bound = Vec::with_capacity(bound.len() + vars.len());
            inner_bound.extend_from_slice(bound);
            inner_bound.extend(vars.iter().cloned());
            let inner = resolve_define_body(body, params, &inner_bound, names, senses, ctx)?;
            if matches!(&inner, DefinedBody::And(xs) | DefinedBody::Or(xs) if xs.is_empty()) {
                let kw = if universal { "forall" } else { "exists" };
                return Err(CompileError {
                    message: format!("`{kw}` body cannot be empty"),
                });
            }
            if universal {
                Ok(DefinedBody::Forall {
                    vars: vars.clone(),
                    body: Box::new(inner),
                })
            } else {
                Ok(DefinedBody::Exists {
                    vars: vars.clone(),
                    body: Box::new(inner),
                })
            }
        }
        Formula::Claim { .. } => Err(CompileError {
            message: if bound.is_empty() {
                "`claim` cannot appear inside a `define` body".into()
            } else {
                "`claim` cannot appear inside a quantifier body".into()
            },
        }),
        Formula::Witness { .. } => Err(CompileError {
            message: if bound.is_empty() {
                "`witness` cannot appear inside a `define` body".into()
            } else {
                "`witness` cannot appear inside a quantifier body".into()
            },
        }),
        Formula::Spec { .. } => Err(CompileError {
            message: "`spec` must be the entire body of `define`".into(),
        }),
        Formula::Apply { reading, args, at } => {
            let mut resolved_args = Vec::with_capacity(args.len());
            for arg in args {
                if let Arg::Name(ident) = arg {
                    if ident.qualifier.is_none() {
                        if bound.iter().any(|var| var == &ident.name) {
                            resolved_args.push(TemplateArg::Bound(ident.name.clone()));
                            continue;
                        }
                        if let Some(index) = params.iter().position(|param| param == &ident.name) {
                            resolved_args.push(TemplateArg::Param(index));
                            continue;
                        }
                    }
                }
                let spelling = resolve_arg(arg, names, senses, ctx)?;
                resolved_args.push(TemplateArg::Fixed {
                    shown: spelling.shown,
                    grounded: spelling.grounded,
                });
            }
            let (what, sense_key, template, holes, head) = match reading {
                Reading::Sense(ident) => {
                    let (key, sense) = resolve_sense_with_key(ident, senses, ctx.imports)?;
                    (
                        format!("`{}`", ident.display()),
                        Some(key),
                        sense.template,
                        sense.holes,
                        Some(ident),
                    )
                }
                Reading::Inline { template, holes } => (
                    format!("\"{template}\""),
                    None,
                    template.clone(),
                    holes.clone(),
                    None,
                ),
            };
            if resolved_args.len() != holes.len() {
                return Err(arity_error(&what, &holes, resolved_args.len()));
            }
            if holes.is_empty() {
                if let Some(ident) = head {
                    note(ctx, ident, Place::Sentence, *at);
                }
            }
            Ok(DefinedBody::Apply {
                sense_key,
                template,
                args: resolved_args,
            })
        }
    }
}

fn check_define_cycles(defines: &HashMap<SenseKey, ResolvedDefine>) -> Result<(), CompileError> {
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for key in defines.keys() {
        if let Some(cycle) = define_cycle(key, defines, &mut visiting, &mut visited) {
            return Err(CompileError {
                message: format!("`{}` is defined in terms of itself", cycle.name),
            });
        }
    }
    Ok(())
}

fn define_cycle(
    key: &SenseKey,
    defines: &HashMap<SenseKey, ResolvedDefine>,
    visiting: &mut HashSet<SenseKey>,
    visited: &mut HashSet<SenseKey>,
) -> Option<SenseKey> {
    if visited.contains(key) {
        return None;
    }
    if !visiting.insert(key.clone()) {
        return Some(key.clone());
    }
    if let Some(def) = defines.get(key) {
        let mut deps = Vec::new();
        collect_define_deps(&def.body, &mut deps);
        for dep in deps {
            if defines.contains_key(&dep) {
                if let Some(found) = define_cycle(&dep, defines, visiting, visited) {
                    return Some(found);
                }
            }
        }
    }
    visiting.remove(key);
    visited.insert(key.clone());
    None
}

fn collect_define_deps(body: &DefinedBody, out: &mut Vec<SenseKey>) {
    match body {
        DefinedBody::And(xs) | DefinedBody::Or(xs) => {
            for x in xs {
                collect_define_deps(x, out);
            }
        }
        DefinedBody::Not(inner) => collect_define_deps(inner, out),
        DefinedBody::Implies {
            antecedent,
            consequent,
        } => {
            collect_define_deps(antecedent, out);
            collect_define_deps(consequent, out);
        }
        DefinedBody::Exists { body, .. } | DefinedBody::Forall { body, .. } => {
            collect_define_deps(body, out)
        }
        DefinedBody::Apply { sense_key, .. } => {
            if let Some(key) = sense_key {
                out.push(key.clone());
            }
        }
        DefinedBody::Spec { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_certifies(
    forms: &[Form],
    senses: &HashMap<String, (SenseKey, Sense)>,
    defines: &HashMap<SenseKey, ResolvedDefine>,
    constraints: &HashMap<SenseKey, ResolvedConstraint>,
    constraint_names: &HashMap<String, SenseKey>,
    scope_prefix: &str,
    certifies: &mut HashMap<SenseKey, CertifyRule>,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<(SenseKey, String)>, CompileError> {
    let mut local = Vec::new();
    let mut local_seen = HashSet::new();
    for form in forms {
        let Form::Certify(certify) = form else {
            continue;
        };
        match &certify.head {
            CertifyHead::Name(ident) => {
                let Some((key, _)) =
                    resolve_constraint_ident(ident, constraints, constraint_names, ctx.imports)?
                else {
                    continue;
                };
                if !local_seen.insert(key.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate `certify` for `{}`", ident.display()),
                    });
                }
                let cert_id = if scope_prefix.is_empty() {
                    format!("certify:{}", key.display())
                } else {
                    format!("certify:{scope_prefix}:{}", key.display())
                };
                certifies.insert(
                    key.clone(),
                    CertifyRule {
                        cert_id: cert_id.clone(),
                        certificate: Vec::new(),
                        assumptions: Vec::new(),
                    },
                );
                local.push((key, cert_id));
            }
            CertifyHead::Formula(Formula::Apply {
                reading: Reading::Sense(ident),
                args,
                at,
            }) => {
                let (key, sense) = resolve_sense_with_key(ident, senses, ctx.imports)?;
                if !defines.contains_key(&key) {
                    continue;
                }
                let _params = extract_certify_params(args, &sense, ident)?;
                if sense.holes.is_empty() {
                    note(ctx, ident, Place::Sentence, *at);
                }
                if !local_seen.insert(key.clone()) {
                    return Err(CompileError {
                        message: format!("duplicate `certify` for `{}`", ident.display()),
                    });
                }
                let cert_id = if scope_prefix.is_empty() {
                    format!("certify:{}", key.display())
                } else {
                    format!("certify:{scope_prefix}:{}", key.display())
                };
                certifies.insert(
                    key.clone(),
                    CertifyRule {
                        cert_id: cert_id.clone(),
                        certificate: Vec::new(),
                        assumptions: Vec::new(),
                    },
                );
                local.push((key, cert_id));
            }
            CertifyHead::Formula(_) => continue,
        }
    }
    Ok(local)
}

fn resolve_constraint_ident<'a>(
    ident: &Ident,
    constraints: &'a HashMap<SenseKey, ResolvedConstraint>,
    constraint_names: &HashMap<String, SenseKey>,
    imports: &Imports,
) -> Result<Option<(SenseKey, &'a ResolvedConstraint)>, CompileError> {
    if let Some(qualifier) = &ident.qualifier {
        let Some(ns) = imports.qualifiers.get(qualifier) else {
            return Err(CompileError {
                message: format!("`{qualifier}` is not a required namespace"),
            });
        };
        let key = SenseKey {
            owner: ns.clone(),
            name: ident.name.clone(),
        };
        Ok(constraints.get(&key).map(|c| (key, c)))
    } else if let Some(key) = constraint_names.get(&ident.name) {
        Ok(constraints.get(key).map(|c| (key.clone(), c)))
    } else {
        Ok(None)
    }
}

fn extract_certify_params(
    args: &[Arg],
    sense: &Sense,
    ident: &Ident,
) -> Result<Vec<String>, CompileError> {
    if args.len() != sense.holes.len() {
        return Err(arity_error(
            &format!("`{}`", ident.display()),
            &sense.holes,
            args.len(),
        ));
    }
    let mut params = Vec::with_capacity(args.len());
    for arg in args {
        let Arg::Name(arg_ident) = arg else {
            return Err(CompileError {
                message: format!(
                    "`certify` for `{}` takes parameter names",
                    ident.display()
                ),
            });
        };
        if arg_ident.qualifier.is_some() {
            return Err(CompileError {
                message: format!(
                    "`certify` parameter `{}` takes no qualifier",
                    arg_ident.display()
                ),
            });
        }
        if params.iter().any(|seen| seen == &arg_ident.name) {
            return Err(CompileError {
                message: format!("duplicate parameter `{}`", arg_ident.name),
            });
        }
        params.push(arg_ident.name.clone());
    }
    Ok(params)
}

fn collect_grounds(
    forms: &[Form],
    senses: &HashMap<String, (SenseKey, Sense)>,
    defines: &HashMap<SenseKey, ResolvedDefine>,
    scope_prefix: &str,
    grounds: &mut HashMap<SenseKey, GroundRule>,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<(SenseKey, String, Vec<String>)>, CompileError> {
    let mut local = Vec::new();
    let mut local_seen = HashSet::new();
    for form in forms {
        let Form::Ground(ground) = form else {
            continue;
        };
        let (key, sense) = resolve_sense_with_key(&ground.sense, senses, ctx.imports)?;
        if !defines.contains_key(&key) {
            return Err(CompileError {
                message: format!(
                    "`{}` cannot be grounded without a `define`",
                    ground.sense.display()
                ),
            });
        }
        if ground.params.len() != sense.holes.len() {
            return Err(arity_error(
                &format!("`{}`", ground.sense.display()),
                &sense.holes,
                ground.params.len(),
            ));
        }
        if sense.holes.is_empty() {
            note(ctx, &ground.sense, Place::Sentence, ground.at);
        }
        if !local_seen.insert(key.clone()) {
            return Err(CompileError {
                message: format!("duplicate `ground` for `{}`", ground.sense.display()),
            });
        }
        let witness = witness_paths(&ground.witness)?;
        let cert_id = if scope_prefix.is_empty() {
            format!("ground:{}", key.display())
        } else {
            format!("ground:{scope_prefix}:{}", key.display())
        };
        grounds.insert(
            key.clone(),
            GroundRule {
                cert_id: cert_id.clone(),
                witness: witness.clone(),
                certificate: Vec::new(),
                assumptions: Vec::new(),
            },
        );
        local.push((key, cert_id, witness));
    }
    Ok(local)
}

fn collect_local_heads(
    forms: &[Form],
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    rules: &HashMap<SenseKey, ResolvedDefine>,
    ctx: &mut Ctx<'_>,
) -> Result<(), CompileError> {
    for form in forms {
        match form {
            Form::Assume(formula) => {
                let sentence = require_raw_sentence(
                    formula,
                    names,
                    senses,
                    ctx,
                    "`assume` takes a sentence",
                )?;
                ctx.assumed.insert(sentence.text);
            }
            Form::Formula(formula) => collect_claimed_heads(formula, names, senses, ctx)?,
            Form::Certify(certify) => {
                if let CertifyHead::Formula(formula) = &certify.head {
                    let is_rule = match formula {
                        Formula::Apply {
                            reading: Reading::Sense(ident),
                            ..
                        } => resolve_sense_with_key(ident, senses, ctx.imports)
                            .is_ok_and(|(key, _)| rules.contains_key(&key)),
                        _ => false,
                    };
                    if !is_rule {
                        let sentence = require_raw_sentence(
                            formula,
                            names,
                            senses,
                            ctx,
                            "`certify` takes a defined sense, name, or fact",
                        )?;
                        ctx.claimed.insert(sentence.text);
                    }
                }
                collect_claimed_heads(&certify.support, names, senses, ctx)?;
            }
            Form::Ground(ground) => collect_claimed_heads(&ground.support, names, senses, ctx)?,
            Form::Name(_) | Form::Param(_) | Form::Sense(_) | Form::Define(_) | Form::Detail(_) => {
            }
        }
    }
    Ok(())
}

fn collect_claimed_heads(
    formula: &Formula,
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    ctx: &mut Ctx<'_>,
) -> Result<(), CompileError> {
    match formula {
        Formula::And(xs) | Formula::Or(xs) => {
            for x in xs {
                collect_claimed_heads(x, names, senses, ctx)?;
            }
        }
        Formula::Not(inner) => collect_claimed_heads(inner, names, senses, ctx)?,
        Formula::Implies {
            antecedent,
            consequent,
        } => {
            collect_claimed_heads(antecedent, names, senses, ctx)?;
            collect_claimed_heads(consequent, names, senses, ctx)?;
        }
        Formula::Claim { sentence, support } => {
            let head =
                require_raw_sentence(sentence, names, senses, ctx, "`claim` heads a sentence")?;
            ctx.claimed.insert(head.text);
            collect_claimed_heads(support, names, senses, ctx)?;
        }
        Formula::Apply { .. }
        | Formula::Witness { .. }
        | Formula::Spec { .. }
        | Formula::Exists { .. }
        | Formula::Forall { .. } => {}
    }
    Ok(())
}

fn resolve_forms(
    forms: &[Form],
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    defs: &Defs,
    scope_prefix: &str,
    ctx: &mut Ctx<'_>,
    assumptions: &mut Vec<String>,
) -> Result<Vec<Proposition>, CompileError> {
    let mut claims = Vec::new();
    for form in forms {
        match form {
            Form::Name(_) | Form::Param(_) | Form::Sense(_) | Form::Define(_) => {}
            Form::Assume(sentence) => {
                let text = require_raw_sentence(
                    sentence,
                    names,
                    senses,
                    ctx,
                    "`assume` takes a sentence",
                )?;
                assumptions.push(text.text.clone());
                claims.push(Proposition::Assume(text));
            }
            Form::Certify(certify) => {
                claims.push(resolve_certify(
                    certify,
                    names,
                    senses,
                    defs,
                    scope_prefix,
                    ctx,
                )?);
            }
            Form::Ground(ground) => {
                claims.push(resolve_ground(
                    ground,
                    names,
                    senses,
                    defs,
                    scope_prefix,
                    ctx,
                )?);
            }
            Form::Detail(body) => {
                claims.push(resolve_detail(
                    body,
                    names,
                    senses,
                    defs,
                    scope_prefix,
                    ctx,
                    assumptions,
                )?);
            }
            Form::Formula(formula) => {
                claims.push(resolve(formula, names, senses, defs, ctx)?);
            }
        }
    }
    Ok(claims)
}

fn resolve_certify(
    certify: &Certify,
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    defs: &Defs,
    scope_prefix: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Proposition, CompileError> {
    let formula = match &certify.head {
        CertifyHead::Name(ident) => {
            let Some((key, constraint)) = resolve_constraint_ident(
                ident,
                &defs.constraints,
                &defs.constraint_names,
                ctx.imports,
            )?
            else {
                return Err(CompileError {
                    message: format!("`{}` cannot be certified without a `define`", ident.display()),
                });
            };
            let cert_id = if scope_prefix.is_empty() {
                format!("certify:{}", key.display())
            } else {
                format!("certify:{scope_prefix}:{}", key.display())
            };
            let label = format!("certify {}", ident.display());
            let target = Sentence {
                text: format!("{}.", constraint.sentence.text.trim_end_matches('.')),
                grounded: format!("{}.", constraint.sentence.grounded.trim_end_matches('.')),
            };
            let mut deps = Vec::new();
            collect_define_deps(&constraint.body, &mut deps);
            let (requires, imported) = collect_cert_prereqs(&deps, defs, ctx);
            let support = match &certify.support {
                Formula::Witness { paths } => {
                    let paths = witness_paths(paths)?;
                    let paths = match &constraint.body {
                        DefinedBody::Spec { paths: spec, .. } => merge_witness_paths(spec, &paths),
                        _ => paths,
                    };
                    Support::Witness { paths }
                }
                other => Support::Sentence(Box::new(resolve(other, names, senses, defs, ctx)?)),
            };
            ctx.certified_rules.push((key, cert_id.clone()));
            return Ok(Proposition::Cert {
                cert_id,
                label,
                targets: vec![target],
                support,
                requires,
                imported,
            });
        }
        CertifyHead::Formula(formula) => formula,
    };

    if let Formula::Apply {
        reading: Reading::Sense(ident),
        args,
        at,
    } = formula
    {
        let (key, sense) = resolve_sense_with_key(ident, senses, ctx.imports)?;
        if let Some(def) = defs.rules.get(&key) {
            let params = extract_certify_params(args, &sense, ident)?;
            if sense.holes.is_empty() {
                note(ctx, ident, Place::Sentence, *at);
            }
            let cert_id = if scope_prefix.is_empty() {
                format!("certify:{}", key.display())
            } else {
                format!("certify:{scope_prefix}:{}", key.display())
            };
            let head =
                apply(&sense.template, &params).map_err(|message| CompileError { message })?;
            let label = format!("certify ({})", format_ground_head(ident, &params));
            let target = synthesize_certify_target(&params, &head, &def.body)?;
            let mut deps = Vec::new();
            collect_define_deps(&def.body, &mut deps);
            let (requires, imported) = collect_cert_prereqs(&deps, defs, ctx);
            let support = match &certify.support {
                Formula::Witness { paths } => {
                    let paths = witness_paths(paths)?;
                    let paths = match &def.body {
                        DefinedBody::Spec { paths: spec, .. } => merge_witness_paths(spec, &paths),
                        _ => paths,
                    };
                    Support::Witness { paths }
                }
                other => Support::Sentence(Box::new(resolve(other, names, senses, defs, ctx)?)),
            };
            ctx.certified_rules.push((key, cert_id.clone()));
            return Ok(Proposition::Cert {
                cert_id,
                label,
                targets: vec![target],
                support,
                requires,
                imported,
            });
        }
    }

    let (sentence, head_key) = require_raw_sentence_with_key(
        formula,
        names,
        senses,
        ctx,
        "`certify` takes a defined sense, name, or fact",
    )?;
    if !defs.facts.contains_key(&sentence.text) {
        let what = head_key
            .map(|(display, _)| display)
            .unwrap_or_else(|| sentence.text.clone());
        return Err(CompileError {
            message: format!("`{what}` cannot be certified without a `define`"),
        });
    }
    let support = match &certify.support {
        Formula::Witness { paths } => Support::Witness {
            paths: witness_paths(paths)?,
        },
        other => Support::Sentence(Box::new(resolve(other, names, senses, defs, ctx)?)),
    };
    ctx.certified_facts.push(sentence.text.clone());
    Ok(Proposition::Claim { sentence, support })
}

fn collect_cert_prereqs(
    dep_keys: &[SenseKey],
    defs: &Defs,
    ctx: &mut Ctx<'_>,
) -> (Vec<String>, Vec<Proposition>) {
    let mut requires = Vec::new();
    let mut imported = Vec::new();
    let mut seen_imported = HashSet::new();
    for dep in dep_keys {
        let Some(rule) = defs.certifies.get(dep) else {
            continue;
        };
        if !requires.contains(&rule.cert_id) {
            requires.push(rule.cert_id.clone());
        }
        if !rule.certificate.is_empty() && seen_imported.insert(rule.cert_id.clone()) {
            imported.extend(rule.certificate.iter().cloned());
            for assumption in &rule.assumptions {
                if !ctx.used_ground_assumptions.contains(assumption) {
                    ctx.used_ground_assumptions.push(assumption.clone());
                }
            }
        }
    }
    (requires, imported)
}

fn resolve_ground(
    ground: &Ground,
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    defs: &Defs,
    scope_prefix: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Proposition, CompileError> {
    let (key, sense) = resolve_sense_with_key(&ground.sense, senses, ctx.imports)?;
    let def = defs.rules.get(&key).ok_or_else(|| CompileError {
        message: format!(
            "`{}` cannot be grounded without a `define`",
            ground.sense.display()
        ),
    })?;
    let witness = witness_paths(&ground.witness)?;
    let cert_id = if scope_prefix.is_empty() {
        format!("ground:{}", key.display())
    } else {
        format!("ground:{scope_prefix}:{}", key.display())
    };
    let head = apply(&sense.template, &ground.params)
        .map_err(|message| CompileError { message })?;
    let witness_display = format_witness_list(&witness);
    let label = format!(
        "ground ({}) via {witness_display}",
        format_ground_head(&ground.sense, &ground.params)
    );
    let has_certify = defs.certifies.contains_key(&key);
    let (targets, requires, imported) = if has_certify {
        let (requires, imported) = collect_cert_prereqs(std::slice::from_ref(&key), defs, ctx);
        (
            vec![synthesize_provenance_target(
                &ground.params,
                &head,
                &witness_display,
            )],
            requires,
            imported,
        )
    } else {
        let mut deps = Vec::new();
        collect_define_deps(&def.body, &mut deps);
        let (requires, imported) = collect_cert_prereqs(&deps, defs, ctx);
        (
            synthesize_targets(&ground.params, &head, &witness_display, &def.body)?,
            requires,
            imported,
        )
    };
    let support = match &ground.support {
        Formula::Witness { paths } => {
            let paths = witness_paths(paths)?;
            let paths = if !has_certify {
                match &def.body {
                    DefinedBody::Spec { paths: spec, .. } => merge_witness_paths(spec, &paths),
                    _ => paths,
                }
            } else {
                paths
            };
            Support::Witness { paths }
        }
        other => Support::Sentence(Box::new(resolve(other, names, senses, defs, ctx)?)),
    };
    Ok(Proposition::Cert {
        cert_id,
        label,
        targets,
        support,
        requires,
        imported,
    })
}

fn format_forall_prefix(params: &[String]) -> String {
    match params {
        [] => String::new(),
        [only] => format!("For all {only}, "),
        [a, b] => format!("For all {a} and {b}, "),
        many => {
            let (last, init) = many.split_last().expect("non-empty");
            format!("For all {}, and {last}, ", init.join(", "))
        }
    }
}

fn synthesize_certify_target(
    params: &[String],
    head: &str,
    body: &DefinedBody,
) -> Result<Sentence, CompileError> {
    let prefix = format_forall_prefix(params);
    let head = head.trim_end_matches('.');
    if let DefinedBody::Spec { labels, .. } = body {
        let spec_list = format_witness_list(labels);
        let rhs = match params {
            [] => format!("the specification in {spec_list} holds"),
            [only] => format!("{only} satisfies the specification in {spec_list}"),
            many => format!(
                "{} satisfy the specification in {spec_list}",
                format_witness_list(many)
            ),
        };
        let text = format!("{prefix}{head} if and only if {rhs}.");
        return Ok(Sentence {
            grounded: text.clone(),
            text,
        });
    }
    let param_spellings: Vec<Spelling> = params
        .iter()
        .map(|p| Spelling {
            shown: p.clone(),
            grounded: p.clone(),
        })
        .collect();
    let shown = render_defined_body(body, &param_spellings, false, true)?;
    let grounded = render_defined_body(body, &param_spellings, true, true)?;
    let shown = shown.trim_end_matches('.');
    let grounded = grounded.trim_end_matches('.');
    Ok(Sentence {
        text: format!("{prefix}{head} if and only if {shown}."),
        grounded: format!("{prefix}{head} if and only if {grounded}."),
    })
}

fn synthesize_provenance_target(params: &[String], head: &str, witness: &str) -> Sentence {
    let prefix = format_quantifier_prefix(params, head, witness);
    let head = head.trim_end_matches('.');
    let text = format!("{prefix}{head}.");
    Sentence {
        grounded: text.clone(),
        text,
    }
}

fn format_ground_head(sense: &Ident, params: &[String]) -> String {
    if params.is_empty() {
        sense.display()
    } else {
        format!("{} {}", sense.display(), params.join(" "))
    }
}

fn display_witness_path(path: &str) -> &str {
    match parse_dollar_witness(path) {
        Some((_, Some(rel))) => rel,
        _ => path,
    }
}

fn format_witness_list(paths: &[String]) -> String {
    match paths {
        [] => String::new(),
        [only] => display_witness_path(only).to_string(),
        [a, b] => format!("{} and {}", display_witness_path(a), display_witness_path(b)),
        many => {
            let (last, init) = many.split_last().expect("non-empty");
            let head: Vec<&str> = init.iter().map(|p| display_witness_path(p)).collect();
            format!("{}, and {}", head.join(", "), display_witness_path(last))
        }
    }
}

fn format_quantifier_prefix(params: &[String], head: &str, witness: &str) -> String {
    let head = head.trim_end_matches('.');
    match params {
        [] => format!("If {witness} supports that {head}, then "),
        [only] => format!("For all {only}, if {witness} supports that {head}, then "),
        [a, b] => format!("For all {a} and {b}, if {witness} supports that {head}, then "),
        many => {
            let (last, init) = many.split_last().expect("non-empty");
            format!(
                "For all {}, and {last}, if {witness} supports that {head}, then ",
                init.join(", ")
            )
        }
    }
}

fn synthesize_targets(
    params: &[String],
    head: &str,
    witness: &str,
    body: &DefinedBody,
) -> Result<Vec<Sentence>, CompileError> {
    let prefix = format_quantifier_prefix(params, head, witness);
    let param_spellings: Vec<Spelling> = params
        .iter()
        .map(|p| Spelling {
            shown: p.clone(),
            grounded: p.clone(),
        })
        .collect();
    let mut conjuncts = Vec::new();
    collect_conjuncts(body, &mut conjuncts);
    let mut targets = Vec::with_capacity(conjuncts.len());
    for conjunct in conjuncts {
        let shown = render_defined_body(conjunct, &param_spellings, false, false)?;
        let grounded = render_defined_body(conjunct, &param_spellings, true, false)?;
        let shown = shown.trim_end_matches('.');
        let grounded = grounded.trim_end_matches('.');
        targets.push(Sentence {
            text: format!("{prefix}{shown}."),
            grounded: format!("{prefix}{grounded}."),
        });
    }
    Ok(targets)
}

fn collect_conjuncts<'a>(body: &'a DefinedBody, out: &mut Vec<&'a DefinedBody>) {
    match body {
        DefinedBody::And(xs) => {
            for x in xs {
                collect_conjuncts(x, out);
            }
        }
        other => out.push(other),
    }
}

fn format_exists_prefix(vars: &[String]) -> String {
    match vars {
        [] => String::new(),
        [only] => format!("there exists {only} such that "),
        [a, b] => format!("there exist {a} and {b} such that "),
        many => {
            let (last, init) = many.split_last().expect("non-empty");
            format!("there exist {}, and {last} such that ", init.join(", "))
        }
    }
}

fn format_forall_lower(vars: &[String]) -> String {
    match vars {
        [] => String::new(),
        [only] => format!("for all {only}, "),
        [a, b] => format!("for all {a} and {b}, "),
        many => {
            let (last, init) = many.split_last().expect("non-empty");
            format!("for all {}, and {last}, ", init.join(", "))
        }
    }
}

fn render_defined_body(
    body: &DefinedBody,
    params: &[Spelling],
    use_grounded: bool,
    nested: bool,
) -> Result<String, CompileError> {
    match body {
        DefinedBody::Apply { template, args, .. } => {
            let values: Vec<String> = args
                .iter()
                .map(|arg| match arg {
                    TemplateArg::Param(index) => {
                        if use_grounded {
                            params[*index].grounded.clone()
                        } else {
                            params[*index].shown.clone()
                        }
                    }
                    TemplateArg::Bound(name) => name.clone(),
                    TemplateArg::Fixed { shown, grounded } => {
                        if use_grounded {
                            grounded.clone()
                        } else {
                            shown.clone()
                        }
                    }
                })
                .collect();
            apply(template, &values).map_err(|message| CompileError { message })
        }
        DefinedBody::Not(inner) => {
            let rendered = render_defined_body(inner, params, use_grounded, true)?;
            if nested {
                Ok(format!("(it is not the case that {rendered})"))
            } else {
                Ok(format!("it is not the case that {rendered}"))
            }
        }
        DefinedBody::Implies {
            antecedent,
            consequent,
        } => {
            let ant = render_defined_body(antecedent, params, use_grounded, true)?;
            let cons = render_defined_body(consequent, params, use_grounded, true)?;
            let rendered = format!("if {ant}, then {cons}");
            if nested {
                Ok(format!("({rendered})"))
            } else {
                Ok(rendered)
            }
        }
        DefinedBody::Exists { vars, body } => {
            let rendered = render_defined_body(body, params, use_grounded, true)?;
            Ok(format!("{}{rendered}", format_exists_prefix(vars)))
        }
        DefinedBody::Forall { vars, body } => {
            let rendered = render_defined_body(body, params, use_grounded, false)?;
            let out = format!("{}{rendered}", format_forall_lower(vars));
            if nested {
                Ok(format!("({out})"))
            } else {
                Ok(out)
            }
        }
        DefinedBody::Or(xs) => {
            let mut parts = Vec::with_capacity(xs.len());
            for x in xs {
                parts.push(render_defined_body(x, params, use_grounded, true)?);
            }
            let joined = parts.join(" or ");
            if nested && xs.len() > 1 {
                Ok(format!("({joined})"))
            } else {
                Ok(joined)
            }
        }
        DefinedBody::And(xs) => {
            let mut parts = Vec::with_capacity(xs.len());
            for x in xs {
                parts.push(render_defined_body(x, params, use_grounded, true)?);
            }
            let joined = parts.join(" and ");
            if nested && xs.len() > 1 {
                Ok(format!("({joined})"))
            } else {
                Ok(joined)
            }
        }
        DefinedBody::Spec { labels, .. } => Ok(format!(
            "this holds under the specification in {}",
            format_witness_list(labels)
        )),
    }
}

/// Names and senses declared in `body` are visible inside the detail and
/// nowhere else. The scope contributes the sentences it validates.
fn resolve_detail(
    body: &[Form],
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    defs: &Defs,
    scope_prefix: &str,
    ctx: &mut Ctx<'_>,
    assumptions: &mut Vec<String>,
) -> Result<Proposition, CompileError> {
    if !contains_claim_or_assume(body) {
        return Err(CompileError {
            message: "a detail has no claim".into(),
        });
    }
    let detail_id = ctx.next_detail_id;
    ctx.next_detail_id += 1;
    let detail_owner = if scope_prefix.is_empty() {
        format!("#detail{detail_id}")
    } else {
        format!("{scope_prefix}#detail{detail_id}")
    };
    let (names, senses) = extend_scope_with_keys(body, names, senses, &detail_owner)?;
    ctx.scopes.push(scope_decls(body));
    let mut defs = defs.clone();
    collect_defines(body, &names, &senses, &detail_owner, &mut defs, ctx)?;
    check_define_cycles(&defs.rules)?;
    collect_certifies(
        body,
        &senses,
        &defs.rules,
        &defs.constraints,
        &defs.constraint_names,
        &detail_owner,
        &mut defs.certifies,
        ctx,
    )?;
    collect_grounds(
        body,
        &senses,
        &defs.rules,
        &detail_owner,
        &mut defs.grounds,
        ctx,
    )?;
    collect_local_heads(body, &names, &senses, &defs.rules, ctx)?;
    let body = resolve_forms(
        body,
        &names,
        &senses,
        &defs,
        &detail_owner,
        ctx,
        assumptions,
    )?;
    let scope = ctx.scopes.pop().expect("detail scope");
    ctx.terms.extend(finish_scope(scope)?);
    Ok(Proposition::Scope { body })
}

fn resolve(
    formula: &Formula,
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    defs: &Defs,
    ctx: &mut Ctx<'_>,
) -> Result<Proposition, CompileError> {
    match formula {
        Formula::And(xs) => Ok(Proposition::And(resolve_all(xs, names, senses, defs, ctx)?)),
        Formula::Or(xs) => Ok(Proposition::Or(resolve_all(xs, names, senses, defs, ctx)?)),
        Formula::Not(x) => Ok(Proposition::Not(Box::new(resolve(
            x, names, senses, defs, ctx,
        )?))),
        Formula::Implies {
            antecedent,
            consequent,
        } => {
            let ant = resolve(antecedent, names, senses, defs, ctx)?;
            let cons = resolve(consequent, names, senses, defs, ctx)?;
            Ok(Proposition::Or(vec![Proposition::Not(Box::new(ant)), cons]))
        }
        Formula::Exists { .. } | Formula::Forall { .. } => {
            let body = resolve_define_body(formula, &[], &[], names, senses, ctx)?;
            instantiate_defined_body(&body, &[], defs, ctx)
        }
        Formula::Claim { sentence, support } => {
            let (sentence, head_key) = require_raw_sentence_with_key(
                sentence,
                names,
                senses,
                ctx,
                "`claim` heads a sentence",
            )?;
            if let Some((ident_display, key)) = head_key {
                if defs.rules.contains_key(&key) {
                    return Err(CompileError {
                        message: format!("`{ident_display}` is already defined by `define`"),
                    });
                }
            }
            let support = match support.as_ref() {
                Formula::Witness { paths } => Support::Witness {
                    paths: witness_paths(paths)?,
                },
                other => Support::Sentence(Box::new(resolve(other, names, senses, defs, ctx)?)),
            };
            Ok(Proposition::Claim { sentence, support })
        }
        Formula::Witness { .. } => Err(CompileError {
            message: "`witness` is only the support of a claim".into(),
        }),
        Formula::Spec { .. } => Err(CompileError {
            message: "`spec` is only allowed as the body of `define`".into(),
        }),
        Formula::Apply { reading, args, at } => {
            let mut arg_spellings = Vec::with_capacity(args.len());
            for arg in args {
                arg_spellings.push(resolve_arg(arg, names, senses, ctx)?);
            }
            let (what, sense_key, template, holes, head) = match reading {
                Reading::Sense(ident) => {
                    let (key, sense) = resolve_sense_with_key(ident, senses, ctx.imports)?;
                    (
                        format!("`{}`", ident.display()),
                        Some(key),
                        sense.template,
                        sense.holes,
                        Some(ident),
                    )
                }
                Reading::Inline { template, holes } => (
                    format!("\"{template}\""),
                    None,
                    template.clone(),
                    holes.clone(),
                    None,
                ),
            };
            if arg_spellings.len() != holes.len() {
                return Err(arity_error(&what, &holes, arg_spellings.len()));
            }
            if holes.is_empty() {
                if let Some(ident) = head {
                    note(ctx, ident, Place::Sentence, *at);
                }
            }
            resolve_applied_sentence(sense_key.as_ref(), &template, &arg_spellings, defs, ctx)
        }
    }
}

fn resolve_applied_sentence(
    sense_key: Option<&SenseKey>,
    template: &str,
    arg_spellings: &[Spelling],
    defs: &Defs,
    ctx: &mut Ctx<'_>,
) -> Result<Proposition, CompileError> {
    let shown_args: Vec<String> = arg_spellings.iter().map(|s| s.shown.clone()).collect();
    let grounded_args: Vec<String> = arg_spellings.iter().map(|s| s.grounded.clone()).collect();
    let text = apply(template, &shown_args).map_err(|message| CompileError { message })?;
    let grounded = apply(template, &grounded_args).map_err(|message| CompileError { message })?;
    let sentence = Sentence { text, grounded };
    remember(&mut ctx.expansions, &sentence)?;

    if ctx.assumed.contains(&sentence.text) || ctx.claimed.contains(&sentence.text) {
        return Ok(Proposition::Sentence(sentence));
    }
    if let Some(key) = sense_key {
        if let Some(ground) = defs.grounds.get(key) {
            for assumption in &ground.assumptions {
                if !ctx.used_ground_assumptions.contains(assumption) {
                    ctx.used_ground_assumptions.push(assumption.clone());
                }
            }
            return Ok(Proposition::Grounded {
                sentence,
                cert_id: ground.cert_id.clone(),
                certificate: ground.certificate.clone(),
                witness: ground.witness.clone(),
            });
        }
        if let Some(def) = defs.rules.get(key) {
            let body = instantiate_defined_body(&def.body, arg_spellings, defs, ctx)?;
            return Ok(Proposition::Defined {
                sentence,
                body: Box::new(body),
            });
        }
    }
    if defs.facts.contains_key(&sentence.text) {
        return Ok(Proposition::Defined {
            sentence,
            body: Box::new(Proposition::And(Vec::new())),
        });
    }

    Ok(Proposition::Sentence(sentence))
}

fn instantiate_defined_body(
    body: &DefinedBody,
    params: &[Spelling],
    defs: &Defs,
    ctx: &mut Ctx<'_>,
) -> Result<Proposition, CompileError> {
    match body {
        DefinedBody::And(xs) => {
            let mut out = Vec::with_capacity(xs.len());
            for x in xs {
                out.push(instantiate_defined_body(x, params, defs, ctx)?);
            }
            Ok(Proposition::And(out))
        }
        DefinedBody::Or(xs) => {
            let mut out = Vec::with_capacity(xs.len());
            for x in xs {
                out.push(instantiate_defined_body(x, params, defs, ctx)?);
            }
            Ok(Proposition::Or(out))
        }
        DefinedBody::Not(inner) => Ok(Proposition::Not(Box::new(instantiate_defined_body(
            inner, params, defs, ctx,
        )?))),
        DefinedBody::Implies {
            antecedent,
            consequent,
        } => {
            let ant = instantiate_defined_body(antecedent, params, defs, ctx)?;
            let cons = instantiate_defined_body(consequent, params, defs, ctx)?;
            Ok(Proposition::Or(vec![Proposition::Not(Box::new(ant)), cons]))
        }
        DefinedBody::Exists { .. } | DefinedBody::Forall { .. } => {
            let text = render_defined_body(body, params, false, false)?;
            let grounded = render_defined_body(body, params, true, false)?;
            let sentence = Sentence { text, grounded };
            remember(&mut ctx.expansions, &sentence)?;
            if !ctx.assumed.contains(&sentence.text)
                && !ctx.claimed.contains(&sentence.text)
                && defs.facts.contains_key(&sentence.text)
            {
                return Ok(Proposition::Defined {
                    sentence,
                    body: Box::new(Proposition::And(Vec::new())),
                });
            }
            Ok(Proposition::Sentence(sentence))
        }
        DefinedBody::Apply {
            sense_key,
            template,
            args,
        } => {
            let mut arg_spellings = Vec::with_capacity(args.len());
            for arg in args {
                match arg {
                    TemplateArg::Param(index) => arg_spellings.push(params[*index].clone()),
                    TemplateArg::Bound(name) => arg_spellings.push(Spelling {
                        shown: name.clone(),
                        grounded: name.clone(),
                    }),
                    TemplateArg::Fixed { shown, grounded } => arg_spellings.push(Spelling {
                        shown: shown.clone(),
                        grounded: grounded.clone(),
                    }),
                }
            }
            resolve_applied_sentence(sense_key.as_ref(), template, &arg_spellings, defs, ctx)
        }
        DefinedBody::Spec { .. } => Err(CompileError {
            message: "a `spec` definition must be certified or grounded".into(),
        }),
    }
}

fn require_raw_sentence(
    formula: &Formula,
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    ctx: &mut Ctx<'_>,
    message: &str,
) -> Result<Sentence, CompileError> {
    Ok(require_raw_sentence_with_key(formula, names, senses, ctx, message)?.0)
}

fn require_raw_sentence_with_key(
    formula: &Formula,
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    ctx: &mut Ctx<'_>,
    message: &str,
) -> Result<(Sentence, Option<(String, SenseKey)>), CompileError> {
    if matches!(
        formula,
        Formula::Exists { .. }
            | Formula::Forall { .. }
            | Formula::Implies { .. }
            | Formula::Not(_)
    ) {
        let body = resolve_define_body(formula, &[], &[], names, senses, ctx)?;
        let text = render_defined_body(&body, &[], false, false)?;
        let grounded = render_defined_body(&body, &[], true, false)?;
        let sentence = Sentence { text, grounded };
        remember(&mut ctx.expansions, &sentence)?;
        return Ok((sentence, None));
    }
    let Formula::Apply { reading, args, at } = formula else {
        return Err(CompileError {
            message: message.into(),
        });
    };
    let mut shown_args = Vec::with_capacity(args.len());
    let mut grounded_args = Vec::with_capacity(args.len());
    for arg in args {
        let spelling = resolve_arg(arg, names, senses, ctx)?;
        shown_args.push(spelling.shown);
        grounded_args.push(spelling.grounded);
    }
    let (what, head_key, template, holes, head) = match reading {
        Reading::Sense(ident) => {
            let (key, sense) = resolve_sense_with_key(ident, senses, ctx.imports)?;
            (
                format!("`{}`", ident.display()),
                Some((ident.display(), key)),
                sense.template,
                sense.holes,
                Some(ident),
            )
        }
        Reading::Inline { template, holes } => (
            format!("\"{template}\""),
            None,
            template.clone(),
            holes.clone(),
            None,
        ),
    };
    if shown_args.len() != holes.len() {
        return Err(arity_error(&what, &holes, shown_args.len()));
    }
    if holes.is_empty() {
        if let Some(ident) = head {
            note(ctx, ident, Place::Sentence, *at);
        }
    }
    let text = apply(&template, &shown_args).map_err(|message| CompileError { message })?;
    let grounded = apply(&template, &grounded_args).map_err(|message| CompileError { message })?;
    let sentence = Sentence { text, grounded };
    remember(&mut ctx.expansions, &sentence)?;
    Ok((sentence, head_key))
}

fn export_certifies(
    local_certifies: &[(SenseKey, String)],
    claims: &[Proposition],
) -> HashMap<SenseKey, ExportedCertify> {
    if local_certifies.is_empty() {
        return HashMap::new();
    }
    let mut local_defs: HashMap<String, Proposition> = HashMap::new();
    let mut local_certs: HashMap<String, Proposition> = HashMap::new();
    collect_exportable_props(claims, &mut local_defs, &mut local_certs);

    let mut exported = HashMap::new();
    for (sense_key, cert_id) in local_certifies {
        let Some((bundled, assumptions)) = bundle_cert(cert_id, &local_defs, &local_certs) else {
            continue;
        };
        exported.insert(
            sense_key.clone(),
            ExportedCertify {
                cert_id: cert_id.clone(),
                certificate: bundled,
                assumptions,
            },
        );
    }
    exported
}

fn export_fact_certifies(
    certified_facts: &[String],
    claims: &[Proposition],
) -> HashMap<String, ExportedCertify> {
    if certified_facts.is_empty() {
        return HashMap::new();
    }
    let mut local_defs: HashMap<String, Proposition> = HashMap::new();
    let mut local_certs: HashMap<String, Proposition> = HashMap::new();
    collect_exportable_props(claims, &mut local_defs, &mut local_certs);

    let mut exported = HashMap::new();
    for fact in certified_facts {
        let Some(claim_prop) = local_defs.get(fact) else {
            continue;
        };
        let mut inline_defs = HashSet::new();
        collect_inline_defs(claim_prop, &mut inline_defs);
        let mut bundled = Vec::new();
        let mut visited_defs = HashSet::new();
        visited_defs.insert(fact.clone());
        let mut visited_certs = HashSet::new();
        bundle_deps(
            claim_prop,
            &inline_defs,
            &local_defs,
            &local_certs,
            &mut visited_defs,
            &mut visited_certs,
            &mut bundled,
        );
        bundled.push(claim_prop.clone());
        let mut assumptions = Vec::new();
        collect_assumptions_from_props(&bundled, &mut assumptions);
        exported.insert(
            fact.clone(),
            ExportedCertify {
                cert_id: format!("fact:{fact}"),
                certificate: bundled,
                assumptions,
            },
        );
    }
    exported
}

fn export_grounds(
    local_grounds: &[(SenseKey, String, Vec<String>)],
    claims: &[Proposition],
) -> HashMap<SenseKey, ExportedGround> {
    if local_grounds.is_empty() {
        return HashMap::new();
    }
    let mut local_defs: HashMap<String, Proposition> = HashMap::new();
    let mut local_certs: HashMap<String, Proposition> = HashMap::new();
    collect_exportable_props(claims, &mut local_defs, &mut local_certs);

    let mut exported = HashMap::new();
    for (sense_key, cert_id, witness) in local_grounds {
        let Some((bundled, assumptions)) = bundle_cert(cert_id, &local_defs, &local_certs) else {
            continue;
        };
        exported.insert(
            sense_key.clone(),
            ExportedGround {
                cert_id: cert_id.clone(),
                witness: witness.clone(),
                certificate: bundled,
                assumptions,
            },
        );
    }
    exported
}

fn bundle_cert(
    cert_id: &str,
    local_defs: &HashMap<String, Proposition>,
    local_certs: &HashMap<String, Proposition>,
) -> Option<(Vec<Proposition>, Vec<String>)> {
    let cert_prop = local_certs.get(cert_id)?;
    let mut inline_defs = HashSet::new();
    collect_inline_defs(cert_prop, &mut inline_defs);

    let mut bundled = Vec::new();
    let mut visited_defs = HashSet::new();
    let mut visited_certs = HashSet::new();
    visited_certs.insert(cert_id.to_string());
    bundle_deps(
        cert_prop,
        &inline_defs,
        local_defs,
        local_certs,
        &mut visited_defs,
        &mut visited_certs,
        &mut bundled,
    );
    bundled.push(cert_prop.clone());

    let mut assumptions = Vec::new();
    collect_assumptions_from_props(&bundled, &mut assumptions);
    Some((bundled, assumptions))
}

fn collect_exportable_props(
    props: &[Proposition],
    defs: &mut HashMap<String, Proposition>,
    certs: &mut HashMap<String, Proposition>,
) {
    for prop in props {
        match prop {
            Proposition::And(xs) | Proposition::Or(xs) | Proposition::Scope { body: xs } => {
                collect_exportable_props(xs, defs, certs);
            }
            Proposition::Not(inner) => {
                collect_exportable_props(std::slice::from_ref(inner), defs, certs);
            }
            Proposition::Defined { body, .. } => {
                collect_exportable_props(std::slice::from_ref(body), defs, certs);
            }
            Proposition::Claim { sentence, support } => {
                if let Support::Sentence(inner) = support {
                    collect_exportable_props(std::slice::from_ref(inner), defs, certs);
                }
                defs.entry(sentence.text.clone())
                    .or_insert_with(|| prop.clone());
            }
            Proposition::Assume(sentence) => {
                defs.entry(sentence.text.clone())
                    .or_insert_with(|| prop.clone());
            }
            Proposition::Cert {
                cert_id, support, ..
            } => {
                if let Support::Sentence(inner) = support {
                    collect_exportable_props(std::slice::from_ref(inner), defs, certs);
                }
                certs
                    .entry(cert_id.clone())
                    .or_insert_with(|| prop.clone());
            }
            Proposition::Sentence(_) | Proposition::Grounded { .. } => {}
        }
    }
}

fn collect_inline_defs(prop: &Proposition, out: &mut HashSet<String>) {
    match prop {
        Proposition::And(xs) | Proposition::Or(xs) | Proposition::Scope { body: xs } => {
            for x in xs {
                collect_inline_defs(x, out);
            }
        }
        Proposition::Not(inner) | Proposition::Defined { body: inner, .. } => {
            collect_inline_defs(inner, out);
        }
        Proposition::Claim { sentence, support } => {
            out.insert(sentence.text.clone());
            if let Support::Sentence(inner) = support {
                collect_inline_defs(inner, out);
            }
        }
        Proposition::Assume(sentence) => {
            out.insert(sentence.text.clone());
        }
        Proposition::Cert { support, .. } => {
            if let Support::Sentence(inner) = support {
                collect_inline_defs(inner, out);
            }
        }
        Proposition::Sentence(_) | Proposition::Grounded { .. } => {}
    }
}

fn bundle_deps(
    prop: &Proposition,
    inline_defs: &HashSet<String>,
    local_defs: &HashMap<String, Proposition>,
    local_certs: &HashMap<String, Proposition>,
    visited_defs: &mut HashSet<String>,
    visited_certs: &mut HashSet<String>,
    out: &mut Vec<Proposition>,
) {
    match prop {
        Proposition::And(xs) | Proposition::Or(xs) | Proposition::Scope { body: xs } => {
            for x in xs {
                bundle_deps(
                    x,
                    inline_defs,
                    local_defs,
                    local_certs,
                    visited_defs,
                    visited_certs,
                    out,
                );
            }
        }
        Proposition::Not(inner) | Proposition::Defined { body: inner, .. } => {
            bundle_deps(
                inner,
                inline_defs,
                local_defs,
                local_certs,
                visited_defs,
                visited_certs,
                out,
            );
        }
        Proposition::Claim { support, .. } => {
            if let Support::Sentence(inner) = support {
                bundle_deps(
                    inner,
                    inline_defs,
                    local_defs,
                    local_certs,
                    visited_defs,
                    visited_certs,
                    out,
                );
            }
        }
        Proposition::Cert {
            support, requires, ..
        } => {
            for req_id in requires {
                if !visited_certs.contains(req_id) {
                    if let Some(req_prop) = local_certs.get(req_id) {
                        visited_certs.insert(req_id.clone());
                        bundle_deps(
                            req_prop,
                            inline_defs,
                            local_defs,
                            local_certs,
                            visited_defs,
                            visited_certs,
                            out,
                        );
                        out.push(req_prop.clone());
                    }
                }
            }
            if let Support::Sentence(inner) = support {
                bundle_deps(
                    inner,
                    inline_defs,
                    local_defs,
                    local_certs,
                    visited_defs,
                    visited_certs,
                    out,
                );
            }
        }
        Proposition::Sentence(sentence) => {
            if inline_defs.contains(&sentence.text) || visited_defs.contains(&sentence.text) {
                return;
            }
            if let Some(def_prop) = local_defs.get(&sentence.text) {
                visited_defs.insert(sentence.text.clone());
                bundle_deps(
                    def_prop,
                    inline_defs,
                    local_defs,
                    local_certs,
                    visited_defs,
                    visited_certs,
                    out,
                );
                out.push(def_prop.clone());
            }
        }
        Proposition::Grounded {
            cert_id,
            certificate,
            ..
        } => {
            if certificate.is_empty() && !visited_certs.contains(cert_id) {
                if let Some(cert_prop) = local_certs.get(cert_id) {
                    visited_certs.insert(cert_id.clone());
                    bundle_deps(
                        cert_prop,
                        inline_defs,
                        local_defs,
                        local_certs,
                        visited_defs,
                        visited_certs,
                        out,
                    );
                    out.push(cert_prop.clone());
                }
            }
        }
        Proposition::Assume(_) => {}
    }
}

fn collect_assumptions_from_props(props: &[Proposition], out: &mut Vec<String>) {
    for prop in props {
        match prop {
            Proposition::Assume(sentence) => {
                if !out.contains(&sentence.text) {
                    out.push(sentence.text.clone());
                }
            }
            Proposition::And(xs) | Proposition::Or(xs) | Proposition::Scope { body: xs } => {
                collect_assumptions_from_props(xs, out);
            }
            Proposition::Not(inner) | Proposition::Defined { body: inner, .. } => {
                collect_assumptions_from_props(std::slice::from_ref(inner), out);
            }
            Proposition::Claim { support, .. } => {
                if let Support::Sentence(inner) = support {
                    collect_assumptions_from_props(std::slice::from_ref(inner), out);
                }
            }
            Proposition::Cert {
                support, imported, ..
            } => {
                collect_assumptions_from_props(imported, out);
                if let Support::Sentence(inner) = support {
                    collect_assumptions_from_props(std::slice::from_ref(inner), out);
                }
            }
            Proposition::Grounded { certificate, .. } => {
                collect_assumptions_from_props(certificate, out);
            }
            Proposition::Sentence(_) => {}
        }
    }
}

fn witness_paths(paths: &[String]) -> Result<Vec<String>, CompileError> {
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
    Ok(paths.to_vec())
}

fn spec_paths(paths: &[String]) -> Result<Vec<String>, CompileError> {
    let mut seen = HashSet::new();
    for path in paths {
        if path.starts_with('$') || !is_brief_path(path) {
            return Err(CompileError {
                message: format!("`{path}` is not a path relative to the brief"),
            });
        }
        if !seen.insert(path.clone()) {
            return Err(CompileError {
                message: format!("spec cites `{path}` twice"),
            });
        }
    }
    Ok(paths.to_vec())
}

fn merge_witness_paths(spec: &[String], support: &[String]) -> Vec<String> {
    let mut merged = Vec::with_capacity(spec.len() + support.len());
    for path in spec.iter().chain(support.iter()) {
        if !merged.contains(path) {
            merged.push(path.clone());
        }
    }
    merged
}

/// A witness path is relative to the brief file. `..` is a parent of that file.
fn is_brief_path(path: &str) -> bool {
    if parse_dollar_witness(path).is_some() {
        return true;
    }
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
            Form::Param(_) => {
                return Err(CompileError {
                    message: "`param` cannot appear inside a `detail`".into(),
                });
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
            Form::Define(_)
            | Form::Certify(_)
            | Form::Ground(_)
            | Form::Assume(_)
            | Form::Detail(_)
            | Form::Formula(_) => {}
        }
    }
    Ok((names, senses))
}

fn extend_scope_with_keys(
    body: &[Form],
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    owner: &str,
) -> Result<(HashSet<String>, HashMap<String, (SenseKey, Sense)>), CompileError> {
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
            Form::Param(_) => {
                return Err(CompileError {
                    message: "`param` cannot appear inside a `detail`".into(),
                });
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
                senses.insert(
                    sense.name.clone(),
                    (
                        SenseKey {
                            owner: owner.to_string(),
                            name: sense.name.clone(),
                        },
                        sense.clone(),
                    ),
                );
            }
            Form::Define(_)
            | Form::Certify(_)
            | Form::Ground(_)
            | Form::Assume(_)
            | Form::Detail(_)
            | Form::Formula(_) => {}
        }
    }
    Ok((names, senses))
}

fn resolve_all(
    formulas: &[Formula],
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    defs: &Defs,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Proposition>, CompileError> {
    let mut out = Vec::with_capacity(formulas.len());
    for formula in formulas {
        out.push(resolve(formula, names, senses, defs, ctx)?);
    }
    Ok(out)
}

fn resolve_arg(
    arg: &Arg,
    names: &HashSet<String>,
    senses: &HashMap<String, (SenseKey, Sense)>,
    ctx: &mut Ctx<'_>,
) -> Result<Spelling, CompileError> {
    match arg {
        Arg::Literal(text) => Ok(Spelling {
            shown: text.clone(),
            grounded: text.clone(),
        }),
        Arg::Name(ident) => {
            let name = resolve_name(ident, names, ctx.imports)?;
            if ident.qualifier.is_none() {
                if let Some(value) = ctx.param_values.get(&name) {
                    return Ok(Spelling {
                        shown: value.clone(),
                        grounded: value.clone(),
                    });
                }
            }
            Ok(Spelling {
                shown: name.clone(),
                grounded: name,
            })
        }
        Arg::Term { ident, at } => {
            let (_, sense) = resolve_sense_with_key(ident, senses, ctx.imports)?;
            if !sense.holes.is_empty() {
                return Err(arity_error(
                    &format!("`{}`", ident.display()),
                    &sense.holes,
                    0,
                ));
            }
            note(ctx, ident, Place::Term, *at);
            Ok(Spelling {
                shown: ident.name.clone(),
                grounded: sense.template,
            })
        }
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

fn resolve_sense_with_key(
    ident: &Ident,
    senses: &HashMap<String, (SenseKey, Sense)>,
    imports: &Imports,
) -> Result<(SenseKey, Sense), CompileError> {
    if let Some(qualifier) = &ident.qualifier {
        let namespace = required(qualifier, imports)?;
        let Some(sense) = imports.opened[&namespace].senses.get(&ident.name) else {
            return Err(CompileError {
                message: format!("`{}` is not a sense in `{namespace}`", ident.name),
            });
        };
        return Ok((
            SenseKey {
                owner: namespace,
                name: ident.name.clone(),
            },
            sense.clone(),
        ));
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
(claim "named" (part_of X Y))
(claim "inline" ("{Name} is a part of {Name}" X Y))
"#,
        );
        assert_eq!(
            c.claims,
            vec![
                Proposition::Claim {
                    sentence: "named".into(),
                    support: Support::Sentence(Box::new(Proposition::Sentence(
                        "X is a part of Y".into()
                    ))),
                },
                Proposition::Claim {
                    sentence: "inline".into(),
                    support: Support::Sentence(Box::new(Proposition::Sentence(
                        "X is a part of Y".into()
                    ))),
                },
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
(claim "both" (and (calls Search Oracle) (not (calls Universe Oracle))))
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Claim {
                sentence: "both".into(),
                support: Support::Sentence(Box::new(Proposition::And(vec![
                    Proposition::Sentence("Search calls Oracle".into()),
                    Proposition::Not(Box::new(Proposition::Sentence(
                        "Universe calls Oracle".into()
                    ))),
                ]))),
            }]
        );
    }

    #[test]
    fn declarations_may_follow_uses() {
        let c = compiled(
            r#"
(claim "edge" (depends_on Frontend Backend))
(name Frontend)
(name Backend)
(sense depends_on "{X} depends on {Y}")
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Claim {
                sentence: "edge".into(),
                support: Support::Sentence(Box::new(Proposition::Sentence(
                    "Frontend depends on Backend".into()
                ))),
            }]
        );
    }

    #[test]
    fn a_string_argument_is_its_own_spelling() {
        let c = compiled(
            r#"
(name Universe)
(sense source "{component} is implemented at {path}")
(claim "where" (source Universe "src/universe"))
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Claim {
                sentence: "where".into(),
                support: Support::Sentence(Box::new(Proposition::Sentence(
                    "Universe is implemented at src/universe".into()
                ))),
            }]
        );
    }

    #[test]
    fn a_detail_keeps_its_locals_inside_the_body() {
        let c = compiled(
            r#"
(name Concept)
(detail
  (name LogicBlock)
  (name Relationship)
  (sense input "{X} is an input into {Y}")
  (claim (input Concept LogicBlock)
    (and
      "Concept is closed over by a LogicBlock"
      (input Relationship LogicBlock))))
"#,
        );
        assert_eq!(c.names, vec!["Concept".to_string()]);
        assert!(c.senses.is_empty());
        assert_eq!(
            c.claims,
            vec![Proposition::Scope {
                body: vec![Proposition::Claim {
                    sentence: "Concept is an input into LogicBlock".into(),
                    support: Support::Sentence(Box::new(Proposition::And(vec![
                        Proposition::Sentence("Concept is closed over by a LogicBlock".into()),
                        Proposition::Sentence("Relationship is an input into LogicBlock".into()),
                    ]))),
                }],
            }]
        );

        let itself = compile(
            &parse(
                r#"
(name Concept)
(name LogicBlock)
(sense input "{X} is an input into {Y}")
(claim (input Concept LogicBlock) (input Concept LogicBlock))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            itself
                .message
                .contains("`Concept is an input into LogicBlock` is claimed in terms of itself"),
            "{itself}"
        );

        let leaked = compile(
            &parse(
                r#"
(detail
  (name Local)
  (claim "Local holds" "given"))
(claim "c" ("{Name} holds" Local))
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
(detail
  (detail
    (name Hidden)
    (claim "Hidden holds" "given"))
  (claim ("{Name} holds" Hidden) "given"))
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
(detail
  (name Concept)
  (claim "Concept holds" "given"))
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
    fn a_sentence_is_defined_once_and_not_in_terms_of_itself() {
        let twice = compile(
            &parse(
                r#"
(claim "arrow" "one")
(claim "arrow" "two")
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            twice.message.contains("duplicate definition of `arrow`"),
            "{twice}"
        );

        let cycle = compile(
            &parse(
                r#"
(claim "arrow" "leaf")
(claim "leaf" "arrow")
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            cycle
                .message
                .contains("`arrow` is claimed in terms of itself")
                || cycle
                    .message
                    .contains("`leaf` is claimed in terms of itself"),
            "{cycle}"
        );
    }

    #[test]
    fn a_witness_is_the_support_of_a_claim() {
        let c = compiled(
            r#"
(name Search)
(name Oracle)
(sense calls "{caller} calls {callee}")
(claim (calls Search Oracle) (witness "src/search/mod.rs" "src/oracle/jev.rs"))
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::Claim {
                sentence: "Search calls Oracle".into(),
                support: Support::Witness {
                    paths: vec!["src/search/mod.rs".into(), "src/oracle/jev.rs".into()],
                },
            }]
        );
    }

    #[test]
    fn a_witness_path_is_relative_to_the_brief() {
        for (src, path) in [
            ("(claim \"a\" (witness \"../notes.txt\"))", "../notes.txt"),
            (
                "(claim \"a\" (witness \"examples/sqrt2.brief\"))",
                "examples/sqrt2.brief",
            ),
        ] {
            let compiled = compiled(src);
            assert_eq!(
                compiled.claims,
                vec![Proposition::Claim {
                    sentence: "a".into(),
                    support: Support::Witness {
                        paths: vec![path.into()],
                    },
                }]
            );
        }
        for (src, needle) in [
            (
                "(claim \"a\" (witness \"/etc/passwd\"))",
                "`/etc/passwd` is not a path relative to the brief",
            ),
            (
                "(claim \"a\" (witness \"src/a.rs\" \"src/a.rs\"))",
                "witness cites `src/a.rs` twice",
            ),
            (
                "(claim \"a\" (and (witness \"src/a.rs\")))",
                "`witness` is only the support of a claim",
            ),
        ] {
            let err = compile(&parse(src).unwrap()).unwrap_err();
            assert!(err.message.contains(needle), "{src}: {err}");
        }
    }

    #[test]
    fn a_claim_is_not_supported_by_itself() {
        let err = compile(
            &parse(
                r#"
(claim "arrow" (claim "arrow" (witness "src/a.rs")))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            err.message.contains("duplicate definition of `arrow`"),
            "{err}"
        );
    }

    #[test]
    fn an_assumption_is_recorded_in_source_order() {
        let c = compiled(
            r#"
(assume "the file is small")
(detail
  (assume "a local step holds")
  (claim "the conclusion" "a local step holds"))
"#,
        );
        assert_eq!(
            c.assumptions,
            vec![
                "the file is small".to_string(),
                "a local step holds".to_string(),
            ]
        );
    }

    #[test]
    fn a_nullary_sense_is_its_reading() {
        let compiled = compile(
            &parse(
                "(sense lemma-1 \"The square root of two is irrational.\")\n(claim (lemma-1) (witness \"proof.txt\"))",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            compiled.claims,
            vec![Proposition::Claim {
                sentence: "The square root of two is irrational.".into(),
                support: Support::Witness {
                    paths: vec!["proof.txt".into()],
                },
            }]
        );
    }

    #[test]
    fn rejects_an_undeclared_name() {
        let err = compile(
            &parse(
                "(sense component \"{Name} is a part of the system\") (claim \"c\" (component Frontend))",
            )
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
        let unknown =
            compile(&parse("(name Frontend) (claim \"c\" (component Frontend))").unwrap())
                .unwrap_err();
        assert!(
            unknown.message.contains("not a declared sense"),
            "{unknown}"
        );

        let arity = compile(
            &parse(
                r#"
(name Ada)
(sense borrowed_from "{borrower} borrows from {lender}")
(claim "c" (borrowed_from Ada))
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

    #[test]
    fn a_term_keeps_its_name_and_grounds_its_text() {
        let c = compiled(
            r#"
(name LogicReasoner)
(sense LogicOutputTables "the tables named in a logic block's output_table_names")
(sense creates "{reasoner} plans deploy units that create {object}")
(claim (creates LogicReasoner (LogicOutputTables)) (witness "logic.py"))
"#,
        );
        let Proposition::Claim { sentence, .. } = &c.claims[0] else {
            panic!("claim");
        };
        assert_eq!(
            sentence.text,
            "LogicReasoner plans deploy units that create LogicOutputTables"
        );
        assert_eq!(
            sentence.grounded,
            "LogicReasoner plans deploy units that create the tables named in a logic block's output_table_names"
        );
        assert!(c.terms.contains(&(
            "LogicOutputTables".to_string(),
            "the tables named in a logic block's output_table_names".to_string(),
        )));
    }

    #[test]
    fn a_nullary_sense_is_a_term_or_a_sentence() {
        let err = compile(
            &parse(
                "\
(sense LogicOutputTables \"the tables\")
(sense creates \"{a} creates {b}\")
(name R)
(claim (creates R (LogicOutputTables)) (witness \"a.py\"))
(claim (LogicOutputTables) (witness \"b.py\"))
",
            )
            .unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\
`LogicOutputTables` is used as a term at 4:19 and as a sentence at 5:8.
A nullary sense reads either as a noun phrase or as a sentence, not both."
        );

        let across = compile(
            &parse(
                r#"
(sense T "a noun phrase")
(sense creates "{a} creates {b}")
(name R)
(detail
  (claim (creates R (T)) (witness "a.py")))
(detail
  (claim (T) (witness "b.py")))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            across.message.contains("`T` is used as a term")
                && across.message.contains("as a sentence"),
            "{across}"
        );

        let siblings = compiled(
            r#"
(detail
  (sense T "a noun phrase")
  (sense creates "{a} creates {b}")
  (name R)
  (claim (creates R (T)) (witness "a.py")))
(detail
  (sense T "A statement holds.")
  (claim (T) (witness "b.py")))
"#,
        );
        assert!(siblings
            .terms
            .contains(&("T".to_string(), "a noun phrase".to_string(),)));
        assert!(!siblings
            .terms
            .iter()
            .any(|(name, template)| name == "T" && template == "A statement holds."));
    }

    #[test]
    fn one_shown_sentence_has_one_grounding() {
        let err = compile(
            &parse(
                r#"
(name R)
(sense T "the tables")
(sense creates "{a} creates {b}")
(claim (creates R (T)) (witness "a.py"))
(claim "later" (creates R "T"))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            err.message
                .contains("`R creates T` expands to `R creates the tables` and `R creates T`"),
            "{err}"
        );
    }

    #[test]
    fn a_term_with_holes_is_an_arity_error() {
        let err = compile(
            &parse(
                r#"
(name R)
(sense holds "{a} holds {b}")
(sense borrowed_from "{borrower} borrows from {lender}")
(claim "c" (holds R (borrowed_from)))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            err.message.contains("`borrowed_from`") && err.message.contains("0 names"),
            "{err}"
        );
    }

    #[test]
    fn define_unfolds_and_enforces_acyclic_single_definition() {
        let c = compiled(
            r#"
(name Acme)
(sense customer "{c} is a customer")
(sense high_value_customer "{c} is a high-value customer")
(define (high_value_customer c)
  (and
    (customer c)
    ("{c} has lifetime sales > $25 million" c)))
(and (high_value_customer Acme))
"#,
        );
        assert_eq!(
            c.claims,
            vec![Proposition::And(vec![Proposition::Defined {
                sentence: "Acme is a high-value customer".into(),
                body: Box::new(Proposition::And(vec![
                    Proposition::Sentence("Acme is a customer".into()),
                    Proposition::Sentence("Acme has lifetime sales > $25 million".into()),
                ])),
            }])]
        );

        let cycle = compile(
            &parse(
                r#"
(sense a "{x} is a")
(sense b "{x} is b")
(define (a x) (b x))
(define (b x) (a x))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            cycle.message.contains("is defined in terms of itself"),
            "{cycle}"
        );

        let claimed_defined = compile(
            &parse(
                r#"
(name Acme)
(sense customer "{c} is a customer")
(sense hv "{c} is high value")
(define (hv c) (customer c))
(claim (hv Acme) (witness "a.csv"))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            claimed_defined
                .message
                .contains("is already defined by `define`"),
            "{claimed_defined}"
        );
    }

    #[test]
    fn ground_splits_top_level_and_but_preserves_or_and_not() {
        let c = compiled(
            r#"
(sense customer "{c} is a customer")
(sense suspended "{c} is suspended")
(sense high_value "{c} is a high-value customer")
(define (high_value c)
  (and
    (customer c)
    (not (suspended c))
    (or
      ("{c} has lifetime sales > $25 million" c)
      ("{c} has > 1000 seats" c))))
(ground (high_value c)
  (witness "out.csv")
  (witness "run.py"))
"#,
        );
        let Proposition::Cert { targets, .. } = &c.claims[0] else {
            panic!("expected Cert");
        };
        let texts: Vec<&str> = targets.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "For all c, if out.csv supports that c is a high-value customer, then c is a customer.",
                "For all c, if out.csv supports that c is a high-value customer, then it is not the case that c is suspended.",
                "For all c, if out.csv supports that c is a high-value customer, then c has lifetime sales > $25 million or c has > 1000 seats.",
            ]
        );

        let binary = compiled(
            r#"
(sense owns "{c} owns {a}")
(define (owns c a) ("{c} is the billing owner of {a}" c a))
(ground (owns customer account)
  (witness "owners.csv")
  (witness "export.py"))
"#,
        );
        let Proposition::Cert { targets, .. } = &binary.claims[0] else {
            panic!("expected Cert");
        };
        assert_eq!(
            targets[0].text,
            "For all customer and account, if owners.csv supports that customer owns account, then customer is the billing owner of account."
        );

        let no_define = compile(
            &parse(
                r#"
(sense hv "{c} is high value")
(ground (hv c) (witness "out.csv") (witness "run.py"))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            no_define
                .message
                .contains("cannot be grounded without a `define`"),
            "{no_define}"
        );
    }

    #[test]
    fn bodyless_define_desugars_to_empty_and_and_allows_multiple_facts_per_sense() {
        let c = compiled(
            r#"
(name CorporateAction)
(name Merger)
(name Dividend)
(sense is_a "{X} is a sub-type of {Y}")
(define (is_a Merger CorporateAction))
(define (is_a Dividend CorporateAction))
(and (is_a Merger CorporateAction) (is_a Dividend CorporateAction))
"#,
        );
        assert!(c.assumptions.is_empty());
        assert_eq!(
            c.claims,
            vec![Proposition::And(vec![
                Proposition::Defined {
                    sentence: "Merger is a sub-type of CorporateAction".into(),
                    body: Box::new(Proposition::And(Vec::new())),
                },
                Proposition::Defined {
                    sentence: "Dividend is a sub-type of CorporateAction".into(),
                    body: Box::new(Proposition::And(Vec::new())),
                },
            ])]
        );

        let dup = compile(
            &parse(
                r#"
(name CorporateAction)
(name Merger)
(sense is_a "{X} is a sub-type of {Y}")
(define (is_a Merger CorporateAction))
(define (is_a Merger CorporateAction))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(dup.message.contains("duplicate definition"), "{dup}");
    }

    #[test]
    fn exists_renders_in_ground_targets_instantiates_to_sentence_and_rejects_shadowing() {
        let c = compiled(
            r#"
(name Tx1)
(sense stacked_cancels "{Transaction} has {Count} cancels stacked on it")
(sense is_odd "{Number} is odd")
(sense canceled "{Transaction} is canceled")
(sense converts_from "{CorporateAction} converts from {Asset}")
(sense ca_canceled_by "{CorporateAction} is canceled by {Cancel}")
(sense acts_on "{CorporateAction} acts on {Asset}")

(define (canceled t)
  (exists [n]
    (and (stacked_cancels t n) (is_odd n))))

(define (acts_on ca a)
  (and
    (converts_from ca a)
    (not (exists [cancel] (ca_canceled_by ca cancel)))))

(ground (canceled t)
  (witness "erased.csv")
  (witness "cancel_chains.py"))

(ground (acts_on ca a)
  (witness "acts.csv")
  (witness "corp_actions.py"))

(claim (exists [n] (and (stacked_cancels Tx1 n) (is_odd n)))
  (witness "log.txt"))
"#,
        );
        let Proposition::Cert { targets, .. } = &c.claims[0] else {
            panic!("expected Cert for canceled");
        };
        assert_eq!(
            targets.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec![
                "For all t, if erased.csv supports that t is canceled, then there exists n such that (t has n cancels stacked on it and n is odd)."
            ]
        );

        let Proposition::Cert { targets, .. } = &c.claims[1] else {
            panic!("expected Cert for acts_on");
        };
        assert_eq!(
            targets.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec![
                "For all ca and a, if acts.csv supports that ca acts on a, then ca converts from a.",
                "For all ca and a, if acts.csv supports that ca acts on a, then it is not the case that there exists cancel such that ca is canceled by cancel.",
            ]
        );

        let ungrounded = compiled(
            r#"
(name Tx1)
(sense stacked_cancels "{Transaction} has {Count} cancels stacked on it")
(sense is_odd "{Number} is odd")
(sense canceled "{Transaction} is canceled")
(define (canceled t)
  (exists [n]
    (and (stacked_cancels t n) (is_odd n))))
(claim (exists [n] (and (stacked_cancels Tx1 n) (is_odd n)))
  (witness "log.txt"))
(and (canceled Tx1))
"#,
        );
        let expected_exists =
            "there exists n such that (Tx1 has n cancels stacked on it and n is odd)";
        assert_eq!(
            ungrounded.claims[0],
            Proposition::Claim {
                sentence: expected_exists.into(),
                support: Support::Witness {
                    paths: vec!["log.txt".into()],
                },
            }
        );
        assert_eq!(
            ungrounded.claims[1],
            Proposition::And(vec![Proposition::Defined {
                sentence: "Tx1 is canceled".into(),
                body: Box::new(Proposition::Sentence(expected_exists.into())),
            }])
        );

        let shadowed = compile(
            &parse(
                r#"
(sense p "{x} holds")
(sense q "{x} implies")
(define (q x) (exists [x] (p x)))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(shadowed.message.contains("already bound"), "{shadowed}");
    }

    #[test]
    fn forall_implies_and_closed_constraints() {
        let c = compiled(
            r#"
(name A)
(sense affects "{ca} affects {t}")
(sense raw_affects "{ca} directly affects {t}")
(sense yields "{ca} yields {ef}")
(sense cancels "{ef} cancels {t}")
(define (affects ca t) (raw_affects ca t))
(define yields_exchange_from
  (forall [ca t]
    (implies (affects ca t)
      (exists [ef] (and (yields ca ef) (cancels ef t))))))
(define (forall [x] (implies ("{x} is positive" x) ("{x} is non-negative" x))))
(certify (affects ca t) (witness "affects.py"))
(certify yields_exchange_from (witness "exchange.py"))
(certify (forall [x] (implies ("{x} is positive" x) ("{x} is non-negative" x)))
  (witness "math.py"))
(implies ("{x} is positive" A) ("{x} is non-negative" A))
"#,
        );

        let Proposition::Cert {
            label,
            targets,
            requires,
            ..
        } = &c.claims[1]
        else {
            panic!("expected Cert for yields_exchange_from");
        };
        assert_eq!(label, "certify yields_exchange_from");
        assert_eq!(
            targets.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec![
                "for all ca and t, if ca affects t, then there exists ef such that (ca yields ef and ef cancels t)."
            ]
        );
        assert_eq!(requires, &vec!["certify:affects".to_string()]);

        assert_eq!(
            c.claims[2],
            Proposition::Claim {
                sentence: "for all x, if x is positive, then x is non-negative".into(),
                support: Support::Witness {
                    paths: vec!["math.py".into()],
                },
            }
        );

        assert_eq!(
            c.claims[3],
            Proposition::Or(vec![
                Proposition::Not(Box::new(Proposition::Sentence("A is positive".into()))),
                Proposition::Sentence("A is non-negative".into()),
            ])
        );

        let shadowed = compile(
            &parse(
                r#"
(sense p "{x} holds")
(define bad (forall [x] (forall [x] (p x))))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(shadowed.message.contains("already bound"), "{shadowed}");
    }

    #[test]
    fn spec_define_merges_witness_synthesizes_refinement_and_rejects_ungrounded_use() {
        let c = compiled(
            r#"
(sense pagerank "{v} has PageRank score {r} in {g}")
(define (pagerank v r g) (spec "specs/pagerank.lean"))
(define pr_props (spec "specs/pagerank.lean"))
(certify (pagerank v r g) (witness "pagerank.py"))
(certify pr_props (witness "pagerank.py"))
"#,
        );
        let Proposition::Cert {
            targets,
            support: Support::Witness { paths },
            ..
        } = &c.claims[0]
        else {
            panic!("expected Cert for pagerank");
        };
        assert_eq!(
            targets[0].text,
            "For all v, r, and g, v has PageRank score r in g if and only if v, r, and g satisfy the specification in specs/pagerank.lean."
        );
        assert_eq!(
            paths,
            &vec!["specs/pagerank.lean".to_string(), "pagerank.py".to_string()]
        );

        let Proposition::Cert {
            targets: prop_targets,
            support: Support::Witness { paths: prop_paths },
            ..
        } = &c.claims[1]
        else {
            panic!("expected Cert for pr_props");
        };
        assert_eq!(
            prop_targets[0].text,
            "The implementation satisfies the specification in specs/pagerank.lean."
        );
        assert_eq!(
            prop_paths,
            &vec!["specs/pagerank.lean".to_string(), "pagerank.py".to_string()]
        );

        let nested = compile(
            &parse(
                r#"
(sense p "{x} holds")
(define (p x) (and ("{x} exists" x) (spec "specs/p.lean")))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            nested.message.contains("must be the entire body of `define`"),
            "{nested}"
        );

        let ungrounded = compile(
            &parse(
                r#"
(name G)
(name V)
(name R)
(sense pagerank "{v} has PageRank score {r} in {g}")
(define (pagerank g v r) (spec "specs/pagerank.lean"))
(and (pagerank G V R))
"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(
            ungrounded
                .message
                .contains("must be certified or grounded"),
            "{ungrounded}"
        );
    }

    #[test]
    fn param_substitution_and_named_dollar_witnesses() {
        let program = parse(
            r#"
(param customer)
(param run_date)
(sense active "{c} is active")
(define (active c) ("{c} has an order" c))
(assume ("snapshot taken on {d}" run_date))
(ground (active c)
  (witness $run/out.csv)
  (and
    ("snapshot taken on {d}" run_date)
    (claim "query writes out.csv" (witness "query.sql"))))
(claim ("{c} qualifies on {d}" customer run_date)
  (and (active customer) ("snapshot taken on {d}" run_date)))
"#,
        )
        .unwrap();

        // Bare compile leaves params symbolic (for align / static inspection).
        let symbolic = compile(&program).unwrap();
        assert_eq!(symbolic.assumptions, vec!["snapshot taken on run_date"]);

        // compile_with_bindings errors on missing params and substitutes when bound.
        let missing = compile_with_bindings(
            &program,
            &HashMap::from([("customer".into(), "Acme".into())]),
        )
        .unwrap_err();
        assert!(
            missing.message.contains("missing binding for parameter `run_date`"),
            "{missing}"
        );

        let bound = compile_with_bindings(
            &program,
            &HashMap::from([
                ("customer".into(), "Acme".into()),
                ("run_date".into(), "2026-11-26".into()),
            ]),
        )
        .unwrap();
        assert_eq!(bound.assumptions, vec!["snapshot taken on 2026-11-26"]);
        let Proposition::Cert { label, targets, .. } = &bound.claims[1] else {
            panic!("expected ground Cert");
        };
        assert_eq!(label, "ground (active c) via out.csv");
        assert!(targets[0].text.contains("if out.csv supports"), "{targets:?}");
        let Proposition::Claim { sentence, .. } = &bound.claims[2] else {
            panic!("expected Claim");
        };
        assert_eq!(sentence.text, "Acme qualifies on 2026-11-26");

        // `param` inside a `detail` is rejected.
        let in_detail = compile(&parse("(detail (param x) (assume \"x\"))\n").unwrap())
            .unwrap_err();
        assert!(
            in_detail.message.contains("`param` cannot appear inside a `detail`"),
            "{in_detail}"
        );
    }
}
