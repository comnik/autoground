//! Trust bar on TypeSafe `confidence`.
//!
//! TypeSafe's `confidence` is not P(chosen option). It is a 0–1 collapse of
//! how peaked `probabilities` is: a winner at 0.60 with a runner-up at 0.38
//! can come back at 0.39. We threshold that number. TypeSafe's own guidance
//! is that the bar scales with the cost of acting
//! (<https://docs.typesafe.ai/confidence.md>,
//! <https://docs.typesafe.ai/patterns/confidence-routing.md>).
//!
//! [`SUPPORT`] (0.85) is TypeSafe's high-stakes automatic-action band
//! (confidence-gated routing uses >0.85 for a transfer). Check treats an
//! `entailed` or `contradicted` Choice below this bar as unsettled. Sweep
//! uses the same bar for a file match and for a filter Noul.

/// Bar at which a Choice may settle a proposition true or false.
///
/// From TypeSafe's confidence-gated routing pattern: a high-stakes action
/// proceeds automatically only above 0.85.
pub const SUPPORT: f64 = 0.85;
