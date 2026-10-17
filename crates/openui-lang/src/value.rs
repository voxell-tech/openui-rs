//! Runtime values and the renderer-neutral component tree.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use crate::library::Library;
use crate::lower::Node;

/// A value produced by evaluating an OpenUI Lang program (spec section 3.1).
///
/// Strings, arrays, objects, component instances and actions are reference
/// counted, so cloning a value is cheap and `==` in the language can compare
/// objects by identity the way JavaScript does.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`. Absent values and unresolved references also read as null.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An IEEE 754 double, like a JavaScript number.
    Number(f64),
    /// A string.
    String(Arc<str>),
    /// An array.
    Array(Arc<[Value]>),
    /// An object with JavaScript property order.
    Object(Arc<Map>),
    /// A component instance.
    Element(Arc<Element>),
    /// An `Action`: an ordered list of steps a component can trigger.
    Action(Arc<ActionPlan>),
}

impl Value {
    /// Builds a string value.
    pub fn string(s: impl Into<Arc<str>>) -> Self {
        Self::String(s.into())
    }

    /// Builds an array value.
    pub fn array(items: impl IntoIterator<Item = Value>) -> Self {
        Self::Array(items.into_iter().collect())
    }

    /// Builds an object value from key-value pairs, with JavaScript property order.
    pub fn object<K: Into<Arc<str>>>(entries: impl IntoIterator<Item = (K, Value)>) -> Self {
        Self::Object(Arc::new(entries.into_iter().collect()))
    }

    /// True for [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The number, if this is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The elements, if this is an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The object, if this is one.
    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    /// The component instance, if this is one.
    pub fn as_element(&self) -> Option<&Element> {
        match self {
            Self::Element(element) => Some(element),
            _ => None,
        }
    }

    /// The action, if this is one.
    pub fn as_action(&self) -> Option<&ActionPlan> {
        match self {
            Self::Action(plan) => Some(plan),
            _ => None,
        }
    }

    /// The value as the language's `String(value)` would print it.
    pub fn to_js_string(&self) -> String {
        crate::js::to_string(self)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Self::Number(f64::from(n))
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(Arc::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::String(Arc::from(s))
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Self::Array(Arc::from(items))
    }
}

impl From<Map> for Value {
    fn from(map: Map) -> Self {
        Self::Object(Arc::new(map))
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

/// An object's properties in JavaScript order: array-index keys ascending,
/// then the other keys in insertion order. Setting an existing key keeps its place.
#[derive(Clone, Debug, Default)]
pub struct Map {
    /// Array-index keys, ascending.
    indexed: BTreeMap<u32, (Arc<str>, Value)>,
    /// The other keys, in insertion order.
    named: Vec<(Arc<str>, Value)>,
    /// Where each key of `named` is.
    positions: HashMap<Arc<str>, usize>,
}

impl Map {
    /// An empty object.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of `key`, if present.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match array_index(key) {
            Some(index) => self.indexed.get(&index).map(|(_, v)| v),
            None => self.positions.get(key).map(|&at| &self.named[at].1),
        }
    }

    /// Sets `key`, keeping JavaScript property order.
    pub fn insert(&mut self, key: impl Into<Arc<str>>, value: Value) {
        let key = key.into();
        if let Some(index) = array_index(&key) {
            self.indexed.insert(index, (key, value));
        } else if let Some(&at) = self.positions.get(&key) {
            self.named[at].1 = value;
        } else {
            self.positions.insert(key.clone(), self.named.len());
            self.named.push((key, value));
        }
    }

    /// Removes `key` and returns its value.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        if let Some(index) = array_index(key) {
            return self.indexed.remove(&index).map(|(_, v)| v);
        }
        let at = self.positions.remove(key)?;
        let (_, value) = self.named.remove(at);
        for (k, _) in &self.named[at..] {
            *self
                .positions
                .get_mut(k)
                .expect("every named key has a position") -= 1;
        }
        Some(value)
    }

    /// True when `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The properties in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.indexed
            .values()
            .chain(&self.named)
            .map(|(k, v)| (&**k, v))
    }

    /// The number of properties.
    pub fn len(&self) -> usize {
        self.indexed.len() + self.named.len()
    }

    /// True when there are no properties.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Maps are equal when they hold the same properties in the same order.
impl PartialEq for Map {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<K: Into<Arc<str>>> FromIterator<(K, Value)> for Map {
    fn from_iter<I: IntoIterator<Item = (K, Value)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// A canonical array index (`0` to `2^32 - 2`), which JavaScript orders first.
pub(crate) fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&i| i != u32::MAX)
}

/// A component instance in the evaluated tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub(crate) component: Arc<str>,
    pub(crate) statement: Option<Arc<str>>,
    pub(crate) props: Vec<Prop>,
}

impl Element {
    /// The component name, as the library defines it.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// The statement that defined this instance, when it has one. Inline
    /// components have none. Hosts can use it as a stable identity.
    pub fn statement(&self) -> Option<&str> {
        self.statement.as_deref()
    }

    /// The props in the component's positional order. Null and absent props are left out.
    pub fn props(&self) -> &[Prop] {
        &self.props
    }

    /// The value of the prop named `name`.
    pub fn prop(&self, name: &str) -> Option<&Value> {
        self.props
            .iter()
            .find(|p| p.name() == name)
            .map(Prop::value)
    }
}

/// One prop of a component instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    pub(crate) name: Arc<str>,
    pub(crate) value: Value,
    pub(crate) binding: Option<Arc<str>>,
}

impl Prop {
    /// The prop name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current value. For a bound prop it is the state variable's value.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// The `$variable` a bindable prop writes user input back to (spec section 5.2).
    pub fn binding(&self) -> Option<&str> {
        self.binding.as_deref()
    }
}

/// An action: steps that run in order when a component triggers it (spec section 6.3).
///
/// The runtime never runs actions. A host decides whether and how to carry
/// them out, and model output stays untrusted even when it is valid.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionPlan {
    pub(crate) steps: Vec<ActionStep>,
}

impl ActionPlan {
    /// The steps in order.
    pub fn steps(&self) -> &[ActionStep] {
        &self.steps
    }
}

/// One step of an [`ActionPlan`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ActionStep {
    /// `@ToAssistant(message, context?)`: send a message to the model.
    ToAssistant {
        /// The message text.
        message: String,
        /// Extra data for the host, passed through unchanged.
        context: Option<Value>,
    },
    /// `@OpenUrl(url)`: ask the host to open a URL. Nothing is fetched by the runtime.
    OpenUrl {
        /// The URL as written.
        url: String,
    },
    /// `@Set($var, value)`: write a state variable. The value is evaluated when the step runs.
    Set {
        /// The state variable, including `$`.
        target: String,
        /// The value expression, evaluated by [`crate::Document::resolve`].
        value: Deferred,
    },
    /// `@Reset($a, ...)`: restore state variables to their declared defaults.
    Reset {
        /// The state variables, including `$`.
        targets: Vec<String>,
    },
    /// `@Run(ref)`: run a mutation or fetch a query again.
    Run {
        /// The query or mutation statement.
        statement: String,
        /// Whether `statement` is a query or a mutation.
        kind: RunKind,
    },
    /// A custom action declared by the library.
    Custom {
        /// The action name, without `@`.
        name: String,
        /// The arguments keyed by parameter name.
        params: Map,
    },
}

/// What [`ActionStep::Run`] targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    /// A `Query(...)` statement.
    Query,
    /// A `Mutation(...)` statement.
    Mutation,
}

/// An expression whose evaluation waits until an action runs, such as the
/// value of `@Set`. Loop variables in scope when it was built are captured.
#[derive(Clone)]
pub struct Deferred {
    pub(crate) node: Arc<Node>,
    pub(crate) scope: Arc<[(Arc<str>, Value)]>,
    /// The library the expression was checked against, which its nodes index.
    pub(crate) library: Library,
}

impl PartialEq for Deferred {
    fn eq(&self, other: &Self) -> bool {
        self.library.same(&other.library) && self.node == other.node && self.scope == other.scope
    }
}

impl fmt::Debug for Deferred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Deferred").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_orders_array_index_keys_first() {
        let map: Map = [
            ("name", Value::from("x")),
            ("2", Value::from("two")),
            ("10", Value::from("ten")),
            ("01", Value::from("not an index")),
            ("0", Value::from("zero")),
        ]
        .into_iter()
        .collect();
        let keys: Vec<&str> = map.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["0", "2", "10", "name", "01"]);
    }

    #[test]
    fn map_removal_keeps_the_order_of_the_rest() {
        let mut map: Map = ["a", "b", "c", "1"]
            .into_iter()
            .map(|k| (k, Value::from(k)))
            .collect();
        assert_eq!(map.remove("b"), Some(Value::from("b")));
        assert_eq!(map.remove("1"), Some(Value::from("1")));
        assert_eq!(map.remove("b"), None);
        map.insert("b", Value::Null);
        let keys: Vec<&str> = map.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["a", "c", "b"]);
        assert_eq!(map.get("c"), Some(&Value::from("c")));
    }

    #[test]
    fn map_keeps_the_first_position_of_a_replaced_key() {
        let map: Map = [
            ("a", Value::from(1)),
            ("b", Value::from(2)),
            ("a", Value::from(3)),
        ]
        .into_iter()
        .collect();
        let entries: Vec<(&str, &Value)> = map.iter().collect();
        assert_eq!(entries, [("a", &Value::from(3)), ("b", &Value::from(2))]);
    }
}
