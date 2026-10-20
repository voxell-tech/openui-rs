//! Evaluation (spec sections 3, 5 and 6).
//!
//! Evaluating never acts. An action call evaluates to an
//! [`ActionPlan`] that the host may carry out, a query reads its
//! result or defaults, and library functions are the only host code
//! that runs.

use std::collections::HashMap;
use std::sync::Arc;

use crate::diagnostic::{Code, Diagnostic, Source};
use crate::document::{Document, Inputs, QueryNode, ToolRequest};
use crate::js;
use crate::library::{Param, Type};
use crate::limits::Limits;
use crate::lower::{
    ElementNode, Node, expected_name, rejects_components,
    same_value_zero,
};
use crate::syntax::{BinaryOp, UnaryOp};
use crate::value::{
    ActionPlan, ActionStep, Deferred, Element, Map, Prop, Value,
    array_index,
};

/// Bytes of string that one step pays to scan, compare or hash.
const BYTES_PER_STEP: usize = 64;

/// Runtime errors one evaluation reports. Past this, one
/// `limit-exceeded` diagnostic says the rest were left out.
const MAX_RUNTIME_ERRORS: usize = 100;

/// What one evaluation may still spend.
///
/// Work grows with string length as well as with steps, so every
/// operation that reads a whole string pays for its bytes, and every
/// copy that the result keeps is charged to the string budget.
pub(crate) struct Budget {
    steps: usize,
    string_bytes: usize,
    exhausted: bool,
}

impl Budget {
    pub(crate) fn new(limits: &Limits) -> Self {
        Self {
            steps: limits.max_steps,
            string_bytes: limits.max_string_bytes,
            exhausted: false,
        }
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Spends `n` steps. False once the budget is gone.
    pub(crate) fn steps(&mut self, n: usize) -> bool {
        if self.exhausted || n > self.steps {
            self.exhausted = true;
            return false;
        }
        self.steps -= n;
        true
    }

    /// Spends steps for scanning, comparing or hashing `bytes` bytes
    /// of strings.
    pub(crate) fn scan(&mut self, bytes: usize) -> bool {
        self.steps(bytes / BYTES_PER_STEP)
    }

    /// Spends string bytes for `bytes` bytes of text the result keeps
    /// a copy of.
    pub(crate) fn copy(&mut self, bytes: usize) -> bool {
        if self.exhausted || bytes > self.string_bytes {
            self.exhausted = true;
            return false;
        }
        self.string_bytes -= bytes;
        true
    }

    /// `toNumber`, paying to parse a string. `None` once the budget
    /// is gone.
    pub(crate) fn number(&mut self, v: &Value) -> Option<f64> {
        self.scan(text_len(v)).then(|| js::to_number(v))
    }

    /// [`property`], paying to hash the key and to walk a string.
    pub(crate) fn property(
        &mut self,
        value: &Value,
        key: &str,
    ) -> Option<Value> {
        self.scan(key.len() + text_len(value))
            .then(|| property(value, key))
    }

    /// `ToString`, charged to the string budget.
    pub(crate) fn string(&mut self, v: &Value) -> Option<String> {
        if self.exhausted {
            return None;
        }
        let Some(s) = js::to_string_within(v, self.string_bytes)
        else {
            self.exhausted = true;
            return None;
        };
        self.string_bytes -= s.len();
        Some(s)
    }

    /// `String(v ?? "")`.
    pub(crate) fn string_or_empty(
        &mut self,
        v: &Value,
    ) -> Option<String> {
        if v.is_null() {
            Some(String::new())
        } else {
            self.string(v)
        }
    }

    /// `x == y`, paying to compare or parse strings. An array
    /// compared with a primitive is joined to a string first,
    /// within the string budget. `None` once the budget is gone.
    pub(crate) fn loosely_equal(
        &mut self,
        x: &Value,
        y: &Value,
    ) -> Option<bool> {
        if !self.scan(text_len(x) + text_len(y)) {
            return None;
        }
        let primitive = |v: &Value| {
            matches!(
                v,
                Value::Bool(_) | Value::Number(_) | Value::String(_)
            )
        };
        let (array, other) = match (x, y) {
            (Value::Array(_), other) if primitive(other) => {
                (x, other)
            }
            (other, Value::Array(_)) if primitive(other) => {
                (y, other)
            }
            _ => return Some(js::loosely_equal(x, y)),
        };
        let joined = self.string(array)?;
        Some(js::loosely_equal(&Value::from(joined), other))
    }
}

/// The bytes of a string value; 0 for anything else.
pub(crate) fn text_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        _ => 0,
    }
}

/// The prop being evaluated, for attributing runtime errors.
#[derive(Clone)]
struct Site {
    component: Arc<str>,
    prop: Arc<str>,
    statement: Option<Arc<str>>,
}

pub(crate) struct Evaluator<'d> {
    document: &'d Document,
    inputs: Option<&'d Inputs>,
    budget: Budget,
    max_depth: usize,
    depth: usize,
    /// Loop variables of enclosing `@Each` templates, innermost
    /// last.
    scope: Vec<(Arc<str>, Value)>,
    statement: Option<Arc<str>>,
    site: Option<Site>,
    /// Query defaults already evaluated: a query read twice gives
    /// one result.
    queries: HashMap<Arc<str>, Value>,
    /// Queries whose defaults are being evaluated, to cut cycles.
    queries_running: Vec<Arc<str>>,
    idle_mutation: Value,
    diagnostics: Vec<Diagnostic>,
    limit_reported: bool,
    runtime_errors: usize,
}

impl<'d> Evaluator<'d> {
    pub(crate) fn new(
        document: &'d Document,
        inputs: Option<&'d Inputs>,
    ) -> Self {
        let limits = document.limits;
        Self {
            document,
            inputs,
            budget: Budget::new(&limits),
            // One bound for all nesting, including query defaults
            // that read other queries, keeps the stack
            // bounded.
            max_depth: limits.max_depth,
            depth: 0,
            scope: Vec::new(),
            statement: None,
            site: None,
            queries: HashMap::new(),
            queries_running: Vec::new(),
            idle_mutation: Value::object([
                ("status", Value::from("idle")),
                ("data", Value::Null),
                ("error", Value::Null),
            ]),
            diagnostics: Vec::new(),
            limit_reported: false,
            runtime_errors: 0,
        }
    }

    /// An evaluator for state defaults, which run before any state
    /// exists.
    pub(crate) fn for_defaults(document: &'d Document) -> Self {
        Self::new(document, None)
    }

    pub(crate) fn set_scope(&mut self, scope: &[(Arc<str>, Value)]) {
        self.scope = scope.to_vec();
    }

    /// The statement that errors outside any component are reported
    /// against.
    pub(crate) fn set_statement(&mut self, name: &Arc<str>) {
        self.statement = Some(name.clone());
    }

    pub(crate) fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    pub(crate) fn eval(&mut self, node: &Node) -> Value {
        if self.depth >= self.max_depth || !self.budget.steps(1) {
            self.report_limit();
            return Value::Null;
        }
        self.depth += 1;
        let value = self.eval_inner(node);
        self.depth -= 1;
        if self.budget.exhausted() {
            self.report_limit();
        }
        value
    }

    fn eval_inner(&mut self, node: &Node) -> Value {
        match node {
            Node::Lit(value) => value.clone(),
            Node::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item));
                }
                Value::array(values)
            }
            Node::Object(entries) => {
                let mut map = Map::new();
                for (key, value) in entries {
                    let value = self.eval(value);
                    if !self.budget.scan(key.len()) {
                        return Value::Null;
                    }
                    map.insert(key.clone(), value);
                }
                Value::from(map)
            }
            Node::Element(element) => self.element(element),
            Node::State(name) | Node::Assign(name, _) => {
                self.state(name)
            }
            Node::Query(name) => self.query(name),
            Node::Mutation(name) => {
                if !self.budget.scan(name.len()) {
                    return Value::Null;
                }
                self.inputs
                    .and_then(|inputs| inputs.mutations.get(&**name))
                    .unwrap_or(&self.idle_mutation)
                    .clone()
            }
            Node::LoopVar(name) => {
                // The lookup compares the name with each enclosing
                // loop's.
                if !self
                    .budget
                    .scan(name.len().saturating_mul(self.scope.len()))
                {
                    return Value::Null;
                }
                self.scope
                    .iter()
                    .rev()
                    .find(|(var, _)| var == name)
                    .map_or(Value::Null, |(_, value)| value.clone())
            }
            Node::Unary(op, operand) => {
                let value = self.eval(operand);
                match op {
                    UnaryOp::Not => Value::Bool(!js::truthy(&value)),
                    UnaryOp::Neg => self
                        .budget
                        .number(&value)
                        .map_or(Value::Null, |n| Value::Number(-n)),
                }
            }
            Node::Binary(op, left, right) => {
                self.binary(*op, left, right)
            }
            Node::Ternary(cond, then, otherwise) => {
                if js::truthy(&self.eval(cond)) {
                    self.eval(then)
                } else {
                    self.eval(otherwise)
                }
            }
            Node::Member(object, field) => self.member(object, field),
            Node::Index(object, index) => self.index(object, index),
            Node::Builtin(builtin, args) => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval(arg));
                }
                builtin
                    .call(&values, &mut self.budget)
                    .unwrap_or(Value::Null)
            }
            Node::Each {
                array,
                var,
                template,
            } => {
                let Value::Array(items) = self.eval(array) else {
                    return Value::array([]);
                };
                let mut out = Vec::with_capacity(items.len());
                for item in items.iter() {
                    if self.budget.exhausted() {
                        break;
                    }
                    self.scope.push((var.clone(), item.clone()));
                    out.push(self.eval(template));
                    self.scope.pop();
                }
                Value::array(out)
            }
            Node::Function(index, args) => {
                self.function(*index, args)
            }
            Node::ToAssistant(message, context) => {
                let message = self.eval(message);
                let message = self
                    .budget
                    .string_or_empty(&message)
                    .unwrap_or_default();
                // The context may be any value and passes through
                // unchanged; null means none.
                let context = context
                    .as_ref()
                    .map(|c| self.eval(c))
                    .filter(|c| !c.is_null());
                plan(vec![ActionStep::ToAssistant {
                    message,
                    context,
                }])
            }
            Node::OpenUrl(url) => {
                let url = self.eval(url);
                let url = self
                    .budget
                    .string_or_empty(&url)
                    .unwrap_or_default();
                plan(vec![ActionStep::OpenUrl { url }])
            }
            Node::Set(target, value) => {
                if !self.budget.copy(target.len())
                    || !self.budget.steps(self.scope.len())
                {
                    return Value::Null;
                }
                plan(vec![ActionStep::Set {
                    target: target.to_string(),
                    value: Deferred {
                        node: value.clone(),
                        scope: self.scope.clone().into(),
                        library: self.document.library.clone(),
                    },
                }])
            }
            Node::Reset(targets) => {
                if !self
                    .budget
                    .copy(targets.iter().map(|t| t.len()).sum())
                {
                    return Value::Null;
                }
                plan(vec![ActionStep::Reset {
                    targets: targets
                        .iter()
                        .map(|t| t.to_string())
                        .collect(),
                }])
            }
            Node::Run(statement, kind) => {
                if !self.budget.copy(statement.len()) {
                    return Value::Null;
                }
                plan(vec![ActionStep::Run {
                    statement: statement.to_string(),
                    kind: *kind,
                }])
            }
            Node::CustomAction(index, args) => {
                self.custom_action(*index, args)
            }
            // `Action([...])` flattens the plans in the list; other
            // values are dropped.
            Node::ActionList(list) => {
                let mut steps = Vec::new();
                if let Value::Array(items) = self.eval(list) {
                    for item in items.iter() {
                        if let Value::Action(plan) = item {
                            // Flattening copies every step's text.
                            let text = plan
                                .steps
                                .iter()
                                .map(step_text_len)
                                .sum();
                            if !self.budget.steps(plan.steps.len())
                                || !self.budget.copy(text)
                            {
                                return Value::Null;
                            }
                            steps.extend(plan.steps.iter().cloned());
                        }
                    }
                }
                plan(steps)
            }
            Node::NoAction => plan(Vec::new()),
        }
    }

    fn element(&mut self, element: &ElementNode) -> Value {
        let document = self.document;
        let params =
            document.library.components()[element.component].params();
        let statement = element
            .statement
            .clone()
            .or_else(|| {
                self.site.as_ref().and_then(|s| s.statement.clone())
            })
            .or_else(|| self.statement.clone());
        let mut props = Vec::with_capacity(element.props.len());
        for (index, name, node) in &element.props {
            let outer = self.site.replace(Site {
                component: element.name.clone(),
                prop: name.clone(),
                statement: statement.clone(),
            });
            // A `$variable` passed to a bindable prop binds it (spec
            // section 5.2).
            let (value, binding) = match node {
                Node::State(var) | Node::Assign(var, _)
                    if params[*index].is_bindable() =>
                {
                    (self.state(var), Some(var.clone()))
                }
                node => (self.eval(node), None),
            };
            self.site = outer;
            if value.is_null() && binding.is_none() {
                continue;
            }
            props.push(Prop {
                name: name.clone(),
                value,
                binding,
            });
        }
        Value::Element(Arc::new(Element {
            component: element.name.clone(),
            statement: element.statement.clone(),
            props,
        }))
    }

    /// The host's value, else the declared default, else null.
    fn state(&mut self, name: &str) -> Value {
        if !self.budget.scan(name.len()) {
            return Value::Null;
        }
        if let Some(value) =
            self.inputs.and_then(|inputs| inputs.state.get(name))
        {
            return value.clone();
        }
        self.document
            .state_default(name)
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// The latest result, else the defaults, which are evaluated
    /// once.
    fn query(&mut self, name: &Arc<str>) -> Value {
        let lookups = 1 + self.queries_running.len();
        if !self.budget.scan(name.len().saturating_mul(lookups)) {
            return Value::Null;
        }
        if let Some(value) =
            self.inputs.and_then(|inputs| inputs.queries.get(&**name))
        {
            return value.clone();
        }
        if let Some(value) = self.queries.get(name) {
            return value.clone();
        }
        if self.queries_running.contains(name) {
            return Value::Null;
        }
        let document = self.document;
        let Some(defaults) =
            document.query(name).and_then(|q| q.defaults.as_ref())
        else {
            return Value::Null;
        };
        self.queries_running.push(name.clone());
        let statement = self.statement.replace(name.clone());
        let site = self.site.take();
        let value = self.eval(defaults);
        self.site = site;
        self.statement = statement;
        self.queries_running.pop();
        self.queries.insert(name.clone(), value.clone());
        value
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        left: &Node,
        right: &Node,
    ) -> Value {
        match op {
            BinaryOp::Or => {
                let left = self.eval(left);
                return if js::truthy(&left) {
                    left
                } else {
                    self.eval(right)
                };
            }
            BinaryOp::And => {
                let left = self.eval(left);
                return if js::truthy(&left) {
                    self.eval(right)
                } else {
                    left
                };
            }
            _ => {}
        }
        let left = self.eval(left);
        let right = self.eval(right);
        let joins = matches!(left, Value::String(_))
            || matches!(right, Value::String(_));
        match op {
            BinaryOp::Add if joins => {
                // Joining strings: null becomes "".
                let (Some(mut joined), Some(right)) = (
                    self.budget.string_or_empty(&left),
                    self.budget.string_or_empty(&right),
                ) else {
                    return Value::Null;
                };
                joined.push_str(&right);
                return Value::from(joined);
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                let Some(equal) =
                    self.budget.loosely_equal(&left, &right)
                else {
                    return Value::Null;
                };
                return Value::Bool(equal == (op == BinaryOp::Eq));
            }
            _ => {}
        }
        let (Some(l), Some(r)) =
            (self.budget.number(&left), self.budget.number(&right))
        else {
            return Value::Null;
        };
        let number = match op {
            BinaryOp::Add => l + r,
            BinaryOp::Sub => l - r,
            BinaryOp::Mul => l * r,
            // A divisor of 0 gives 0.
            BinaryOp::Div => {
                if r == 0.0 {
                    0.0
                } else {
                    l / r
                }
            }
            BinaryOp::Rem => {
                if r == 0.0 {
                    0.0
                } else {
                    l % r
                }
            }
            BinaryOp::Gt => return Value::Bool(l > r),
            BinaryOp::Lt => return Value::Bool(l < r),
            BinaryOp::Ge => return Value::Bool(l >= r),
            BinaryOp::Le => return Value::Bool(l <= r),
            BinaryOp::Eq | BinaryOp::Ne => {
                unreachable!("compared above")
            }
            BinaryOp::Or | BinaryOp::And => {
                unreachable!("short-circuited above")
            }
        };
        Value::Number(number)
    }

    /// `a.b`: a field of an object, a pluck on an array, null on null
    /// (spec section 3.3).
    fn member(&mut self, object: &Node, field: &Arc<str>) -> Value {
        let object = self.eval(object);
        match &object {
            Value::Array(items) if &**field == "length" => {
                Value::Number(items.len() as f64)
            }
            Value::Array(items) => {
                if !self.budget.steps(items.len()) {
                    return Value::Null;
                }
                let mut plucked = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let Some(value) =
                        self.budget.property(item, field)
                    else {
                        return Value::Null;
                    };
                    plucked.push(value);
                }
                Value::array(plucked)
            }
            other => self
                .budget
                .property(other, field)
                .unwrap_or(Value::Null),
        }
    }

    /// `a[i]`: arrays by number, everything else by string key.
    fn index(&mut self, object: &Node, index: &Node) -> Value {
        let object = self.eval(object);
        let index = self.eval(index);
        if object.is_null() || index.is_null() {
            return Value::Null;
        }
        if let Value::Array(items) = &object {
            let Some(i) = self.budget.number(&index) else {
                return Value::Null;
            };
            let valid = i >= 0.0
                && i.fract() == 0.0
                && i < items.len() as f64;
            return if valid {
                items[i as usize].clone()
            } else {
                Value::Null
            };
        }
        self.budget
            .string(&index)
            .and_then(|key| self.budget.property(&object, &key))
            .unwrap_or(Value::Null)
    }

    fn function(
        &mut self,
        index: usize,
        args: &[Option<Node>],
    ) -> Value {
        let document = self.document;
        let def = &document.library.functions()[index];
        let Some(args) =
            self.call_args(def.name(), def.params(), args, false)
        else {
            return Value::Null;
        };
        let result = match def.call(&args) {
            Ok(result) => result,
            Err(error) => {
                self.report_runtime(format!(
                    "@{} threw: {error}",
                    def.name()
                ));
                return Value::Null;
            }
        };
        let Some(returns) = def.return_type() else {
            return result;
        };
        let mut errors = Vec::new();
        let checked = check_value(
            result,
            returns,
            "/returns",
            &mut errors,
            &mut self.budget,
        );
        if self.budget.exhausted() {
            return Value::Null;
        }
        for error in errors {
            self.report_runtime(format!("@{}: {error}", def.name()));
        }
        checked.unwrap_or(Value::Null)
    }

    /// A library action is a one-step plan; any invalid argument
    /// makes it an empty one.
    fn custom_action(
        &mut self,
        index: usize,
        args: &[Option<Node>],
    ) -> Value {
        let document = self.document;
        let def = &document.library.actions()[index];
        match self.call_args(def.name(), def.params(), args, true) {
            Some(params) => plan(vec![ActionStep::Custom {
                name: def.name().to_owned(),
                params,
            }]),
            None => plan(Vec::new()),
        }
    }

    /// Evaluates and checks call arguments (spec section 3.6). `None`
    /// means the call does not happen. `strict` rejects any
    /// invalid argument.
    fn call_args(
        &mut self,
        name: &str,
        params: &[Param],
        args: &[Option<Node>],
        strict: bool,
    ) -> Option<Map> {
        let mut map = Map::new();
        for (param, arg) in params.iter().zip(args) {
            let value = match arg {
                Some(node) => self.eval(node),
                None => Value::Null,
            };
            // An explicit null for an optional parameter is left out,
            // so its default applies.
            if value.is_null() && !param.is_required() {
                continue;
            }
            map.insert(param.name(), value);
        }
        // A required argument that is null with no default: no call
        // and no error.
        let unset =
            |p: &Param| map.get(p.name()).is_none_or(Value::is_null);
        if params.iter().any(|p| {
            p.is_required() && p.default_value().is_none() && unset(p)
        }) {
            return None;
        }
        let mut errors = Vec::new();
        let checked = check_fields(
            map,
            params,
            "",
            &mut errors,
            &mut self.budget,
        );
        if self.budget.exhausted() {
            return None;
        }
        let failed = !errors.is_empty();
        for error in errors {
            self.report_runtime(format!("@{name}: {error}"));
        }
        let mut map = checked.filter(|_| !(strict && failed))?;
        for param in params {
            if let Some(default) = param.default_value() {
                if !map.contains_key(param.name()) {
                    map.insert(param.name(), default.clone());
                }
            }
        }
        Some(map)
    }

    /// Evaluates a query's or mutation's tool name, arguments and
    /// refresh interval. `None` when the budget runs out: a
    /// request with parts left out would call the tool with the
    /// wrong arguments.
    pub(crate) fn request(
        &mut self,
        node: &QueryNode,
    ) -> Option<ToolRequest> {
        let statement = self.statement.replace(node.name.clone());
        let tool = match &node.tool {
            Some(tool) => self.eval(tool),
            None => Value::Null,
        };
        let tool = self.budget.string_or_empty(&tool);
        let args =
            match node.args.as_ref().map(|args| self.eval(args)) {
                Some(args) if !args.is_null() => args,
                _ => Value::from(Map::new()),
            };
        let refresh_seconds = match &node.refresh {
            Some(refresh) => {
                let refresh = self.eval(refresh);
                self.budget.number(&refresh)
            }
            None => None,
        }
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0);
        self.statement = statement;
        if self.budget.exhausted() {
            return None;
        }
        Some(ToolRequest {
            statement: node.name.to_string(),
            tool: tool?,
            args,
            refresh_seconds,
        })
    }

    fn report_runtime(&mut self, message: String) {
        // While streaming, arguments may not be valid yet; errors
        // wait for the end.
        if !self.document.complete {
            return;
        }
        self.runtime_errors += 1;
        if self.runtime_errors > MAX_RUNTIME_ERRORS {
            if self.runtime_errors == MAX_RUNTIME_ERRORS + 1 {
                let message = format!(
                    "more than {MAX_RUNTIME_ERRORS} runtime errors; \
                     the rest are not reported"
                );
                let mut diagnostic = Diagnostic::new(
                    Source::Runtime,
                    Code::LimitExceeded,
                    message,
                );
                diagnostic.statement =
                    self.statement.as_deref().map(str::to_owned);
                self.diagnostics.push(diagnostic);
            }
            return;
        }
        let mut diagnostic = Diagnostic::new(
            Source::Runtime,
            Code::RuntimeError,
            message,
        );
        match &self.site {
            Some(site) => {
                diagnostic.message = format!(
                    "evaluating prop \"{}\" of {} failed: {}",
                    site.prop, site.component, diagnostic.message
                );
                diagnostic.hint = Some(format!(
                    "Check the expression for prop \"{}\"",
                    site.prop
                ));
                diagnostic.component =
                    Some(site.component.to_string());
                diagnostic.statement =
                    site.statement.as_deref().map(str::to_owned);
                diagnostic.path = Some(format!("/{}", site.prop));
            }
            None => {
                diagnostic.statement =
                    self.statement.as_deref().map(str::to_owned)
            }
        }
        self.diagnostics.push(diagnostic);
    }

    fn report_limit(&mut self) {
        if self.limit_reported {
            return;
        }
        self.limit_reported = true;
        let limits = self.document.limits;
        let message = format!(
            "evaluation went past a limit ({} steps, nesting {}, or \
             {} bytes of built strings); the rest is null",
            limits.max_steps, self.max_depth, limits.max_string_bytes
        );
        let mut diagnostic = Diagnostic::new(
            Source::Runtime,
            Code::LimitExceeded,
            message,
        );
        diagnostic.statement = self
            .site
            .as_ref()
            .and_then(|site| site.statement.as_deref())
            .or(self.statement.as_deref())
            .map(str::to_owned);
        self.diagnostics.push(diagnostic);
    }
}

fn plan(steps: Vec<ActionStep>) -> Value {
    Value::Action(Arc::new(ActionPlan { steps }))
}

/// The bytes of text a step holds, which copying it copies.
fn step_text_len(step: &ActionStep) -> usize {
    match step {
        ActionStep::ToAssistant { message, .. } => message.len(),
        ActionStep::OpenUrl { url } => url.len(),
        ActionStep::Set { target, .. } => target.len(),
        ActionStep::Reset { targets } => {
            targets.iter().map(String::len).sum()
        }
        ActionStep::Run { statement, .. } => statement.len(),
        ActionStep::Custom { name, .. } => name.len(),
    }
}

/// Reads `value[key]` the way JavaScript does for the values the
/// language has: array elements and `length`, string code units and
/// `length`, object fields. Everything else, including component
/// instances, reads null.
pub(crate) fn property(value: &Value, key: &str) -> Value {
    match value {
        Value::Array(items) => {
            if key == "length" {
                return Value::Number(items.len() as f64);
            }
            array_index(key)
                .and_then(|i| items.get(i as usize))
                .cloned()
                .unwrap_or(Value::Null)
        }
        Value::String(s) => {
            if key == "length" {
                return Value::Number(s.encode_utf16().count() as f64);
            }
            // JavaScript strings index UTF-16 code units; half a
            // surrogate pair reads as U+FFFD.
            array_index(key)
                .and_then(|i| s.encode_utf16().nth(i as usize))
                .map_or(Value::Null, |unit| {
                    let c = char::from_u32(u32::from(unit))
                        .unwrap_or(char::REPLACEMENT_CHARACTER);
                    Value::from(c.to_string())
                })
        }
        Value::Object(map) => {
            map.get(key).cloned().unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

/// The type name of a value, as error messages print it.
pub(crate) fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) | Value::Action(_) => "object",
        Value::Element(_) => "component",
    }
}

/// Checks a runtime value against a type, with the recovery of spec
/// section 8.2: invalid array items are pruned, invalid optional
/// fields removed or defaulted. `None` means the value is invalid or
/// the budget ran out. Problems go to `errors`. Each value visited
/// costs a step.
pub(crate) fn check_value(
    value: Value,
    ty: &Type,
    path: &str,
    errors: &mut Vec<String>,
    budget: &mut Budget,
) -> Option<Value> {
    if !budget.steps(1) {
        return None;
    }
    let mismatch = |errors: &mut Vec<String>,
                    expected: &str,
                    actual: &str| {
        errors.push(format!(
            "field \"{path}\" expects {expected} but got {actual}"
        ));
    };
    match &value {
        // Absence is checked by the parent's required fields.
        Value::Null => return Some(value),
        Value::Element(element) => {
            if rejects_components(ty) {
                let actual =
                    format!("component {}", element.component);
                mismatch(errors, &expected_name(ty), &actual);
                return None;
            }
            return Some(value);
        }
        _ => {}
    }
    if ty.is_component_slot() {
        mismatch(errors, &expected_name(ty), value_kind(&value));
        return None;
    }
    match ty {
        Type::Any
        | Type::Action(_)
        | Type::Union(_)
        | Type::Component(_) => Some(value),
        Type::String | Type::Number | Type::Boolean => {
            if value_kind(&value) == expected_name(ty) {
                Some(value)
            } else {
                mismatch(
                    errors,
                    &expected_name(ty),
                    value_kind(&value),
                );
                None
            }
        }
        Type::Enum(values) => {
            let scalar = matches!(
                value,
                Value::Bool(_) | Value::Number(_) | Value::String(_)
            );
            if !scalar {
                return Some(value);
            }
            // A string is compared with each allowed value.
            if !budget
                .scan(text_len(&value).saturating_mul(values.len()))
            {
                return None;
            }
            if !values.iter().any(|v| same_value_zero(v, &value)) {
                mismatch(
                    errors,
                    &expected_name(ty),
                    &js::quote(&value),
                );
                return None;
            }
            Some(value)
        }
        Type::List(item) => match &value {
            Value::Array(_) if matches!(**item, Type::Any) => {
                Some(value)
            }
            Value::Array(items) => {
                let mut kept = Vec::with_capacity(items.len());
                for (i, v) in items.iter().enumerate() {
                    let path = format!("{path}/{i}");
                    if let Some(v) = check_value(
                        v.clone(),
                        item,
                        &path,
                        errors,
                        budget,
                    ) {
                        kept.push(v);
                    }
                    if budget.exhausted() {
                        return None;
                    }
                }
                Some(Value::array(kept))
            }
            _ => {
                mismatch(errors, "array", value_kind(&value));
                None
            }
        },
        Type::Object(fields) => match &value {
            Value::Object(map) => {
                if !budget.steps(map.len()) {
                    return None;
                }
                check_fields(
                    (**map).clone(),
                    fields,
                    path,
                    errors,
                    budget,
                )
                .map(Value::from)
            }
            _ => {
                mismatch(errors, "object", value_kind(&value));
                None
            }
        },
    }
}

/// Checks an object's declared fields. Absent or null required fields
/// take their defaults silently; without one the object is invalid.
fn check_fields(
    mut map: Map,
    fields: &[Param],
    path: &str,
    errors: &mut Vec<String>,
    budget: &mut Budget,
) -> Option<Map> {
    let mut invalid = false;
    for field in fields.iter().filter(|f| f.is_required()) {
        let present = map.get(field.name());
        if present.is_some_and(|v| !v.is_null()) {
            continue;
        }
        if let Some(default) = field.default_value() {
            map.insert(field.name(), default.clone());
            continue;
        }
        let field_path = format!("{path}/{}", field.name());
        errors.push(match present {
            Some(_) => {
                format!("required field \"{field_path}\" is null")
            }
            None => {
                format!("missing required field \"{field_path}\"")
            }
        });
        invalid = true;
    }
    for field in fields {
        let Some(value) = map.get(field.name()).cloned() else {
            continue;
        };
        let field_path = format!("{path}/{}", field.name());
        let checked = check_value(
            value,
            field.ty(),
            &field_path,
            errors,
            budget,
        );
        if budget.exhausted() {
            return None;
        }
        match checked {
            Some(value) => map.insert(field.name(), value),
            None => match field.default_value() {
                Some(default) => {
                    map.insert(field.name(), default.clone())
                }
                None if field.is_required() => invalid = true,
                None => {
                    map.remove(field.name());
                }
            },
        }
    }
    (!invalid).then_some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_follow_javascript_reads() {
        let s = Value::from("h€😀");
        assert_eq!(property(&s, "length"), Value::from(4));
        assert_eq!(property(&s, "1"), Value::from("€"));
        assert_eq!(property(&s, "2"), Value::from("\u{FFFD}"));
        assert_eq!(property(&s, "01"), Value::Null);
        let a = Value::array([Value::from(1), Value::from(2)]);
        assert_eq!(property(&a, "1"), Value::from(2));
        assert_eq!(property(&a, "length"), Value::from(2));
        assert_eq!(property(&Value::from(5), "x"), Value::Null);
    }

    #[test]
    fn checked_values_prune_items_and_fields() {
        let ty = Type::list(Type::object([
            Param::required("id", Type::Number),
            Param::optional("tag", Type::one_of(["a", "b"])),
        ]));
        let value = Value::array([
            Value::object([
                ("id", Value::from(1)),
                ("tag", Value::from("c")),
            ]),
            Value::object([("tag", Value::from("a"))]),
            Value::from("x"),
        ]);
        let mut errors = Vec::new();
        let mut budget = Budget::new(&Limits::DEFAULT);
        let checked = check_value(
            value,
            &ty,
            "/rows",
            &mut errors,
            &mut budget,
        )
        .unwrap();
        assert_eq!(
            checked,
            Value::array([Value::object([("id", Value::from(1))])])
        );
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn the_string_budget_bounds_joins_of_shared_values() {
        let mut big = Value::from("x".repeat(1000));
        for _ in 0..20 {
            big = Value::array([big.clone(), big]);
        }
        let mut budget = Budget::new(&Limits::DEFAULT);
        assert_eq!(budget.string(&big), None);
        assert!(budget.exhausted());
        assert_eq!(
            budget.loosely_equal(&big, &Value::from("x")),
            None
        );
    }
}
