//! The ECMAScript conversions OpenUI Lang 1.0 requires clients to reproduce
//! (spec section 3.1): `ToNumber` on strings, `Number::toString`, `ToBoolean`,
//! `IsLooselyEqual`, and `ToString`.

use std::sync::Arc;

use crate::value::Value;

/// Nesting beyond which `ToString` of nested arrays gives up and yields `""`.
const MAX_JOIN_DEPTH: usize = 256;

/// `Number::toString(10)`: the shortest round-trip digits, laid out as ECMA-262 does.
pub(crate) fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_owned();
    }
    if x == 0.0 {
        return "0".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    // Rust's `{:e}` prints the shortest digits that round-trip, like ECMA-262.
    let scientific = format!("{:e}", x.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("LowerExp output always has an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().expect("LowerExp exponent is an integer");
    let k = digits.len() as i32;
    let n = exponent + 1;

    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if n > 0 { '+' } else { '-' });
        out.push_str(&(n - 1).abs().to_string());
    }
    out
}

/// ECMAScript `StrWhiteSpaceChar`: `WhiteSpace` plus `LineTerminator`.
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'
            | '\u{B}'
            | '\u{C}'
            | ' '
            | '\u{A0}'
            | '\u{FEFF}'
            | '\n'
            | '\r'
            | '\u{2028}'
            | '\u{2029}'
    ) || (c > '\u{7F}' && is_space_separator(c))
}

/// Unicode general category `Zs`.
fn is_space_separator(c: char) -> bool {
    matches!(
        c,
        '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// `String.prototype.trim`.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// ECMAScript `StringToNumber`: `NaN` when the text is not a numeric literal.
pub(crate) fn string_to_number(s: &str) -> f64 {
    let s = trim(s);
    if s.is_empty() {
        return 0.0;
    }
    match s {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let bytes = s.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' {
        let radix = match bytes[1] {
            b'x' | b'X' => Some(16),
            b'o' | b'O' => Some(8),
            b'b' | b'B' => Some(2),
            _ => None,
        };
        if let Some(radix) = radix {
            return parse_radix_integer(&s[2..], radix);
        }
    }
    if is_decimal_literal(bytes) {
        s.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

fn parse_radix_integer(digits: &str, radix: u32) -> f64 {
    let mut exact: Option<u128> = Some(0);
    let mut approx = 0.0f64;
    for c in digits.chars() {
        let Some(d) = c.to_digit(radix) else {
            return f64::NAN;
        };
        exact = exact
            .and_then(|v| v.checked_mul(u128::from(radix)))
            .and_then(|v| v.checked_add(u128::from(d)));
        approx = approx * f64::from(radix) + f64::from(d);
    }
    match exact {
        Some(v) => v as f64,
        None => approx,
    }
}

/// `StrDecimalLiteral` without `Infinity`: `[+-]? (digits [. digits?] | . digits) ([eE] [+-]? digits)?`.
fn is_decimal_literal(b: &[u8]) -> bool {
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - frac_start;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        i += 1;
        if i < b.len() && matches!(b[i], b'+' | b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return false;
        }
    }
    i == b.len()
}

/// OpenUI `toNumber`: numbers as is, numeric strings parsed (else 0), `true` is 1, everything else 0.
pub(crate) fn to_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => *n,
        Value::String(s) => {
            let n = string_to_number(s);
            if n.is_nan() { 0.0 } else { n }
        }
        Value::Bool(b) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

/// ECMAScript `ToBoolean`.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => !(*n == 0.0 || n.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) | Value::Element(_) | Value::Action(_) => true,
    }
}

/// ECMAScript `ToString`. Objects, component instances and actions are `[object Object]`.
pub(crate) fn to_string(v: &Value) -> String {
    to_string_within(v, usize::MAX).expect("no limit")
}

/// `ToString`, or `None` once the result would pass `limit` bytes. Values
/// share their parts, so a small value can print as a very long string.
pub(crate) fn to_string_within(v: &Value, limit: usize) -> Option<String> {
    let mut out = String::new();
    write_string(v, &mut out, 0, limit).then_some(out)
}

/// A value as diagnostic messages quote it: `String(value)` in double quotes,
/// cut after 40 characters. Model output can make a value arbitrarily long.
pub(crate) fn quote(v: &Value) -> String {
    const MAX_CHARS: usize = 40;
    let mut text = String::new();
    let whole = write_string(v, &mut text, 0, MAX_CHARS * 4);
    match text.char_indices().nth(MAX_CHARS) {
        Some((cut, _)) => {
            text.truncate(cut);
            format!("{text:?}…")
        }
        None if !whole => format!("{text:?}…"),
        None => format!("{text:?}"),
    }
}

fn write_string(v: &Value, out: &mut String, depth: usize, limit: usize) -> bool {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number_to_string(*n)),
        Value::String(s) => {
            // Copy no more than the limit allows; `quote` keeps the prefix.
            let room = limit.saturating_sub(out.len());
            if s.len() > room {
                let mut end = room;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&s[..end]);
                return false;
            }
            out.push_str(s);
        }
        Value::Array(items) => {
            if depth >= MAX_JOIN_DEPTH {
                return true;
            }
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                // `Array.prototype.join` writes null and undefined as "".
                if !item.is_null() && !write_string(item, out, depth + 1, limit) {
                    return false;
                }
                if out.len() > limit {
                    return false;
                }
            }
        }
        Value::Object(_) | Value::Element(_) | Value::Action(_) => out.push_str("[object Object]"),
    }
    out.len() <= limit
}

/// ECMAScript `IsLooselyEqual` (`==`). Objects compare by identity, like JavaScript.
pub(crate) fn loosely_equal(x: &Value, y: &Value) -> bool {
    use Value::{Bool, Null, Number, String};
    match (x, y) {
        (Null, Null) => true,
        (Null, _) | (_, Null) => false,
        (Number(a), Number(b)) => a == b,
        (String(a), String(b)) => a == b,
        (Bool(a), Bool(b)) => a == b,
        (Number(a), String(b)) => *a == string_to_number(b),
        (String(a), Number(b)) => string_to_number(a) == *b,
        (Bool(b), other) | (other, Bool(b)) => {
            loosely_equal(&Number(f64::from(u8::from(*b))), other)
        }
        (Number(_) | String(_), _) => loosely_equal(x, &to_primitive(y)),
        (_, Number(_) | String(_)) => loosely_equal(&to_primitive(x), y),
        (a, b) => same_object(a, b),
    }
}

/// ECMAScript `ToPrimitive` for objects: arrays join, everything else is `[object Object]`.
fn to_primitive(object: &Value) -> Value {
    Value::String(Arc::from(to_string(object)))
}

fn same_object(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(a), Value::Array(b)) => Arc::ptr_eq(a, b),
        (Value::Object(a), Value::Object(b)) => Arc::ptr_eq(a, b),
        (Value::Element(a), Value::Element(b)) => Arc::ptr_eq(a, b),
        (Value::Action(a), Value::Action(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

/// JavaScript `Math.round`: halves round toward positive infinity.
pub(crate) fn math_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let floor = x.floor();
    let rounded = if x - floor >= 0.5 { floor + 1.0 } else { floor };
    if rounded == 0.0 && x < 0.0 {
        -0.0
    } else {
        rounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_to_string_matches_ecmascript() {
        let cases = [
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123e-20, "1.23e-18"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (9007199254740992.0, "9007199254740992"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (-1.5, "-1.5"),
            (100.0, "100"),
            (-0.0, "0"),
            (123.456, "123.456"),
            (1.5e300, "1.5e+300"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (input, expected) in cases {
            assert_eq!(number_to_string(input), expected, "{input:e}");
        }
    }

    #[test]
    fn string_to_number_follows_the_ecmascript_grammar() {
        let cases = [
            ("  12  ", 12.0),
            ("", 0.0),
            (" \n\t", 0.0),
            ("1.", 1.0),
            (".5", 0.5),
            ("+.5e1", 5.0),
            ("0x1F", 31.0),
            ("0b101", 5.0),
            ("0o17", 15.0),
            ("-Infinity", f64::NEG_INFINITY),
            ("1e3", 1000.0),
        ];
        for (input, expected) in cases {
            assert_eq!(string_to_number(input), expected, "{input:?}");
        }
        for input in [
            "abc",
            "12abc",
            "inf",
            "nan",
            "1_000",
            "-0x10",
            "0x",
            ".",
            "1e",
            "Infinityx",
        ] {
            assert!(string_to_number(input).is_nan(), "{input:?}");
        }
    }

    #[test]
    fn loose_equality_coerces_like_javascript() {
        let s = |v: &str| Value::from(v);
        let n = Value::Number;
        assert!(loosely_equal(&n(5.0), &s("5")));
        assert!(loosely_equal(&s(""), &n(0.0)));
        assert!(!loosely_equal(&Value::Null, &n(0.0)));
        assert!(loosely_equal(&Value::Bool(true), &s("1")));
        assert!(!loosely_equal(&s("abc"), &n(0.0)));
        assert!(loosely_equal(&Value::array([n(1.0)]), &n(1.0)));
        assert!(loosely_equal(&Value::array([n(1.0), n(2.0)]), &s("1,2")));
        assert!(!loosely_equal(&Value::array([]), &Value::array([])));
        let shared = Value::array([n(1.0)]);
        assert!(loosely_equal(&shared, &shared.clone()));
        assert!(!loosely_equal(&n(f64::NAN), &n(f64::NAN)));
    }

    #[test]
    fn math_round_rounds_halves_up() {
        assert_eq!(math_round(2.5), 3.0);
        assert_eq!(math_round(-2.5), -2.0);
        assert_eq!(math_round(0.49999999999999994), 0.0);
        assert!(math_round(-0.2).is_sign_negative());
    }
}
