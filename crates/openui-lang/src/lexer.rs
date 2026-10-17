//! The lexer (spec section 1.3). Lexing never fails: a character that matches
//! no rule is skipped.

use std::borrow::Cow;

/// A token. Names borrow the source; strings are decoded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok<'a> {
    Newline,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Assign,
    True,
    False,
    Null,
    Str(Cow<'a, str>),
    Num(f64),
    /// A name with a lowercase or `_` first letter.
    Ident(&'a str),
    /// A name with an uppercase first letter.
    Type(&'a str),
    /// `$name`, including the `$`.
    State(&'a str),
    /// `@name`, without the `@`.
    At(&'a str),
    Dot,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Gt,
    Lt,
    Ge,
    Le,
    And,
    Or,
    Not,
    Question,
}

impl Tok<'_> {
    /// True for tokens after which `-` is subtraction, not a negative number.
    fn is_value(&self) -> bool {
        matches!(
            self,
            Tok::Num(_)
                | Tok::Str(_)
                | Tok::Ident(_)
                | Tok::Type(_)
                | Tok::State(_)
                | Tok::At(_)
                | Tok::RParen
                | Tok::RBracket
                | Tok::True
                | Tok::False
                | Tok::Null
        )
    }
}

/// A token and its byte range in the lexed text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Token<'a> {
    pub(crate) tok: Tok<'a>,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// The tokens of a text.
#[derive(Debug)]
pub(crate) struct Lexed<'a> {
    pub(crate) tokens: Vec<Token<'a>>,
    /// The text ended inside a string.
    pub(crate) open_string: bool,
}

pub(crate) fn lex(src: &str) -> Lexed<'_> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token<'_>> = Vec::new();
    let mut open_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        let (tok, len) = match b {
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            b'\n' => (Tok::Newline, 1),
            b'(' => (Tok::LParen, 1),
            b')' => (Tok::RParen, 1),
            b'[' => (Tok::LBracket, 1),
            b']' => (Tok::RBracket, 1),
            b'{' => (Tok::LBrace, 1),
            b'}' => (Tok::RBrace, 1),
            b',' => (Tok::Comma, 1),
            b':' => (Tok::Colon, 1),
            b'.' => (Tok::Dot, 1),
            b'?' => (Tok::Question, 1),
            b'+' => (Tok::Plus, 1),
            b'*' => (Tok::Star, 1),
            b'/' => (Tok::Slash, 1),
            b'%' => (Tok::Percent, 1),
            b'=' if next == Some(b'=') => (Tok::EqEq, 2),
            b'=' => (Tok::Assign, 1),
            b'!' if next == Some(b'=') => (Tok::NotEq, 2),
            b'!' => (Tok::Not, 1),
            b'>' if next == Some(b'=') => (Tok::Ge, 2),
            b'>' => (Tok::Gt, 1),
            b'<' if next == Some(b'=') => (Tok::Le, 2),
            b'<' => (Tok::Lt, 1),
            // A single `&` or `|` means `&&` or `||`.
            b'&' => (Tok::And, if next == Some(b'&') { 2 } else { 1 }),
            b'|' => (Tok::Or, if next == Some(b'|') { 2 } else { 1 }),
            b'"' | b'\'' => {
                let (value, end, terminated) = lex_string(src, i);
                open_string = !terminated;
                (Tok::Str(value), end - i)
            }
            b'-' if next.is_some_and(|d| d.is_ascii_digit())
                && !tokens.last().is_some_and(|t| t.tok.is_value()) =>
            {
                lex_number(src, i)
            }
            b'-' => (Tok::Minus, 1),
            b'0'..=b'9' => lex_number(src, i),
            b'$' | b'@' if next.is_some_and(is_ident_start) => {
                let end = ident_end(bytes, i + 1);
                let tok = if b == b'$' {
                    Tok::State(&src[i..end])
                } else {
                    Tok::At(&src[i + 1..end])
                };
                (tok, end - i)
            }
            _ if is_ident_start(b) => {
                let end = ident_end(bytes, i);
                let word = &src[i..end];
                let tok = match word {
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "null" => Tok::Null,
                    _ if b.is_ascii_uppercase() => Tok::Type(word),
                    _ => Tok::Ident(word),
                };
                (tok, end - i)
            }
            _ => {
                // Skip one character, which may be several bytes.
                i += src[i..].chars().next().map_or(1, char::len_utf8);
                continue;
            }
        };
        i += len;
        tokens.push(Token { tok, start, end: i });
    }
    Lexed {
        tokens,
        open_string,
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn ident_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))
        .map_or(bytes.len(), |n| from + n)
}

/// `-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?`. An `e` after the digits is always
/// an exponent marker; without exponent digits the token is `NaN`, like JavaScript's `+"1e"`.
fn lex_number(src: &str, start: usize) -> (Tok<'static>, usize) {
    let bytes = src.as_bytes();
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(bytes.len(), |n| from + n)
    };
    let mut i = start;
    if bytes[i] == b'-' {
        i += 1;
    }
    i = digits(i);
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i = digits(i + 1);
    }
    let mut dangling_exponent = false;
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exponent_start = i;
        i = digits(i);
        dangling_exponent = i == exponent_start;
    }
    let value = if dangling_exponent {
        f64::NAN
    } else {
        src[start..i].parse().unwrap_or(f64::NAN)
    };
    (Tok::Num(value), i - start)
}

/// Decodes a string starting at the quote at `start`. Returns the value, the
/// end offset, and whether the closing quote was found.
///
/// Double-quoted strings use JSON escapes; single-quoted ones support `\'`,
/// `\\`, `\n` and `\t`. Any other escaped character is kept as is. Raw
/// newlines are allowed. At the end of the text the string closes
/// implicitly, and a dangling `\` stays a backslash (spec section 4, rule 2).
fn lex_string(src: &str, start: usize) -> (Cow<'_, str>, usize, bool) {
    let bytes = src.as_bytes();
    let quote = bytes[start];
    let body = start + 1;
    let mut i = body;
    let mut escaped = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                escaped = true;
                i += 2;
            }
            b if b == quote => {
                let value = if escaped {
                    Cow::Owned(unescape(&src[body..i], quote))
                } else {
                    Cow::Borrowed(&src[body..i])
                };
                return (value, i + 1, true);
            }
            _ => i += 1,
        }
    }
    let raw = &src[body..];
    let value = if escaped {
        Cow::Owned(unescape(raw, quote))
    } else {
        Cow::Borrowed(raw)
    };
    (value, src.len(), false)
}

fn unescape(raw: &str, quote: u8) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(escaped) = chars.next() else {
            // The text ended mid-escape: implicit closing adds a `\`.
            out.push('\\');
            break;
        };
        match (quote, escaped) {
            (_, '\\') => out.push('\\'),
            (_, 'n') => out.push('\n'),
            (_, 't') => out.push('\t'),
            (b'"', 'b') => out.push('\u{8}'),
            (b'"', 'f') => out.push('\u{C}'),
            (b'"', 'r') => out.push('\r'),
            (b'"', 'u') => {
                let rest = chars.as_str();
                match hex4(rest) {
                    Some(unit) => {
                        chars = rest[4..].chars();
                        out.push(decode_utf16_unit(unit, &mut chars));
                    }
                    // A unicode escape cut off by the end of the text is dropped.
                    None if rest.len() < 4 && rest.bytes().all(|b| b.is_ascii_hexdigit()) => {
                        break;
                    }
                    None => out.push('u'),
                }
            }
            (_, other) => out.push(other),
        }
    }
    out
}

fn hex4(s: &str) -> Option<u16> {
    let digits = s.get(..4)?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(digits, 16).ok()
}

/// Joins a `\uXXXX` high surrogate with a following low-surrogate escape.
/// A lone surrogate, which Rust strings cannot hold, becomes U+FFFD.
fn decode_utf16_unit(unit: u16, chars: &mut std::str::Chars<'_>) -> char {
    if (0xD800..0xDC00).contains(&unit) {
        let rest = chars.as_str();
        if let Some(low) = rest
            .strip_prefix("\\u")
            .and_then(hex4)
            .filter(|low| (0xDC00..0xE000).contains(low))
        {
            *chars = rest[6..].chars();
            let code = 0x10000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
            return char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER);
        }
    }
    char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok<'_>> {
        lex(src).tokens.into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn minus_after_a_value_is_subtraction() {
        assert_eq!(
            kinds("[1 -2, -5]"),
            [
                Tok::LBracket,
                Tok::Num(1.0),
                Tok::Minus,
                Tok::Num(2.0),
                Tok::Comma,
                Tok::Num(-5.0),
                Tok::RBracket
            ]
        );
        assert_eq!(kinds("{} -1"), [Tok::LBrace, Tok::RBrace, Tok::Num(-1.0)]);
    }

    #[test]
    fn a_dot_needs_a_digit_to_join_a_number() {
        assert_eq!(kinds("1.x"), [Tok::Num(1.0), Tok::Dot, Tok::Ident("x")]);
        assert_eq!(kinds("m.0.1"), [Tok::Ident("m"), Tok::Dot, Tok::Num(0.1)]);
    }

    #[test]
    fn strings_decode_escapes() {
        assert_eq!(kinds(r#""q\"d\\b\ttA""#), [Tok::Str("q\"d\\b\ttA".into())]);
        assert_eq!(kinds(r"'it\'s \q'"), [Tok::Str("it's q".into())]);
        assert_eq!(kinds(r#""😀""#), [Tok::Str("😀".into())]);
    }

    #[test]
    fn an_unterminated_string_closes_at_the_end() {
        let lexed = lex(r#"Text("a\"#);
        assert!(lexed.open_string);
        assert_eq!(lexed.tokens[2].tok, Tok::Str("a\\".into()));
        assert_eq!(lex(r#""\u00"#).tokens[0].tok, Tok::Str("".into()));
    }

    #[test]
    fn unknown_characters_are_skipped() {
        assert_eq!(kinds("a;b é $ @"), [Tok::Ident("a"), Tok::Ident("b")]);
    }

    #[test]
    fn single_ampersand_and_pipe_are_logical_operators() {
        assert_eq!(
            kinds("a & b | c"),
            [
                Tok::Ident("a"),
                Tok::And,
                Tok::Ident("b"),
                Tok::Or,
                Tok::Ident("c")
            ]
        );
    }
}
