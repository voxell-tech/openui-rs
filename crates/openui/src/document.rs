//! A checked program, and what evaluating it gives.

use std::collections::HashMap;
use std::sync::Arc;

use crate::diagnostic::{Code, Diagnostic, Source};
use crate::eval::Evaluator;
use crate::library::Library;
use crate::limits::Limits;
use crate::lower::Node;
use crate::value::{Deferred, Element, Value};

/// What a statement declares (spec section 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StatementKind {
    /// Anything else: a value, usually a component.
    Value,
    /// `$name = default`.
    State,
    /// `name = Query(tool, args, defaults, refresh?)`.
    Query,
    /// `name = Mutation(tool, args)`.
    Mutation,
}

/// A statement of the program, in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementInfo {
    pub(crate) name: String,
    pub(crate) kind: StatementKind,
}

impl StatementInfo {
    /// The name, including `$` for state.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The statement kind.
    pub fn kind(&self) -> StatementKind {
        self.kind
    }
}

/// The statement that renders (spec section 2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub(crate) statement: String,
    pub(crate) fallback: bool,
}

impl Entry {
    /// The entry statement's name.
    pub fn statement(&self) -> &str {
        &self.statement
    }

    /// True when there is no `root` statement and this is the first
    /// statement that calls the library's root component.
    pub fn is_fallback(&self) -> bool {
        self.fallback
    }
}

/// The checked entry.
#[derive(Clone, Debug)]
pub(crate) enum Root {
    None,
    /// The entry is a component.
    Element(Node),
    /// The entry is an expression that may give a component, such as
    /// `$t ? A : B`.
    Expression(Node),
}

/// A checked query or mutation statement.
#[derive(Clone, Debug)]
pub(crate) struct QueryNode {
    pub(crate) name: Arc<str>,
    pub(crate) tool: Option<Node>,
    pub(crate) args: Option<Node>,
    pub(crate) defaults: Option<Node>,
    pub(crate) refresh: Option<Node>,
}

/// A parsed and checked OpenUI Lang program.
///
/// A document is the result of [`crate::parse`] or of each
/// [`crate::StreamParser`] update. It is immutable;
/// [`Document::evaluate`] turns it into a component tree for a given
/// state.
#[derive(Clone, Debug)]
pub struct Document {
    pub(crate) library: Library,
    pub(crate) limits: Limits,
    pub(crate) statements: Vec<StatementInfo>,
    pub(crate) entry: Option<Entry>,
    pub(crate) root: Root,
    /// State defaults: declarations in order, then undeclared names
    /// as null.
    pub(crate) state: Vec<(Arc<str>, Value)>,
    pub(crate) state_index: HashMap<Arc<str>, usize>,
    pub(crate) queries: Vec<QueryNode>,
    pub(crate) query_index: HashMap<Arc<str>, usize>,
    pub(crate) mutations: Vec<QueryNode>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) unresolved: Vec<String>,
    pub(crate) orphans: Vec<String>,
    pub(crate) truncated: bool,
    pub(crate) complete: bool,
}

impl Document {
    pub(crate) fn set_state_defaults(
        &mut self,
        defaults: Vec<(Arc<str>, Value)>,
    ) {
        self.state_index = defaults
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i))
            .collect();
        self.state = defaults;
    }

    pub(crate) fn state_default(&self, name: &str) -> Option<&Value> {
        self.state_index.get(name).map(|&i| &self.state[i].1)
    }

    pub(crate) fn query(&self, name: &str) -> Option<&QueryNode> {
        self.query_index.get(name).map(|&i| &self.queries[i])
    }

    /// The library the program was checked against.
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// The statements in source order. A repeated name appears once,
    /// where it first appeared, with its last definition.
    pub fn statements(&self) -> &[StatementInfo] {
        &self.statements
    }

    /// The entry, if there is one.
    pub fn entry(&self) -> Option<&Entry> {
        self.entry.as_ref()
    }

    /// The parser's diagnostics. While streaming they are
    /// provisional: spec section 8.3 has clients report them only
    /// once the stream ends.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Names that are referenced but not defined, or that close a
    /// cycle.
    pub fn unresolved(&self) -> &[String] {
        &self.unresolved
    }

    /// Value statements the entry cannot reach. Empty when there is
    /// no entry.
    pub fn orphans(&self) -> &[String] {
        &self.orphans
    }

    /// True when the text ended inside an unclosed string or bracket.
    pub fn is_incomplete(&self) -> bool {
        self.truncated
    }

    /// True once the stream has ended. Queries run only then (spec
    /// section 4).
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// The state defaults: declared ones in order, then undeclared
    /// `$names` as null.
    pub fn state_defaults(
        &self,
    ) -> impl Iterator<Item = (&str, &Value)> {
        self.state.iter().map(|(name, value)| (&**name, value))
    }

    /// Evaluates the program with the host's state and data.
    ///
    /// Evaluation is pure: actions become [`crate::ActionPlan`]s and
    /// queries become [`ToolRequest`]s, and the host decides what
    /// to run.
    pub fn evaluate(&self, inputs: &Inputs) -> Evaluation {
        let mut evaluator = Evaluator::new(self, Some(inputs));
        let entry_name: Option<Arc<str>> = self
            .entry
            .as_ref()
            .map(|e| Arc::from(e.statement.as_str()));
        if let Some(name) = &entry_name {
            evaluator.set_statement(name);
        }
        let mut no_root = false;
        let root = match &self.root {
            Root::None => None,
            Root::Element(node) | Root::Expression(node) => {
                match evaluator.eval(node) {
                    Value::Element(element) => Some(element),
                    _ => {
                        no_root =
                            matches!(self.root, Root::Expression(_));
                        None
                    }
                }
            }
        };
        let mut diagnostics = evaluator.into_diagnostics();
        let queries = if self.complete {
            // Requests get their own budget, so a costly page cannot
            // cut them short.
            let mut requests = Evaluator::new(self, Some(inputs));
            let queries = self
                .queries
                .iter()
                .filter_map(|q| requests.request(q))
                .collect();
            diagnostics.extend(requests.into_diagnostics());
            queries
        } else {
            Vec::new()
        };
        if no_root && self.complete {
            let mut diagnostic = Diagnostic::new(
                Source::Runtime,
                Code::NoRoot,
                "root does not evaluate to a component",
            );
            diagnostic.statement = entry_name.map(|n| n.to_string());
            diagnostics.push(diagnostic);
        }
        Evaluation {
            root,
            diagnostics,
            queries,
        }
    }

    /// Evaluates a deferred expression, such as the value of an
    /// [`crate::ActionStep::Set`], with the state at the time the
    /// action runs. A deferred expression from a document with
    /// another library is null.
    pub fn resolve(
        &self,
        deferred: &Deferred,
        inputs: &Inputs,
    ) -> Value {
        if !deferred.library.same(&self.library) {
            return Value::Null;
        }
        let mut evaluator = Evaluator::new(self, Some(inputs));
        evaluator.set_scope(&deferred.scope);
        evaluator.eval(&deferred.node)
    }

    /// The tool call a query or mutation statement makes with the
    /// current state, for example when an
    /// [`crate::ActionStep::Run`] runs it. `None`
    /// when `statement` is not a query or mutation, or when
    /// evaluating its arguments goes past the [`crate::Limits`].
    pub fn request(
        &self,
        statement: &str,
        inputs: &Inputs,
    ) -> Option<ToolRequest> {
        let node = self
            .queries
            .iter()
            .chain(&self.mutations)
            .find(|q| &*q.name == statement)?;
        Evaluator::new(self, Some(inputs)).request(node)
    }
}

/// What the host supplies to an evaluation: the state store and the
/// latest query and mutation results.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub(crate) state: HashMap<String, Value>,
    pub(crate) queries: HashMap<String, Value>,
    pub(crate) mutations: HashMap<String, Value>,
}

impl Inputs {
    /// No state and no results: state reads its defaults, queries
    /// their defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a state variable. The name includes `$`.
    pub fn set_state(
        &mut self,
        name: impl Into<String>,
        value: Value,
    ) -> &mut Self {
        self.state.insert(name.into(), value);
        self
    }

    /// The state variable's value, if the host set one.
    pub fn state(&self, name: &str) -> Option<&Value> {
        self.state.get(name)
    }

    /// Sets the latest result of a query statement.
    pub fn set_query_result(
        &mut self,
        statement: impl Into<String>,
        value: Value,
    ) -> &mut Self {
        self.queries.insert(statement.into(), value);
        self
    }

    /// Sets what a mutation statement reads as, normally `{ status,
    /// data, error }`. Unset mutations read `{ status: "idle",
    /// data: null, error: null }`.
    pub fn set_mutation_result(
        &mut self,
        statement: impl Into<String>,
        value: Value,
    ) -> &mut Self {
        self.mutations.insert(statement.into(), value);
        self
    }
}

/// The result of [`Document::evaluate`].
#[derive(Clone, Debug)]
pub struct Evaluation {
    root: Option<Arc<Element>>,
    diagnostics: Vec<Diagnostic>,
    queries: Vec<ToolRequest>,
}

impl Evaluation {
    /// The rendered tree, or `None` when nothing renders.
    pub fn root(&self) -> Option<&Element> {
        self.root.as_deref()
    }

    /// Runtime diagnostics, such as a library function that failed.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The tool calls the program's queries make with this state, in
    /// statement order. Empty until the stream ends. A query whose
    /// arguments go past the [`crate::Limits`] is left out and
    /// reported.
    pub fn queries(&self) -> &[ToolRequest] {
        &self.queries
    }
}

/// A tool call for a query or mutation. The runtime never makes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRequest {
    pub(crate) statement: String,
    pub(crate) tool: String,
    pub(crate) args: Value,
    pub(crate) refresh_seconds: Option<f64>,
}

impl ToolRequest {
    /// The query or mutation statement.
    pub fn statement(&self) -> &str {
        &self.statement
    }

    /// The tool name, case-sensitive (spec section 6.4).
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The evaluated arguments, an empty object when there are none.
    pub fn args(&self) -> &Value {
        &self.args
    }

    /// How often a query asks to run again, in seconds.
    pub fn refresh_seconds(&self) -> Option<f64> {
        self.refresh_seconds
    }
}
