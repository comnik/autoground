use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::oracle::{Answer, Classification, Oracle, OracleError, Ticket};
use crate::universe::fs::{list_entries, repo_join, ListingFilter};

use super::target_of;

/// Keyless stand-in. A directory whose own name occurs in the proposition is
/// `true`; one that only contains such a name is `refine`; otherwise `prune`.
/// A file whose stem occurs is `true`; otherwise `unknown`.
pub struct HeuristicOracle {
    root: PathBuf,
    filter: ListingFilter,
    tokens: HashSet<String>,
    next: u64,
    pending: Vec<(Ticket, Answer)>,
    resolved: HashMap<Ticket, Answer>,
}

impl HeuristicOracle {
    pub fn new(root: impl Into<PathBuf>, proposition: &str) -> Self {
        let root = root.into();
        let filter = ListingFilter::from_repo(&root);
        Self {
            root,
            filter,
            tokens: tokens(proposition),
            next: 0,
            pending: Vec::new(),
            resolved: HashMap::new(),
        }
    }

    fn answer(&self, req: &Classification) -> Answer {
        let id = target_of(&req.instructions);
        let choice = if is_file(&self.root, id) {
            if file_matches(id, &self.tokens) {
                "true"
            } else {
                "unknown"
            }
        } else if dir_matches(id, &self.tokens) {
            "true"
        } else if descendant_matches(&self.root, &self.filter, id, &self.tokens) {
            "refine"
        } else {
            "prune"
        };
        Answer {
            choice: choice.to_string(),
            confidence: 0.9,
            exchange: None,
        }
    }
}

impl Oracle for HeuristicOracle {
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

fn is_file(root: &Path, id: &str) -> bool {
    std::fs::metadata(repo_join(root, id))
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

fn dir_matches(id: &str, tokens: &HashSet<String>) -> bool {
    let name = basename(id).to_ascii_lowercase();
    !name.is_empty() && tokens.contains(&name)
}

fn file_matches(id: &str, tokens: &HashSet<String>) -> bool {
    let name = basename(id).to_ascii_lowercase();
    tokens.contains(&stem(&name).to_ascii_lowercase())
}

fn descendant_matches(
    root: &Path,
    filter: &ListingFilter,
    id: &str,
    tokens: &HashSet<String>,
) -> bool {
    let mut stack = vec![id.to_string()];
    while let Some(dir) = stack.pop() {
        let Ok(children) = list_entries(root, &dir, filter) else {
            continue;
        };
        for child in children {
            let name = child.name.to_ascii_lowercase();
            let hit = if child.is_dir {
                tokens.contains(&name)
            } else {
                tokens.contains(&stem(&name).to_ascii_lowercase())
            };
            if hit {
                return true;
            }
            if child.is_dir {
                stack.push(child.rel_path);
            }
        }
    }
    false
}
