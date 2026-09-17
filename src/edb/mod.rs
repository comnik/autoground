use std::collections::{HashMap, HashSet};

/// Three-valued truth. Stored facts are `True`; absence is `False` only when closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    Open,
    Closed,
}

/// A ground EDB tuple.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tuple {
    pub pred: String,
    pub args: Vec<String>,
}

/// Provenance for a stored fact or binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub pred: String,
    pub args: Vec<String>,
    pub source: String,
}

/// `constant → {entity*}` working denotation. Several rows per constant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Binding {
    pub constant: String,
    pub entity: String,
}

/// In-memory EDB matching the DuckDB schema we will persist later:
///
/// ```text
/// facts(pred TEXT, args TEXT[], ...)
/// completeness(pred TEXT, closed BOOLEAN)
/// bindings(constant TEXT, entity TEXT)
/// provenance(pred TEXT, args TEXT[], source TEXT)
/// ```
#[derive(Debug, Clone, Default)]
pub struct Store {
    facts: HashSet<Tuple>,
    /// Predicates closed at every argument position.
    closed: HashSet<String>,
    /// Projected closedness: `Some(v)` is a bound column, `None` is free.
    closed_patterns: HashSet<(String, Vec<Option<String>>)>,
    bindings: HashSet<Binding>,
    provenance: Vec<Provenance>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_fact(
        &mut self,
        pred: impl Into<String>,
        args: Vec<String>,
        source: impl Into<String>,
    ) {
        let pred = pred.into();
        let source = source.into();
        self.provenance.push(Provenance {
            pred: pred.clone(),
            args: args.clone(),
            source,
        });
        self.facts.insert(Tuple { pred, args });
    }

    pub fn has_fact(&self, pred: &str, args: &[String]) -> bool {
        self.facts.contains(&Tuple {
            pred: pred.to_string(),
            args: args.to_vec(),
        })
    }

    pub fn facts_for(&self, pred: &str) -> Vec<Vec<String>> {
        self.facts
            .iter()
            .filter(|t| t.pred == pred)
            .map(|t| t.args.clone())
            .collect()
    }

    pub fn close_pred(&mut self, pred: impl Into<String>) {
        self.closed.insert(pred.into());
    }

    pub fn close_pattern(&mut self, pred: impl Into<String>, pattern: Vec<Option<String>>) {
        if pattern.iter().all(|p| p.is_none()) {
            self.closed.insert(pred.into());
        } else {
            self.closed_patterns.insert((pred.into(), pattern));
        }
    }

    pub fn is_closed(&self, pred: &str) -> bool {
        self.closed.contains(pred)
    }

    /// A ground atom is false (not unknown) when its predicate is closed
    /// at a covering pattern.
    pub fn is_closed_for(&self, pred: &str, args: &[String]) -> bool {
        if self.closed.contains(pred) {
            return true;
        }
        self.closed_patterns.iter().any(|(p, pat)| {
            p == pred
                && pat.len() == args.len()
                && pat.iter().zip(args).all(|(slot, arg)| match slot {
                    None => true,
                    Some(v) => v == arg,
                })
        })
    }

    /// Whether a demand pattern (bound columns `Some`, free `None`) is closed.
    pub fn pattern_closed(&self, pred: &str, demand: &[Option<String>]) -> bool {
        if self.closed.contains(pred) {
            return true;
        }
        self.closed_patterns.iter().any(|(p, stored)| {
            p == pred
                && stored.len() == demand.len()
                && stored.iter().zip(demand).all(|(s, d)| match (s, d) {
                    (None, _) => true,
                    (Some(sv), Some(dv)) => sv == dv,
                    (Some(_), None) => false,
                })
        })
    }

    /// True when every named predicate is closed. Used as a stand-in for a
    /// closed universe of constants drawn from those predicates.
    pub fn all_closed(&self, preds: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
        preds.into_iter().all(|p| self.is_closed(p.as_ref()))
    }

    pub fn bind(&mut self, constant: impl Into<String>, entity: impl Into<String>) {
        self.bindings.insert(Binding {
            constant: constant.into(),
            entity: entity.into(),
        });
    }

    pub fn bindings_for(&self, constant: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .iter()
            .filter(|b| b.constant == constant)
            .map(|b| b.entity.clone())
            .collect();
        v.sort();
        v
    }

    pub fn all_bindings(&self) -> Vec<Binding> {
        let mut v: Vec<_> = self.bindings.iter().cloned().collect();
        v.sort_by(|a, b| a.constant.cmp(&b.constant).then(a.entity.cmp(&b.entity)));
        v
    }

    pub fn provenance(&self) -> &[Provenance] {
        &self.provenance
    }

    /// Constants that appear as arguments of stored facts, plus bound names.
    pub fn known_constants(&self) -> Vec<String> {
        let mut s = HashSet::new();
        for t in &self.facts {
            for a in &t.args {
                s.insert(a.clone());
            }
        }
        for b in &self.bindings {
            s.insert(b.constant.clone());
            s.insert(b.entity.clone());
        }
        let mut v: Vec<_> = s.into_iter().collect();
        v.sort();
        v
    }

    /// DDL for the on-disk DuckDB artefact. Persistence lands in a later slice.
    pub fn duckdb_schema() -> &'static str {
        r#"
CREATE TABLE IF NOT EXISTS facts (
  pred TEXT NOT NULL,
  args TEXT[] NOT NULL
);
CREATE TABLE IF NOT EXISTS completeness (
  pred TEXT PRIMARY KEY,
  closed BOOLEAN NOT NULL
);
CREATE TABLE IF NOT EXISTS bindings (
  constant TEXT NOT NULL,
  entity TEXT NOT NULL,
  PRIMARY KEY (constant, entity)
);
CREATE TABLE IF NOT EXISTS provenance (
  pred TEXT NOT NULL,
  args TEXT[] NOT NULL,
  source TEXT NOT NULL
);
"#
    }
}

/// Group facts by predicate for a scripted navigator seed.
pub fn facts_by_pred(store: &Store) -> HashMap<String, Vec<Vec<String>>> {
    let mut m: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    for t in &store.facts {
        m.entry(t.pred.clone()).or_default().push(t.args.clone());
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_valued_bindings() {
        let mut s = Store::new();
        s.bind("Backend", "api/");
        s.bind("Backend", "server/");
        assert_eq!(s.bindings_for("Backend"), vec!["api/", "server/"]);
    }

    #[test]
    fn schema_is_nonempty() {
        assert!(Store::duckdb_schema().contains("CREATE TABLE"));
    }
}
