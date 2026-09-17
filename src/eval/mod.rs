use std::collections::{HashMap, HashSet};

use crate::core::{program_constants, Compiled};
use crate::edb::Store;
use crate::navigator::{Arg, Demand, Navigator, Need};
use crate::syntax::{Formula, Term};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    True,
    False,
    Unknown,
}

impl Value {
    pub fn and(self, other: Value) -> Value {
        match (self, other) {
            (Value::False, _) | (_, Value::False) => Value::False,
            (Value::Unknown, _) | (_, Value::Unknown) => Value::Unknown,
            (Value::True, Value::True) => Value::True,
        }
    }

    pub fn or(self, other: Value) -> Value {
        match (self, other) {
            (Value::True, _) | (_, Value::True) => Value::True,
            (Value::Unknown, _) | (_, Value::Unknown) => Value::Unknown,
            (Value::False, Value::False) => Value::False,
        }
    }

    pub fn not(self) -> Value {
        match self {
            Value::True => Value::False,
            Value::False => Value::True,
            Value::Unknown => Value::Unknown,
        }
    }

    pub fn implies(self, other: Value) -> Value {
        self.not().or(other)
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::True => write!(f, "true"),
            Value::False => write!(f, "false"),
            Value::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness {
    pub assignment: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequireResult {
    pub value: Value,
    pub witness: Option<Witness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Grounded,
    Violated,
    Ungrounded,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub requires: Vec<RequireResult>,
    pub outcome: Outcome,
    pub demands: Vec<Demand>,
}

struct Engine<'a> {
    compiled: &'a Compiled,
    store: &'a mut Store,
    navigator: Option<&'a mut dyn Navigator>,
    demands: Vec<Demand>,
    memo: HashMap<(String, Vec<String>), Value>,
    stack: HashSet<(String, Vec<String>)>,
    last_exists_witness: Option<Witness>,
    last_forall_counterexample: Option<Witness>,
}

/// Evaluate every `require` against the store, asking the navigator for missing EDB.
pub fn run<'a>(
    compiled: &'a Compiled,
    store: &'a mut Store,
    navigator: Option<&'a mut dyn Navigator>,
) -> RunResult {
    let mut eng = Engine {
        compiled,
        store,
        navigator,
        demands: Vec::new(),
        memo: HashMap::new(),
        stack: HashSet::new(),
        last_exists_witness: None,
        last_forall_counterexample: None,
    };
    let mut requires = Vec::new();
    for req in &compiled.requires {
        eng.last_exists_witness = None;
        eng.last_forall_counterexample = None;
        let value = eng.eval(req, &HashMap::new());
        let witness = match value {
            Value::True => eng.last_exists_witness.clone(),
            Value::False => eng.last_forall_counterexample.clone(),
            Value::Unknown => None,
        };
        requires.push(RequireResult { value, witness });
    }
    let demands = eng.demands;

    let outcome = if requires.iter().any(|r| r.value == Value::False) {
        Outcome::Violated
    } else if requires.iter().any(|r| r.value == Value::Unknown) {
        Outcome::Ungrounded
    } else {
        Outcome::Grounded
    };

    RunResult {
        requires,
        outcome,
        demands,
    }
}

impl Engine<'_> {
    fn eval(&mut self, f: &Formula, env: &HashMap<String, String>) -> Value {
        match f {
            Formula::And(xs) => {
                let mut acc = Value::True;
                for x in xs {
                    acc = acc.and(self.eval(x, env));
                    if acc == Value::False {
                        return Value::False;
                    }
                }
                acc
            }
            Formula::Or(xs) => {
                let mut acc = Value::False;
                for x in xs {
                    acc = acc.or(self.eval(x, env));
                    if acc == Value::True {
                        return Value::True;
                    }
                }
                acc
            }
            Formula::Not(x) => self.eval(x, env).not(),
            Formula::Implies(a, b) => self.eval(a, env).implies(self.eval(b, env)),
            Formula::Exists(vars, body) => self.eval_exists(vars, body, env),
            Formula::Forall(vars, body) => self.eval_forall(vars, body, env),
            Formula::Atom { pred, args } => self.eval_atom(pred, args, env),
        }
    }

    fn eval_exists(
        &mut self,
        vars: &[String],
        body: &Formula,
        env: &HashMap<String, String>,
    ) -> Value {
        self.demand_generators(vars, body, env);
        let domain = self.generated_domain(vars, body, env);
        let closed = self.generators_closed(vars, body, env);
        if domain.is_empty() {
            return if closed { Value::False } else { Value::Unknown };
        }

        let assignments = cartesian_rows(&domain, vars.len());
        let mut saw_unknown = false;
        for asg in assignments {
            let mut env2 = env.clone();
            let mut pairs = Vec::new();
            for (v, val) in vars.iter().zip(asg.iter()) {
                env2.insert(v.clone(), val.clone());
                pairs.push((v.clone(), val.clone()));
            }
            match self.eval(body, &env2) {
                Value::True => {
                    self.last_exists_witness = Some(Witness { assignment: pairs });
                    return Value::True;
                }
                Value::False => {}
                Value::Unknown => saw_unknown = true,
            }
        }
        if !closed || saw_unknown {
            Value::Unknown
        } else {
            Value::False
        }
    }

    fn eval_forall(
        &mut self,
        vars: &[String],
        body: &Formula,
        env: &HashMap<String, String>,
    ) -> Value {
        let domain = self.universe();
        if domain.is_empty() {
            return Value::True;
        }

        let assignments = cartesian(&domain, vars.len());
        let mut saw_unknown = false;
        for asg in assignments {
            let mut env2 = env.clone();
            let mut pairs = Vec::new();
            for (v, val) in vars.iter().zip(asg.iter()) {
                env2.insert(v.clone(), val.clone());
                pairs.push((v.clone(), val.clone()));
            }
            match self.eval(body, &env2) {
                Value::False => {
                    self.last_forall_counterexample = Some(Witness { assignment: pairs });
                    return Value::False;
                }
                Value::True => {}
                Value::Unknown => saw_unknown = true,
            }
        }
        if saw_unknown {
            Value::Unknown
        } else {
            Value::True
        }
    }

    fn demand_generators(
        &mut self,
        vars: &[String],
        body: &Formula,
        env: &HashMap<String, String>,
    ) {
        let mut atoms = Vec::new();
        collect_edb_atoms(
            body,
            &self.compiled.idb_preds,
            &self.compiled.defs,
            &mut atoms,
        );
        for (pred, args) in atoms {
            let mut demand_args = Vec::new();
            let mut any_free = false;
            let mut any_unresolved = false;
            for t in &args {
                match t {
                    Term::Const(c) => demand_args.push(Arg::Bound(c.clone())),
                    Term::Var(v) if env.contains_key(v) => {
                        demand_args.push(Arg::Bound(env[v].clone()));
                    }
                    Term::Var(v) if vars.iter().any(|x| x == v) => {
                        demand_args.push(Arg::Free(v.clone()));
                        any_free = true;
                    }
                    Term::Var(_) => {
                        any_unresolved = true;
                    }
                }
            }
            if any_unresolved || !any_free {
                continue;
            }
            // file/1 and defines/2 are tests of a candidate, not generators.
            // Generating from them would walk the whole repo (including decoy/).
            if pred == "file" || pred == "defines" {
                continue;
            }
            if demand_args.iter().all(|a| matches!(a, Arg::Free(_))) {
                continue;
            }
            self.demand_and_fill(&pred, &demand_args, Need::Truth);
        }
    }

    fn generators_closed(
        &self,
        vars: &[String],
        body: &Formula,
        env: &HashMap<String, String>,
    ) -> bool {
        let mut atoms = Vec::new();
        collect_edb_atoms(
            body,
            &self.compiled.idb_preds,
            &self.compiled.defs,
            &mut atoms,
        );
        let atoms: Vec<_> = atoms
            .into_iter()
            .filter(|(pred, _)| pred != "file" && pred != "defines")
            .collect();
        if atoms.is_empty() {
            return self.universe_closed(body);
        }
        atoms.iter().all(|(pred, args)| {
            let pattern: Vec<Option<String>> = args
                .iter()
                .map(|t| match t {
                    Term::Const(c) => Some(c.clone()),
                    Term::Var(v) if env.contains_key(v) => env.get(v).cloned(),
                    Term::Var(v) if vars.iter().any(|x| x == v) => None,
                    Term::Var(_) => None,
                })
                .collect();
            self.store.pattern_closed(pred, &pattern)
        })
    }

    /// Values for exists-vars taken from generator tuples (e.g. includes), not the
    /// whole universe of model constants.
    fn generated_domain(
        &self,
        vars: &[String],
        body: &Formula,
        env: &HashMap<String, String>,
    ) -> Vec<String> {
        let mut atoms = Vec::new();
        collect_edb_atoms(
            body,
            &self.compiled.idb_preds,
            &self.compiled.defs,
            &mut atoms,
        );
        let mut values = HashSet::new();
        for (pred, args) in atoms {
            if pred == "file" || pred == "defines" {
                continue;
            }
            for row in self.store.facts_for(&pred) {
                if row.len() != args.len() {
                    continue;
                }
                let mut ok = true;
                let mut extracted = Vec::new();
                for (cell, t) in row.iter().zip(args.iter()) {
                    match t {
                        Term::Const(c) if c != cell => ok = false,
                        Term::Var(v) if env.get(v).is_some_and(|e| e != cell) => ok = false,
                        Term::Var(v) if vars.iter().any(|x| x == v) => {
                            extracted.push(cell.clone());
                        }
                        _ => {}
                    }
                }
                if ok {
                    values.extend(extracted);
                }
            }
        }
        let mut v: Vec<_> = values.into_iter().collect();
        v.sort();
        v
    }

    fn eval_atom(&mut self, pred: &str, args: &[Term], env: &HashMap<String, String>) -> Value {
        let resolved: Vec<Option<String>> = args
            .iter()
            .map(|t| match t {
                Term::Const(c) => Some(c.clone()),
                Term::Var(v) => env.get(v).cloned(),
            })
            .collect();

        if let Some(def) = self.compiled.defs.get(pred) {
            if resolved.iter().any(|a| a.is_none()) {
                return Value::Unknown;
            }
            let ground: Vec<String> = resolved.into_iter().map(|a| a.unwrap()).collect();
            let key = (pred.to_string(), ground.clone());
            if let Some(v) = self.memo.get(&key) {
                return *v;
            }
            if !self.stack.insert(key.clone()) {
                return Value::Unknown;
            }
            let mut env2 = HashMap::new();
            for (var, val) in def.vars.iter().zip(ground.iter()) {
                env2.insert(var.clone(), val.clone());
            }
            let v = self.eval(&def.body, &env2);
            self.stack.remove(&key);
            if v != Value::Unknown {
                self.memo.insert(key, v);
            }
            return v;
        }

        // EDB
        if resolved.iter().all(|a| a.is_some()) {
            let ground: Vec<String> = resolved.into_iter().map(|a| a.unwrap()).collect();
            if self.store.has_fact(pred, &ground) {
                return Value::True;
            }
            self.demand_and_fill(
                pred,
                &ground
                    .iter()
                    .map(|g| Arg::Bound(g.clone()))
                    .collect::<Vec<_>>(),
                Need::Truth,
            );
            if self.store.has_fact(pred, &ground) {
                return Value::True;
            }
            if self.store.is_closed_for(pred, &ground) {
                return Value::False;
            }
            return Value::Unknown;
        }

        // Free arguments: generation is handled by the enclosing exists,
        // which iterates the universe. If we get here, a var was unbound.
        Value::Unknown
    }

    fn demand_and_fill(&mut self, pred: &str, args: &[Arg], need: Need) {
        let demand = Demand {
            pred: pred.to_string(),
            args: args.to_vec(),
            need,
        };
        self.demands.push(demand.clone());
        let Some(nav) = self.navigator.as_mut() else {
            return;
        };
        let answer = nav.answer(&demand, self.store);
        for row in answer.tuples {
            self.store.insert_fact(pred, row, answer.source.clone());
        }
        for (c, e) in answer.bindings {
            self.store.bind(c, e);
        }
        if answer.close {
            let pattern: Vec<Option<String>> = demand
                .args
                .iter()
                .map(|a| match a {
                    Arg::Bound(v) => Some(v.clone()),
                    Arg::Free(_) => None,
                })
                .collect();
            self.store.close_pattern(pred, pattern);
        }
    }

    fn universe(&self) -> Vec<String> {
        // v0: model constants, plus labels that acquired a binding. File paths
        // and directory entities are not in the forall domain.
        let mut s: HashSet<String> = program_constants(self.compiled).into_iter().collect();
        for b in self.store.all_bindings() {
            s.insert(b.constant);
        }
        let mut v: Vec<_> = s.into_iter().collect();
        v.sort();
        v
    }

    fn universe_closed(&self, body: &Formula) -> bool {
        let mut preds = HashSet::new();
        collect_edb_preds(body, &self.compiled.idb_preds, &mut preds);
        // Unfold one level of IDB so closedness of depends_on follows its EDB.
        for pred in self.compiled.idb_preds.iter() {
            if formula_uses(body, pred) {
                if let Some(def) = self.compiled.defs.get(pred) {
                    collect_edb_preds(&def.body, &self.compiled.idb_preds, &mut preds);
                }
            }
        }
        if preds.is_empty() {
            return true;
        }
        self.store.all_closed(&preds)
    }
}

fn formula_uses(f: &Formula, pred: &str) -> bool {
    let mut yes = false;
    f.walk_atoms(&mut |p, _| {
        if p == pred {
            yes = true;
        }
    });
    yes
}

fn collect_edb_preds(f: &Formula, idb: &HashSet<String>, out: &mut HashSet<String>) {
    f.walk_atoms(&mut |p, _| {
        if !idb.contains(p) {
            out.insert(p.to_string());
        }
    });
}

fn collect_edb_atoms(
    f: &Formula,
    idb: &HashSet<String>,
    defs: &HashMap<String, crate::core::Def>,
    out: &mut Vec<(String, Vec<Term>)>,
) {
    match f {
        Formula::And(xs) | Formula::Or(xs) => {
            for x in xs {
                collect_edb_atoms(x, idb, defs, out);
            }
        }
        Formula::Not(x) | Formula::Exists(_, x) | Formula::Forall(_, x) => {
            collect_edb_atoms(x, idb, defs, out);
        }
        Formula::Implies(a, b) => {
            collect_edb_atoms(a, idb, defs, out);
            collect_edb_atoms(b, idb, defs, out);
        }
        Formula::Atom { pred, args } => {
            if idb.contains(pred) {
                if let Some(def) = defs.get(pred) {
                    collect_edb_atoms(&def.body, idb, defs, out);
                }
            } else {
                out.push((pred.clone(), args.clone()));
            }
        }
    }
}

fn cartesian(domain: &[String], n: usize) -> Vec<Vec<String>> {
    cartesian_rows(domain, n)
}

fn cartesian_rows(domain: &[String], n: usize) -> Vec<Vec<String>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut acc = vec![Vec::new()];
    for _ in 0..n {
        let mut next = Vec::new();
        for prefix in &acc {
            for d in domain {
                let mut p = prefix.clone();
                p.push(d.clone());
                next.push(p);
            }
        }
        acc = next;
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compile;
    use crate::navigator::ScriptedNavigator;
    use crate::syntax::parse;

    fn layered() -> Compiled {
        let src = std::fs::read_to_string("examples/layered.ag").unwrap();
        let p = parse(&src).unwrap();
        compile(&p).unwrap()
    }

    fn seed_layered(store: &mut Store, cycle: bool) {
        store.insert_fact("component", vec!["Frontend".into()], "test");
        store.insert_fact("component", vec!["Backend".into()], "test");
        store.insert_fact("file", vec!["frontend/app.py".into()], "test");
        store.insert_fact("file", vec!["backend/api.py".into()], "test");
        store.insert_fact(
            "includes",
            vec!["Frontend".into(), "frontend/app.py".into()],
            "test",
        );
        store.insert_fact(
            "includes",
            vec!["Backend".into(), "backend/api.py".into()],
            "test",
        );
        store.insert_fact(
            "defines",
            vec!["frontend/app.py".into(), "Backend".into()],
            "test",
        );
        if cycle {
            store.insert_fact(
                "defines",
                vec!["backend/api.py".into(), "Frontend".into()],
                "test",
            );
        }
        for p in ["component", "file", "includes", "defines"] {
            store.close_pred(p);
        }
    }

    #[test]
    fn kleene_and() {
        assert_eq!(Value::True.and(Value::True), Value::True);
        assert_eq!(Value::True.and(Value::Unknown), Value::Unknown);
        assert_eq!(Value::False.and(Value::Unknown), Value::False);
    }

    #[test]
    fn empty_open_edb_acyclicity_is_unknown() {
        let c = layered();
        let mut store = Store::new();
        let result = run(&c, &mut store, None);
        assert_eq!(result.outcome, Outcome::Ungrounded);
        assert!(result.requires.iter().all(|r| r.value == Value::Unknown));
    }

    #[test]
    fn closed_empty_depends_on_acyclicity_is_true() {
        let c = layered();
        let mut store = Store::new();
        for p in ["component", "file", "includes", "defines"] {
            store.close_pred(p);
        }
        let result = run(&c, &mut store, None);
        // No components exist: first require is false (closed empty component).
        assert_eq!(result.requires[0].value, Value::False);
        // depends_on is false everywhere, so the layering require (depends_on F B)
        // is false, and not(depends_on B F) is true, and forall is true.
        assert_eq!(result.requires[1].value, Value::False);
        assert_eq!(result.requires[2].value, Value::True);
        assert_eq!(result.requires[3].value, Value::True);
        assert_eq!(result.outcome, Outcome::Violated);
    }

    #[test]
    fn closed_empty_acyclicity_formula_alone() {
        // Isolate the forall: closed empty EDB ⇒ vacuously true.
        let src = r#"
(def depends_on [x y]
  (and (component x) (component y)))
(require
  (forall [x y]
    (implies
      (depends_on x y)
      (not (depends_on y x)))))
"#;
        let p = parse(src).unwrap();
        let c = compile(&p).unwrap();
        let mut store = Store::new();
        store.close_pred("component");
        let result = run(&c, &mut store, None);
        assert_eq!(result.requires[0].value, Value::True);
        assert_eq!(result.outcome, Outcome::Grounded);
    }

    #[test]
    fn handmade_layered_grounds() {
        let c = layered();
        let mut store = Store::new();
        seed_layered(&mut store, false);
        let result = run(&c, &mut store, None);
        assert_eq!(
            result.requires.iter().map(|r| r.value).collect::<Vec<_>>(),
            vec![Value::True, Value::True, Value::True, Value::True]
        );
        assert_eq!(result.outcome, Outcome::Grounded);
    }

    #[test]
    fn handmade_cycle_violates() {
        let c = layered();
        let mut store = Store::new();
        seed_layered(&mut store, true);
        let result = run(&c, &mut store, None);
        assert_eq!(result.requires[0].value, Value::True);
        assert_eq!(result.requires[1].value, Value::True);
        assert_eq!(result.requires[2].value, Value::False);
        assert_eq!(result.requires[3].value, Value::False);
        assert_eq!(result.outcome, Outcome::Violated);
        assert!(result.requires[3].witness.is_some());
    }

    #[test]
    fn scripted_navigator_fills_edb_and_logs_demands() {
        let c = layered();
        let mut store = Store::new();
        let mut nav = ScriptedNavigator::new()
            .with_pred(
                "component",
                vec![vec!["Frontend".into()], vec!["Backend".into()]],
            )
            .with_pred("file", vec![vec!["frontend/app.py".into()]])
            .with_pred(
                "includes",
                vec![vec!["Frontend".into(), "frontend/app.py".into()]],
            )
            .with_pred(
                "defines",
                vec![vec!["frontend/app.py".into(), "Backend".into()]],
            )
            .closing("component")
            .closing("file")
            .closing("includes")
            .closing("defines");
        let result = run(&c, &mut store, Some(&mut nav));
        assert!(nav.log.iter().any(|d| d.pred == "component"));
        assert!(result.requires[0].value == Value::True);
        assert_eq!(result.requires[1].value, Value::True);
        assert!(!nav.log.iter().any(|d| format!("{:?}", d).contains("decoy")));
    }
}
