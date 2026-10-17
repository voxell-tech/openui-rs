//! The built-in functions of spec section 3.4. `@Each` defers its template,
//! so the evaluator runs it.

use std::cmp::Ordering;

use crate::collate;
use crate::eval::Budget;
use crate::js::{math_round, string_to_number};
use crate::value::Value;

/// A built-in function other than `@Each`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Count,
    First,
    Last,
    Sum,
    Avg,
    Min,
    Max,
    Sort,
    Filter,
    Round,
    Abs,
    Floor,
    Ceil,
}

impl Builtin {
    const ALL: [Self; 13] = [
        Self::Count,
        Self::First,
        Self::Last,
        Self::Sum,
        Self::Avg,
        Self::Min,
        Self::Max,
        Self::Sort,
        Self::Filter,
        Self::Round,
        Self::Abs,
        Self::Floor,
        Self::Ceil,
    ];

    /// The names, in the order the prompt lists them. `@Each` is separate.
    pub(crate) const NAMES: [&'static str; 13] = [
        "Count", "First", "Last", "Sum", "Avg", "Min", "Max", "Sort", "Filter", "Round", "Abs",
        "Floor", "Ceil",
    ];

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| Self::ALL[i])
    }

    /// The number of parameters; extra arguments are dropped.
    pub(crate) fn arity(self) -> usize {
        match self {
            Self::Sort => 3,
            Self::Filter => 4,
            Self::Round => 2,
            _ => 1,
        }
    }

    /// Runs the built-in. Built-ins coerce their arguments instead of
    /// checking them. Absent arguments are null. `None` means the budget ran out.
    pub(crate) fn call(self, args: &[Value], budget: &mut Budget) -> Option<Value> {
        let arg = |i: usize| args.get(i).unwrap_or(&Value::Null);
        let items = arg(0).as_array().unwrap_or_default();
        // The built-ins that walk the array pay a step per element.
        let walks = !matches!(self, Self::Count | Self::First | Self::Last);
        if walks && !budget.steps(items.len()) {
            return None;
        }
        let number = |x: f64| Some(Value::Number(x));
        match self {
            Self::Count => number(items.len() as f64),
            Self::First => Some(items.first().cloned().unwrap_or(Value::Null)),
            Self::Last => Some(items.last().cloned().unwrap_or(Value::Null)),
            Self::Sum => number(sum(items, budget)?),
            Self::Avg if items.is_empty() => number(0.0),
            Self::Avg => number(sum(items, budget)? / items.len() as f64),
            Self::Min => number(fold(items, js_min, budget)?),
            Self::Max => number(fold(items, js_max, budget)?),
            Self::Sort => sort(arg(0), arg(1), arg(2), budget),
            Self::Filter => filter(arg(0), arg(1), arg(2), arg(3), budget),
            Self::Round => {
                let decimals = if arg(1).is_null() {
                    0.0
                } else {
                    budget.number(arg(1))?
                };
                let factor = 10f64.powf(decimals);
                number(math_round(budget.number(arg(0))? * factor) / factor)
            }
            Self::Abs => number(budget.number(arg(0))?.abs()),
            Self::Floor => number(budget.number(arg(0))?.floor()),
            Self::Ceil => number(budget.number(arg(0))?.ceil()),
        }
    }
}

fn sum(items: &[Value], budget: &mut Budget) -> Option<f64> {
    let mut total = 0.0;
    for item in items {
        total += budget.number(item)?;
    }
    Some(total)
}

/// `Math.min` or `Math.max` over the elements; 0 for empty arrays and non-arrays.
fn fold(items: &[Value], f: fn(f64, f64) -> f64, budget: &mut Budget) -> Option<f64> {
    let Some((first, rest)) = items.split_first() else {
        return Some(0.0);
    };
    let mut acc = budget.number(first)?;
    for item in rest {
        acc = f(acc, budget.number(item)?);
    }
    Some(acc)
}

/// `Math.min`: NaN wins, and -0 is less than +0.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else {
        a.min(b)
    }
}

/// `Math.max`: NaN wins, and +0 is greater than -0.
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_positive() { a } else { b }
    } else {
        a.max(b)
    }
}

/// Reads a field path such as `"customer.name"`; null where a step is missing.
/// `None` means the budget ran out.
pub(crate) fn resolve_field(value: &Value, path: &str, budget: &mut Budget) -> Option<Value> {
    let mut current = value.clone();
    for part in path.split('.') {
        if current.is_null() {
            return Some(Value::Null);
        }
        current = budget.property(&current, part)?;
    }
    Some(current)
}

/// The field of an element that `@Sort` and `@Filter` look at.
fn field_of(item: &Value, field: &str, budget: &mut Budget) -> Option<Value> {
    if field.is_empty() {
        Some(item.clone())
    } else {
        resolve_field(item, field, budget)
    }
}

fn filter(
    array: &Value,
    field: &Value,
    operator: &Value,
    value: &Value,
    budget: &mut Budget,
) -> Option<Value> {
    let Value::Array(items) = array else {
        return Some(Value::array([]));
    };
    let field = budget.string_or_empty(field)?;
    let operator = if operator.is_null() {
        "==".to_owned()
    } else {
        budget.string(operator)?
    };
    let needle = match operator.as_str() {
        "contains" => budget.string_or_empty(value)?,
        _ => String::new(),
    };
    let target = match operator.as_str() {
        ">" | "<" | ">=" | "<=" => budget.number(value)?,
        _ => 0.0,
    };
    let mut kept = Vec::new();
    for item in items.iter() {
        let v = field_of(item, &field, budget)?;
        let keep = match operator.as_str() {
            "==" => budget.loosely_equal(&v, value)?,
            "!=" => !budget.loosely_equal(&v, value)?,
            ">" => budget.number(&v)? > target,
            "<" => budget.number(&v)? < target,
            ">=" => budget.number(&v)? >= target,
            "<=" => budget.number(&v)? <= target,
            "contains" => match &v {
                Value::String(s) => budget.scan(s.len() + needle.len()) && s.contains(&*needle),
                other => budget.string_or_empty(other)?.contains(&*needle),
            },
            _ => false,
        };
        if budget.exhausted() {
            return None;
        }
        if keep {
            kept.push(item.clone());
        }
    }
    Some(Value::array(kept))
}

struct SortKey {
    number: Option<f64>,
    text: collate::Key,
}

fn sort(array: &Value, field: &Value, direction: &Value, budget: &mut Budget) -> Option<Value> {
    let Value::Array(items) = array else {
        return Some(array.clone());
    };
    let field = budget.string_or_empty(field)?;
    let descending = !direction.is_null() && budget.string(direction)? == "desc";
    let n = items.len();
    let rounds = n.max(2).ilog2() as usize + 1;
    if !budget.steps(n.saturating_mul(rounds)) {
        return None;
    }
    let mut keys = Vec::with_capacity(n);
    let mut text_bytes = 0usize;
    for item in items.iter() {
        let v = field_of(item, &field, budget)?;
        let (number, text) = match &v {
            Value::String(s) => {
                // The key holds a copy of the text.
                if !budget.copy(s.len()) {
                    return None;
                }
                let parsed = string_to_number(s);
                let number = (!s.is_empty() && !parsed.is_nan()).then_some(parsed);
                text_bytes += s.len();
                (number, collate::key(s))
            }
            other => {
                let text = budget.string_or_empty(other)?;
                text_bytes += text.len();
                let number = match other {
                    Value::Number(x) => Some(*x),
                    _ => None,
                };
                (number, collate::key(&text))
            }
        };
        keys.push(SortKey { number, text });
    }
    // Each element takes part in about `rounds` comparisons of its text.
    if !budget.scan(text_bytes.saturating_mul(rounds)) {
        return None;
    }
    let compare = |a: usize, b: usize| -> f64 {
        let (a, b) = (&keys[a], &keys[b]);
        let order = match (a.number, b.number) {
            (Some(x), Some(y)) => x - y,
            _ => match collate::compare(&a.text, &b.text) {
                Ordering::Less => -1.0,
                Ordering::Equal => 0.0,
                Ordering::Greater => 1.0,
            },
        };
        let order = if descending { -order } else { order };
        // A comparator result of NaN counts as equal.
        if order.is_nan() { 0.0 } else { order }
    };
    let order = stable_sort(n, compare);
    Some(Value::array(order.into_iter().map(|i| items[i].clone())))
}

/// Sorts indices `0..n` stably by `compare`, whose sign orders two indices.
///
/// The comparison may be inconsistent (numbers and text mixed), so this never
/// relies on it being a total order. Below 64 elements it follows V8's
/// `Array.prototype.sort` step for step (one run, then binary insertion), so
/// even inconsistent comparisons give the browser's order. Longer arrays are
/// merged run by run; consistent comparisons give the same result as V8.
fn stable_sort(n: usize, compare: impl Fn(usize, usize) -> f64) -> Vec<usize> {
    let mut work: Vec<usize> = (0..n).collect();
    if n < 2 {
        return work;
    }
    let min_run = min_run_length(n);
    let mut runs = Vec::new();
    let mut low = 0;
    while low < n {
        let remaining = n - low;
        let mut run = count_and_make_run(&mut work, low, n, &compare);
        if run < min_run {
            let forced = min_run.min(remaining);
            binary_insertion_sort(&mut work, low, low + run, low + forced, &compare);
            run = forced;
        }
        runs.push((low, low + run));
        low += run;
    }
    while runs.len() > 1 {
        let mut merged = Vec::with_capacity(runs.len().div_ceil(2));
        for pair in runs.chunks(2) {
            match *pair {
                [(start, mid), (_, end)] => {
                    merge(&mut work, start, mid, end, &compare);
                    merged.push((start, end));
                }
                [run] => merged.push(run),
                _ => unreachable!("chunks of two"),
            }
        }
        runs = merged;
    }
    work
}

fn min_run_length(mut n: usize) -> usize {
    let mut r = 0;
    while n >= 64 {
        r |= n & 1;
        n >>= 1;
    }
    n + r
}

/// V8's `CountAndMakeRun`: the length of the run at `low`, reversed in place
/// when it is strictly descending.
fn count_and_make_run(
    work: &mut [usize],
    low: usize,
    high: usize,
    compare: &impl Fn(usize, usize) -> f64,
) -> usize {
    if low + 1 == high {
        return 1;
    }
    let descending = compare(work[low + 1], work[low]) < 0.0;
    let mut length = 2;
    let mut previous = work[low + 1];
    for &current in &work[low + 2..high] {
        let order = compare(current, previous);
        if (descending && order >= 0.0) || (!descending && order < 0.0) {
            break;
        }
        previous = current;
        length += 1;
    }
    if descending {
        work[low..low + length].reverse();
    }
    length
}

/// V8's `BinaryInsertionSort`: `work[low..start]` is sorted; inserts the rest up to `high`.
fn binary_insertion_sort(
    work: &mut [usize],
    low: usize,
    start: usize,
    high: usize,
    compare: &impl Fn(usize, usize) -> f64,
) {
    let start = if low == start { start + 1 } else { start };
    for i in start..high {
        let pivot = work[i];
        let (mut left, mut right) = (low, i);
        while left < right {
            let mid = left + (right - left) / 2;
            if compare(pivot, work[mid]) < 0.0 {
                right = mid;
            } else {
                left = mid + 1;
            }
        }
        work.copy_within(left..i, left + 1);
        work[left] = pivot;
    }
}

/// Merges the sorted ranges `start..mid` and `mid..end`, left first on ties.
fn merge(
    work: &mut [usize],
    start: usize,
    mid: usize,
    end: usize,
    compare: &impl Fn(usize, usize) -> f64,
) {
    let left = work[start..mid].to_vec();
    let (mut i, mut j, mut out) = (0, mid, start);
    while i < left.len() && j < end {
        if compare(work[j], left[i]) < 0.0 {
            work[out] = work[j];
            j += 1;
        } else {
            work[out] = left[i];
            i += 1;
        }
        out += 1;
    }
    for &rest in &left[i..] {
        work[out] = rest;
        out += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    fn call(builtin: Builtin, args: &[Value]) -> Value {
        builtin
            .call(args, &mut Budget::new(&Limits::DEFAULT))
            .expect("within budget")
    }

    fn numbers(values: &[f64]) -> Value {
        Value::array(values.iter().map(|&n| Value::Number(n)))
    }

    #[test]
    fn aggregates_coerce_and_default_to_zero() {
        let mixed = Value::array([
            Value::from("2"),
            Value::from(true),
            Value::from("x"),
            Value::Null,
        ]);
        assert_eq!(
            call(Builtin::Sum, std::slice::from_ref(&mixed)),
            Value::Number(3.0)
        );
        assert_eq!(
            call(Builtin::Avg, std::slice::from_ref(&mixed)),
            Value::Number(0.75)
        );
        assert_eq!(call(Builtin::Min, &[mixed]), Value::Number(0.0));
        assert_eq!(
            call(Builtin::Max, &[Value::from("nope")]),
            Value::Number(0.0)
        );
        assert_eq!(call(Builtin::Avg, &[Value::array([])]), Value::Number(0.0));
        assert_eq!(
            call(Builtin::Count, &[Value::from("abc")]),
            Value::Number(0.0)
        );
        assert_eq!(call(Builtin::Last, &[Value::array([])]), Value::Null);
    }

    #[test]
    fn min_and_max_follow_math_min_and_max() {
        let min = call(Builtin::Min, &[numbers(&[0.0, -0.0])])
            .as_f64()
            .unwrap();
        assert!(min == 0.0 && min.is_sign_negative());
        assert!(
            call(Builtin::Max, &[numbers(&[1.0, f64::NAN])])
                .as_f64()
                .unwrap()
                .is_nan()
        );
    }

    #[test]
    fn round_uses_math_round_halves() {
        let round = |n: f64, d: Value| call(Builtin::Round, &[Value::Number(n), d]);
        assert_eq!(round(2.5, Value::Null), Value::Number(3.0));
        assert_eq!(round(-2.5, Value::Null), Value::Number(-2.0));
        assert_eq!(round(1.005, Value::from(2)), Value::Number(1.0));
        assert_eq!(round(1234.5, Value::from(-2)), Value::Number(1200.0));
    }

    #[test]
    fn filter_operators() {
        let rows = Value::array([
            Value::object([("s", Value::from("open")), ("n", Value::from(5))]),
            Value::object([("s", Value::from("closed")), ("n", Value::from("12"))]),
            Value::object([("n", Value::from(1))]),
        ]);
        let count = |op: &str, field: &str, value: Value| {
            let args = [rows.clone(), Value::from(field), Value::from(op), value];
            call(Builtin::Filter, &args).as_array().unwrap().len()
        };
        assert_eq!(count("==", "s", Value::from("open")), 1);
        assert_eq!(count("==", "s", Value::Null), 1);
        assert_eq!(count(">", "n", Value::from("4")), 2);
        assert_eq!(count("contains", "s", Value::from("lo")), 1);
        assert_eq!(count("~", "s", Value::from("open")), 0);
        let null_op = [
            rows.clone(),
            Value::from("n"),
            Value::Null,
            Value::from("5"),
        ];
        assert_eq!(call(Builtin::Filter, &null_op).as_array().unwrap().len(), 1);
    }

    #[test]
    fn sort_is_stable_numeric_when_both_parse_and_text_otherwise() {
        let rows = Value::array(
            [("b", 2), ("a", 1), ("c", 2), ("d", 1)]
                .map(|(id, n)| Value::object([("id", Value::from(id)), ("n", Value::from(n))])),
        );
        let ids = |sorted: Value| -> Vec<String> {
            sorted
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_object().unwrap().get("id").unwrap().to_js_string())
                .collect()
        };
        let asc = call(Builtin::Sort, &[rows.clone(), Value::from("n")]);
        assert_eq!(ids(asc), ["a", "d", "b", "c"]);
        let desc = call(
            Builtin::Sort,
            &[rows, Value::from("n"), Value::from("desc")],
        );
        assert_eq!(ids(desc), ["b", "c", "a", "d"]);
        let mixed = Value::array([
            Value::from("10"),
            Value::from(9),
            Value::from(" "),
            Value::from("x"),
        ]);
        let sorted = call(Builtin::Sort, &[mixed, Value::from("")]);
        let text: Vec<String> = sorted
            .as_array()
            .unwrap()
            .iter()
            .map(Value::to_js_string)
            .collect();
        assert_eq!(text, [" ", "9", "10", "x"]);
    }

    #[test]
    fn long_sorts_are_stable() {
        let values: Vec<Value> = (0..500)
            .map(|i| Value::Number(f64::from((i * 7919) % 50)))
            .collect();
        let sorted = call(
            Builtin::Sort,
            &[Value::array(values.clone()), Value::from("")],
        );
        let mut expected = values;
        expected.sort_by(|a, b| a.as_f64().unwrap().total_cmp(&b.as_f64().unwrap()));
        assert_eq!(sorted, Value::array(expected));
    }

    #[test]
    fn field_paths_walk_objects() {
        let row = Value::object([("customer", Value::object([("name", Value::from("Ada"))]))]);
        let field = |value: &Value, path: &str| {
            resolve_field(value, path, &mut Budget::new(&Limits::DEFAULT)).expect("within budget")
        };
        assert_eq!(field(&row, "customer.name"), Value::from("Ada"));
        assert_eq!(field(&row, "customer.missing.deeper"), Value::Null);
        assert_eq!(field(&Value::from("abc"), "length"), Value::from(3));
    }
}
