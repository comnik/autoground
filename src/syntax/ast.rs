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

/// An argument. A name is a declared symbol. A literal is a spelling written
/// where it is used: the text is the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Name(Ident),
    Literal(String),
}

/// A ground claim. `and`, `or`, `not`, and `detail` are left for the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formula {
    And(Vec<Formula>),
    Or(Vec<Formula>),
    Not(Box<Formula>),
    /// A reading applied to arguments. Saturation fills every hole.
    Apply {
        reading: Reading,
        args: Vec<Arg>,
    },
    /// A sentence established by the claims of a lect local to this detail.
    ///
    /// Names and senses in `body` are visible to the head and to those claims,
    /// and nowhere else.
    Detail {
        head: Box<Formula>,
        body: Vec<Form>,
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

/// What a saturated formula says: boolean structure over sentences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposition {
    And(Vec<Proposition>),
    Or(Vec<Proposition>),
    Not(Box<Proposition>),
    Sentence(String),
    /// `head` is the sentence being detailed. `body` is the lect's claims, in
    /// order, and the detail holds when every one of them holds.
    Detail {
        head: String,
        body: Vec<Proposition>,
    },
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
