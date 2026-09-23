use std::collections::{HashMap, HashSet};

use crate::universe::Universe;

use super::{Answer, Classification, Oracle, OracleError, Ticket};

/// Keyless stand-in over a [`Universe`]. A Noul is the probability that the
/// path is irrelevant to the proposition: high when no component
/// of the path occurs in it, low when one does. Search keeps a directory that
/// is not confidently irrelevant.
///
/// The proposition and the path are fields on the instruction object, so one
/// oracle can answer questions for many propositions.
pub struct HeuristicOracle<U> {
    universe: U,
    next: u64,
    pending: Vec<(Ticket, Answer)>,
    resolved: HashMap<Ticket, Answer>,
}

impl<U: Universe> HeuristicOracle<U> {
    pub fn new(universe: U) -> Self {
        Self {
            universe,
            next: 0,
            pending: Vec::new(),
            resolved: HashMap::new(),
        }
    }

    fn answer(&self, req: &Classification) -> Answer {
        let proposition = field(&req.instructions, "proposition");
        let path = field(&req.instructions, "path");
        let known = self.universe.is_leaf(path).is_ok();
        let noul = if known && path_matches(path, &tokens(proposition)) {
            0.05
        } else {
            0.95
        };
        Answer {
            choice: String::new(),
            confidence: noul,
            noul: Some(noul),
            exchange: None,
        }
    }
}

impl<U: Universe> Oracle for HeuristicOracle<U> {
    fn submit(&mut self, req: Classification) -> Ticket {
        let ticket = Ticket(self.next);
        self.next += 1;
        let answer = self.answer(&req);
        self.pending.push((ticket, answer));
        ticket
    }

    fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError> {
        if let Some(answer) = self.resolved.get(&ticket) {
            return Ok(answer.clone());
        }
        for (pending, answer) in self.pending.drain(..) {
            self.resolved.insert(pending, answer);
        }
        self.resolved
            .get(&ticket)
            .cloned()
            .ok_or_else(|| OracleError::Parse(format!("unknown ticket {}", ticket.0)))
    }
}

fn tokens(proposition: &str) -> HashSet<String> {
    proposition
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name)
}

fn field<'a>(instructions: &'a serde_json::Value, key: &str) -> &'a str {
    instructions
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn path_matches(id: &str, tokens: &HashSet<String>) -> bool {
    id.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        tokens.contains(&part) || tokens.contains(&stem(&part).to_ascii_lowercase())
    })
}
