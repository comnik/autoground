use std::collections::{HashMap, HashSet};

use crate::universe::Universe;

use super::{Answer, Classification, Oracle, OracleError, Ticket};

/// Keyless stand-in over a [`Universe`]. A directory is `keep` when its own
/// name occurs in the proposition or a descendant name does; otherwise
/// `prune`. A file is `true` when its stem or a parent directory name occurs;
/// otherwise `unknown`.
///
/// The proposition is the `Proposition:` line of the instructions, so one
/// oracle can answer questions for many propositions. File versus directory
/// comes from a `kind:` line. The judged id comes from a `target:` line.
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
        let id = target_of(&req.instructions);
        let tokens = tokens(proposition_of(&req.instructions));
        let choice = if is_leaf_question(&req.instructions, &self.universe, id) {
            if path_matches(id, &tokens) {
                "true"
            } else {
                "unknown"
            }
        } else if dir_matches(id, &tokens) || self.descendant_matches(id, &tokens) {
            "keep"
        } else {
            "prune"
        };
        Answer {
            choice: choice.to_string(),
            confidence: 0.9,
            exchange: None,
        }
    }

    fn descendant_matches(&self, id: &str, tokens: &HashSet<String>) -> bool {
        let mut stack = vec![id.to_string()];
        while let Some(dir) = stack.pop() {
            let Ok(children) = self.universe.children(&dir) else {
                continue;
            };
            for child in children {
                let name = basename(&child).to_ascii_lowercase();
                let is_dir = self.universe.is_leaf(&child).ok() == Some(false);
                let hit = if is_dir {
                    tokens.contains(&name)
                } else {
                    tokens.contains(&stem(&name).to_ascii_lowercase())
                };
                if hit {
                    return true;
                }
                if is_dir {
                    stack.push(child);
                }
            }
        }
        false
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

fn basename(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name)
}

fn is_leaf_question<U: Universe>(instructions: &str, universe: &U, id: &str) -> bool {
    match kind_of(instructions) {
        "file" => true,
        "directory" => false,
        _ => universe.is_leaf(id).unwrap_or(false),
    }
}

fn proposition_of(instructions: &str) -> &str {
    instructions
        .lines()
        .find_map(|line| line.strip_prefix("Proposition: "))
        .unwrap_or("")
}

fn kind_of(instructions: &str) -> &str {
    instructions
        .lines()
        .find_map(|line| line.strip_prefix("kind: "))
        .unwrap_or("")
}

fn target_of(instructions: &str) -> &str {
    instructions
        .lines()
        .find_map(|line| line.strip_prefix("target: "))
        .unwrap_or("")
}

fn dir_matches(id: &str, tokens: &HashSet<String>) -> bool {
    let name = basename(id).to_ascii_lowercase();
    !name.is_empty() && tokens.contains(&name)
}

fn path_matches(id: &str, tokens: &HashSet<String>) -> bool {
    id.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        tokens.contains(&part) || tokens.contains(&stem(&part).to_ascii_lowercase())
    })
}
