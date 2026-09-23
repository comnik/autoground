//! Verdict helpers. The bars live in [`crate::confidence`] so a reader does
//! not depend on the navigator.

pub use crate::confidence::{
    confident_look, confident_skip, sure_confidence, FLOOR, RESOLVE, SUPPORT,
};

use super::judge::{DocumentVerdict, ResolveValue};

/// Entailed at [`SUPPORT`]: this file may be a witness.
pub fn is_witness(verdict: DocumentVerdict, confidence: f64) -> bool {
    verdict == DocumentVerdict::Entailed && confidence >= SUPPORT
}

/// Contradicted at [`FLOOR`]: this file blocks treating the atom as supported.
pub fn vetoes(verdict: DocumentVerdict, confidence: f64) -> bool {
    verdict == DocumentVerdict::Contradicted && confidence >= FLOOR
}

/// Contradicted at [`SUPPORT`]: this file may close the atom false.
pub fn is_refutation(verdict: DocumentVerdict, confidence: f64) -> bool {
    verdict == DocumentVerdict::Contradicted && confidence >= SUPPORT
}

/// Accept a conflict `value` only at [`RESOLVE`]; otherwise unknown.
pub fn accept_resolve(value: ResolveValue, confidence: f64) -> ResolveValue {
    match value {
        ResolveValue::Unknown => ResolveValue::Unknown,
        ResolveValue::True | ResolveValue::False if confidence >= RESOLVE => value,
        _ => ResolveValue::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_vetoes_without_closing_false() {
        assert!(!vetoes(DocumentVerdict::Contradicted, 0.49));
        assert!(vetoes(DocumentVerdict::Contradicted, FLOOR));
        assert!(!is_refutation(DocumentVerdict::Contradicted, FLOOR));
        assert!(!is_refutation(DocumentVerdict::Contradicted, 0.84));
        assert!(is_refutation(DocumentVerdict::Contradicted, SUPPORT));
    }

    #[test]
    fn support_is_required_to_witness_or_look() {
        assert!(!is_witness(DocumentVerdict::Entailed, 0.84));
        assert!(is_witness(DocumentVerdict::Entailed, SUPPORT));
        assert!(!confident_look(0.84));
        assert!(confident_look(SUPPORT));
        assert!(!confident_skip(FLOOR));
        assert!(confident_skip(SUPPORT));
    }

    #[test]
    fn resolve_matches_support_for_now() {
        assert_eq!(RESOLVE, SUPPORT);
        assert_eq!(
            accept_resolve(ResolveValue::True, 0.84),
            ResolveValue::Unknown
        );
        assert_eq!(
            accept_resolve(ResolveValue::True, RESOLVE),
            ResolveValue::True
        );
        assert_eq!(
            accept_resolve(ResolveValue::False, 0.84),
            ResolveValue::Unknown
        );
        assert_eq!(
            accept_resolve(ResolveValue::False, RESOLVE),
            ResolveValue::False
        );
        assert_eq!(
            accept_resolve(ResolveValue::Unknown, 1.0),
            ResolveValue::Unknown
        );
    }

    #[test]
    fn entailed_never_vetoes() {
        assert!(!vetoes(DocumentVerdict::Entailed, 1.0));
        assert!(!is_refutation(DocumentVerdict::Entailed, 1.0));
    }
}
