use std::collections::{HashMap, HashSet};

use crate::syntax::template::is_builtin_sort;
use crate::syntax::{Declare, Form, Formula, Program, Term};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    pub vars: Vec<String>,
    pub body: Formula,
}

/// A program with EDB/IDB classified and stratification checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub defs: HashMap<String, Def>,
    pub requires: Vec<Formula>,
    pub edb_preds: HashSet<String>,
    pub idb_preds: HashSet<String>,
    pub declarations: HashMap<String, Declare>,
}

/// Classify predicates and reject unstratified IDB.
pub fn compile(program: &Program) -> Result<Compiled, CompileError> {
    let mut defs = HashMap::new();
    let mut requires = Vec::new();
    let mut declarations: HashMap<String, Declare> = HashMap::new();
    for form in &program.forms {
        match form {
            Form::Def { name, vars, body } => {
                if defs.contains_key(name) {
                    return Err(CompileError {
                        message: format!("duplicate definition of `{name}`"),
                    });
                }
                defs.insert(
                    name.clone(),
                    Def {
                        name: name.clone(),
                        vars: vars.clone(),
                        body: body.clone(),
                    },
                );
            }
            Form::Require(f) => requires.push(f.clone()),
            Form::Declare(d) => {
                if declarations.contains_key(&d.name) {
                    return Err(CompileError {
                        message: format!("duplicate declaration of `{}`", d.name),
                    });
                }
                declarations.insert(d.name.clone(), d.clone());
            }
        }
    }

    for d in declarations.values() {
        for slot in &d.slots {
            if is_builtin_sort(slot) {
                continue;
            }
            match declarations.get(slot) {
                Some(ty) if ty.slots.len() == 1 => {}
                Some(ty) => {
                    return Err(CompileError {
                        message: format!(
                            "`{}` uses `{{{slot}}}` but `{slot}` has arity {}, not 1",
                            d.name,
                            ty.slots.len()
                        ),
                    });
                }
                None => {
                    return Err(CompileError {
                        message: format!(
                            "`{}` uses `{{{slot}}}` but `{slot}` is not declared (and is not String)",
                            d.name
                        ),
                    });
                }
            }
        }
        if let Some(def) = defs.get(&d.name) {
            if def.vars.len() != d.slots.len() {
                return Err(CompileError {
                    message: format!(
                        "`{}` is declared with arity {} but defined with {}",
                        d.name,
                        d.slots.len(),
                        def.vars.len()
                    ),
                });
            }
        }
    }

    let mut used = HashSet::new();
    for def in defs.values() {
        collect_preds(&def.body, &mut used);
    }
    for req in &requires {
        collect_preds(req, &mut used);
    }

    let idb_preds: HashSet<String> = defs.keys().cloned().collect();
    let edb_preds: HashSet<String> = used.difference(&idb_preds).cloned().collect();

    check_stratification(&defs, &idb_preds)?;
    check_declared_arities(&declarations, &defs, &requires)?;

    Ok(Compiled {
        defs,
        requires,
        edb_preds,
        idb_preds,
        declarations,
    })
}

fn check_declared_arities(
    declarations: &HashMap<String, Declare>,
    defs: &HashMap<String, Def>,
    requires: &[Formula],
) -> Result<(), CompileError> {
    let mut atoms = Vec::new();
    for def in defs.values() {
        collect_atoms(&def.body, &mut atoms);
    }
    for req in requires {
        collect_atoms(req, &mut atoms);
    }
    for (pred, args) in atoms {
        if let Some(d) = declarations.get(&pred) {
            if args.len() != d.slots.len() {
                return Err(CompileError {
                    message: format!(
                        "`{pred}` is declared with arity {} but used with {}",
                        d.slots.len(),
                        args.len()
                    ),
                });
            }
        }
    }
    Ok(())
}

fn collect_atoms(f: &Formula, out: &mut Vec<(String, Vec<Term>)>) {
    f.walk_atoms(&mut |pred, args| {
        out.push((pred.to_string(), args.to_vec()));
    });
}

fn collect_preds(f: &Formula, out: &mut HashSet<String>) {
    f.walk_atoms(&mut |pred, _| {
        out.insert(pred.to_string());
    });
}

fn check_stratification(
    defs: &HashMap<String, Def>,
    idb: &HashSet<String>,
) -> Result<(), CompileError> {
    let mut pos: HashMap<String, HashSet<String>> = HashMap::new();
    let mut neg: HashMap<String, HashSet<String>> = HashMap::new();
    for def in defs.values() {
        walk_deps(&def.body, 1, &def.name, idb, &mut pos, &mut neg);
    }

    let mut all: HashMap<String, HashSet<String>> = HashMap::new();
    for (from, tos) in pos.iter().chain(neg.iter()) {
        all.entry(from.clone())
            .or_default()
            .extend(tos.iter().cloned());
    }

    for (from, tos) in &neg {
        for to in tos {
            if reaches(&all, to, from) {
                return Err(CompileError {
                    message: format!(
                        "unstratified negation: `{from}` depends negatively on `{to}` in a cycle"
                    ),
                });
            }
        }
    }
    Ok(())
}

fn walk_deps(
    f: &Formula,
    polarity: i8,
    from: &str,
    idb: &HashSet<String>,
    pos: &mut HashMap<String, HashSet<String>>,
    neg: &mut HashMap<String, HashSet<String>>,
) {
    match f {
        Formula::And(xs) | Formula::Or(xs) => {
            for x in xs {
                walk_deps(x, polarity, from, idb, pos, neg);
            }
        }
        Formula::Not(x) | Formula::Exists(_, x) | Formula::Forall(_, x) => {
            let p = if matches!(f, Formula::Not(_)) {
                -polarity
            } else {
                polarity
            };
            walk_deps(x, p, from, idb, pos, neg);
        }
        Formula::Implies(a, b) => {
            walk_deps(a, -polarity, from, idb, pos, neg);
            walk_deps(b, polarity, from, idb, pos, neg);
        }
        Formula::Atom { pred, args: _ } => {
            if idb.contains(pred) {
                if polarity < 0 {
                    neg.entry(from.to_string())
                        .or_default()
                        .insert(pred.clone());
                } else {
                    pos.entry(from.to_string())
                        .or_default()
                        .insert(pred.clone());
                }
            }
        }
    }
}

fn reaches(graph: &HashMap<String, HashSet<String>>, start: &str, goal: &str) -> bool {
    if start == goal {
        return true;
    }
    let mut seen = HashSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(n) = stack.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        if n == goal {
            return true;
        }
        if let Some(next) = graph.get(&n) {
            stack.extend(next.iter().cloned());
        }
    }
    false
}

/// String constants mentioned in the compiled program.
pub fn program_constants(compiled: &Compiled) -> Vec<String> {
    let mut cs = Vec::new();
    for def in compiled.defs.values() {
        def.body.constants(&mut cs);
    }
    for req in &compiled.requires {
        req.constants(&mut cs);
    }
    cs.sort();
    cs.dedup();
    cs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    #[test]
    fn classifies_readme() {
        let src = std::fs::read_to_string("examples/layered.ag").unwrap();
        let p = parse(&src).unwrap();
        let c = compile(&p).unwrap();
        assert!(c.idb_preds.contains("depends_on"));
        assert!(c.edb_preds.contains("component"));
        assert!(c.edb_preds.contains("file"));
        assert!(c.edb_preds.contains("includes"));
        assert!(c.edb_preds.contains("belongs_to"));
        assert_eq!(c.requires.len(), 4);
        assert!(c.declarations.contains_key("component"));
        assert_eq!(
            c.declarations["belongs_to"].slots,
            vec!["file", "component"]
        );
    }

    #[test]
    fn rejects_unknown_slot_type() {
        let src = r#"(declare defines "{Widget} is related")"#;
        let p = parse(src).unwrap();
        let err = compile(&p).unwrap_err();
        assert!(err.message.contains("Widget"));
    }

    #[test]
    fn rejects_arity_mismatch() {
        let src = r#"
(declare component "{String} is a component in the codebase")
(require (component "Frontend" "extra"))
"#;
        let p = parse(src).unwrap();
        let err = compile(&p).unwrap_err();
        assert!(err.message.contains("arity"));
    }

    #[test]
    fn rejects_unstratified() {
        let src = r#"
(def p [x] (not (q x)))
(def q [x] (not (p x)))
(require (p "a"))
"#;
        let p = parse(src).unwrap();
        let err = compile(&p).unwrap_err();
        assert!(err.message.contains("unstratified"));
    }
}
