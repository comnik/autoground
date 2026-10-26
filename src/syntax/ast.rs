/// A sense: a named reading whose `{holes}` are filled left to right.
///
/// Hole labels disambiguate the reading. They are not types, and two holes
/// may share a label. The name is a Clojure identifier, such as `lemma-1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sense {
    pub name: String,
    pub template: String,
    pub holes: Vec<String>,
}

/// A name at a use site. `product/Frontend` is a qualifier plus a spelling.
///
/// The qualifier is an `:as` alias or the full name of a required namespace.
/// The spelling is what a sentence says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub qualifier: Option<String>,
    pub name: String,
}

impl Ident {
    pub fn display(&self) -> String {
        match &self.qualifier {
            Some(qualifier) => format!("{qualifier}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// A relation at a use site: a declared sense, or a reading written inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    Sense(Ident),
    Inline {
        template: String,
        holes: Vec<String>,
    },
}

/// Where a use starts. Equality ignores it: a position locates an error and
/// is not part of the model.
#[derive(Debug, Clone, Copy)]
pub struct At {
    pub line: usize,
    pub col: usize,
}

impl PartialEq for At {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for At {}

/// A filled sentence.
///
/// `text` is what a report, a citation, and an alignment use. `grounded` is
/// what the judge is asked. A term puts its name in `text` and its reading
/// in `grounded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    pub text: String,
    pub grounded: String,
}

impl From<&str> for Sentence {
    fn from(text: &str) -> Self {
        Self {
            grounded: text.to_string(),
            text: text.to_string(),
        }
    }
}

impl From<String> for Sentence {
    fn from(text: String) -> Self {
        Self {
            grounded: text.clone(),
            text,
        }
    }
}

/// An argument. A name is a declared symbol. A literal is a spelling written
/// where it is used. A term is a nullary sense.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Name(Ident),
    Literal(String),
    Term { ident: Ident, at: At },
}

/// A ground formula. A `claim` offers a sentence with support. A `witness` is
/// only legal as that support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formula {
    And(Vec<Formula>),
    Or(Vec<Formula>),
    Not(Box<Formula>),
    /// A reading applied to arguments. Saturation fills every hole.
    Apply {
        reading: Reading,
        args: Vec<Arg>,
        at: At,
    },
    /// `sentence` established by `support`.
    Claim {
        sentence: Box<Formula>,
        support: Box<Formula>,
    },
    /// One or more repository paths, judged together. The sentence is the
    /// enclosing claim.
    Witness {
        paths: Vec<String>,
    },
}

/// What `(:require ...)` pulls in from one namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refer {
    /// Link the namespace and import nothing.
    Nothing,
    /// Import every name and sense it declares.
    All,
    /// Import these names and senses.
    Only(Vec<String>),
}

/// One libspec: `[namespace]`, optionally `:as` and `:refer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Require {
    pub namespace: String,
    pub alias: Option<String>,
    pub refer: Refer,
}

/// The namespace a file declares, and the namespaces it requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ns {
    pub name: String,
    pub requires: Vec<Require>,
}

/// How a claim is supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    /// The sentence judged against the bodies at `paths`, as one state.
    Witness { paths: Vec<String> },
    /// Another sentence. An `and` is several premises.
    Sentence(Box<Proposition>),
}

/// What a saturated formula says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposition {
    And(Vec<Proposition>),
    Or(Vec<Proposition>),
    Not(Box<Proposition>),
    Sentence(Sentence),
    /// `sentence` established by `support`.
    Claim {
        sentence: Sentence,
        support: Support,
    },
    /// A sentence taken as true, with no evidence.
    Assume(Sentence),
    /// A detail. Names declared in the body stay there. `body` is the claims,
    /// assumptions, and nested details, in order.
    Scope {
        body: Vec<Proposition>,
    },
}

/// A top-level form, or a form inside a detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    Name(String),
    Sense(Sense),
    Assume(Formula),
    Detail(Vec<Form>),
    Formula(Formula),
}

/// Byte range of a formula, with one child per `and` / `or` / `not` operand.
///
/// An applied sentence has no children. Positions are in one source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseSpan {
    pub span: std::ops::Range<usize>,
    pub children: Vec<ClauseSpan>,
}

/// A namespace, then names, senses, and claims, in source order.
#[derive(Debug, Clone)]
pub struct Program {
    pub namespace: Option<Ns>,
    pub forms: Vec<Form>,
    /// Byte range of each form in the source, parallel to [`Self::forms`].
    /// The `ns` form has no span here.
    pub spans: Vec<std::ops::Range<usize>>,
    /// Clause tree of each claim, in claim order.
    pub clauses: Vec<ClauseSpan>,
}

impl PartialEq for Program {
    fn eq(&self, other: &Self) -> bool {
        // Spans are positions in one source text, not part of the model.
        self.namespace == other.namespace && self.forms == other.forms
    }
}

impl Eq for Program {}

impl Program {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.forms.iter().filter_map(|form| match form {
            Form::Name(name) => Some(name.as_str()),
            Form::Sense(_) | Form::Assume(_) | Form::Detail(_) | Form::Formula(_) => None,
        })
    }

    pub fn senses(&self) -> impl Iterator<Item = &Sense> {
        self.forms.iter().filter_map(|form| match form {
            Form::Sense(sense) => Some(sense),
            Form::Name(_) | Form::Assume(_) | Form::Detail(_) | Form::Formula(_) => None,
        })
    }

    pub fn claims(&self) -> impl Iterator<Item = &Formula> {
        self.forms.iter().filter_map(|form| match form {
            Form::Formula(formula) => Some(formula),
            Form::Name(_) | Form::Sense(_) | Form::Assume(_) | Form::Detail(_) => None,
        })
    }

    /// Byte range of each claim, in claim order.
    pub fn claim_spans(&self) -> Vec<std::ops::Range<usize>> {
        self.forms
            .iter()
            .zip(self.spans.iter())
            .filter_map(|(form, span)| match form {
                Form::Formula(_) | Form::Assume(_) | Form::Detail(_) => Some(span.clone()),
                Form::Name(_) | Form::Sense(_) => None,
            })
            .collect()
    }
}
