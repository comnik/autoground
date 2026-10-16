//! Cutoffs on TypeSafe `confidence`, and why these particular values.
//!
//! TypeSafe's `confidence` is not P(chosen option). It is a 0–1 collapse of
//! how peaked `probabilities` is: a winner at 0.60 with a runner-up at 0.38
//! can come back at 0.39. We threshold that number. TypeSafe's own guidance
//! is that the bar scales with the cost of acting
//! (<https://docs.typesafe.ai/confidence.md>,
//! <https://docs.typesafe.ai/patterns/confidence-routing.md>).
//!
//! The costly act here is reporting a proposition true, or false, when the
//! read was not actually decisive. Unsupported (unknown) is the conservative
//! bucket; a refutation is the report that a database fights the proposition.
//!
//! - [`FLOOR`] (0.5) is TypeSafe's "don't guess" band. Below it we do not
//!   act. At or above it, a `contradicted` document is enough to *veto* a
//!   witness without by itself closing the proposition false.
//! - [`SUPPORT`] (0.85) is TypeSafe's high-stakes automatic-action band
//!   (confidence-gated routing uses >0.85 for a transfer). An `entailed`
//!   document may become a witness and a `contradicted` document may close
//!   false.
//! - [`RESOLVE`] is the same 0.85. Taking a conflict Choice of `true` or
//!   `false` in the face of mixed polar files is the same class of automatic
//!   yes.

/// TypeSafe's floor for "the model is genuinely unsure; do not guess."
///
/// From the worked example on <https://docs.typesafe.ai/confidence.md>:
/// `if confidence < 0.5: route_to_human`. A `contradicted` at this bar
/// blocks support; it does not assert false.
pub const FLOOR: f64 = 0.5;

/// Bar at which a Choice may settle a proposition true or false.
///
/// From TypeSafe's confidence-gated routing pattern: a high-stakes action
/// proceeds automatically only above 0.85.
pub const SUPPORT: f64 = 0.85;

/// Bar at which we accept a conflict Choice of `true` or `false`.
///
/// Same number as [`SUPPORT`]: mixed polar files already disagree, and
/// picking a side is the same class of automatic yes as a single-document
/// witness.
pub const RESOLVE: f64 = SUPPORT;

/// Any verdict at [`SUPPORT`]: this document was a real look.
pub fn confident_look(confidence: f64) -> bool {
    confidence >= SUPPORT
}

/// A listing skip at [`SUPPORT`] may count as completeness.
pub fn confident_skip(confidence: f64) -> bool {
    confidence >= SUPPORT
}

/// Default confidence for local judges that already picked a side.
pub fn sure_confidence() -> f64 {
    1.0
}
