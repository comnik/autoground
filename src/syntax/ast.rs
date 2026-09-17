/// A term in an atom: a variable, a string constant, or a typed constant
/// such as `(File "ui/app.py")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Var(String),
    Const(String),
    /// `(File "ui/app.py")` — a constant annotated with a unary predicate.
    Typed {
        ty: String,
        value: String,
    },
}

/// A formula of the modeling language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formula {
    And(Vec<Formula>),
    Or(Vec<Formula>),
    Not(Box<Formula>),
    Implies(Box<Formula>, Box<Formula>),
    Exists(Vec<String>, Box<Formula>),
    Forall(Vec<String>, Box<Formula>),
    Atom { pred: String, args: Vec<Term> },
}

/// A predicate declaration: name plus a verbalization template whose `{Type}`
/// slots are the arity and signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declare {
    pub name: String,
    pub template: String,
    pub slots: Vec<String>,
}

/// A top-level form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    Require(Formula),
    Def {
        name: String,
        vars: Vec<String>,
        body: Formula,
    },
    Declare(Declare),
}

/// An unordered bag of `require`, `def`, and `declare` forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub forms: Vec<Form>,
}

impl Term {
    pub fn as_var(&self) -> Option<&str> {
        match self {
            Term::Var(v) => Some(v),
            Term::Const(_) | Term::Typed { .. } => None,
        }
    }

    /// The constant payload, if this term is ground.
    pub fn as_const(&self) -> Option<&str> {
        match self {
            Term::Const(c) | Term::Typed { value: c, .. } => Some(c),
            Term::Var(_) => None,
        }
    }
}

impl Formula {
    pub fn walk_atoms(&self, f: &mut impl FnMut(&str, &[Term])) {
        match self {
            Formula::And(xs) | Formula::Or(xs) => {
                for x in xs {
                    x.walk_atoms(f);
                }
            }
            Formula::Not(x) | Formula::Exists(_, x) | Formula::Forall(_, x) => {
                x.walk_atoms(f);
            }
            Formula::Implies(a, b) => {
                a.walk_atoms(f);
                b.walk_atoms(f);
            }
            Formula::Atom { pred, args } => f(pred, args),
        }
    }

    pub fn constants(&self, out: &mut Vec<String>) {
        self.walk_atoms(&mut |_, args| {
            for arg in args {
                if let Some(c) = arg.as_const() {
                    out.push(c.to_string());
                }
            }
        });
    }
}

impl Program {
    pub fn defs(&self) -> impl Iterator<Item = (&str, &[String], &Formula)> {
        self.forms.iter().filter_map(|form| match form {
            Form::Def { name, vars, body } => Some((name.as_str(), vars.as_slice(), body)),
            Form::Require(_) | Form::Declare(_) => None,
        })
    }

    pub fn requires(&self) -> impl Iterator<Item = &Formula> {
        self.forms.iter().filter_map(|form| match form {
            Form::Require(f) => Some(f),
            Form::Def { .. } | Form::Declare(_) => None,
        })
    }

    pub fn declares(&self) -> impl Iterator<Item = &Declare> {
        self.forms.iter().filter_map(|form| match form {
            Form::Declare(d) => Some(d),
            Form::Require(_) | Form::Def { .. } => None,
        })
    }
}
