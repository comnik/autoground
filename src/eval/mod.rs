use std::collections::{HashMap, HashSet};

use crate::core::Compiled;
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

/// A ground EDB atom the evaluator bottomed out on, with the judge's 3VL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdbAtom {
    pub pred: String,
    pub args: Vec<String>,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequireResult {
    pub value: Value,
    pub witness: Option<Witness>,
    pub edb: Vec<EdbAtom>,
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
    memo: HashMap<(String, Vec<String>), (Value, Vec<EdbAtom>)>,
    stack: HashSet<(String, Vec<String>)>,
    last_exists_witness: Option<Witness>,
    last_forall_counterexample: Option<Witness>,
    current_edb: Vec<EdbAtom>,
    edb_seen: HashSet<(String, Vec<String>)>,
    idb_leaf_stack: Vec<Vec<EdbAtom>>,
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
        current_edb: Vec::new(),
        edb_seen: HashSet::new(),
        idb_leaf_stack: Vec::new(),
    };
    if let Some(nav) = eng.navigator.as_mut() {
        nav.prepare(compiled);
    }
    let mut requires = Vec::new();
    for req in &compiled.requires {
        eng.last_exists_witness = None;
        eng.last_forall_counterexample = None;
        eng.current_edb.clear();
        eng.edb_seen.clear();
        let value = eng.eval(req, &HashMap::new());
        let witness = match value {
            Value::True => eng.last_exists_witness.clone(),
            Value::False => eng.last_forall_counterexample.clone(),
            Value::Unknown => None,
        };
        requires.push(RequireResult {
            value,
            witness,
            edb: std::mem::take(&mut eng.current_edb),
        });
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
        let negated = Formula::Not(Box::new(body.clone()));
        match self.eval_exists(vars, &negated, env) {
            Value::True => {
                self.last_forall_counterexample = self.last_exists_witness.take();
                Value::False
            }
            Value::False => {
                self.last_exists_witness = None;
                Value::True
            }
            Value::Unknown => Value::Unknown,
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
            let Some(demand_args) = self.generator_demand(&pred, &args, vars, env) else {
                continue;
            };
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
        let mut covered = HashSet::new();
        let mut any = false;
        for (pred, args) in &atoms {
            let Some(demand_args) = self.generator_demand(pred, args, vars, env) else {
                continue;
            };
            any = true;
            let pattern: Vec<Option<String>> = demand_args
                .iter()
                .map(|a| match a {
                    Arg::Bound(v) => Some(v.clone()),
                    Arg::Free(_) => None,
                })
                .collect();
            if !self.store.pattern_closed(pred, &pattern) {
                continue;
            }
            for a in demand_args {
                if let Arg::Free(v) = a {
                    covered.insert(v);
                }
            }
        }
        if !any {
            return self.universe_closed(body);
        }
        vars.iter().all(|v| covered.contains(v))
    }

    /// Values for exists-vars taken from generator tuples, not the forall universe.
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
            if self.generator_demand(&pred, &args, vars, env).is_none() {
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
                        Term::Const(c) | Term::Typed { value: c, .. } if c != cell => ok = false,
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
                Term::Const(c) | Term::Typed { value: c, .. } => Some(c.clone()),
                Term::Var(v) => env.get(v).cloned(),
            })
            .collect();

        if let Some(def) = self.compiled.defs.get(pred) {
            if resolved.iter().any(|a| a.is_none()) {
                return Value::Unknown;
            }
            let vars = def.vars.clone();
            let body = def.body.clone();
            let ground: Vec<String> = resolved.into_iter().map(|a| a.unwrap()).collect();
            let key = (pred.to_string(), ground.clone());
            if let Some((v, leaves)) = self.memo.get(&key).cloned() {
                for leaf in leaves {
                    self.record_edb(&leaf.pred, leaf.args, leaf.value);
                }
                return v;
            }
            if !self.stack.insert(key.clone()) {
                return Value::Unknown;
            }
            let mut env2 = HashMap::new();
            for (var, val) in vars.iter().zip(ground.iter()) {
                env2.insert(var.clone(), val.clone());
            }
            self.idb_leaf_stack.push(Vec::new());
            let v = self.eval(&body, &env2);
            let leaves = self.idb_leaf_stack.pop().unwrap_or_default();
            self.stack.remove(&key);
            if v != Value::Unknown {
                self.memo.insert(key, (v, leaves));
            }
            return v;
        }

        // EDB
        if resolved.iter().all(|a| a.is_some()) {
            let ground: Vec<String> = resolved.into_iter().map(|a| a.unwrap()).collect();
            let value = if self.store.has_fact(pred, &ground) {
                Value::True
            } else {
                self.demand_and_fill(
                    pred,
                    &ground
                        .iter()
                        .map(|g| Arg::Bound(g.clone()))
                        .collect::<Vec<_>>(),
                    Need::Truth,
                );
                if self.store.has_fact(pred, &ground) {
                    Value::True
                } else if self.store.is_closed_for(pred, &ground) {
                    Value::False
                } else {
                    Value::Unknown
                }
            };
            self.record_edb(pred, ground, value);
            return value;
        }

        // Free arguments: generation is handled by the enclosing exists.
        Value::Unknown
    }

    fn record_edb(&mut self, pred: &str, args: Vec<String>, value: Value) {
        let key = (pred.to_string(), args.clone());
        let atom = EdbAtom {
            pred: pred.to_string(),
            args,
            value,
        };
        if self.edb_seen.insert(key) {
            self.current_edb.push(atom.clone());
        }
        for frame in &mut self.idb_leaf_stack {
            if !frame
                .iter()
                .any(|a| a.pred == atom.pred && a.args == atom.args)
            {
                frame.push(atom.clone());
            }
        }
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

    /// An EDB atom generates exists-vars when every non-exists argument is
    /// bound. Whether that pattern yields tuples is the navigator's decision.
    fn generator_demand(
        &self,
        _pred: &str,
        args: &[Term],
        vars: &[String],
        env: &HashMap<String, String>,
    ) -> Option<Vec<Arg>> {
        let mut demand_args = Vec::new();
        let mut any_free = false;
        for t in args {
            match t {
                Term::Const(c) | Term::Typed { value: c, .. } => {
                    demand_args.push(Arg::Bound(c.clone()))
                }
                Term::Var(v) if env.contains_key(v) => {
                    demand_args.push(Arg::Bound(env[v].clone()));
                }
                Term::Var(v) if vars.iter().any(|x| x == v) => {
                    demand_args.push(Arg::Free(v.clone()));
                    any_free = true;
                }
                Term::Var(_) => return None,
            }
        }
        if !any_free {
            return None;
        }
        Some(demand_args)
    }

    fn universe_closed(&self, body: &Formula) -> bool {
        let mut preds = HashSet::new();
        collect_edb_preds(body, &self.compiled.idb_preds, &mut preds);
        // Unfold one level of IDB so closedness follows the body's EDB.
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
            "belongs_to",
            vec!["frontend/app.py".into(), "Frontend".into()],
            "test",
        );
        store.insert_fact(
            "belongs_to",
            vec!["backend/api.py".into(), "Backend".into()],
            "test",
        );
        store.insert_fact(
            "includes",
            vec!["frontend/app.py".into(), "backend/api.py".into()],
            "test",
        );
        if cycle {
            store.insert_fact(
                "includes",
                vec!["backend/api.py".into(), "frontend/app.py".into()],
                "test",
            );
        }
        for p in ["component", "file", "includes", "belongs_to"] {
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
        for p in ["component", "file", "includes", "belongs_to"] {
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
            .with_pred(
                "file",
                vec![
                    vec!["frontend/app.py".into()],
                    vec!["backend/api.py".into()],
                ],
            )
            .with_pred(
                "belongs_to",
                vec![
                    vec!["frontend/app.py".into(), "Frontend".into()],
                    vec!["backend/api.py".into(), "Backend".into()],
                ],
            )
            .with_pred(
                "includes",
                vec![vec!["frontend/app.py".into(), "backend/api.py".into()]],
            )
            .closing("component")
            .closing("file")
            .closing("includes")
            .closing("belongs_to");
        let result = run(&c, &mut store, Some(&mut nav));
        assert!(nav.log.iter().any(|d| d.pred == "component"));
        assert!(result.requires[0].value == Value::True);
        assert_eq!(result.requires[1].value, Value::True);
        assert!(!nav.log.iter().any(|d| format!("{:?}", d).contains("decoy")));
    }

    #[test]
    fn require_records_ground_edb_leaves() {
        let src = r#"(require (and (component "Frontend") (component "Missing")))"#;
        let c = compile(&parse(src).unwrap()).unwrap();
        let mut store = Store::new();
        store.insert_fact("component", vec!["Frontend".into()], "test");
        store.close_pred("component");
        let result = run(&c, &mut store, None);
        assert_eq!(result.requires[0].value, Value::False);
        let edb = &result.requires[0].edb;
        assert_eq!(edb.len(), 2, "{edb:?}");
        assert_eq!(edb[0].pred, "component");
        assert_eq!(edb[0].args, vec!["Frontend".to_string()]);
        assert_eq!(edb[0].value, Value::True);
        assert_eq!(edb[1].args, vec!["Missing".to_string()]);
        assert_eq!(edb[1].value, Value::False);
    }

    #[test]
    fn idb_memo_replays_edb_leaves_on_later_require() {
        let src = r#"
(def ok [x] (component x))
(require (ok "Frontend"))
(require (ok "Frontend"))
"#;
        let c = compile(&parse(src).unwrap()).unwrap();
        let mut store = Store::new();
        store.insert_fact("component", vec!["Frontend".into()], "test");
        store.close_pred("component");
        let result = run(&c, &mut store, None);
        assert_eq!(result.requires[0].edb.len(), 1);
        assert_eq!(result.requires[1].edb.len(), 1);
        assert_eq!(result.requires[1].edb[0].pred, "component");
        assert_eq!(result.requires[1].edb[0].value, Value::True);
    }

    #[test]
    fn depends_on_generates_files_from_belongs_to() {
        let c = layered();
        let mut store = Store::new();
        store.insert_fact("component", vec!["Frontend".into()], "test");
        store.insert_fact("component", vec!["Backend".into()], "test");
        store.close_pred("component");
        let mut nav = ScriptedNavigator::new()
            .with_pred(
                "belongs_to",
                vec![
                    vec!["frontend/app.py".into(), "Frontend".into()],
                    vec!["backend/api.py".into(), "Backend".into()],
                ],
            )
            .with_pred(
                "file",
                vec![
                    vec!["frontend/app.py".into()],
                    vec!["backend/api.py".into()],
                ],
            )
            .with_pred(
                "includes",
                vec![vec!["frontend/app.py".into(), "backend/api.py".into()]],
            )
            .closing("belongs_to")
            .closing("file")
            .closing("includes");
        let result = run(&c, &mut store, Some(&mut nav));
        assert!(
            nav.log.iter().any(|d| d.pred == "belongs_to"
                && d.args.iter().any(|a| matches!(a, Arg::Free(_)))),
            "expected a belongs_to generator demand, got {:?}",
            nav.log
        );
        assert_eq!(result.requires[1].value, Value::True);
        assert!(result.requires[1].edb.iter().any(|a| {
            a.pred == "includes"
                && a.args == ["frontend/app.py".to_string(), "backend/api.py".to_string()]
                && a.value == Value::True
        }));
    }

    #[test]
    fn exists_ranges_over_component() {
        let src = r#"
(declare component "{String} is a component in the codebase")
(require (exists [c] (component c)))
"#;
        let c = compile(&parse(src).unwrap()).unwrap();
        let mut store = Store::new();
        let mut nav = ScriptedNavigator::new()
            .with_pred(
                "component",
                vec![vec!["Frontend".into()], vec!["Backend".into()]],
            )
            .closing("component");
        let result = run(&c, &mut store, Some(&mut nav));
        assert_eq!(result.outcome, Outcome::Grounded);
        assert_eq!(result.requires[0].value, Value::True);
        assert!(
            nav.log
                .iter()
                .any(|d| d.pred == "component" && d.args.iter().any(|a| matches!(a, Arg::Free(_)))),
            "expected a free component demand, got {:?}",
            nav.log
        );
        let w = result.requires[0].witness.as_ref().expect("witness");
        assert_eq!(w.assignment[0].0, "c");
        assert!(w.assignment[0].1 == "Backend" || w.assignment[0].1 == "Frontend");
    }

    #[test]
    fn exists_over_unanswered_file_stays_unknown() {
        let src = r#"(require (exists [f] (file f)))"#;
        let c = compile(&parse(src).unwrap()).unwrap();
        let mut store = Store::new();
        let result = run(&c, &mut store, None);
        assert_eq!(result.requires[0].value, Value::Unknown);
        assert_eq!(result.outcome, Outcome::Ungrounded);
    }

    fn every_component_has_a_file() -> Compiled {
        let src = r#"
(declare component "{String} is a component")
(declare file "{String} is a file")
(declare belongs_to "{file} belongs to {component}")
(require
  (forall [c]
    (implies
      (component c)
      (exists [f]
        (and
          (file f)
          (belongs_to f c))))))
"#;
        compile(&parse(src).unwrap()).unwrap()
    }

    #[test]
    fn forall_without_constants_is_unknown_while_edb_is_open() {
        let c = every_component_has_a_file();
        let mut store = Store::new();
        let result = run(&c, &mut store, None);
        assert_eq!(result.requires[0].value, Value::Unknown);
        assert_eq!(result.outcome, Outcome::Ungrounded);
    }

    #[test]
    fn forall_generated_components_need_a_member_file() {
        let c = every_component_has_a_file();
        let mut store = Store::new();
        let mut nav = ScriptedNavigator::new()
            .with_pred("component", vec![vec!["Frontend".into()]])
            .with_pred("file", vec![vec!["ui/app.py".into()]])
            .with_pred(
                "belongs_to",
                vec![vec!["ui/app.py".into(), "Frontend".into()]],
            )
            .closing("component")
            .closing("file")
            .closing("belongs_to");
        let result = run(&c, &mut store, Some(&mut nav));
        assert_eq!(result.requires[0].value, Value::True);
        assert_eq!(result.outcome, Outcome::Grounded);
        assert!(
            nav.log
                .iter()
                .any(|d| d.pred == "component" && d.args.iter().any(|a| matches!(a, Arg::Free(_)))),
            "forall should generate components, got {:?}",
            nav.log
        );
    }

    #[test]
    fn forall_fails_when_a_component_has_no_file() {
        let c = every_component_has_a_file();
        let mut store = Store::new();
        let mut nav = ScriptedNavigator::new()
            .with_pred("component", vec![vec!["Frontend".into()]])
            .closing("component")
            .closing("file")
            .closing("belongs_to");
        let result = run(&c, &mut store, Some(&mut nav));
        assert_eq!(result.requires[0].value, Value::False);
        assert_eq!(result.outcome, Outcome::Violated);
        let w = result.requires[0].witness.as_ref().expect("counterexample");
        assert_eq!(w.assignment, vec![("c".into(), "Frontend".into())]);
    }
}
