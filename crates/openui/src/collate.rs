//! An approximation of the collation behind
//! `String.prototype.localeCompare` with no arguments, which `@Sort`
//! uses (spec section 3.4).
//!
//! JavaScript engines compare with the ICU root collation. This
//! compares on the same three levels: base characters first, then
//! accents, then case, with lowercase first. It knows the root order
//! of ASCII punctuation, puts whitespace, punctuation, digits and
//! letters in that order, and decomposes the accented letters of
//! Latin-1 and Latin Extended-A. Other letters sort by code point
//! after the Latin ones. Strings that differ only in ways this does
//! not model compare equal, and `@Sort` keeps their order.

use std::cmp::Ordering;

/// One collation element. A zero primary weight means accents or case
/// only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Element {
    primary: u32,
    secondary: u8,
    tertiary: u8,
}

/// A string's collation elements, computed once per sorted value.
#[derive(Clone, Debug)]
pub(crate) struct Key(Vec<Element>);

/// The collation key of `s`.
pub(crate) fn key(s: &str) -> Key {
    let mut elements = Vec::with_capacity(s.len());
    for c in s.chars() {
        push_elements(c, &mut elements);
    }
    Key(elements)
}

/// Compares two keys level by level, like `a.localeCompare(b)`.
pub(crate) fn compare(a: &Key, b: &Key) -> Ordering {
    let primaries = |k: &Key| {
        k.0.iter()
            .filter(|e| e.primary != 0)
            .map(|e| e.primary)
            .collect::<Vec<_>>()
    };
    let secondaries =
        |k: &Key| k.0.iter().map(|e| e.secondary).collect::<Vec<_>>();
    let tertiaries =
        |k: &Key| k.0.iter().map(|e| e.tertiary).collect::<Vec<_>>();
    primaries(a)
        .cmp(&primaries(b))
        .then_with(|| secondaries(a).cmp(&secondaries(b)))
        .then_with(|| tertiaries(a).cmp(&tertiaries(b)))
}

const WHITESPACE: u32 = 1 << 24;
const PUNCTUATION: u32 = 2 << 24;
const DIGIT: u32 = 3 << 24;
const LATIN: u32 = 4 << 24;
const OTHER_LETTER: u32 = 5 << 24;

/// Base characters have the common secondary weight; accents weigh
/// more.
const COMMON: u8 = 1;
const LOWER: u8 = 1;
const UPPER: u8 = 2;

/// ASCII punctuation and symbols in root collation order.
const ASCII_PUNCTUATION: &[u8] =
    b"_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn push_elements(c: char, out: &mut Vec<Element>) {
    let base = |primary: u32, upper: bool| Element {
        primary,
        secondary: COMMON,
        tertiary: if upper { UPPER } else { LOWER },
    };
    if let Some((letter, mark)) = decompose(c) {
        out.push(base(
            latin(letter.to_ascii_lowercase()),
            letter.is_ascii_uppercase(),
        ));
        out.push(accent(0x300 + u32::from(mark)));
        return;
    }
    // `ß` sorts as `ss`, after it at the case level.
    if c == 'ß' || c == 'ẞ' {
        let tertiary = if c == 'ß' { UPPER + 1 } else { UPPER + 2 };
        for _ in 0..2 {
            out.push(Element {
                tertiary,
                ..base(latin(b's'), false)
            });
        }
        return;
    }
    let special: Option<(&[u8], u8)> = match c {
        'æ' => Some((b"ae", 0)),
        'Æ' => Some((b"AE", 0)),
        'œ' => Some((b"oe", 0)),
        'Œ' => Some((b"OE", 0)),
        // Letters with a stroke sort as the letter with an accent.
        'ø' => Some((b"o", STROKE)),
        'Ø' => Some((b"O", STROKE)),
        'đ' => Some((b"d", STROKE)),
        'Đ' => Some((b"D", STROKE)),
        'ł' => Some((b"l", STROKE)),
        'Ł' => Some((b"L", STROKE)),
        'ħ' => Some((b"h", STROKE)),
        'Ħ' => Some((b"H", STROKE)),
        _ => None,
    };
    if let Some((letters, mark)) = special {
        for &letter in letters {
            out.push(base(
                latin(letter.to_ascii_lowercase()),
                letter.is_ascii_uppercase(),
            ));
        }
        if mark != 0 {
            out.push(Element {
                primary: 0,
                secondary: mark,
                tertiary: LOWER,
            });
        }
        return;
    }
    match c {
        'ð' | 'Ð' => out.push(base(latin(b'd') + 1, c == 'Ð')),
        'ı' => out.push(base(latin(b'i') + 1, false)),
        'þ' | 'Þ' => out.push(base(latin(b'z') + 2, c == 'Þ')),
        'a'..='z' | 'A'..='Z' => out.push(base(
            latin(c.to_ascii_lowercase() as u8),
            c.is_ascii_uppercase(),
        )),
        '0'..='9' => out.push(base(
            DIGIT | (u32::from(c) - u32::from('0')),
            false,
        )),
        '\u{300}'..='\u{36F}' => out.push(accent(u32::from(c))),
        _ if is_ignorable(c) => {}
        _ if c.is_whitespace() => {
            out.push(base(WHITESPACE | whitespace_weight(c), false))
        }
        _ if c.is_numeric() => {
            out.push(base(DIGIT | (16 + u32::from(c)), false))
        }
        _ if c.is_alphabetic() => {
            let lower = c.to_lowercase().next().unwrap_or(c);
            out.push(base(
                OTHER_LETTER | u32::from(lower),
                c.is_uppercase(),
            ));
        }
        _ => {
            out.push(base(PUNCTUATION | punctuation_weight(c), false))
        }
    }
}

/// Latin letters are spaced two apart, leaving room for `ð` after
/// `d`, and so on.
fn latin(lower: u8) -> u32 {
    LATIN | (u32::from(lower - b'a') * 2)
}

fn whitespace_weight(c: char) -> u32 {
    match c {
        '\t' => 1,
        '\n' => 2,
        '\u{B}' => 3,
        '\u{C}' => 4,
        '\r' => 5,
        '\u{85}' => 6,
        '\u{2028}' => 7,
        '\u{2029}' => 8,
        ' ' => 9,
        _ => 16 + u32::from(c),
    }
}

fn punctuation_weight(c: char) -> u32 {
    let ascii = u8::try_from(c)
        .ok()
        .and_then(|b| ASCII_PUNCTUATION.iter().position(|&p| p == b));
    match ascii {
        Some(i) => i as u32 + 1,
        None => 256 + u32::from(c),
    }
}

/// Controls and format characters have no weight at all.
fn is_ignorable(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{E}'..='\u{1F}' | '\u{7F}'..='\u{84}' | '\u{86}'..='\u{9F}')
        || matches!(
            c,
            '\u{AD}' | '\u{200B}'
                ..='\u{200F}' | '\u{2060}' | '\u{FEFF}'
        )
}

const STROKE: u8 = 15;

/// The secondary weight of a combining mark, in root order for the
/// common ones.
fn accent(mark: u32) -> Element {
    const ORDER: [u32; 13] = [
        0x301, 0x300, 0x306, 0x302, 0x30C, 0x30A, 0x308, 0x30B,
        0x303, 0x307, 0x327, 0x328, 0x304,
    ];
    let rank = ORDER
        .iter()
        .position(|&m| m == mark)
        .map_or(STROKE + 1 + (mark - 0x300) as u8, |i| i as u8 + 1);
    Element {
        primary: 0,
        secondary: COMMON + rank,
        tertiary: LOWER,
    }
}

/// The base letter and combining mark (as an offset from U+0300) of a
/// precomposed letter in Latin-1 Supplement or Latin Extended-A.
fn decompose(c: char) -> Option<(u8, u8)> {
    DECOMPOSITIONS
        .binary_search_by_key(&c, |&(composed, _, _)| composed)
        .ok()
        .map(|i| (DECOMPOSITIONS[i].1, DECOMPOSITIONS[i].2))
}

/// Canonical decompositions from the Unicode Character Database, U+00C0 to U+017F.
#[rustfmt::skip]
const DECOMPOSITIONS: &[(char, u8, u8)] = &[
    ('\u{00C0}', b'A', 0x00), ('\u{00C1}', b'A', 0x01), ('\u{00C2}', b'A', 0x02), ('\u{00C3}', b'A', 0x03),
    ('\u{00C4}', b'A', 0x08), ('\u{00C5}', b'A', 0x0A), ('\u{00C7}', b'C', 0x27), ('\u{00C8}', b'E', 0x00),
    ('\u{00C9}', b'E', 0x01), ('\u{00CA}', b'E', 0x02), ('\u{00CB}', b'E', 0x08), ('\u{00CC}', b'I', 0x00),
    ('\u{00CD}', b'I', 0x01), ('\u{00CE}', b'I', 0x02), ('\u{00CF}', b'I', 0x08), ('\u{00D1}', b'N', 0x03),
    ('\u{00D2}', b'O', 0x00), ('\u{00D3}', b'O', 0x01), ('\u{00D4}', b'O', 0x02), ('\u{00D5}', b'O', 0x03),
    ('\u{00D6}', b'O', 0x08), ('\u{00D9}', b'U', 0x00), ('\u{00DA}', b'U', 0x01), ('\u{00DB}', b'U', 0x02),
    ('\u{00DC}', b'U', 0x08), ('\u{00DD}', b'Y', 0x01), ('\u{00E0}', b'a', 0x00), ('\u{00E1}', b'a', 0x01),
    ('\u{00E2}', b'a', 0x02), ('\u{00E3}', b'a', 0x03), ('\u{00E4}', b'a', 0x08), ('\u{00E5}', b'a', 0x0A),
    ('\u{00E7}', b'c', 0x27), ('\u{00E8}', b'e', 0x00), ('\u{00E9}', b'e', 0x01), ('\u{00EA}', b'e', 0x02),
    ('\u{00EB}', b'e', 0x08), ('\u{00EC}', b'i', 0x00), ('\u{00ED}', b'i', 0x01), ('\u{00EE}', b'i', 0x02),
    ('\u{00EF}', b'i', 0x08), ('\u{00F1}', b'n', 0x03), ('\u{00F2}', b'o', 0x00), ('\u{00F3}', b'o', 0x01),
    ('\u{00F4}', b'o', 0x02), ('\u{00F5}', b'o', 0x03), ('\u{00F6}', b'o', 0x08), ('\u{00F9}', b'u', 0x00),
    ('\u{00FA}', b'u', 0x01), ('\u{00FB}', b'u', 0x02), ('\u{00FC}', b'u', 0x08), ('\u{00FD}', b'y', 0x01),
    ('\u{00FF}', b'y', 0x08), ('\u{0100}', b'A', 0x04), ('\u{0101}', b'a', 0x04), ('\u{0102}', b'A', 0x06),
    ('\u{0103}', b'a', 0x06), ('\u{0104}', b'A', 0x28), ('\u{0105}', b'a', 0x28), ('\u{0106}', b'C', 0x01),
    ('\u{0107}', b'c', 0x01), ('\u{0108}', b'C', 0x02), ('\u{0109}', b'c', 0x02), ('\u{010A}', b'C', 0x07),
    ('\u{010B}', b'c', 0x07), ('\u{010C}', b'C', 0x0C), ('\u{010D}', b'c', 0x0C), ('\u{010E}', b'D', 0x0C),
    ('\u{010F}', b'd', 0x0C), ('\u{0112}', b'E', 0x04), ('\u{0113}', b'e', 0x04), ('\u{0114}', b'E', 0x06),
    ('\u{0115}', b'e', 0x06), ('\u{0116}', b'E', 0x07), ('\u{0117}', b'e', 0x07), ('\u{0118}', b'E', 0x28),
    ('\u{0119}', b'e', 0x28), ('\u{011A}', b'E', 0x0C), ('\u{011B}', b'e', 0x0C), ('\u{011C}', b'G', 0x02),
    ('\u{011D}', b'g', 0x02), ('\u{011E}', b'G', 0x06), ('\u{011F}', b'g', 0x06), ('\u{0120}', b'G', 0x07),
    ('\u{0121}', b'g', 0x07), ('\u{0122}', b'G', 0x27), ('\u{0123}', b'g', 0x27), ('\u{0124}', b'H', 0x02),
    ('\u{0125}', b'h', 0x02), ('\u{0128}', b'I', 0x03), ('\u{0129}', b'i', 0x03), ('\u{012A}', b'I', 0x04),
    ('\u{012B}', b'i', 0x04), ('\u{012C}', b'I', 0x06), ('\u{012D}', b'i', 0x06), ('\u{012E}', b'I', 0x28),
    ('\u{012F}', b'i', 0x28), ('\u{0130}', b'I', 0x07), ('\u{0134}', b'J', 0x02), ('\u{0135}', b'j', 0x02),
    ('\u{0136}', b'K', 0x27), ('\u{0137}', b'k', 0x27), ('\u{0139}', b'L', 0x01), ('\u{013A}', b'l', 0x01),
    ('\u{013B}', b'L', 0x27), ('\u{013C}', b'l', 0x27), ('\u{013D}', b'L', 0x0C), ('\u{013E}', b'l', 0x0C),
    ('\u{0143}', b'N', 0x01), ('\u{0144}', b'n', 0x01), ('\u{0145}', b'N', 0x27), ('\u{0146}', b'n', 0x27),
    ('\u{0147}', b'N', 0x0C), ('\u{0148}', b'n', 0x0C), ('\u{014C}', b'O', 0x04), ('\u{014D}', b'o', 0x04),
    ('\u{014E}', b'O', 0x06), ('\u{014F}', b'o', 0x06), ('\u{0150}', b'O', 0x0B), ('\u{0151}', b'o', 0x0B),
    ('\u{0154}', b'R', 0x01), ('\u{0155}', b'r', 0x01), ('\u{0156}', b'R', 0x27), ('\u{0157}', b'r', 0x27),
    ('\u{0158}', b'R', 0x0C), ('\u{0159}', b'r', 0x0C), ('\u{015A}', b'S', 0x01), ('\u{015B}', b's', 0x01),
    ('\u{015C}', b'S', 0x02), ('\u{015D}', b's', 0x02), ('\u{015E}', b'S', 0x27), ('\u{015F}', b's', 0x27),
    ('\u{0160}', b'S', 0x0C), ('\u{0161}', b's', 0x0C), ('\u{0162}', b'T', 0x27), ('\u{0163}', b't', 0x27),
    ('\u{0164}', b'T', 0x0C), ('\u{0165}', b't', 0x0C), ('\u{0168}', b'U', 0x03), ('\u{0169}', b'u', 0x03),
    ('\u{016A}', b'U', 0x04), ('\u{016B}', b'u', 0x04), ('\u{016C}', b'U', 0x06), ('\u{016D}', b'u', 0x06),
    ('\u{016E}', b'U', 0x0A), ('\u{016F}', b'u', 0x0A), ('\u{0170}', b'U', 0x0B), ('\u{0171}', b'u', 0x0B),
    ('\u{0172}', b'U', 0x28), ('\u{0173}', b'u', 0x28), ('\u{0174}', b'W', 0x02), ('\u{0175}', b'w', 0x02),
    ('\u{0176}', b'Y', 0x02), ('\u{0177}', b'y', 0x02), ('\u{0178}', b'Y', 0x08), ('\u{0179}', b'Z', 0x01),
    ('\u{017A}', b'z', 0x01), ('\u{017B}', b'Z', 0x07), ('\u{017C}', b'z', 0x07), ('\u{017D}', b'Z', 0x0C),
    ('\u{017E}', b'z', 0x0C),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(items: &[&str]) -> Vec<String> {
        let mut items: Vec<(Key, String)> =
            items.iter().map(|s| (key(s), (*s).to_owned())).collect();
        items.sort_by(|a, b| compare(&a.0, &b.0));
        items.into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn case_and_accents_are_later_levels() {
        assert_eq!(
            sorted(&["b", "B", "a", "é", "Z"]),
            ["a", "b", "B", "é", "Z"]
        );
        assert_eq!(
            sorted(&["Résumé", "resume", "Resume", "résumé"]),
            ["resume", "Resume", "résumé", "Résumé"]
        );
    }

    #[test]
    fn groups_are_whitespace_punctuation_digits_letters() {
        assert_eq!(
            sorted(&["a", "1", "_", " ", "$", "-"]),
            [" ", "_", "-", "$", "1", "a"]
        );
        assert_eq!(sorted(&["10", "9", "1"]), ["1", "10", "9"]);
    }

    #[test]
    fn expansions_and_ignorables() {
        assert_eq!(
            compare(&key("straße"), &key("strasse")),
            Ordering::Greater
        );
        assert_eq!(
            compare(&key("straße"), &key("Strasse")),
            Ordering::Less
        );
        assert_eq!(
            compare(&key("a\u{200B}b"), &key("ab")),
            Ordering::Equal
        );
        assert_eq!(
            compare(&key("e\u{301}"), &key("é")),
            Ordering::Equal
        );
    }
}
