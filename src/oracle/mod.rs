//! Classification oracle: enqueue questions, batch on wait.
//!
//! Callers (the navigator now; search later) `submit` one classification at a
//! time and `wait` when they need the label. The 3VL engine does not hold an
//! oracle. Grouping is over the `state` JSON the caller passed — typically a
//! Universe `render()`, a listing tree, or a file body. The oracle hashes that
//! JSON; Universe does not grow a digest method.

pub mod jev;

use std::collections::BTreeMap;
use std::fmt;
use std::time::SystemTime;

pub use jev::{JevOracle, Transport, UreqTransport};

/// One Choice to evaluate against a shared System One `state`.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    /// Shared context. Equal states share a POST.
    pub state: serde_json::Value,
    pub instructions: String,
    /// Label → criterion. Keys are the answer space.
    pub criteria: BTreeMap<String, String>,
}

/// Opaque id returned by [`Oracle::submit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket(pub(crate) u64);

/// A Choice answer, plus the exchange that produced it when the impl is remote.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub choice: String,
    pub confidence: f64,
    pub exchange: Option<OracleExchange>,
}

/// One transport round-trip. Shared across every ticket resolved by that POST.
#[derive(Debug, Clone, PartialEq)]
pub struct OracleExchange {
    pub request: serde_json::Value,
    pub response: serde_json::Value,
    pub started_at: Option<SystemTime>,
    pub ended_at: Option<SystemTime>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleError {
    Config(String),
    Http(String),
    Parse(String),
    /// Cumulative spend for this oracle hit the hard cap.
    Limit(String),
}

impl fmt::Display for OracleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OracleError::Config(s)
            | OracleError::Http(s)
            | OracleError::Parse(s)
            | OracleError::Limit(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for OracleError {}

/// Classification oracle. Jev is the first implementation.
///
/// `submit` only enqueues. `wait` flushes pending work: same-state questions
/// share a POST (chunked), independent states overlap. A later Jev impl may
/// POST from `submit` once a same-state group fills a request; not yet.
pub trait Oracle {
    fn submit(&mut self, req: Classification) -> Ticket;
    fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError>;

    fn wait_many(&mut self, tickets: &[Ticket]) -> Vec<Result<Answer, OracleError>> {
        tickets.iter().copied().map(|t| self.wait(t)).collect()
    }

    /// USD recorded on this instance so far. Local impls stay at 0.
    fn spent_usd(&self) -> f64 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_are_copy_ids() {
        let a = Ticket(0);
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, Ticket(1));
    }
}
