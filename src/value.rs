use serde::{Deserialize, Serialize};

/// Kleene three-valued truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Value {
    True,
    False,
    Unknown,
}

impl Value {
    pub fn and(self, other: Value) -> Value {
        match (self, other) {
            (Value::False, _) | (_, Value::False) => Value::False,
            (Value::Unknown, _) | (_, Value::Unknown) => Value::Unknown,
            (Value::True, Value::True) => Value::True,
        }
    }

    pub fn or(self, other: Value) -> Value {
        match (self, other) {
            (Value::True, _) | (_, Value::True) => Value::True,
            (Value::Unknown, _) | (_, Value::Unknown) => Value::Unknown,
            (Value::False, Value::False) => Value::False,
        }
    }

    pub fn not(self) -> Value {
        match self {
            Value::True => Value::False,
            Value::False => Value::True,
            Value::Unknown => Value::Unknown,
        }
    }

    pub fn implies(self, other: Value) -> Value {
        self.not().or(other)
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::True => write!(f, "true"),
            Value::False => write!(f, "false"),
            Value::Unknown => write!(f, "unknown"),
        }
    }
}
