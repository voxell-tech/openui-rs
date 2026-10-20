//! Component libraries: the components, functions and actions a host
//! allows.
//!
//! Positional arguments map to parameters in declaration order, so a
//! definition is an ordered list, never a map.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::js;
use crate::value::{Map, Value};

/// Names the language reserves (spec section 1.3). Libraries cannot
/// use them.
pub(crate) const RESERVED_NAMES: &[&str] = &[
    "true",
    "false",
    "null",
    "Query",
    "Mutation",
    "Action",
    "Count",
    "First",
    "Last",
    "Sum",
    "Avg",
    "Min",
    "Max",
    "Filter",
    "Sort",
    "Round",
    "Abs",
    "Floor",
    "Ceil",
    "Each",
    "Set",
    "Reset",
    "Run",
    "ToAssistant",
    "OpenUrl",
];

/// Built-in actions a restricted action slot may name.
const BUILTIN_ACTION_REFS: &[&str] = &["OpenUrl", "ToAssistant"];

/// The type a parameter accepts. It drives validation and the
/// signature shown to models.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Type {
    /// Anything.
    Any,
    /// A string.
    String,
    /// A number.
    Number,
    /// A boolean.
    Boolean,
    /// One of the listed strings, numbers or booleans.
    Enum(Vec<Value>),
    /// A list written with `[...]`. Items of type [`Type::Any`] are
    /// unchecked.
    List(Box<Type>),
    /// An object with the listed fields. Unlisted keys are kept
    /// unchecked.
    Object(Vec<Param>),
    /// A component. An empty list accepts any component; names only
    /// shape the signature.
    Component(Vec<String>),
    /// An action. An empty list accepts any action; names only shape
    /// the signature.
    Action(Vec<String>),
    /// Any of several types. Only a union of components is checked
    /// when parsing.
    Union(Vec<Type>),
}

impl Type {
    /// One of the given literal values.
    pub fn one_of<V: Into<Value>>(
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        Self::Enum(values.into_iter().map(Into::into).collect())
    }

    /// A list of `item`.
    pub fn list(item: Type) -> Self {
        Self::List(Box::new(item))
    }

    /// An object with the given fields, in order.
    pub fn object(fields: impl IntoIterator<Item = Param>) -> Self {
        Self::Object(fields.into_iter().collect())
    }

    /// Any component.
    pub fn component() -> Self {
        Self::Component(Vec::new())
    }

    /// One of the named components.
    pub fn components<S: Into<String>>(
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        Self::Component(names.into_iter().map(Into::into).collect())
    }

    /// Any action, printed as `ActionExpression`.
    pub fn action() -> Self {
        Self::Action(Vec::new())
    }

    /// One of the named actions, printed as `@Name | ...`.
    pub fn actions<S: Into<String>>(
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        Self::Action(names.into_iter().map(Into::into).collect())
    }

    /// Any of the given types.
    pub fn union(types: impl IntoIterator<Item = Type>) -> Self {
        Self::Union(types.into_iter().collect())
    }

    /// True when every option is a component, so data in this slot is
    /// a type mismatch.
    pub(crate) fn is_component_slot(&self) -> bool {
        match self {
            Self::Component(_) => true,
            Self::Union(options) => {
                !options.is_empty()
                    && options.iter().all(Self::is_component_slot)
            }
            _ => false,
        }
    }

    fn write_signature(&self, out: &mut String) {
        match self {
            Self::Any => out.push_str("any"),
            Self::Component(names) if names.is_empty() => {
                out.push_str("any")
            }
            Self::String => out.push_str("string"),
            Self::Number => out.push_str("number"),
            Self::Boolean => out.push_str("boolean"),
            Self::Enum(values) => {
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" | ");
                    }
                    match v {
                        Value::String(s) => {
                            out.push('"');
                            out.push_str(s);
                            out.push('"');
                        }
                        other => out.push_str(&js::to_string(other)),
                    }
                }
            }
            Self::List(item) => {
                let grouped = item.is_alternation();
                if grouped {
                    out.push('(');
                }
                item.write_signature(out);
                if grouped {
                    out.push(')');
                }
                out.push_str("[]");
            }
            Self::Object(fields) => {
                out.push('{');
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    field.write_signature(out);
                }
                out.push('}');
            }
            Self::Component(names) => {
                out.push_str(&names.join(" | "))
            }
            Self::Action(names) if names.is_empty() => {
                out.push_str("ActionExpression")
            }
            Self::Action(names) => {
                for (i, name) in names.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" | ");
                    }
                    out.push('@');
                    out.push_str(name);
                }
            }
            Self::Union(options) => {
                for (i, option) in options.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" | ");
                    }
                    option.write_signature(out);
                }
            }
        }
    }

    /// True when the printed form has top-level `|`, so a list must
    /// parenthesize it.
    fn is_alternation(&self) -> bool {
        match self {
            Self::Enum(values) => values.len() > 1,
            Self::Component(names) | Self::Action(names) => {
                names.len() > 1
            }
            Self::Union(options) => options.len() > 1,
            _ => false,
        }
    }
}

/// A positional parameter of a component, function or action, or a
/// field of an object type.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    name: String,
    ty: Type,
    required: bool,
    default: Option<Value>,
    bindable: bool,
}

impl Param {
    /// A parameter that must be present and not null.
    pub fn required(name: impl Into<String>, ty: Type) -> Self {
        Self::new(name.into(), ty, true)
    }

    /// A parameter that may be left out or passed as `null`.
    pub fn optional(name: impl Into<String>, ty: Type) -> Self {
        Self::new(name.into(), ty, false)
    }

    fn new(name: String, ty: Type, required: bool) -> Self {
        Self {
            name,
            ty,
            required,
            default: None,
            bindable: false,
        }
    }

    /// The value used when the argument is missing or invalid.
    pub fn with_default(mut self, value: impl Into<Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    /// Marks a component prop as two-way bindable to a `$variable`
    /// (spec section 5.2).
    pub fn bindable(mut self) -> Self {
        self.bindable = true;
        self
    }

    /// The parameter name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The accepted type.
    pub fn ty(&self) -> &Type {
        &self.ty
    }

    /// True when the parameter is required.
    pub fn is_required(&self) -> bool {
        self.required
    }

    /// The default value, if any.
    pub fn default_value(&self) -> Option<&Value> {
        self.default.as_ref()
    }

    /// True when the prop binds to a `$variable`.
    pub fn is_bindable(&self) -> bool {
        self.bindable
    }

    fn write_signature(&self, out: &mut String) {
        out.push_str(&self.name);
        if !self.required {
            out.push('?');
        }
        out.push_str(": ");
        if self.bindable {
            out.push_str("$binding<");
            self.ty.write_signature(out);
            out.push('>');
        } else {
            self.ty.write_signature(out);
        }
    }
}

fn write_call_signature(
    name: &str,
    params: &[Param],
    out: &mut String,
) {
    out.push_str(name);
    out.push('(');
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        param.write_signature(out);
    }
    out.push(')');
}

/// The short form used in error hints: names only, required ones
/// starred.
pub(crate) fn hint_signature(name: &str, params: &[Param]) -> String {
    let names: Vec<String> = params
        .iter()
        .map(|p| {
            if p.required {
                format!("{}*", p.name)
            } else {
                p.name.clone()
            }
        })
        .collect();
    format!(
        "Signature: {name}({}). * marks required",
        names.join(", ")
    )
}

/// A component the host can render.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentDef {
    name: String,
    description: String,
    params: Vec<Param>,
}

impl ComponentDef {
    /// A component with no props yet.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            params: Vec::new(),
        }
    }

    /// Appends the next positional prop.
    pub fn param(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }

    /// The component name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The description shown to models.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The props in positional order.
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// The model-facing signature, for example `Text(text: string,
    /// variant?: "body" | "heading")`.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        write_call_signature(&self.name, &self.params, &mut out);
        out
    }
}

/// The error a custom function returns to make its call evaluate to
/// null.
pub type FunctionError = Box<dyn std::error::Error + Send + Sync>;

type FunctionImpl =
    dyn Fn(&Map) -> Result<Value, FunctionError> + Send + Sync;

/// A pure, synchronous function programs call as `@Name(...)` (spec
/// section 3.6).
///
/// The implementation receives the checked arguments keyed by
/// parameter name. The runtime may call it any number of times, in
/// any order.
#[derive(Clone)]
pub struct FunctionDef {
    name: String,
    description: String,
    params: Vec<Param>,
    returns: Option<Type>,
    implementation: Arc<FunctionImpl>,
}

impl FunctionDef {
    /// A function with no parameters yet.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        implementation: impl Fn(&Map) -> Result<Value, FunctionError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            params: Vec::new(),
            returns: None,
            implementation: Arc::new(implementation),
        }
    }

    /// Appends the next positional parameter.
    pub fn param(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }

    /// Declares the return type. A result that does not match it
    /// reports `runtime-error`.
    pub fn returns(mut self, ty: Type) -> Self {
        self.returns = Some(ty);
        self
    }

    /// The function name, without `@`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The description shown to models.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The parameters in positional order.
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// The declared return type.
    pub fn return_type(&self) -> Option<&Type> {
        self.returns.as_ref()
    }

    /// The model-facing signature, for example `Upper(text: string) →
    /// string`.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        write_call_signature(&self.name, &self.params, &mut out);
        if let Some(returns) = &self.returns {
            out.push_str(" → ");
            returns.write_signature(&mut out);
        }
        out
    }

    pub(crate) fn call(
        &self,
        args: &Map,
    ) -> Result<Value, FunctionError> {
        (self.implementation)(args)
    }
}

impl fmt::Debug for FunctionDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FunctionDef")
            .field("name", &self.name)
            .field("params", &self.params)
            .field("returns", &self.returns)
            .finish_non_exhaustive()
    }
}

/// A custom action programs trigger as `@Name(...)`. The host carries
/// it out.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionDef {
    name: String,
    description: String,
    params: Vec<Param>,
}

impl ActionDef {
    /// An action with no parameters yet.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            params: Vec::new(),
        }
    }

    /// Appends the next positional parameter.
    pub fn param(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }

    /// The action name, without `@`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The description shown to models.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The parameters in positional order.
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// The model-facing signature, for example `CopyToClipboard(text:
    /// string)`.
    pub fn signature(&self) -> String {
        let mut out = String::new();
        write_call_signature(&self.name, &self.params, &mut out);
        out
    }
}

/// Why a library definition was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LibraryError {
    /// The name is not an identifier.
    #[error("`{0}` is not a valid identifier")]
    InvalidName(String),
    /// Component calls need an uppercase first letter.
    #[error("component `{0}` must start with an uppercase letter")]
    LowercaseComponent(String),
    /// The name is reserved by the language.
    #[error("`{0}` is reserved by OpenUI Lang")]
    Reserved(String),
    /// Components, functions and actions share one namespace.
    #[error("`{0}` is defined more than once")]
    Duplicate(String),
    /// A definition repeats a parameter name.
    #[error("`{owner}` declares `{param}` more than once")]
    DuplicateParam {
        /// The component, function or action.
        owner: String,
        /// The repeated parameter.
        param: String,
    },
    /// The root component is not defined.
    #[error("root component `{0}` is not defined")]
    UnknownRoot(String),
    /// A type names a component the library does not define.
    #[error("`{owner}` refers to unknown component `{name}`")]
    UnknownComponent {
        /// The definition holding the reference.
        owner: String,
        /// The missing component.
        name: String,
    },
    /// A type names an action the library does not define.
    #[error("`{owner}` refers to unknown action `{name}`")]
    UnknownAction {
        /// The definition holding the reference.
        owner: String,
        /// The missing action.
        name: String,
    },
    /// An enum lists something other than a string, number or
    /// boolean.
    #[error(
        "`{owner}` has an enum value that is not a string, number \
         or boolean"
    )]
    InvalidEnum {
        /// The definition holding the enum.
        owner: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Entry {
    Component(usize),
    Function(usize),
    Action(usize),
}

#[derive(Debug)]
struct Inner {
    root: Option<String>,
    components: Vec<ComponentDef>,
    functions: Vec<FunctionDef>,
    actions: Vec<ActionDef>,
    names: HashMap<String, Entry>,
}

/// A validated set of components, functions and actions. Cloning is
/// cheap.
#[derive(Clone, Debug)]
pub struct Library(Arc<Inner>);

impl Library {
    /// Starts a library definition.
    pub fn builder() -> LibraryBuilder {
        LibraryBuilder::default()
    }

    /// The root component used to pick an entry when there is no
    /// `root` statement.
    pub fn root(&self) -> Option<&str> {
        self.0.root.as_deref()
    }

    /// The components in declaration order.
    pub fn components(&self) -> &[ComponentDef] {
        &self.0.components
    }

    /// The custom functions in declaration order.
    pub fn functions(&self) -> &[FunctionDef] {
        &self.0.functions
    }

    /// The custom actions in declaration order.
    pub fn actions(&self) -> &[ActionDef] {
        &self.0.actions
    }

    /// The component named `name`.
    pub fn component(&self, name: &str) -> Option<&ComponentDef> {
        match self.0.names.get(name)? {
            Entry::Component(i) => Some(&self.0.components[*i]),
            _ => None,
        }
    }

    /// The custom function named `name`.
    pub fn function(&self, name: &str) -> Option<&FunctionDef> {
        match self.0.names.get(name)? {
            Entry::Function(i) => Some(&self.0.functions[*i]),
            _ => None,
        }
    }

    /// The custom action named `name`.
    pub fn action(&self, name: &str) -> Option<&ActionDef> {
        match self.0.names.get(name)? {
            Entry::Action(i) => Some(&self.0.actions[*i]),
            _ => None,
        }
    }

    pub(crate) fn entry(&self, name: &str) -> Option<Entry> {
        self.0.names.get(name).copied()
    }

    /// True when both handles share one definition.
    pub(crate) fn same(&self, other: &Library) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Collects definitions for [`Library::builder`].
#[derive(Debug, Default)]
pub struct LibraryBuilder {
    root: Option<String>,
    components: Vec<ComponentDef>,
    functions: Vec<FunctionDef>,
    actions: Vec<ActionDef>,
}

impl LibraryBuilder {
    /// Adds a component.
    pub fn component(mut self, component: ComponentDef) -> Self {
        self.components.push(component);
        self
    }

    /// Adds a custom function.
    pub fn function(mut self, function: FunctionDef) -> Self {
        self.functions.push(function);
        self
    }

    /// Adds a custom action.
    pub fn action(mut self, action: ActionDef) -> Self {
        self.actions.push(action);
        self
    }

    /// Names the root component (spec section 2.2).
    pub fn root(mut self, name: impl Into<String>) -> Self {
        self.root = Some(name.into());
        self
    }

    /// Validates the definitions.
    pub fn build(self) -> Result<Library, LibraryError> {
        let mut names = HashMap::new();
        let defs = self
            .components
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    c.name.as_str(),
                    Entry::Component(i),
                    c.params.as_slice(),
                )
            })
            .chain(self.functions.iter().enumerate().map(|(i, f)| {
                (
                    f.name.as_str(),
                    Entry::Function(i),
                    f.params.as_slice(),
                )
            }))
            .chain(self.actions.iter().enumerate().map(|(i, a)| {
                (
                    a.name.as_str(),
                    Entry::Action(i),
                    a.params.as_slice(),
                )
            }));
        for (name, entry, params) in defs {
            check_name(name)?;
            if matches!(entry, Entry::Component(_))
                && !name.starts_with(|c: char| c.is_ascii_uppercase())
            {
                return Err(LibraryError::LowercaseComponent(
                    name.to_owned(),
                ));
            }
            if names.insert(name.to_owned(), entry).is_some() {
                return Err(LibraryError::Duplicate(name.to_owned()));
            }
            check_params(name, params)?;
        }
        if let Some(root) = &self.root {
            if !matches!(names.get(root), Some(Entry::Component(_))) {
                return Err(LibraryError::UnknownRoot(root.clone()));
            }
        }
        let inner = Inner {
            root: self.root,
            components: self.components,
            functions: self.functions,
            actions: self.actions,
            names,
        };
        let all_params =
            inner
                .components
                .iter()
                .map(|c| (c.name.as_str(), c.params.as_slice()))
                .chain(
                    inner.functions.iter().map(|f| {
                        (f.name.as_str(), f.params.as_slice())
                    }),
                )
                .chain(
                    inner.actions.iter().map(|a| {
                        (a.name.as_str(), a.params.as_slice())
                    }),
                );
        for (owner, params) in all_params {
            for param in params {
                check_refs(owner, &param.ty, &inner.names)?;
            }
        }
        for function in &inner.functions {
            if let Some(returns) = &function.returns {
                check_refs(&function.name, returns, &inner.names)?;
            }
        }
        Ok(Library(Arc::new(inner)))
    }
}

fn check_name(name: &str) -> Result<(), LibraryError> {
    if !is_identifier(name) {
        return Err(LibraryError::InvalidName(name.to_owned()));
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(LibraryError::Reserved(name.to_owned()));
    }
    Ok(())
}

pub(crate) fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn check_params(
    owner: &str,
    params: &[Param],
) -> Result<(), LibraryError> {
    for (i, param) in params.iter().enumerate() {
        if params[..i].iter().any(|p| p.name == param.name) {
            return Err(LibraryError::DuplicateParam {
                owner: owner.to_owned(),
                param: param.name.clone(),
            });
        }
    }
    Ok(())
}

fn check_refs(
    owner: &str,
    ty: &Type,
    names: &HashMap<String, Entry>,
) -> Result<(), LibraryError> {
    match ty {
        Type::Component(refs) => {
            for name in refs {
                if !matches!(
                    names.get(name),
                    Some(Entry::Component(_))
                ) {
                    return Err(LibraryError::UnknownComponent {
                        owner: owner.to_owned(),
                        name: name.clone(),
                    });
                }
            }
        }
        Type::Action(refs) => {
            for name in refs {
                let known = BUILTIN_ACTION_REFS
                    .contains(&name.as_str())
                    || matches!(
                        names.get(name),
                        Some(Entry::Action(_))
                    );
                if !known {
                    return Err(LibraryError::UnknownAction {
                        owner: owner.to_owned(),
                        name: name.clone(),
                    });
                }
            }
        }
        Type::Enum(values) => {
            let scalar = values.iter().all(|v| {
                matches!(
                    v,
                    Value::String(_)
                        | Value::Number(_)
                        | Value::Bool(_)
                )
            });
            if !scalar {
                return Err(LibraryError::InvalidEnum {
                    owner: owner.to_owned(),
                });
            }
        }
        Type::List(item) => check_refs(owner, item, names)?,
        Type::Object(fields) => {
            check_params(owner, fields)?;
            for field in fields {
                check_refs(owner, &field.ty, names)?;
            }
        }
        Type::Union(options) => {
            for option in options {
                check_refs(owner, option, names)?;
            }
        }
        Type::Any | Type::String | Type::Number | Type::Boolean => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The examples in prompt.md sections 2 and 5.
    #[test]
    fn signatures_match_the_spec_examples() {
        let button =
            ComponentDef::new("Button", "A clickable button")
                .param(Param::required("label", Type::String))
                .param(Param::optional("action", Type::action()))
                .param(Param::optional(
                    "variant",
                    Type::one_of(["primary", "secondary"]),
                ));
        assert_eq!(
            button.signature(),
            r#"Button(label: string, action?: ActionExpression, variant?: "primary" | "secondary")"#
        );
        let card = ComponentDef::new("Card", "A container").param(
            Param::required(
                "children",
                Type::list(Type::components(["Button"])),
            ),
        );
        assert_eq!(card.signature(), "Card(children: Button[])");
        let percent = FunctionDef::new(
            "Percent",
            "Format part / total",
            |_| Ok(Value::Null),
        )
        .param(Param::required("part", Type::Number))
        .param(Param::required("total", Type::Number))
        .returns(Type::String);
        assert_eq!(
            percent.signature(),
            "Percent(part: number, total: number) → string"
        );
    }

    #[test]
    fn signatures_print_bindings_restricted_actions_and_grouped_lists()
     {
        let input = ComponentDef::new("Input", "A text field")
            .param(Param::required("name", Type::String))
            .param(Param::optional("value", Type::String).bindable())
            .param(Param::optional("rules", Type::object([])))
            .param(Param::optional(
                "share",
                Type::actions(["CopyToClipboard", "OpenUrl"]),
            ))
            .param(Param::optional(
                "tags",
                Type::list(Type::one_of(["a", "b"])),
            ));
        assert_eq!(
            input.signature(),
            r#"Input(name: string, value?: $binding<string>, rules?: {}, share?: @CopyToClipboard | @OpenUrl, tags?: ("a" | "b")[])"#
        );
    }

    #[test]
    fn build_rejects_reserved_duplicate_and_dangling_names() {
        let text = || {
            ComponentDef::new("Text", "")
                .param(Param::required("text", Type::String))
        };
        let reserved = Library::builder()
            .component(ComponentDef::new("Count", ""))
            .build();
        assert_eq!(
            reserved.unwrap_err(),
            LibraryError::Reserved("Count".into())
        );
        let duplicate = Library::builder()
            .component(text())
            .action(ActionDef::new("Text", ""))
            .build();
        assert_eq!(
            duplicate.unwrap_err(),
            LibraryError::Duplicate("Text".into())
        );
        let lowercase = Library::builder()
            .component(ComponentDef::new("text", ""))
            .build();
        assert_eq!(
            lowercase.unwrap_err(),
            LibraryError::LowercaseComponent("text".into())
        );
        let dangling = Library::builder()
            .component(ComponentDef::new("Table", "").param(
                Param::required(
                    "columns",
                    Type::list(Type::components(["Col"])),
                ),
            ))
            .build();
        assert!(matches!(
            dangling,
            Err(LibraryError::UnknownComponent { .. })
        ));
        let root = Library::builder()
            .component(text())
            .root("Stack")
            .build();
        assert_eq!(
            root.unwrap_err(),
            LibraryError::UnknownRoot("Stack".into())
        );
    }
}
