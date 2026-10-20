//! Bounds on the work untrusted model output can cause.

/// Bounds on parsing and evaluation work.
///
/// Model output is untrusted. These bounds keep deeply nested input,
/// reference fan-out and runaway string growth from exhausting the
/// stack or memory. Anything beyond a bound is dropped and reported
/// with [`crate::Code::LimitExceeded`]; the rest of the program still
/// renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum nesting of expressions, and of statements inlined
    /// through references. Evaluation has the same bound,
    /// counting query defaults that read other queries, so the
    /// default fits a 2 MiB thread stack even in a debug build.
    pub max_depth: usize,
    /// Maximum nodes in the checked program after references are
    /// inlined.
    pub max_nodes: usize,
    /// Maximum evaluation steps in one [`crate::Document::evaluate`]
    /// call. Reading a string costs a step per 64 bytes, and
    /// checking a library function's arguments a step per value.
    /// Query requests get a budget of their own.
    pub max_steps: usize,
    /// Maximum bytes of strings built by `+` and conversions in one
    /// evaluation.
    pub max_string_bytes: usize,
}

impl Limits {
    /// The defaults: depth 96, 100 000 nodes, 1 000 000 steps, 8 MiB
    /// of built strings.
    pub const DEFAULT: Self = Self {
        max_depth: 96,
        max_nodes: 100_000,
        max_steps: 1_000_000,
        max_string_bytes: 8 << 20,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}
