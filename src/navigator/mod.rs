pub mod confidence;
pub mod fixture;
pub mod heuristic;
pub mod jev;
pub mod judge;
pub mod record;
pub mod trace;
pub mod tree;
pub mod walk;

use crate::edb::Store;

pub use fixture::{FixtureFile, FixtureJudge, RequireTrace};
pub use heuristic::HeuristicJudge;
pub use jev::JevJudge;
pub use judge::{Judge, JudgeError, Transition};
pub use record::RecordingJudge;
pub use trace::GroundStep;
pub use tree::TreeNavigator;

/// What the evaluator needs from an EDB atom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Need {
    Truth,
    Closed,
}

/// An argument in a demand: already bound, or free (generate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Bound(String),
    Free(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demand {
    pub pred: String,
    pub args: Vec<Arg>,
    pub need: Need,
}

/// Result of answering a demand: tuples to insert, optional close, optional bindings.
#[derive(Debug, Clone, Default)]
pub struct Answer {
    pub tuples: Vec<Vec<String>>,
    pub close: bool,
    pub bindings: Vec<(String, String)>,
    pub source: String,
}

pub trait Navigator {
    fn answer(&mut self, demand: &Demand, store: &Store) -> Answer;

    /// Answer several demands. Default calls `answer` in order. A tree
    /// navigator overrides this so locate atoms share one listing walk and
    /// document questions on the same file share one request.
    fn answer_many(&mut self, demands: &[Demand], store: &Store) -> Vec<Answer> {
        demands.iter().map(|d| self.answer(d, store)).collect()
    }

    /// Receive declarations from the compiled model (verbalization, signatures).
    fn prepare(&mut self, _compiled: &crate::core::Compiled) {}
}

/// Replays a dict of tuples. Used in tests of the evaluator itself.
#[derive(Debug, Clone, Default)]
pub struct ScriptedNavigator {
    pub tuples: std::collections::HashMap<String, Vec<Vec<String>>>,
    pub close: std::collections::HashSet<String>,
    pub bindings: Vec<(String, String)>,
    pub log: Vec<Demand>,
}

impl ScriptedNavigator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_pred(mut self, pred: impl Into<String>, rows: Vec<Vec<String>>) -> Self {
        self.tuples.insert(pred.into(), rows);
        self
    }

    pub fn closing(mut self, pred: impl Into<String>) -> Self {
        self.close.insert(pred.into());
        self
    }
}

impl Navigator for ScriptedNavigator {
    fn answer(&mut self, demand: &Demand, _store: &Store) -> Answer {
        self.log.push(demand.clone());
        let rows = self.tuples.get(&demand.pred).cloned().unwrap_or_default();
        let tuples = rows
            .into_iter()
            .filter(|row| matches_args(row, &demand.args))
            .collect();
        let has_free = demand.args.iter().any(|a| matches!(a, Arg::Free(_)));
        let close = self.close.contains(&demand.pred) && (has_free || demand.need == Need::Closed);
        Answer {
            tuples,
            close,
            bindings: self
                .bindings
                .iter()
                .filter(|(c, _)| {
                    demand.args.iter().any(|a| match a {
                        Arg::Bound(v) => v == c,
                        Arg::Free(_) => false,
                    })
                })
                .cloned()
                .collect(),
            source: "scripted".into(),
        }
    }
}

fn matches_args(row: &[String], args: &[Arg]) -> bool {
    if row.len() != args.len() {
        return false;
    }
    row.iter().zip(args).all(|(cell, arg)| match arg {
        Arg::Bound(v) => cell == v,
        Arg::Free(_) => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_bound_args() {
        let mut nav = ScriptedNavigator::new().with_pred(
            "includes",
            vec![
                vec!["Frontend".into(), "a.py".into()],
                vec!["Backend".into(), "b.py".into()],
            ],
        );
        let d = Demand {
            pred: "includes".into(),
            args: vec![Arg::Bound("Frontend".into()), Arg::Free("f".into())],
            need: Need::Truth,
        };
        let a = nav.answer(&d, &Store::new());
        assert_eq!(
            a.tuples,
            vec![vec!["Frontend".to_string(), "a.py".to_string()]]
        );
        assert_eq!(nav.log.len(), 1);
    }
}
