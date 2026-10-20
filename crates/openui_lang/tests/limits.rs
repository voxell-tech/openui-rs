//! Bounded failure: hostile programs end quickly with a
//! `limit-exceeded` diagnostic instead of exhausting the stack or
//! memory.
//!
//! Each test also bounds its wall-clock time, far above what it needs
//! in a debug build, so that work growing faster than the input fails
//! the test instead of only slowing it down.

mod common;

use std::time::{Duration, Instant};

use common::library;
use openui_lang::{
    Code, ComponentDef, Document, FunctionDef, Inputs, Library,
    Limits, Param, Source, StreamParser, Type, Value, parse,
    parse_with_limits,
};

/// Runs `f`, failing if it takes more than five seconds.
fn quickly<T>(f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    out
}

fn codes(document: &Document) -> Vec<(Source, Code, Option<String>)> {
    let evaluation = document.evaluate(&Inputs::new());
    document
        .diagnostics()
        .iter()
        .chain(evaluation.diagnostics())
        .map(|d| (d.source, d.code, d.statement.clone()))
        .collect()
}

fn has_limit(document: &Document, source: Source) -> bool {
    codes(document).iter().any(|(s, code, _)| {
        *s == source && *code == Code::LimitExceeded
    })
}

fn renders(document: &Document) -> bool {
    document.evaluate(&Inputs::new()).root().is_some()
}

#[test]
fn deep_nesting_drops_only_its_statement() {
    quickly(|| {
        let library = library();
        for deep in [
            format!(
                "{}1{}",
                "[".repeat(100_000),
                "]".repeat(100_000)
            ),
            format!(
                "{}1{}",
                "(".repeat(100_000),
                ")".repeat(100_000)
            ),
            format!("{}1", "!".repeat(100_000)),
            format!("{}1", "-".repeat(100_000)),
            format!("a{}", ".b".repeat(100_000)),
            format!("{}1", "1 ? 1 : ".repeat(100_000)),
            // Unclosed: the stream ended inside 100 000 brackets.
            "{a: [".repeat(100_000),
        ] {
            let text = format!(
                "root = Stack([t])\nt = Text(\"ok\")\nbad = {deep}"
            );
            let document = parse(&library, &text);
            assert!(
                renders(&document),
                "the rest of the program renders"
            );
            let limits: Vec<_> = codes(&document)
                .into_iter()
                .filter(|(_, code, _)| *code == Code::LimitExceeded)
                .collect();
            assert_eq!(
                limits,
                [(
                    Source::Parser,
                    Code::LimitExceeded,
                    Some("bad".to_owned())
                )]
            );
        }
    });
}

#[test]
fn long_reference_chains_stop_at_the_depth_limit() {
    quickly(|| {
        let mut text = String::from("a0 = Text(\"x\")\n");
        for i in 1..10_000 {
            text.push_str(&format!("a{i} = Stack([a{}])\n", i - 1));
        }
        text.push_str("root = a9999\n");
        let document = parse(&library(), &text);
        assert!(has_limit(&document, Source::Parser));
        assert!(
            renders(&document),
            "the part within the limit renders"
        );
    });
}

#[test]
fn exponential_fan_out_stops_at_the_node_limit() {
    quickly(|| {
        let mut text = String::from(
            "root = Stack([Text(\"n\" + @Count(a40))])\na0 = [1, \
             1]\n",
        );
        for i in 1..=40 {
            text.push_str(&format!("a{i} = [a{0}, a{0}]\n", i - 1));
        }
        let document = parse(&library(), &text);
        assert!(has_limit(&document, Source::Parser));
        assert!(renders(&document));
    });
}

#[test]
fn reference_cycles_are_unresolved_not_infinite() {
    quickly(|| {
        let document = parse(
            &library(),
            "root = Stack([a, s])\na = Card(\"A\", [b])\nb = \
             Card(\"B\", [a])\ns = [s, s]",
        );
        let mut unresolved = document.unresolved().to_vec();
        unresolved.sort();
        assert_eq!(unresolved, ["a", "s"]);
        assert!(renders(&document));
    });
}

#[test]
fn nested_iteration_stops_at_the_step_limit() {
    quickly(|| {
        let items = vec!["1"; 200].join(", ");
        let text = format!(
            "r = [{items}]\nroot = Stack(@Each(r, \"a\", @Each(r, \
             \"b\", @Each(r, \"c\", Text(\"x\")))))"
        );
        let document = parse(&library(), &text);
        assert!(has_limit(&document, Source::Runtime));
        assert!(
            renders(&document),
            "what was built before the limit renders"
        );
    });
}

#[test]
fn string_growth_stops_at_the_string_limit() {
    quickly(|| {
        let items = vec!["1"; 100].join(", ");
        let ten_kib = "x".repeat(10 * 1024);
        let library = library();
        // Joining strings in a loop: 100 x 100 x 20 KiB is far past 8
        // MiB.
        let joins = format!(
            "$s = \"{ten_kib}\"\nr = [{items}]\nroot = \
             Stack(@Each(r, \"a\", @Each(r, \"b\", Text($s + $s))))"
        );
        assert!(has_limit(&parse(&library, &joins), Source::Runtime));
        // One value shared many times prints as a long string: 100 x
        // 100 x 10 KiB.
        let shared = format!(
            "$s = [{}]\nx = [{}]\nroot = Stack([Text(\"\" + x)])",
            vec![format!("\"{ten_kib}\"").as_str(); 100].join(", "),
            vec!["$s"; 100].join(", ")
        );
        let document = parse(&library, &shared);
        assert!(has_limit(&document, Source::Runtime));
        assert!(renders(&document));
    });
}

#[test]
fn limits_can_be_tightened() {
    quickly(|| {
        let mut limits = Limits::DEFAULT;
        limits.max_steps = 5;
        let text =
            "root = Stack([Text(\"a\"), Text(\"b\"), Text(\"c\")])";
        assert!(!has_limit(
            &parse(&library(), text),
            Source::Runtime
        ));
        assert!(has_limit(
            &parse_with_limits(&library(), text, limits),
            Source::Runtime
        ));
        let mut limits = Limits::DEFAULT;
        limits.max_depth = 3;
        let deep = parse_with_limits(
            &library(),
            "root = Stack([Card(\"a\", [Card(\"b\", \
             [Text(\"c\")])])])",
            limits,
        );
        assert!(has_limit(&deep, Source::Parser));
    });
}

#[test]
fn many_statements_and_tiny_chunks_finish() {
    quickly(|| {
        let library = library();
        let mut text = String::from("root = Stack([s0, s4999])\n");
        for i in 0..5_000 {
            text.push_str(&format!(
                "s{i} = Text(\"statement {i}\")\n"
            ));
        }
        let batch = parse(&library, &text);
        assert!(renders(&batch));
        assert_eq!(batch.statements().len(), 5_001);
        // A 3 KiB program pushed one byte at a time.
        let small = &text[..3_000];
        let mut parser = StreamParser::new(&library);
        for i in 0..small.len() {
            parser.push(&small[i..i + 1]);
        }
        assert_eq!(
            parser.finish().statements(),
            parse(&library, small).statements()
        );
    });
}

#[test]
fn wide_inputs_cost_linear_time() {
    quickly(|| {
        let library = library();
        let n = 50_000;
        let keys: Vec<String> =
            (0..n).map(|i| format!("k{i}: {i}")).collect();
        let refs: Vec<String> =
            (0..n).map(|i| format!("missing{i}")).collect();
        let states: Vec<String> =
            (0..n).map(|i| format!("$s{i}")).collect();
        let text = format!(
            "root = Stack([Text(\"\" + @Count([o]))])\no = \
             {{{}}}\nu = [{}]\nv = [{}]\nw = 1{}? 2 : 3\nx = \
             Stack([u, v, Text(\"\" + w)])",
            keys.join(", "),
            refs.join(", "),
            states.join(", "),
            "\n".repeat(200_000)
        );
        let document = parse(&library, &text);
        assert!(renders(&document));
        assert_eq!(document.orphans(), ["u", "v", "w", "x"]);
        let all = parse(
            &library,
            &text.replace(
                "root = Stack([Text(\"\" + @Count([o]))])",
                "root = x",
            ),
        );
        assert_eq!(all.unresolved().len(), n);
        assert_eq!(all.state_defaults().count(), n);
        assert!(renders(&all));
    });
}

/// A library whose functions check their arguments at runtime.
fn checked_library() -> Library {
    let pick = FunctionDef::new("Pick", "Picks a mode", |_| {
        Ok(Value::from("picked"))
    })
    .param(Param::required("mode", Type::one_of(["a", "b"])))
    .returns(Type::String);
    let total = FunctionDef::new("Total", "Counts numbers", |args| {
        let n = args
            .get("xs")
            .and_then(Value::as_array)
            .map_or(0, <[_]>::len);
        Ok(Value::from(n as f64))
    })
    .param(Param::required("xs", Type::list(Type::Number)))
    .returns(Type::Number);
    Library::builder()
        .component(ComponentDef::new("Stack", "A column").param(
            Param::required(
                "children",
                Type::list(Type::component()),
            ),
        ))
        .component(
            ComponentDef::new("Text", "Text")
                .param(Param::required("text", Type::String)),
        )
        .function(pick)
        .function(total)
        .root("Stack")
        .build()
        .unwrap()
}

#[test]
fn work_on_long_strings_is_charged() {
    let library = library();
    let long = "1".repeat(256 * 1024);
    let items = vec!["1"; 100].join(", ");
    // Each expression reads all of `s`; 10 000 of them would read 2.5
    // GiB.
    for expr in [
        "s > 0".to_owned(),
        "s == 1".to_owned(),
        "-s".to_owned(),
        "s.length".to_owned(),
        "@Abs(s)".to_owned(),
        "@Count(@Filter([s], \"\", \"contains\", \"2\"))".to_owned(),
        format!("o.k{long}"),
    ] {
        let text = format!(
            "s = \"{long}\"\no = {{k{long}: 1}}\nr = \
             [{items}]\nroot = Stack([Text(\"\" + @Count(@Each(r, \
             \"a\", @Each(r, \"b\", {expr}))))])"
        );
        let document = parse(&library, &text);
        let charged =
            quickly(|| has_limit(&document, Source::Runtime));
        assert!(charged, "{expr} goes past the step limit");
    }
}

#[test]
fn checking_library_arguments_is_charged() {
    let items = vec!["1"; 5_000].join(", ");
    let text = format!(
        "r = [{items}]\nroot = Stack([Text(\"\" + @Count(@Each(r, \
         \"i\", @Total($xs))))])"
    );
    let document = parse(&checked_library(), &text);
    let mut inputs = Inputs::new();
    // 5 000 calls that each check 5 000 host values.
    inputs
        .set_state("$xs", Value::array((0..5_000).map(Value::from)));
    let evaluation = quickly(|| document.evaluate(&inputs));
    assert!(
        evaluation
            .diagnostics()
            .iter()
            .any(|d| d.code == Code::LimitExceeded)
    );
}

#[test]
fn queries_reading_queries_stay_within_the_stack() {
    // Each default nests 80 calls around the next query.
    let mut text = String::from("root = Stack([Text(q0)])\n");
    for i in 0..6 {
        let calls = format!(
            "{}q{}{}",
            "@Upper(".repeat(80),
            i + 1,
            ")".repeat(80)
        );
        text.push_str(&format!(
            "q{i} = Query(\"t\", {{}}, {calls})\n"
        ));
    }
    text.push_str("q6 = Query(\"t\", {}, \"x\")\n");
    // A spawned thread's default stack in a debug build; release
    // frames are smaller.
    let stack = if cfg!(debug_assertions) {
        2 << 20
    } else {
        512 << 10
    };
    let limited = std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            has_limit(&parse(&library(), &text), Source::Runtime)
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(limited);
}

#[test]
fn runtime_errors_stay_few_and_short() {
    let items = vec!["1"; 2_000].join(", ");
    let text = format!(
        "r = [{items}]\nroot = Stack(@Each(r, \"i\", \
         Text(@Pick($mode))))"
    );
    let document = parse(&checked_library(), &text);
    let mut inputs = Inputs::new();
    // 2 000 failing calls, each quoting a 2 000-character value.
    inputs.set_state("$mode", Value::from("z".repeat(2_000)));
    let evaluation = quickly(|| document.evaluate(&inputs));
    let count = |code| {
        evaluation
            .diagnostics()
            .iter()
            .filter(|d| d.code == code)
            .count()
    };
    assert_eq!(count(Code::RuntimeError), 100);
    assert_eq!(count(Code::LimitExceeded), 1);
    for diagnostic in evaluation.diagnostics() {
        assert!(
            diagnostic.message.len() < 300,
            "{}",
            diagnostic.message
        );
    }
}
