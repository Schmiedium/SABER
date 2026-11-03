//! Importers (IB-13): external data turned into scenario components, converted to SI at import,
//! so a campaign never reads an external file at run time.
//!
//! - [`fragment_drag`]: a fragment drag table (CSV) → `sim_core::FragmentDrag` (IB-16).

pub mod fragment_drag;

/// Why a file could not be imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportError {
    /// The 1-based line it was found on, when it has one.
    pub line: Option<usize>,
    /// What is wrong, for people.
    pub message: String,
}

impl ImportError {
    pub(crate) fn at(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line),
            message: message.into(),
        }
    }

    pub(crate) fn file(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ImportError {}
