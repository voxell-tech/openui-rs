//! Checking a parsed program against the library (spec sections 2 and
//! 8).
//!
//! Lowering resolves references by inlining each use separately, maps
//! positional arguments to named props, validates literal values, and
//! keeps everything that needs state or a query result as an
//! expression [`Node`] for evaluation. Each mistake removes the
//! smallest unit and is reported.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::builtins::Builtin;
use crate::diagnostic::{Code, Diagnostic, Severity, Source};
use crate::document::{
    Document, Entry, QueryNode, Root, StatementInfo, StatementKind,
};
use crate::eval::Evaluator;
use crate::library::{
    Entry as LibraryEntry, Library, Param, Type, hint_signature,
};
use crate::limits::Limits;
use crate::syntax::{BinaryOp, Expr, UnaryOp, is_builtin_call};
use crate::value::{RunKind, Value};

/// A checked expression, ready to evaluate.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Lit(Value),
    Array(Vec<Node>),
    Object(Vec<(Arc<str>, Node)>),
    Element(Box<ElementNode>),
    State(Arc<str>),
    Query(Arc<str>),
    Mutation(Arc<str>),
    LoopVar(Arc<str>),
    Unary(UnaryOp, Box<Node>),
    Binary(BinaryOp, Box<Node>, Box<Node>),
    Ternary(Box<Node>, Box<Node>, Box<Node>),
    Member(Box<Node>, Arc<str>),
    Index(Box<Node>, Box<Node>),
    /// `$name = value`: a binding to `$name`; reads as its current
    /// value elsewhere.
    Assign(Arc<str>, Box<Node>),
    Builtin(Builtin, Vec<Node>),
    Each {
        array: Box<Node>,
        var: Arc<str>,
        template: Box<Node>,
    },
    /// A library function; arguments by parameter position, `None`
    /// when absent.
    Function(usize, Vec<Option<Node>>),
    ToAssistant(Box<Node>, Option<Box<Node>>),
    OpenUrl(Box<Node>),
    Set(Arc<str>, Arc<Node>),
    Reset(Vec<Arc<str>>),
    Run(Arc<str>, RunKind),
    /// A library action; arguments by parameter position, `None` when
    /// absent.
    CustomAction(usize, Vec<Option<Node>>),
    /// `Action([...])`.
    ActionList(Box<Node>),
    /// An invalid action: an empty plan, which does nothing.
    NoAction,
}

impl Node {
    fn null() -> Self {
        Self::Lit(Value::Null)
    }

    fn is_null(&self) -> bool {
        matches!(self, Self::Lit(Value::Null))
    }

    /// Literal data the parser can check: scalars, arrays and
    /// objects.
    fn is_data(&self) -> bool {
        matches!(
            self,
            Self::Lit(
                Value::String(_) | Value::Number(_) | Value::Bool(_)
            ) | Self::Array(_)
                | Self::Object(_)
        )
    }

    fn contains_element(&self) -> bool {
        match self {
            Self::Element(_) => true,
            Self::Array(items) | Self::Builtin(_, items) => {
                items.iter().any(Self::contains_element)
            }
            Self::Object(entries) => {
                entries.iter().any(|(_, n)| n.contains_element())
            }
            Self::Unary(_, a)
            | Self::Member(a, _)
            | Self::Assign(_, a)
            | Self::OpenUrl(a) => a.contains_element(),
            Self::ActionList(a) => a.contains_element(),
            Self::Binary(_, a, b) | Self::Index(a, b) => {
                a.contains_element() || b.contains_element()
            }
            Self::Ternary(a, b, c) => {
                a.contains_element()
                    || b.contains_element()
                    || c.contains_element()
            }
            Self::Each {
                array, template, ..
            } => {
                array.contains_element()
                    || template.contains_element()
            }
            Self::Function(_, args) | Self::CustomAction(_, args) => {
                args.iter().flatten().any(Self::contains_element)
            }
            Self::ToAssistant(a, b) => {
                a.contains_element()
                    || b.as_ref()
                        .is_some_and(|b| b.contains_element())
            }
            Self::Set(_, a) => a.contains_element(),
            Self::Lit(_)
            | Self::State(_)
            | Self::Query(_)
            | Self::Mutation(_)
            | Self::LoopVar(_)
            | Self::Reset(_)
            | Self::Run(..)
            | Self::NoAction => false,
        }
    }
}

/// A component call whose arguments are mapped to props.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ElementNode {
    /// Index into [`Library::components`].
    pub(crate) component: usize,
    pub(crate) name: Arc<str>,
    pub(crate) statement: Option<Arc<str>>,
    /// Present props in positional order: parameter index, name,
    /// value.
    pub(crate) props: Vec<(usize, Arc<str>, Node)>,
}

/// A parsed statement.
#[derive(Clone, Debug)]
pub(crate) struct Statement {
    pub(crate) name: Arc<str>,
    pub(crate) kind: StatementKind,
    pub(crate) expr: Expr,
}

impl Statement {
    /// Classifies a statement (spec section 2.1).
    pub(crate) fn new(name: &str, expr: Expr) -> Self {
        let kind = match &expr {
            Expr::Call { name, .. } if &**name == "Query" => {
                StatementKind::Query
            }
            Expr::Call { name, .. } if &**name == "Mutation" => {
                StatementKind::Mutation
            }
            _ if name.starts_with('$') => StatementKind::State,
            _ => StatementKind::Value,
        };
        Self {
            name: Arc::from(name),
            kind,
            expr,
        }
    }
}

/// What the document builder needs besides the statements.
pub(crate) struct BuildOptions {
    pub(crate) limits: Limits,
    /// The stream has ended, so final checks apply.
    pub(crate) complete: bool,
    /// The text ended inside an unclosed string or bracket.
    pub(crate) truncated: bool,
    /// The prepared text was not empty.
    pub(crate) has_text: bool,
    /// A `$name` at the very end of a streaming buffer, which may
    /// still grow, and the pending statement it ends. That
    /// statement does not declare it.
    pub(crate) growing_state: Option<(Arc<str>, Arc<str>)>,
    /// Diagnostics from parsing the statements.
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// Checks a program and builds its document. `statements` are in
/// source order; for a repeated name the last definition wins, at the
/// first's place.
pub(crate) fn build<'s>(
    library: &Library,
    statements: impl IntoIterator<Item = &'s Statement>,
    options: BuildOptions,
) -> Document {
    let mut order: Vec<&Statement> = Vec::new();
    let mut index: HashMap<Arc<str>, usize> = HashMap::new();
    for statement in statements {
        match index.get(&statement.name) {
            Some(&i) => order[i] = statement,
            None => {
                index.insert(statement.name.clone(), order.len());
                order.push(statement);
            }
        }
    }
    let program = Program { order, index };
    let entry = pick_entry(&program, library);

    let mut lowerer = Lowerer {
        library,
        program: &program,
        limits: options.limits,
        streaming: !options.complete,
        path: Vec::new(),
        current: None,
        scope: Vec::new(),
        unreached: program
            .order
            .iter()
            .filter(|s| s.kind == StatementKind::Value)
            .filter(|s| {
                entry.as_ref().is_none_or(|e| *e.statement != *s.name)
            })
            .map(|s| s.name.clone())
            .collect(),
        unresolved: Vec::new(),
        unresolved_seen: HashSet::new(),
        unresolved_hits: 0,
        diagnostics: options.diagnostics,
        nodes: 0,
        depth: 0,
        limit_reported: false,
    };

    let root = entry.as_ref().map_or(Root::None, |entry| {
        lowerer.lower_entry(&entry.statement)
    });

    let mut state_nodes: Vec<(Arc<str>, Node)> = Vec::new();
    let mut queries = Vec::new();
    let mut mutations = Vec::new();
    for statement in &program.order {
        lowerer.enter_statement(&statement.name);
        match (statement.kind, &statement.expr) {
            (StatementKind::State, expr) => {
                let node =
                    lowerer.lower_or_null(expr, Position::Value);
                state_nodes.push((statement.name.clone(), node));
            }
            (StatementKind::Query, Expr::Call { args, .. }) => {
                let mut lowered = args.iter().take(4).map(|a| {
                    lowerer.lower_or_null(a, Position::Computed)
                });
                queries.push(QueryNode {
                    name: statement.name.clone(),
                    tool: lowered.next(),
                    args: lowered.next(),
                    defaults: lowered.next(),
                    refresh: lowered.next(),
                });
            }
            (StatementKind::Mutation, Expr::Call { args, .. }) => {
                let mut lowered = args.iter().take(2).map(|a| {
                    lowerer.lower_or_null(a, Position::Computed)
                });
                mutations.push(QueryNode {
                    name: statement.name.clone(),
                    tool: lowered.next(),
                    args: lowered.next(),
                    defaults: None,
                    refresh: None,
                });
            }
            _ => {}
        }
        lowerer.leave_statement();
    }

    // An undeclared `$name` is declared with the default null.
    let mut declared: HashSet<Arc<str>> =
        state_nodes.iter().map(|(n, _)| n.clone()).collect();
    let mut auto_declared: Vec<Arc<str>> = Vec::new();
    for statement in &program.order {
        let growing = match &options.growing_state {
            Some((name, pending)) if *pending == statement.name => {
                Some(name)
            }
            _ => None,
        };
        statement.expr.visit_state(&mut |name| {
            let growing = growing == Some(name);
            if !growing && declared.insert(name.clone()) {
                auto_declared.push(name.clone());
            }
        });
    }

    if options.complete {
        lowerer.report_entry_problems(
            entry.as_ref(),
            &root,
            options.has_text,
        );
    }

    let orphans = match &entry {
        Some(_) => program
            .order
            .iter()
            .filter(|s| lowerer.unreached.contains(&s.name))
            .map(|s| s.name.to_string())
            .collect(),
        None => Vec::new(),
    };

    let mut document = Document {
        library: library.clone(),
        limits: options.limits,
        statements: program
            .order
            .iter()
            .map(|s| StatementInfo {
                name: s.name.to_string(),
                kind: s.kind,
            })
            .collect(),
        entry,
        root,
        state: Vec::new(),
        state_index: HashMap::new(),
        query_index: queries
            .iter()
            .enumerate()
            .map(|(i, q)| (q.name.clone(), i))
            .collect(),
        queries,
        mutations,
        diagnostics: lowerer.diagnostics,
        unresolved: lowerer
            .unresolved
            .iter()
            .map(|n| n.to_string())
            .collect(),
        orphans,
        truncated: options.truncated,
        complete: options.complete,
    };

    // Defaults are evaluated once, with no state: `$` reads inside
    // them are null.
    let mut defaults =
        Vec::with_capacity(state_nodes.len() + auto_declared.len());
    {
        let mut evaluator = Evaluator::for_defaults(&document);
        for (name, node) in &state_nodes {
            evaluator.set_statement(name);
            defaults.push((name.clone(), evaluator.eval(node)));
        }
        document.diagnostics.extend(evaluator.into_diagnostics());
    }
    defaults.extend(
        auto_declared.into_iter().map(|name| (name, Value::Null)),
    );
    document.set_state_defaults(defaults);
    document
}

struct Program<'s> {
    order: Vec<&'s Statement>,
    index: HashMap<Arc<str>, usize>,
}

impl Program<'_> {
    fn get(&self, name: &str) -> Option<&Statement> {
        self.index.get(name).map(|&i| self.order[i])
    }
}

/// The entry is `root`, else the first value statement that calls the
/// library's root component.
fn pick_entry(
    program: &Program<'_>,
    library: &Library,
) -> Option<Entry> {
    if program.get("root").is_some() {
        return Some(Entry {
            statement: "root".to_owned(),
            fallback: false,
        });
    }
    let root = library.root()?;
    program
        .order
        .iter()
        .find(|s| {
            s.kind == StatementKind::Value
                && matches!(&s.expr, Expr::Call { name, at: false, .. } if &**name == root)
        })
        .map(|s| Entry {
            statement: s.name.to_string(),
            fallback: true,
        })
}

fn is_component_call(expr: &Expr) -> bool {
    matches!(expr, Expr::Call { name, at: false, .. }
        if !is_builtin_call(name) && &**name != "Query" && &**name != "Mutation")
}

/// Whether a statement is, through references, a component call
/// (perhaps one validation dropped).
fn resolves_to_component_call(
    program: &Program<'_>,
    name: &str,
) -> bool {
    let mut seen = HashSet::new();
    let mut expr = match program.get(name) {
        Some(statement) => &statement.expr,
        None => return false,
    };
    while let Expr::Ref(next) = expr {
        if !seen.insert(next.clone()) {
            return false;
        }
        match program.get(next) {
            Some(statement) => expr = &statement.expr,
            None => return false,
        }
    }
    is_component_call(expr)
}

/// Where an expression sits (spec section 2.1). A value position is a
/// direct argument of a component call or a direct element of an
/// array literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    Value,
    Computed,
}

enum Checked {
    Valid(Node),
    Invalid,
}

/// Call arguments mapped to parameters, by parameter position.
struct CallArgs {
    args: Vec<Option<Node>>,
    /// A required argument is invalid, so the call does not happen.
    invalid: bool,
    /// Checking an argument reported a problem. Extra arguments do
    /// not count.
    reported: bool,
}

struct Lowerer<'a, 's> {
    library: &'a Library,
    program: &'a Program<'s>,
    limits: Limits,
    streaming: bool,
    /// Statements being inlined, outermost first, for cycle
    /// detection.
    path: Vec<Arc<str>>,
    /// The innermost statement, for error attribution.
    current: Option<Arc<str>>,
    /// Loop variables of enclosing `@Each` templates.
    scope: Vec<Arc<str>>,
    unreached: HashSet<Arc<str>>,
    unresolved: Vec<Arc<str>>,
    unresolved_seen: HashSet<Arc<str>>,
    unresolved_hits: usize,
    diagnostics: Vec<Diagnostic>,
    nodes: usize,
    depth: usize,
    limit_reported: bool,
}

impl Lowerer<'_, '_> {
    fn enter_statement(&mut self, name: &Arc<str>) {
        self.path = vec![name.clone()];
        self.current = Some(name.clone());
    }

    fn leave_statement(&mut self) {
        self.path.clear();
        self.current = None;
    }

    fn lower_entry(&mut self, name: &str) -> Root {
        let Some(statement) = self.program.get(name) else {
            return Root::None;
        };
        if statement.kind != StatementKind::Value
            && statement.kind != StatementKind::State
        {
            return Root::None;
        }
        let name = statement.name.clone();
        self.unreached.remove(&name);
        self.enter_statement(&name);
        let node = self.lower(&statement.expr, Position::Value);
        self.leave_statement();
        match node {
            Some(Node::Element(mut element)) => {
                element.statement = Some(name);
                Root::Element(Node::Element(element))
            }
            Some(node) if node.contains_element() => {
                Root::Expression(node)
            }
            _ => Root::None,
        }
    }

    fn report_entry_problems(
        &mut self,
        entry: Option<&Entry>,
        root: &Root,
        has_text: bool,
    ) {
        let root_name = self.library.root();
        let Some(entry) = entry else {
            if self.program.order.is_empty() {
                if has_text {
                    self.report(
                        Diagnostic::new(
                            Source::Parser,
                            Code::ParseFailed,
                            "the response has text but no statements",
                        ),
                        None,
                    );
                }
                return;
            }
            let message = match root_name {
                Some(root) => {
                    format!(
                        "no root statement, and no statement calls \
                         the root component {root}"
                    )
                }
                None => "no root statement".to_owned(),
            };
            let mut diagnostic = Diagnostic::new(
                Source::Parser,
                Code::NoRoot,
                message,
            );
            diagnostic.component = root_name.map(str::to_owned);
            diagnostic.hint = Some(match root_name {
                Some(root) => format!("Add `root = {root}(...)`"),
                None => "Add `root = ...`".to_owned(),
            });
            self.diagnostics.push(diagnostic);
            return;
        };
        let rendered = !matches!(root, Root::None);
        let message;
        let mut severity = Severity::Error;
        if !rendered
            && !resolves_to_component_call(
                self.program,
                &entry.statement,
            )
        {
            message = format!(
                "`{}` does not evaluate to a component",
                entry.statement
            );
        } else if entry.fallback {
            let root = root_name.unwrap_or_default();
            if rendered {
                severity = Severity::Warning;
                message = format!(
                    "no root statement; rendering `{}`, the first \
                     statement that calls the root component {root}",
                    entry.statement
                );
            } else {
                message = format!(
                    "no root statement, and `{}` did not render",
                    entry.statement
                );
            }
        } else {
            return;
        }
        let mut diagnostic =
            Diagnostic::new(Source::Parser, Code::NoRoot, message);
        diagnostic.statement = Some(entry.statement.clone());
        diagnostic.component = root_name.map(str::to_owned);
        diagnostic.severity = severity;
        self.diagnostics.push(diagnostic);
    }

    fn report(
        &mut self,
        mut diagnostic: Diagnostic,
        component: Option<&str>,
    ) {
        diagnostic.statement =
            self.current.as_deref().map(str::to_owned);
        if let Some(component) = component {
            diagnostic.component = Some(component.to_owned());
        }
        self.diagnostics.push(diagnostic);
    }

    fn report_at(
        &mut self,
        code: Code,
        component: &str,
        path: &str,
        message: String,
        hint: Option<String>,
    ) {
        let mut diagnostic =
            Diagnostic::new(Source::Parser, code, message);
        if !path.is_empty() {
            diagnostic.path = Some(path.to_owned());
        }
        diagnostic.hint = hint;
        self.report(diagnostic, Some(component));
    }

    fn report_limit(&mut self) {
        if self.limit_reported {
            return;
        }
        self.limit_reported = true;
        let message = format!(
            "the program nests deeper than {} or expands to more \
             than {} nodes; the excess is dropped",
            self.limits.max_depth, self.limits.max_nodes
        );
        self.report(
            Diagnostic::new(
                Source::Parser,
                Code::LimitExceeded,
                message,
            ),
            None,
        );
    }

    fn report_unresolved(&mut self, name: &Arc<str>) {
        self.unresolved_hits += 1;
        if self.unresolved_seen.insert(name.clone()) {
            self.unresolved.push(name.clone());
        }
    }

    fn lower_or_null(
        &mut self,
        expr: &Expr,
        position: Position,
    ) -> Node {
        self.lower(expr, position).unwrap_or_else(Node::null)
    }

    /// Lowers an expression. `None` means it renders nothing: an
    /// unresolved reference, an invalid component, or `Query` in
    /// a value position.
    fn lower(
        &mut self,
        expr: &Expr,
        position: Position,
    ) -> Option<Node> {
        if self.depth >= self.limits.max_depth
            || self.nodes >= self.limits.max_nodes
        {
            self.report_limit();
            return None;
        }
        self.depth += 1;
        self.nodes += 1;
        let node = self.lower_inner(expr, position);
        self.depth -= 1;
        node
    }

    fn lower_inner(
        &mut self,
        expr: &Expr,
        position: Position,
    ) -> Option<Node> {
        let computed = Position::Computed;
        let node = match expr {
            Expr::Str(s) => Node::Lit(Value::String(s.clone())),
            Expr::Num(n) => Node::Lit(Value::Number(*n)),
            Expr::Bool(b) => Node::Lit(Value::Bool(*b)),
            Expr::Null => Node::null(),
            Expr::Array(items) => {
                Node::Array(self.lower_items(items))
            }
            Expr::Object(entries) => Node::Object(
                entries
                    .iter()
                    .map(|(k, v)| {
                        (k.clone(), self.lower_or_null(v, computed))
                    })
                    .collect(),
            ),
            Expr::Ref(name) => return self.lower_ref(name, position),
            Expr::State(name) => Node::State(name.clone()),
            Expr::Call { name, at, args } => {
                return self.lower_call(name, *at, args, position);
            }
            Expr::Unary(op, operand) => Node::Unary(
                *op,
                Box::new(self.lower_or_null(operand, computed)),
            ),
            Expr::Binary(op, left, right) => Node::Binary(
                *op,
                Box::new(self.lower_or_null(left, computed)),
                Box::new(self.lower_or_null(right, computed)),
            ),
            Expr::Ternary(cond, then, otherwise) => Node::Ternary(
                Box::new(self.lower_or_null(cond, computed)),
                Box::new(self.lower_or_null(then, computed)),
                Box::new(self.lower_or_null(otherwise, computed)),
            ),
            Expr::Member(object, field) => Node::Member(
                Box::new(self.lower_or_null(object, computed)),
                field.clone(),
            ),
            Expr::Index(object, index) => Node::Index(
                Box::new(self.lower_or_null(object, computed)),
                Box::new(self.lower_or_null(index, computed)),
            ),
            Expr::Assign(name, value) => Node::Assign(
                name.clone(),
                Box::new(self.lower_or_null(value, computed)),
            ),
        };
        Some(node)
    }

    /// Array elements are value positions. Ones that render nothing
    /// are left out, and so are references whose value is null; a
    /// `null` literal stays.
    fn lower_items(&mut self, items: &[Expr]) -> Vec<Node> {
        items
            .iter()
            .filter_map(|item| {
                let node = self.lower(item, Position::Value)?;
                let null_ref =
                    matches!(item, Expr::Ref(_)) && node.is_null();
                (!null_ref).then_some(node)
            })
            .collect()
    }

    fn lower_ref(
        &mut self,
        name: &Arc<str>,
        position: Position,
    ) -> Option<Node> {
        if self.scope.iter().rev().any(|v| v == name) {
            return Some(Node::LoopVar(name.clone()));
        }
        let Some(statement) = self.program.get(name) else {
            self.report_unresolved(name);
            return None;
        };
        // A query or mutation reads its result and is never inlined,
        // so a reference to one, even from its own arguments,
        // closes no cycle.
        match statement.kind {
            StatementKind::Query => {
                self.unreached.remove(name);
                return Some(Node::Query(name.clone()));
            }
            StatementKind::Mutation => {
                self.unreached.remove(name);
                return Some(Node::Mutation(name.clone()));
            }
            StatementKind::Value | StatementKind::State => {}
        }
        // A reference that closes a cycle is unresolved at that
        // point.
        if self.path.contains(name) {
            self.report_unresolved(name);
            return None;
        }
        self.unreached.remove(name);
        self.path.push(name.clone());
        let previous = self.current.replace(name.clone());
        // Loop variables are lexical: a referenced statement does not
        // see them.
        let scope = std::mem::take(&mut self.scope);
        let mut node = self.lower(&statement.expr, position);
        self.scope = scope;
        self.current = previous;
        self.path.pop();
        if let Some(Node::Element(element)) = &mut node {
            element.statement = Some(name.clone());
        }
        node
    }

    fn lower_call(
        &mut self,
        name: &Arc<str>,
        at: bool,
        args: &[Expr],
        position: Position,
    ) -> Option<Node> {
        let computed = Position::Computed;
        if &**name == "Query" || &**name == "Mutation" {
            if position == Position::Value {
                self.report_at(
                    Code::InlineReserved,
                    name,
                    "",
                    format!(
                        "{name}() must be a top-level statement, \
                         not a value"
                    ),
                    Some(format!(
                        "Declare it as a statement: myData = \
                         {name}(...)"
                    )),
                );
                return None;
            }
            return Some(Node::null());
        }
        if &**name == "Action" {
            return Some(match args.first() {
                Some(list) => Node::ActionList(Box::new(
                    self.lower_or_null(list, computed),
                )),
                None => Node::NoAction,
            });
        }
        if at {
            if let Some(builtin) = Builtin::from_name(name) {
                let args = args
                    .iter()
                    .take(builtin.arity())
                    .map(|a| self.lower_or_null(a, computed))
                    .collect();
                return Some(Node::Builtin(builtin, args));
            }
            if let Some(step) = self.lower_step(name, args) {
                return Some(step);
            }
            return match self.library.entry(name) {
                Some(LibraryEntry::Function(index)) => {
                    Some(self.lower_function(index, args))
                }
                Some(LibraryEntry::Action(index)) => {
                    Some(self.lower_custom_action(index, args))
                }
                _ => {
                    self.report_at(
                        Code::UnknownFunction,
                        name,
                        "",
                        format!(
                            "unknown function @{name}: not a \
                             built-in, an action, or a library \
                             function; the call is null"
                        ),
                        self.function_hint(),
                    );
                    // Null in an expression; in a value position it
                    // renders nothing.
                    (position == Position::Computed).then(Node::null)
                }
            };
        }
        if let Some(LibraryEntry::Component(index)) =
            self.library.entry(name)
        {
            return self.lower_component(index, args);
        }
        let hint = match self.library.entry(name) {
            Some(
                LibraryEntry::Function(_) | LibraryEntry::Action(_),
            ) => Some(format!("Call it with @: @{name}(...)")),
            _ => Some(format!(
                "Available components: {}",
                self.library
                    .components()
                    .iter()
                    .map(|c| c.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
        self.report_at(
            Code::UnknownComponent,
            name,
            "",
            format!("unknown component {name}: not in the library"),
            hint,
        );
        // Lower the arguments anyway so their references count as
        // reached.
        for arg in args {
            self.lower(arg, Position::Computed);
        }
        None
    }

    fn function_hint(&self) -> Option<String> {
        let mut names: Vec<String> =
            Builtin::NAMES.iter().map(|n| format!("@{n}")).collect();
        names.push("@Each".to_owned());
        names.extend(
            self.library
                .functions()
                .iter()
                .map(|f| format!("@{}", f.name())),
        );
        Some(format!("Available functions: {}", names.join(", ")))
    }

    /// The built-in actions, or `None` when `name` is not one.
    fn lower_step(
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> Option<Node> {
        let computed = Position::Computed;
        let node = match name {
            "Each" => self.lower_each(args),
            "ToAssistant" => {
                let message = args.first().map_or_else(
                    || Node::Lit(Value::from("")),
                    |a| self.lower_or_null(a, computed),
                );
                let context = args.get(1).map(|a| {
                    Box::new(self.lower_or_null(a, computed))
                });
                Node::ToAssistant(Box::new(message), context)
            }
            "OpenUrl" => {
                let url = args.first().map_or_else(Node::null, |a| {
                    self.lower_or_null(a, computed)
                });
                Node::OpenUrl(Box::new(url))
            }
            "Set" => match args {
                [Expr::State(target), value, ..] => Node::Set(
                    target.clone(),
                    Arc::new(self.lower_or_null(value, computed)),
                ),
                _ => Node::NoAction,
            },
            "Reset" => {
                let targets: Vec<Arc<str>> = args
                    .iter()
                    .filter_map(|a| match a {
                        Expr::State(name) => Some(name.clone()),
                        _ => None,
                    })
                    .collect();
                if targets.is_empty() {
                    Node::NoAction
                } else {
                    Node::Reset(targets)
                }
            }
            "Run" => match args.first() {
                Some(Expr::Ref(target)) => {
                    match self.lower_ref(target, computed) {
                        Some(Node::Query(id)) => {
                            Node::Run(id, RunKind::Query)
                        }
                        Some(Node::Mutation(id)) => {
                            Node::Run(id, RunKind::Mutation)
                        }
                        _ => Node::NoAction,
                    }
                }
                _ => Node::NoAction,
            },
            _ => return None,
        };
        Some(node)
    }

    fn lower_each(&mut self, args: &[Expr]) -> Node {
        let var = match args.get(1) {
            Some(Expr::Ref(name) | Expr::Str(name))
                if args.len() >= 3 =>
            {
                name.clone()
            }
            _ => {
                // Lowered anyway, so the references count as reached.
                for arg in args {
                    self.lower(arg, Position::Computed);
                }
                return Node::Lit(Value::array([]));
            }
        };
        let array = self.lower_or_null(&args[0], Position::Computed);
        self.scope.push(var.clone());
        let template =
            self.lower_or_null(&args[2], Position::Computed);
        self.scope.pop();
        Node::Each {
            array: Box::new(array),
            var,
            template: Box::new(template),
        }
    }

    fn lower_component(
        &mut self,
        index: usize,
        args: &[Expr],
    ) -> Option<Node> {
        let library = self.library;
        let def = &library.components()[index];
        let params = def.params();
        let mut props: Vec<(usize, Arc<str>, Node)> = Vec::new();
        let mut drop = false;
        // Required props already dropped for invalid data, so not
        // reported twice.
        let mut settled = vec![false; params.len()];
        let mut from_unresolved = vec![false; params.len()];
        for (i, (param, arg)) in params.iter().zip(args).enumerate() {
            let hits = self.unresolved_hits;
            let lowered = self.lower(arg, Position::Value);
            from_unresolved[i] =
                lowered.is_none() && self.unresolved_hits > hits;
            let node = lowered.unwrap_or_else(Node::null);
            let path = format!("/{}", param.name());
            match self.check(node, param.ty(), def.name(), &path) {
                Checked::Valid(node) => {
                    props.push((i, Arc::from(param.name()), node))
                }
                Checked::Invalid => match param.default_value() {
                    Some(default) => props.push((
                        i,
                        Arc::from(param.name()),
                        Node::Lit(default.clone()),
                    )),
                    None if param.is_required() => {
                        drop = true;
                        settled[i] = true;
                    }
                    None => {}
                },
            }
        }
        if args.len() > params.len() {
            self.report_at(
                Code::ExcessArgs,
                def.name(),
                "",
                format!(
                    "{} takes {} arguments, got {}; the extra ones \
                     are dropped",
                    def.name(),
                    params.len(),
                    args.len()
                ),
                Some(hint_signature(def.name(), params)),
            );
        }
        for (i, param) in params.iter().enumerate() {
            if !param.is_required() || settled[i] {
                continue;
            }
            let slot = props.iter().position(|(j, _, _)| *j == i);
            if slot.is_some_and(|s| !props[s].2.is_null()) {
                continue;
            }
            if let Some(default) = param.default_value() {
                let node = Node::Lit(default.clone());
                match slot {
                    Some(s) => props[s].2 = node,
                    None => {
                        props.push((i, Arc::from(param.name()), node))
                    }
                }
                continue;
            }
            drop = true;
            // While streaming, a reference may resolve once more text
            // arrives.
            if self.streaming && from_unresolved[i] {
                continue;
            }
            let present = i < args.len();
            let (code, message) = if present {
                (
                    Code::NullRequired,
                    format!(
                        "required field \"/{}\" is null",
                        param.name()
                    ),
                )
            } else {
                (
                    Code::MissingRequired,
                    format!(
                        "missing required field \"/{}\"",
                        param.name()
                    ),
                )
            };
            self.report_at(
                code,
                def.name(),
                &format!("/{}", param.name()),
                message,
                Some(hint_signature(def.name(), params)),
            );
        }
        if drop {
            return None;
        }
        props.sort_by_key(|(i, _, _)| *i);
        Some(Node::Element(Box::new(ElementNode {
            component: index,
            name: Arc::from(def.name()),
            statement: None,
            props,
        })))
    }

    /// Lowers a library function call. Literal arguments are checked
    /// now; an invalid required one makes the call null.
    fn lower_function(
        &mut self,
        index: usize,
        args: &[Expr],
    ) -> Node {
        let library = self.library;
        let def = &library.functions()[index];
        let call =
            self.lower_call_args(def.name(), def.params(), args);
        if call.invalid {
            Node::null()
        } else {
            Node::Function(index, call.args)
        }
    }

    /// Lowers a library action. Any invalid literal argument makes it
    /// do nothing.
    fn lower_custom_action(
        &mut self,
        index: usize,
        args: &[Expr],
    ) -> Node {
        let library = self.library;
        let def = &library.actions()[index];
        let call =
            self.lower_call_args(def.name(), def.params(), args);
        if call.invalid || call.reported {
            Node::NoAction
        } else {
            Node::CustomAction(index, call.args)
        }
    }

    /// Maps call arguments to parameters and checks the literal ones.
    fn lower_call_args(
        &mut self,
        name: &str,
        params: &[Param],
        args: &[Expr],
    ) -> CallArgs {
        // `None` for an argument that renders nothing, such as an
        // unresolved reference.
        let lowered: Vec<Option<Node>> = args
            .iter()
            .take(params.len())
            .map(|a| self.lower(a, Position::Computed))
            .collect();
        if args.len() > params.len() {
            self.report_at(
                Code::ExcessArgs,
                name,
                "",
                format!(
                    "@{name} takes {} arguments, got {}; the extra \
                     ones are dropped",
                    params.len(),
                    args.len()
                ),
                Some(hint_signature(name, params)),
            );
        }
        let reported_before = self.diagnostics.len();
        let mut mapped: Vec<Option<Node>> =
            Vec::with_capacity(params.len());
        let mut invalid = false;
        for (i, param) in params.iter().enumerate() {
            let present = i < args.len();
            let node = match lowered.get(i) {
                Some(Some(node)) => node.clone(),
                // An unresolved reference is null when the call runs,
                // with no error.
                Some(None) => {
                    mapped.push(Some(Node::null()));
                    continue;
                }
                None => Node::null(),
            };
            // Arguments only known at runtime are checked when the
            // call runs.
            if !is_literal(&node) {
                mapped.push(Some(node));
                continue;
            }
            let path = format!("/{}", param.name());
            if node.is_null() {
                if param.is_required()
                    && param.default_value().is_none()
                {
                    let (code, message) = if present {
                        (
                            Code::NullRequired,
                            format!(
                                "required argument \"{path}\" of \
                                 @{name} is null"
                            ),
                        )
                    } else {
                        (
                            Code::MissingRequired,
                            format!(
                                "missing required argument \
                                 \"{path}\" of @{name}"
                            ),
                        )
                    };
                    self.report_at(
                        code,
                        name,
                        &path,
                        message,
                        Some(hint_signature(name, params)),
                    );
                    invalid = true;
                }
                mapped.push(present.then_some(node));
                continue;
            }
            mapped.push(
                match self.check(node, param.ty(), name, &path) {
                    Checked::Valid(node) => Some(node),
                    Checked::Invalid => match param.default_value() {
                        Some(default) => {
                            Some(Node::Lit(default.clone()))
                        }
                        None => {
                            invalid |= param.is_required();
                            None
                        }
                    },
                },
            );
        }
        CallArgs {
            args: mapped,
            invalid,
            reported: self.diagnostics.len() > reported_before,
        }
    }

    /// Checks a lowered value against a type. Invalid array items are
    /// pruned, invalid optional object fields removed, and every
    /// problem reported.
    fn check(
        &mut self,
        node: Node,
        ty: &Type,
        component: &str,
        path: &str,
    ) -> Checked {
        if let Node::Element(element) = &node {
            if rejects_components(ty) {
                let actual = format!("component {}", element.name);
                self.mismatch(
                    component,
                    path,
                    &expected_name(ty),
                    &actual,
                );
                return Checked::Invalid;
            }
            return Checked::Valid(node);
        }
        // Null is handled by required checks; expressions resolve at
        // runtime.
        if !node.is_data() {
            return Checked::Valid(node);
        }
        if ty.is_component_slot() {
            self.mismatch(
                component,
                path,
                &expected_name(ty),
                node_kind(&node),
            );
            return Checked::Invalid;
        }
        match ty {
            Type::Any
            | Type::Action(_)
            | Type::Union(_)
            | Type::Component(_) => Checked::Valid(node),
            Type::String | Type::Number | Type::Boolean => {
                if node_kind(&node) == expected_name(ty) {
                    Checked::Valid(node)
                } else {
                    self.mismatch(
                        component,
                        path,
                        &expected_name(ty),
                        node_kind(&node),
                    );
                    Checked::Invalid
                }
            }
            Type::Enum(values) => {
                // Enum checks wait for the stream to end: a partial
                // literal may still grow.
                let Node::Lit(value) = &node else {
                    return Checked::Valid(node);
                };
                if self.streaming
                    || values
                        .iter()
                        .any(|v| same_value_zero(v, value))
                {
                    return Checked::Valid(node);
                }
                let actual = crate::js::quote(value);
                self.mismatch(
                    component,
                    path,
                    &expected_name(ty),
                    &actual,
                );
                Checked::Invalid
            }
            Type::List(item) => match node {
                Node::Array(items) if matches!(**item, Type::Any) => {
                    Checked::Valid(Node::Array(items))
                }
                Node::Array(items) => {
                    let mut kept = Vec::with_capacity(items.len());
                    for (i, item_node) in
                        items.into_iter().enumerate()
                    {
                        if let Checked::Valid(n) = self.check(
                            item_node,
                            item,
                            component,
                            &format!("{path}/{i}"),
                        ) {
                            kept.push(n);
                        }
                    }
                    Checked::Valid(Node::Array(kept))
                }
                other => {
                    self.mismatch(
                        component,
                        path,
                        "array",
                        node_kind(&other),
                    );
                    Checked::Invalid
                }
            },
            Type::Object(fields) => match node {
                Node::Object(entries) => self
                    .check_object(entries, fields, component, path),
                other => {
                    self.mismatch(
                        component,
                        path,
                        "object",
                        node_kind(&other),
                    );
                    Checked::Invalid
                }
            },
        }
    }

    fn check_object(
        &mut self,
        mut entries: Vec<(Arc<str>, Node)>,
        fields: &[Param],
        component: &str,
        path: &str,
    ) -> Checked {
        let mut invalid = false;
        let find = |entries: &[(Arc<str>, Node)], key: &str| {
            entries.iter().position(|(k, _)| &**k == key)
        };
        // Structural checks wait for the stream to end.
        if !self.streaming {
            for field in fields.iter().filter(|f| f.is_required()) {
                let at = find(&entries, field.name());
                if at.is_some_and(|at| !entries[at].1.is_null()) {
                    continue;
                }
                if let Some(default) = field.default_value() {
                    let node = Node::Lit(default.clone());
                    match at {
                        Some(at) => entries[at].1 = node,
                        None => entries
                            .push((Arc::from(field.name()), node)),
                    }
                    continue;
                }
                let field_path = format!("{path}/{}", field.name());
                let (code, message) = match at {
                    Some(_) => (
                        Code::NullRequired,
                        format!(
                            "required field \"{field_path}\" is null"
                        ),
                    ),
                    None => (
                        Code::MissingRequired,
                        format!(
                            "missing required field \"{field_path}\""
                        ),
                    ),
                };
                self.report_at(
                    code,
                    component,
                    &field_path,
                    message,
                    None,
                );
                invalid = true;
            }
        }
        for field in fields {
            let Some(at) = find(&entries, field.name()) else {
                continue;
            };
            let node =
                std::mem::replace(&mut entries[at].1, Node::null());
            let field_path = format!("{path}/{}", field.name());
            match self.check(node, field.ty(), component, &field_path)
            {
                Checked::Valid(node) => entries[at].1 = node,
                Checked::Invalid => match field.default_value() {
                    Some(default) => {
                        entries[at].1 = Node::Lit(default.clone())
                    }
                    None if field.is_required() => invalid = true,
                    None => {
                        entries.remove(at);
                    }
                },
            }
        }
        if invalid {
            Checked::Invalid
        } else {
            Checked::Valid(Node::Object(entries))
        }
    }

    fn mismatch(
        &mut self,
        component: &str,
        path: &str,
        expected: &str,
        actual: &str,
    ) {
        self.report_at(
            Code::TypeMismatch,
            component,
            path,
            format!(
                "field \"{path}\" expects {expected} but got \
                 {actual}"
            ),
            None,
        );
    }
}

/// True when a lowered argument is literal data, checkable before
/// evaluation.
fn is_literal(node: &Node) -> bool {
    match node {
        Node::Lit(_) => true,
        Node::Array(items) => items.iter().all(is_literal),
        Node::Object(entries) => {
            entries.iter().all(|(_, n)| is_literal(n))
        }
        _ => false,
    }
}

/// Slots that declare plain data can never hold a component.
pub(crate) fn rejects_components(ty: &Type) -> bool {
    matches!(
        ty,
        Type::String
            | Type::Number
            | Type::Boolean
            | Type::Enum(_)
            | Type::List(_)
            | Type::Object(_)
    )
}

pub(crate) fn expected_name(ty: &Type) -> String {
    match ty {
        Type::String => "string".to_owned(),
        Type::Number => "number".to_owned(),
        Type::Boolean => "boolean".to_owned(),
        Type::Enum(values) => {
            let values: Vec<String> = values
                .iter()
                .map(|v| format!("{:?}", crate::js::to_string(v)))
                .collect();
            format!("one of [{}]", values.join(", "))
        }
        Type::List(_) => "array".to_owned(),
        Type::Object(_) => "object".to_owned(),
        Type::Component(names) if names.is_empty() => {
            "a component".to_owned()
        }
        Type::Component(names) => names.join(" | "),
        Type::Union(_) => "a component".to_owned(),
        Type::Action(_) => "an action".to_owned(),
        Type::Any => "any value".to_owned(),
    }
}

fn node_kind(node: &Node) -> &'static str {
    match node {
        Node::Lit(value) => crate::eval::value_kind(value),
        Node::Array(_) => "array",
        Node::Object(_) => "object",
        _ => "an expression",
    }
}

/// `SameValueZero`, the equality of `Array.prototype.includes`.
pub(crate) fn same_value_zero(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            x == y || (x.is_nan() && y.is_nan())
        }
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => false,
    }
}
