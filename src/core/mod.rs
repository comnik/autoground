use std::collections::{HashMap, HashSet};

use crate::syntax::{Form, Formula, Program};

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
}

/// Classify predicates and reject unstratified IDB.
pub fn compile(program: &Program) -> Result<Compiled, CompileError> {
    let mut defs = HashMap::new();
    let mut requires = Vec::new();
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

    Ok(Compiled {
        defs,
        requires,
        edb_preds,
        idb_preds,
    })
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
        assert!(c.edb_preds.contains("defines"));
        assert_eq!(c.requires.len(), 4);
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
