//! Cutoffs on TypeSafe `confidence`, and why these particular values.
//!
//! TypeSafe's `confidence` is not P(chosen option). It is a 0–1 collapse of
//! how peaked `probabilities` is: a winner at 0.60 with a runner-up at 0.38
//! can come back at 0.39. We threshold that number. TypeSafe's own guidance
//! is that the bar scales with the cost of acting
//! (<https://docs.typesafe.ai/confidence.md>,
//! <https://docs.typesafe.ai/patterns/confidence-routing.md>).
//!
//! The costly act here is reporting the model **supported** when the
//! codebase actively contradicts it. A run is supported only when every
//! `require` is true. `not φ` is true when `φ` is false, so closing an atom
//! false is the same kind of automatic yes as closing it true. Unsupported
//! (unknown) is the conservative bucket; contradicted (false) is the report
//! that the codebase fights the model.
//!
//! - [`FLOOR`] (0.5) is TypeSafe's "don't guess" band. Below it we do not
//!   act. At or above it, a `contradicted` document is enough to *veto* a
//!   witness without by itself closing the atom false. A weaker
//!   `contradicted` or `neutral` also does not undo confident neutrals:
//!   those already close a ground atom false, and a noisy extra look
//!   must not reopen it.
//! - [`SUPPORT`] (0.85) is TypeSafe's high-stakes automatic-action band
//!   (confidence-gated routing uses >0.85 for a transfer). An `entailed`
//!   document may become a witness, a `contradicted` document may close
//!   false, a listing skip may count as completeness, and a confident
//!   `neutral` may count as a look. Any of those can make a `require` true.
//! - [`RESOLVE`] is the same 0.85 for now. Taking a conflict Choice of
//!   `true` or `false` in the face of mixed polar files is the same class
//!   of automatic yes; a stricter split is a named constant so it can
//!   move independently later.

use super::judge::{DocumentVerdict, ResolveValue};

/// TypeSafe's floor for "the model is genuinely unsure; do not guess."
///
/// From the worked example on <https://docs.typesafe.ai/confidence.md>:
/// `if confidence < 0.5: route_to_human`. A `contradicted` at this bar
/// blocks support; it does not assert false.
pub const FLOOR: f64 = 0.5;

/// Bar at which a Choice may make a `require` true.
///
/// From TypeSafe's confidence-gated routing pattern: a high-stakes action
/// proceeds automatically only above 0.85. Witnesses, refutations,
/// confident neutrals, and generating-skip completeness all sit here
/// because each can close a `require` (directly, or via `not`).
pub const SUPPORT: f64 = 0.85;

/// Bar at which we accept a conflict Choice of `true` or `false`.
///
/// Same number as [`SUPPORT`]: mixed polar files already disagree, and
/// picking a side is the same class of automatic yes as a single-document
/// witness. Own constant so it can rise independently later.
pub const RESOLVE: f64 = SUPPORT;

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

/// Any verdict at [`SUPPORT`]: this document was a real look and may close.
pub fn confident_look(confidence: f64) -> bool {
    confidence >= SUPPORT
}

/// A listing skip at [`SUPPORT`] may count as generating-atom completeness.
pub fn confident_skip(confidence: f64) -> bool {
    confidence >= SUPPORT
}

/// Accept a conflict `value` only at [`RESOLVE`]; otherwise unknown.
pub fn accept_resolve(value: ResolveValue, confidence: f64) -> ResolveValue {
    match value {
        ResolveValue::Unknown => ResolveValue::Unknown,
        ResolveValue::True | ResolveValue::False if confidence >= RESOLVE => value,
        _ => ResolveValue::Unknown,
    }
}

/// Default confidence for local judges that already picked a side.
pub fn sure_confidence() -> f64 {
    1.0
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
