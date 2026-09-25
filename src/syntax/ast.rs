/// A sense: a named reading whose `{holes}` are filled left to right.
///
/// Hole labels disambiguate the reading. They are not types, and two holes
/// may share a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sense {
    pub name: String,
    pub template: String,
    pub holes: Vec<String>,
}

/// A relation at a use site: a declared sense, or a reading written inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    Sense(String),
    Inline {
        template: String,
        holes: Vec<String>,
    },
}

/// A ground claim. `and`, `or`, and `not` are left for the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formula {
    And(Vec<Formula>),
    Or(Vec<Formula>),
    Not(Box<Formula>),
    /// A reading applied to names. Saturation fills every hole.
    Apply {
        reading: Reading,
        args: Vec<String>,
    },
}

/// What a saturated formula says: boolean structure over sentences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposition {
    And(Vec<Proposition>),
    Or(Vec<Proposition>),
    Not(Box<Proposition>),
    Sentence(String),
}

/// A top-level form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    Name(String),
    Sense(Sense),
    Claim(Formula),
}

/// Byte range of a formula, with one child per `and` / `or` / `not` operand.
///
/// An applied sentence has no children. Positions are in one source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseSpan {
    pub span: std::ops::Range<usize>,
    pub children: Vec<ClauseSpan>,
}

/// Names, senses, and claims, in source order.
#[derive(Debug, Clone)]
pub struct Program {
    pub forms: Vec<Form>,
    /// Byte range of each form in the source, parallel to [`Self::forms`].
    pub spans: Vec<std::ops::Range<usize>>,
    /// Clause tree of each claim, in claim order.
    pub clauses: Vec<ClauseSpan>,
}

impl PartialEq for Program {
    fn eq(&self, other: &Self) -> bool {
        // Spans are positions in one source text, not part of the model.
        self.forms == other.forms
    }
}

impl Eq for Program {}

impl Program {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.forms.iter().filter_map(|form| match form {
            Form::Name(name) => Some(name.as_str()),
            Form::Sense(_) | Form::Claim(_) => None,
        })
    }

    pub fn senses(&self) -> impl Iterator<Item = &Sense> {
        self.forms.iter().filter_map(|form| match form {
            Form::Sense(sense) => Some(sense),
            Form::Name(_) | Form::Claim(_) => None,
        })
    }

    pub fn claims(&self) -> impl Iterator<Item = &Formula> {
        self.forms.iter().filter_map(|form| match form {
            Form::Claim(formula) => Some(formula),
            Form::Name(_) | Form::Sense(_) => None,
        })
    }

    /// Byte range of each claim, in claim order.
    pub fn claim_spans(&self) -> Vec<std::ops::Range<usize>> {
        self.forms
            .iter()
            .zip(self.spans.iter())
            .filter_map(|(form, span)| match form {
                Form::Claim(_) => Some(span.clone()),
                Form::Name(_) | Form::Sense(_) => None,
            })
            .collect()
    }
}
