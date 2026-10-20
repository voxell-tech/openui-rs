//! Streaming equals parsing (spec section 4): however a text is
//! chunked, the document at stream end equals a batch parse of the
//! whole text.

mod common;

use common::{canonical, cases, library};
use openui_lang::{Document, Inputs, Library, StreamParser, parse};

/// Texts beyond the fixtures that stress chunk boundaries: comment
/// markers, fences and escapes split across chunks, ternary
/// continuations, CRLF, multi-byte characters, empty definitions and
/// mistyped closers.
const EXTRA: &[&str] = &[
    "a = Text(\"x\")\n/",
    "root = Stack([a])\na = Text(\"one\") // note\n# more\nb = \
     Text('it\\'s')\n",
    "Intro ``` not a fence\n```openui\nroot = Stack([t])\nt = \
     Text(\"a ``` b\")\n```\nprose\n```\nx = 1",
    "root = Stack([Text(s)])\n$n = 3\ns = $n > 2\n  ? \"big\"\n  : \
     $n > 0\n    ? \"small\"\n    : \"none\"\nz = 1",
    "root = Stack([Text(\"é😀\\u00e9\\uD83D\\uDE00\")])\r\nq = \
     Query(\"t\", {}, {rows: [1, 2]})\r\n",
    "root = Stack([Text(\"a\\\",\n\"b\")]",
    "root = Stack([a, b])\na = Text(\"first\")\nb = Text(\"x\")\na \
     = Text(\"seco",
    "page = Stack(@Each([1, 2, 3], \"i\", Text(\"n\" + i)))\n$sel = \
     null\nbtn = Button(\"Go\", Action([@Set($sel, 1), \
     @Run(q)]))\nq = Mutation(\"m\", {v: $sel})",
    "root = Stack([Text(1 -2), Text(-3), Text(4 - -5)])",
    "root = Stack([Input(\"q\", \"Search\", $query), Text($que",
    "root = Stack([a])\na = Text(\"kept\")\na =\nb = 1\nroot =",
    "root = Stack([a, b)]\na = Card(\"A\", [Text(\"x\")}\nb = \
     Text(\"B\")",
];

fn strict(document: &Document) -> String {
    let evaluation = document.evaluate(&Inputs::new());
    format!(
        "{}\n{:?}\n{:?}\n{:?}",
        canonical(document),
        document.diagnostics(),
        evaluation.diagnostics(),
        document.statements(),
    )
}

fn stream(library: &Library, chunks: &[&str]) -> Document {
    let mut parser = StreamParser::new(library);
    for chunk in chunks {
        // Every intermediate document must evaluate (spec section 4:
        // a valid render after every chunk).
        parser.push(chunk).evaluate(&Inputs::new());
    }
    parser.finish()
}

fn texts() -> Vec<String> {
    let mut texts: Vec<String> =
        cases().iter().filter_map(|c| c.text()).collect();
    texts.extend(EXTRA.iter().map(|s| (*s).to_owned()));
    texts
}

#[test]
fn every_two_chunk_split_matches_the_batch_parse() {
    let library = library();
    for text in texts() {
        let batch = strict(&parse(&library, &text));
        for (at, _) in text.char_indices().skip(1) {
            let streamed = strict(&stream(
                &library,
                &[&text[..at], &text[at..]],
            ));
            assert_eq!(
                streamed, batch,
                "text {text:?} split at byte {at}"
            );
        }
    }
}

#[test]
fn one_character_chunks_and_random_chunks_match_the_batch_parse() {
    let library = library();
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut random = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    for text in texts() {
        let batch = strict(&parse(&library, &text));
        let chars: Vec<&str> = text
            .char_indices()
            .map(|(i, c)| &text[i..i + c.len_utf8()])
            .collect();
        assert_eq!(
            strict(&stream(&library, &chars)),
            batch,
            "text {text:?} one character at a time"
        );
        for _ in 0..20 {
            let mut chunks = Vec::new();
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let mut at = (1 + random(12)).min(rest.len());
                while !rest.is_char_boundary(at) {
                    at += 1;
                }
                chunks.push(&rest[..at]);
                rest = &rest[at..];
            }
            assert_eq!(
                strict(&stream(&library, &chunks)),
                batch,
                "text {text:?} as {chunks:?}"
            );
        }
    }
}

#[test]
fn a_pending_statement_that_needs_closing_does_not_replace_a_completed_one()
 {
    let library = library();
    let text = |doc: &Document| {
        let evaluation = doc.evaluate(&Inputs::new());
        let root = evaluation.root().expect("renders");
        let children = root
            .prop("children")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        children[0]
            .as_element()
            .unwrap()
            .prop("text")
            .unwrap()
            .to_js_string()
    };
    let mut parser = StreamParser::new(&library);
    parser.push("root = Stack([a])\na = Text(\"one\")\nb = 1\n");
    assert_eq!(text(&parser.push("a = Text(\"tw")), "one");
    // Once it no longer needs closing it is the later definition, and
    // it wins.
    assert_eq!(text(&parser.push("o\")")), "two");
    // At stream end a truncated redefinition is final too.
    let mut parser = StreamParser::new(&library);
    parser
        .push("root = Stack([a])\na = Text(\"one\")\na = Text(\"tw");
    assert_eq!(text(&parser.finish()), "tw");
}

/// Spec Appendix A, with the fixture library, whose root component is
/// Stack.
#[test]
fn every_frame_of_the_worked_example_renders_without_errors() {
    let chunks = [
        "root = Stack([header, kpis])\nheader = Te",
        "xt(\"Tickets\", \"heading\")\nkpis = Stack([open",
        ", closed])\nopen = Metric(\"Open\", \
         @Count(@Filter(tickets.rows, \"status\", \"==\", \
         \"open\")))\n",
        "closed = Metric(\"Closed\", 8)\ntickets = \
         Query(\"listTickets\", {}, { rows: [] })\n",
    ];
    let library = library();
    let mut parser = StreamParser::new(&library);
    let mut frames = Vec::new();
    for chunk in chunks {
        let document = parser.push(chunk);
        let evaluation = document.evaluate(&Inputs::new());
        assert!(
            document.diagnostics().is_empty(),
            "{:?}",
            document.diagnostics()
        );
        assert!(
            evaluation.diagnostics().is_empty(),
            "{:?}",
            evaluation.diagnostics()
        );
        assert!(
            evaluation.queries().is_empty(),
            "queries wait for the stream to end"
        );
        frames.push(canonical(&document)["root"].to_string());
    }
    assert_eq!(
        frames[0],
        r#"{"type":"Stack","props":{"children":[]}}"#
    );
    assert_eq!(
        frames[1],
        r#"{"type":"Stack","props":{"children":[{"type":"Text","props":{"text":"Tickets","variant":"heading"}},{"type":"Stack","props":{"children":[]}}]}}"#
    );
    assert!(frames[2].contains(
        r#"{"type":"Metric","props":{"label":"Open","value":0}}"#
    ));
    let document = parser.finish();
    let queries =
        document.evaluate(&Inputs::new()).queries().to_vec();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        (queries[0].statement(), queries[0].tool()),
        ("tickets", "listTickets")
    );
}

#[test]
fn a_state_name_at_the_end_of_the_stream_may_still_grow() {
    let library = library();
    let names = |doc: &Document| {
        doc.state_defaults()
            .map(|(n, _)| n.to_owned())
            .collect::<Vec<_>>()
    };
    let mut parser = StreamParser::new(&library);
    assert!(names(&parser.push("root = Stack([Text($d")).is_empty());
    assert_eq!(names(&parser.push("ishes)])")), ["$dishes"]);
    let mut parser = StreamParser::new(&library);
    parser.push("root = Stack([Text($d");
    assert_eq!(names(&parser.finish()), ["$d"]);
}
