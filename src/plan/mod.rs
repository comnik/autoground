//! Decompose a compiled model into atomic sentences.
//!
//! Each claim is boolean structure over sentences whose holes are already
//! filled. Planning names each distinct sentence once, in the order the claims
//! mention it, and keeps the claim as an expression over those sentences.
//! [`Plan::evaluate`] combines a value per sentence with Kleene `and`, `or`,
//! and `not`. Nothing here reads a universe or asks an oracle.

use std::collections::HashMap;

use crate::syntax::{verbalize_proposition, Proposition};
use crate::value::Value;
use crate::Compiled;

/// Kleene structure over indexes into [`Plan::atoms`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Atom(usize),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

/// One claim: the ground sentence as written, and how its atoms combine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub verbalized: String,
    pub expr: Expr,
}

/// Atomic sentences and the claim that mentions each of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Distinct sentences, first mention first.
    pub atoms: Vec<String>,
    pub claims: Vec<Claim>,
}

impl Plan {
    pub fn of(compiled: &Compiled) -> Self {
        let mut atoms = Vec::new();
        let mut index = HashMap::new();
        let mut claims = Vec::new();
        for proposition in &compiled.claims {
            claims.push(Claim {
                verbalized: verbalize_proposition(proposition),
                expr: lower(proposition, &mut atoms, &mut index),
            });
        }
        Self { atoms, claims }
    }

    /// Value of each claim. `atom_values` lines up with [`Plan::atoms`].
    pub fn evaluate(&self, atom_values: &[Value]) -> Vec<Value> {
        self.claims
            .iter()
            .map(|claim| eval(&claim.expr, atom_values))
            .collect()
    }
}

fn lower(
    proposition: &Proposition,
    atoms: &mut Vec<String>,
    index: &mut HashMap<String, usize>,
) -> Expr {
    match proposition {
        Proposition::And(xs) => Expr::And(xs.iter().map(|x| lower(x, atoms, index)).collect()),
        Proposition::Or(xs) => Expr::Or(xs.iter().map(|x| lower(x, atoms, index)).collect()),
        Proposition::Not(inner) => Expr::Not(Box::new(lower(inner, atoms, index))),
        Proposition::Sentence(sentence) => Expr::Atom(intern(sentence, atoms, index)),
    }
}

fn intern(sentence: &str, atoms: &mut Vec<String>, index: &mut HashMap<String, usize>) -> usize {
    if let Some(id) = index.get(sentence) {
        return *id;
    }
    let id = atoms.len();
    atoms.push(sentence.to_string());
    index.insert(sentence.to_string(), id);
    id
}

fn eval(expr: &Expr, values: &[Value]) -> Value {
    match expr {
        Expr::Atom(i) => values[*i],
        Expr::Not(inner) => eval(inner, values).not(),
        // Vacuous `and` is true and vacuous `or` is false. A parsed claim has
        // at least the head; an empty tail is still a claim.
        Expr::And(xs) => xs
            .iter()
            .fold(Value::True, |acc, x| acc.and(eval(x, values))),
        Expr::Or(xs) => xs
            .iter()
            .fold(Value::False, |acc, x| acc.or(eval(x, values))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use crate::syntax::parse;

    fn planned(src: &str) -> Plan {
        Plan::of(&compile(&parse(src).unwrap()).unwrap())
    }

    #[test]
    fn sentences_are_named_once_in_mention_order() {
        let plan = planned(
            r#"
(name Frontend)
(name Backend)
(name Oracle)
(name TypeSafe)
(sense depends_on "{X} depends on {Y}")
(sense calls "{caller} calls {callee}")
(depends_on Frontend Backend)
(not (depends_on Backend Frontend))
(or (calls Oracle TypeSafe) ("Oracle calls the TypeSafe API"))
(and (depends_on Frontend Backend) (not (calls Oracle TypeSafe)))
"#,
        );
        assert_eq!(
            plan.atoms,
            vec![
                "Frontend depends on Backend".to_string(),
                "Backend depends on Frontend".to_string(),
                "Oracle calls TypeSafe".to_string(),
                "Oracle calls the TypeSafe API".to_string(),
            ]
        );
        assert_eq!(plan.claims[0].expr, Expr::Atom(0),);
        assert_eq!(plan.claims[1].expr, Expr::Not(Box::new(Expr::Atom(1))),);
        assert_eq!(
            plan.claims[2].expr,
            Expr::Or(vec![Expr::Atom(2), Expr::Atom(3)]),
        );
        assert_eq!(
            plan.claims[3].expr,
            Expr::And(vec![Expr::Atom(0), Expr::Not(Box::new(Expr::Atom(2))),]),
        );
        assert_eq!(
            plan.claims[3].verbalized,
            "(and (\"Frontend depends on Backend\") (not (\"Oracle calls TypeSafe\")))"
        );
    }

    #[test]
    fn diagram_example_lists_each_sentence_once() {
        let plan = planned(include_str!("../../examples/diagram.ag"));
        assert_eq!(
            plan.atoms,
            vec![
                "Frontend depends on Backend".to_string(),
                "Backend depends on Frontend".to_string(),
                "Oracle calls TypeSafe".to_string(),
                "Oracle calls the TypeSafe API".to_string(),
                "Frontend is a component of the system".to_string(),
                "Backend is a component of the system".to_string(),
                "Frontend calls Backend".to_string(),
            ]
        );
    }

    #[test]
    fn kleene_and_or_not() {
        let plan = planned(
            r#"
(and ("p") (not ("q")))
(or ("q") (not ("p")))
(not (or ("p") ("q")))
(and)
(or)
"#,
        );
        assert_eq!(plan.atoms, vec!["p".to_string(), "q".to_string()]);
        assert_eq!(
            plan.evaluate(&[Value::True, Value::Unknown]),
            vec![
                Value::Unknown,
                Value::Unknown,
                Value::False,
                Value::True,
                Value::False,
            ]
        );
        assert_eq!(
            plan.evaluate(&[Value::False, Value::False]),
            vec![
                Value::False,
                Value::True,
                Value::True,
                Value::True,
                Value::False,
            ]
        );
    }
}
