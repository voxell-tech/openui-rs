//! Source preparation (spec section 1.2): fence extraction, comment
//! stripping, then trimming.

use std::borrow::Cow;

use crate::js;

/// The program text a response holds, ready for lexing.
pub(crate) fn prepare(raw: &str) -> String {
    let program = extract_fences(raw);
    let stripped = strip_comments(&program);
    js::trim(&stripped).to_owned()
}

/// Joins the contents of every fence with newlines, or returns the text as is
/// when it has no fence. A fence opens only with three backticks at the start
/// of a line. Inside a fence, backticks in double-quoted strings do not close it.
fn extract_fences(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut blocks: Vec<&str> = Vec::new();
    let mut line = 0;
    while line < bytes.len() {
        if !bytes[line..].starts_with(b"```") {
            line = next_line(bytes, line);
            continue;
        }
        // The rest of the opener line is the language tag.
        let start = next_line(bytes, line);
        match find_closer(bytes, start) {
            Some(close) => {
                blocks.push(&text[start..close]);
                // Text after a closer is prose until the next line.
                line = next_line(bytes, close + 3);
            }
            None => {
                blocks.push(&text[start..]);
                line = bytes.len();
            }
        }
    }
    if blocks.is_empty() {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(blocks.join("\n"))
    }
}

/// The index just past the next `\n` at or after `from`, or the end.
fn next_line(bytes: &[u8], from: usize) -> usize {
    bytes[from.min(bytes.len())..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |offset| from + offset + 1)
}

fn find_closer(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            match b {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else if b == b'"' {
            in_string = true;
        } else if bytes[i..].starts_with(b"```") {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Removes `//` and `#` comments that start outside strings. Strings of
/// either quote may span lines and shield comment markers.
fn strip_comments(text: &str) -> Cow<'_, str> {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        String(u8),
        Escape(u8),
        Comment,
    }

    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut stripped = false;
    let mut state = State::Code;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        state = match state {
            State::Code => match b {
                b'"' | b'\'' => State::String(b),
                b'#' => {
                    out.push_str(&text[copied..i]);
                    stripped = true;
                    State::Comment
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    out.push_str(&text[copied..i]);
                    stripped = true;
                    State::Comment
                }
                _ => State::Code,
            },
            State::String(quote) => match b {
                b'\\' => State::Escape(quote),
                _ if b == quote => State::Code,
                _ => State::String(quote),
            },
            State::Escape(quote) => State::String(quote),
            State::Comment => {
                if b == b'\n' {
                    copied = i;
                    State::Code
                } else {
                    State::Comment
                }
            }
        };
        i += 1;
    }
    if !stripped {
        return Cow::Borrowed(text);
    }
    if !matches!(state, State::Comment) {
        out.push_str(&text[copied..]);
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfenced_text_is_the_program() {
        assert_eq!(prepare("  a = 1\n"), "a = 1");
    }

    #[test]
    fn fences_join_and_prose_quotes_do_not_shield() {
        let raw = "Don't \"worry:\n```openui\na = \"x ``` y\"\n```\nmore\n```\nb = 2";
        assert_eq!(prepare(raw), "a = \"x ``` y\"\n\nb = 2");
    }

    #[test]
    fn a_fence_opens_only_at_line_start() {
        assert_eq!(prepare("say ```x``` and\na = 1"), "say ```x``` and\na = 1");
    }

    #[test]
    fn single_quotes_do_not_shield_a_closer() {
        assert_eq!(prepare("```\na = 'x ``` y'\n"), "a = 'x");
    }

    #[test]
    fn comments_are_stripped_outside_strings() {
        let raw = "a = 1 // one\n# note\nb = \"// kept\n# kept\" # gone\nc = 'it''s' // x";
        assert_eq!(
            prepare(raw),
            "a = 1 \n\nb = \"// kept\n# kept\" \nc = 'it''s'"
        );
    }

    #[test]
    fn an_open_fence_without_content_yields_nothing() {
        assert_eq!(prepare("Here:\n```open"), "");
    }
}
