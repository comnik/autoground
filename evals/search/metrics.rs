use std::collections::HashSet;

use autoground::search::{retained_ids, Decision};
use autoground::universe::{Filesystem, Universe, UniverseError};

#[derive(Debug, Clone, PartialEq)]
pub struct FloorPoint {
    pub floor: f64,
    pub pruned: Option<f64>,
    pub evidence: Option<f64>,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub selected: usize,
    pub missed: Vec<String>,
}

pub fn score_floors(
    universe: &Filesystem,
    decisions: &[Decision],
    evidence: &[String],
    floors: &[f64],
) -> Result<Vec<FloorPoint>, UniverseError> {
    let bytes_before = reachable_bytes(universe)? as u64;
    let mut points = Vec::with_capacity(floors.len());
    for &floor in floors {
        let ids = retained_ids(universe, decisions, floor)?;
        let mut view = universe.clone();
        view.set(ids)?;
        let bytes_after = view.estimate()? as u64;
        let mut files = HashSet::new();
        for id in view.objects() {
            if view.is_leaf(id)? {
                files.insert(id.clone());
            }
        }
        let mut missed = Vec::new();
        let mut selected = 0;
        for path in evidence {
            if files.contains(path) {
                selected += 1;
            } else {
                missed.push(path.clone());
            }
        }
        let evidence_rate = if evidence.is_empty() {
            None
        } else {
            Some(selected as f64 / evidence.len() as f64)
        };
        let pruned = if bytes_before == 0 {
            None
        } else {
            Some((bytes_before - bytes_after) as f64 / bytes_before as f64)
        };
        points.push(FloorPoint {
            floor,
            pruned,
            evidence: evidence_rate,
            bytes_before,
            bytes_after,
            selected,
            missed,
        });
    }
    Ok(points)
}

/// Prune rate of a window that contains exactly `evidence` and nothing else.
pub fn ideal_pruned(
    universe: &Filesystem,
    evidence: &[String],
) -> Result<Option<f64>, UniverseError> {
    if evidence.is_empty() {
        return Ok(None);
    }
    let bytes_before = reachable_bytes(universe)? as u64;
    if bytes_before == 0 {
        return Ok(None);
    }
    let mut view = universe.clone();
    view.set(evidence.to_vec())?;
    let bytes_after = view.estimate()? as u64;
    Ok(Some(
        (bytes_before - bytes_after) as f64 / bytes_before as f64,
    ))
}

pub fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn reachable_bytes(universe: &Filesystem) -> Result<usize, UniverseError> {
    let mut full = universe.clone();
    let mut ids = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        for child in full.children(&dir)? {
            ids.push(child.clone());
            if !full.is_leaf(&child)? {
                stack.push(child);
            }
        }
    }
    full.set(ids)?;
    full.estimate()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(id: &str, confidence: f64) -> Decision {
        Decision {
            id: id.to_string(),
            leaf: false,
            raw: "prune".to_string(),
            applied: autoground::search::Applied::Keep,
            confidence,
        }
    }

    #[test]
    fn a_lower_floor_prunes_more_and_drops_evidence() {
        let root = std::env::temp_dir().join(format!(
            "autoground-search-eval-{}-{}",
            std::process::id(),
            "floors"
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "root\n").unwrap();
        std::fs::write(root.join("src/a.txt"), "aaaa\n").unwrap();
        let universe = Filesystem::at_root(&root).unwrap();
        let decisions = vec![decision("src", 0.60)];
        let evidence = vec!["README.md".to_string(), "src/a.txt".to_string()];
        let points = score_floors(&universe, &decisions, &evidence, &[0.85, 0.50]).unwrap();

        let loose = &points[0];
        assert!((loose.evidence.unwrap() - 1.0).abs() < 1e-12);
        assert!(loose.missed.is_empty());
        let mut kept = universe.clone();
        kept.set(vec![
            "README.md".to_string(),
            "src".to_string(),
            "src/a.txt".to_string(),
        ])
        .unwrap();
        assert_eq!(loose.bytes_after, kept.estimate().unwrap() as u64);
        assert!(
            (loose.pruned.unwrap() - (1.0 - loose.bytes_after as f64 / loose.bytes_before as f64))
                .abs()
                < 1e-12
        );

        let tight = &points[1];
        assert!((tight.evidence.unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(tight.missed, vec!["src/a.txt".to_string()]);
        assert!(tight.pruned.unwrap() > loose.pruned.unwrap());
        let mut readme = universe.clone();
        readme.set(vec!["README.md".to_string()]).unwrap();
        assert_eq!(tight.bytes_after, readme.estimate().unwrap() as u64);
        let ideal = ideal_pruned(&universe, &["README.md".to_string()]).unwrap();
        assert!((ideal.unwrap() - tight.pruned.unwrap()).abs() < 1e-9);
        assert!(ideal.unwrap() > loose.pruned.unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }
}
