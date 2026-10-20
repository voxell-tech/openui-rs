//! Batch and streaming parsing (spec sections 1 and 4).
//!
//! A batch parse is a stream of one chunk that then ends, so the two
//! cannot disagree (spec section 4, "streaming equals parsing").

use std::sync::Arc;

use crate::diagnostic::{Code, Diagnostic, Source};
use crate::document::Document;
use crate::lexer::{Tok, lex};
use crate::library::Library;
use crate::limits::Limits;
use crate::lower::{self, BuildOptions, Statement};
use crate::source;
use crate::syntax::{self, RawStatement};

/// Parses a complete response with the default [`Limits`].
///
/// The text may be a bare program or prose with fenced program
/// blocks.
pub fn parse(library: &Library, text: &str) -> Document {
    parse_with_limits(library, text, Limits::DEFAULT)
}

/// Parses a complete response.
pub fn parse_with_limits(
    library: &Library,
    text: &str,
    limits: Limits,
) -> Document {
    let mut parser = StreamParser::with_limits(library, limits);
    parser.raw.push_str(text);
    parser.update(true)
}

/// Parses a response as it streams in (spec section 4).
///
/// Each [`push`](Self::push) returns a document for the text so far.
/// Completed statements are parsed once and kept; the statement still
/// being written is closed implicitly and parsed again on every
/// chunk. [`finish`](Self::finish) ends the stream, and its document
/// equals [`parse`] of the whole text. Pushing after that starts a
/// new stream.
///
/// ```
/// use openui_lang::{
///     ComponentDef, Inputs, Library, Param, StreamParser, Type,
/// };
///
/// let library = Library::builder()
///     .component(
///         ComponentDef::new("Text", "Text")
///             .param(Param::required("text", Type::String)),
///     )
///     .root("Text")
///     .build()
///     .unwrap();
/// let mut parser = StreamParser::new(&library);
/// parser.push("root = Text(\"Hel");
/// let document = parser.finish();
/// let evaluation = document.evaluate(&Inputs::new());
/// assert_eq!(
///     evaluation.root().unwrap().prop("text").unwrap().as_str(),
///     Some("Hel")
/// );
/// ```
#[derive(Debug)]
pub struct StreamParser {
    library: Library,
    limits: Limits,
    raw: String,
    /// The prepared text as of the last update.
    prepared: String,
    /// Statements whose text is complete, parsed once, in order.
    completed: Vec<Statement>,
    /// Diagnostics from parsing the completed statements.
    completed_diagnostics: Vec<Diagnostic>,
    /// Where the undecided rest of `prepared` starts.
    resume_at: usize,
    /// How much of `prepared` the completed statements depend on,
    /// including the look-ahead token. If this prefix changes,
    /// they are found again (spec section 4, rule 4).
    decided_through: usize,
    ended: bool,
}

impl StreamParser {
    /// A parser with the default [`Limits`].
    pub fn new(library: &Library) -> Self {
        Self::with_limits(library, Limits::DEFAULT)
    }

    /// A parser with custom limits.
    pub fn with_limits(library: &Library, limits: Limits) -> Self {
        Self {
            library: library.clone(),
            limits,
            raw: String::new(),
            prepared: String::new(),
            completed: Vec::new(),
            completed_diagnostics: Vec::new(),
            resume_at: 0,
            decided_through: 0,
            ended: false,
        }
    }

    /// Adds a chunk and returns the document for the text so far.
    pub fn push(&mut self, chunk: &str) -> Document {
        if self.ended {
            *self = Self::with_limits(&self.library, self.limits);
        }
        self.raw.push_str(chunk);
        self.update(false)
    }

    /// Ends the stream: the pending statement becomes final, and
    /// entry and enum checks run. The result equals [`parse`] of
    /// [`text`](Self::text).
    pub fn finish(&mut self) -> Document {
        self.ended = true;
        self.update(true)
    }

    /// The text received so far.
    pub fn text(&self) -> &str {
        &self.raw
    }

    fn update(&mut self, complete: bool) -> Document {
        let prepared = source::prepare(&self.raw);
        // The cache stays valid only if the text it read is
        // unchanged, up to where scanning resumes. Then
        // `resume_at` is a character boundary too.
        let kept = self.decided_through.max(self.resume_at);
        if !prepared
            .as_bytes()
            .starts_with(&self.prepared.as_bytes()[..kept])
        {
            self.completed.clear();
            self.completed_diagnostics.clear();
            self.resume_at = 0;
            self.decided_through = 0;
        }
        self.prepared = prepared;

        let base = self.resume_at;
        let tail = &self.prepared[base..];
        let lexed = lex(tail);
        let scan = syntax::scan(&lexed.tokens);
        for raw in &scan.completed {
            let parsed = parse_statement(
                raw,
                &self.limits,
                &mut self.completed_diagnostics,
            );
            self.completed.extend(parsed);
        }
        self.resume_at = base + scan.resume_at;
        self.decided_through =
            self.decided_through.max(base + scan.decided_through);

        let mut diagnostics = self.completed_diagnostics.clone();
        let mut pending = None;
        let mut truncated = lexed.open_string;
        if let Some(raw) = &scan.pending {
            let needed_closing = raw.unclosed || lexed.open_string;
            truncated |= raw.unclosed;
            // A pending statement that needed closing does not
            // replace a completed one (rule 3).
            let shadowed = !complete
                && needed_closing
                && self
                    .completed
                    .iter()
                    .any(|s| &*s.name == raw.name);
            if !shadowed {
                pending = parse_statement(
                    raw,
                    &self.limits,
                    &mut diagnostics,
                );
            }
        }
        let growing_state = match &pending {
            Some(statement) if !complete => growing_state(
                &self.raw,
                &self.prepared[self.resume_at..],
            )
            .map(|name| (name, statement.name.clone())),
            _ => None,
        };
        lower::build(
            &self.library,
            self.completed.iter().chain(pending.as_ref()),
            BuildOptions {
                limits: self.limits,
                complete,
                truncated,
                has_text: !self.prepared.is_empty(),
                growing_state,
                diagnostics,
            },
        )
    }
}

/// Parses a statement's expression. One nested past the depth limit
/// is dropped and reported. `name =` with nothing after it is not a
/// statement (spec section 1.4), so it never replaces an earlier
/// definition.
fn parse_statement(
    raw: &RawStatement<'_, '_>,
    limits: &Limits,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Statement> {
    if raw.tokens.iter().all(|t| t.tok == Tok::Newline) {
        return None;
    }
    match syntax::parse_expression(raw.tokens, limits.max_depth) {
        Ok(expr) => Some(Statement::new(raw.name, expr)),
        Err(syntax::TooDeep) => {
            let message = format!(
                "`{}` nests deeper than {} levels; the statement is \
                 dropped",
                raw.name, limits.max_depth
            );
            let mut diagnostic = Diagnostic::new(
                Source::Parser,
                Code::LimitExceeded,
                message,
            );
            diagnostic.statement = Some(raw.name.to_owned());
            diagnostics.push(diagnostic);
            None
        }
    }
}

/// A `$name` that ends the received text may still grow (`$d` of
/// `$dishes`), so the pending statement does not declare it, unless
/// the same name already appears in full earlier in that statement.
fn growing_state(raw: &str, pending: &str) -> Option<Arc<str>> {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = raw.as_bytes();
    let word_start = bytes
        .iter()
        .rposition(|&b| !is_word(b))
        .map_or(0, |i| i + 1);
    if word_start == bytes.len()
        || word_start == 0
        || bytes[word_start - 1] != b'$'
    {
        return None;
    }
    let name = &raw[word_start - 1..];
    let earlier = pending.strip_suffix(name)?;
    let appears_in_full =
        earlier.match_indices(name).any(|(i, _)| {
            earlier
                .as_bytes()
                .get(i + name.len())
                .is_none_or(|&b| !is_word(b))
        });
    (!appears_in_full).then(|| Arc::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_state_name_may_still_grow() {
        assert_eq!(
            growing_state("x = $d", "x = $d").as_deref(),
            Some("$d")
        );
        assert_eq!(growing_state("x = [$d, $d", "x = [$d, $d"), None);
        assert_eq!(
            growing_state("x = [$dd, $d", "x = [$dd, $d").as_deref(),
            Some("$d")
        );
        assert_eq!(growing_state("x = $d ", "x = $d"), None);
        assert_eq!(growing_state("x = $", "x = $"), None);
    }
}
