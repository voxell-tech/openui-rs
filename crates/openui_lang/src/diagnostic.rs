//! Structured errors in the shape of spec section 8.3.

use std::fmt;

/// Where a diagnostic came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Found while parsing and checking the program.
    Parser,
    /// Found while evaluating it.
    Runtime,
}

impl Source {
    /// The wire name: `parser` or `runtime`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parser => "parser",
            Self::Runtime => "runtime",
        }
    }
}

/// A diagnostic code. The spec's codes keep their wire names (section
/// 8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Code {
    /// A component that is not in the library. It is dropped.
    UnknownComponent,
    /// A required prop or argument is absent.
    MissingRequired,
    /// A required prop or argument is null.
    NullRequired,
    /// A value does not match its type or enum.
    TypeMismatch,
    /// More arguments than parameters. The extras are dropped.
    ExcessArgs,
    /// `Query` or `Mutation` used as a value instead of a statement.
    InlineReserved,
    /// An `@Name` call that is not a built-in, an action, or a
    /// library function.
    UnknownFunction,
    /// No usable entry statement.
    NoRoot,
    /// The response has text but no statements.
    ParseFailed,
    /// Evaluating an expression failed, for example a library
    /// function returned an error.
    RuntimeError,
    /// An extension of this crate: the program exceeded a
    /// [`crate::Limits`] bound. The part beyond the bound is
    /// dropped.
    LimitExceeded,
}

impl Code {
    /// The wire name, for example `missing-required`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownComponent => "unknown-component",
            Self::MissingRequired => "missing-required",
            Self::NullRequired => "null-required",
            Self::TypeMismatch => "type-mismatch",
            Self::ExcessArgs => "excess-args",
            Self::InlineReserved => "inline-reserved",
            Self::UnknownFunction => "unknown-function",
            Self::NoRoot => "no-root",
            Self::ParseFailed => "parse-failed",
            Self::RuntimeError => "runtime-error",
            Self::LimitExceeded => "limit-exceeded",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How bad a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Something was dropped, or nothing renders when the code is
    /// fatal.
    Error,
    /// The page still renders, as with the non-fatal `no-root`
    /// fallback.
    Warning,
}

/// One reported problem. `message` and `hint` are written for a model
/// to read.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Diagnostic {
    /// Parser or runtime.
    pub source: Source,
    /// The machine-readable code.
    pub code: Code,
    /// The statement to patch, when known.
    pub statement: Option<String>,
    /// The component, function or action involved, when known.
    pub component: Option<String>,
    /// A JSON pointer to the prop, for example `/value`.
    pub path: Option<String>,
    /// What went wrong.
    pub message: String,
    /// A short signature or the available names.
    pub hint: Option<String>,
    /// Error or warning.
    pub severity: Severity,
}

impl Diagnostic {
    pub(crate) fn new(
        source: Source,
        code: Code,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source,
            code,
            statement: None,
            component: None,
            path: None,
            message: message.into(),
            hint: None,
            severity: Severity::Error,
        }
    }

    /// True when nothing renders because of this diagnostic.
    pub fn is_fatal(&self) -> bool {
        matches!(self.code, Code::NoRoot | Code::ParseFailed)
            && self.severity == Severity::Error
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)?;
        if let Some(statement) = &self.statement {
            write!(f, " in `{statement}`")?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}
