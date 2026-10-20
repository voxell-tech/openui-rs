//! The OpenUI Lang 1.0 conformance fixtures (spec Appendix B), with
//! the comparison rules of `tests/fixtures/openui-1.0/README.md`.
//!
//! Every Level 1 program case runs, except the ones listed in
//! [`DEFERRED`]. Run with `--nocapture` to print the per-case report.

mod common;

use common::{
    Case, canonical, cases, compare, fixture_spec, library,
};
use openui_lang::{StreamParser, parse};

/// Cases this milestone does not implement, by id prefix, with the
/// reason.
const DEFERRED: &[(&str, &str)] = &[
    (
        "editing/",
        "incremental editing (spec section 7) is not implemented",
    ),
    (
        "state/005-validation-rules",
        "form validation rules (spec section 5.4) are not \
         implemented",
    ),
];

/// Checks inside running cases that this milestone does not
/// implement.
const DEFERRED_CHECKS: &[(&str, &str)] = &[
    (
        "-roundtrip-",
        "the round-trip check needs serialization (spec section \
         9.2); the program check runs",
    ),
    (
        "interactions.json",
        "Level 2 interaction checks need a renderer that runs \
         actions; the program check runs",
    ),
];

#[test]
fn conformance_fixtures() {
    let library = library();
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for case in cases() {
        if let Some((_, reason)) = DEFERRED
            .iter()
            .find(|(prefix, _)| case.id.starts_with(prefix))
        {
            report.push(format!("deferred  {}: {reason}", case.id));
            continue;
        }
        let diffs = run(&case, &library);
        let partial: Vec<&str> = DEFERRED_CHECKS
            .iter()
            .filter(|(marker, _)| {
                case.id.contains(marker)
                    || case.files.contains_key(*marker)
            })
            .map(|(_, reason)| *reason)
            .collect();
        let status = if !diffs.is_empty() {
            "FAIL    "
        } else if partial.is_empty() {
            "pass    "
        } else {
            "pass (L1)"
        };
        report.push(format!(
            "{status}  {}{}",
            case.id,
            partial
                .iter()
                .map(|r| format!("; {r}"))
                .collect::<String>()
        ));
        if !diffs.is_empty() {
            failures.push(format!(
                "{}\n    {}",
                case.id,
                diffs.join("\n    ")
            ));
        }
    }
    println!("{}", report.join("\n"));
    assert!(
        failures.is_empty(),
        "{} cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Runs one program case. Streaming cases must match both pushed
/// chunk by chunk and as a batch parse of the joined chunks.
fn run(case: &Case, library: &openui_lang::Library) -> Vec<String> {
    let expected =
        case.expected().expect("program cases have expected.json");
    match case.chunks() {
        Some(chunks) => {
            let mut parser = StreamParser::new(library);
            for chunk in &chunks {
                parser.push(chunk);
            }
            let streamed = canonical(&parser.finish());
            let batch = canonical(&parse(library, &chunks.concat()));
            let mut diffs: Vec<String> =
                compare(&expected, &streamed)
                    .into_iter()
                    .map(|d| format!("streamed {d}"))
                    .collect();
            diffs.extend(
                compare(&expected, &batch)
                    .into_iter()
                    .map(|d| format!("batch {d}")),
            );
            diffs
        }
        None => {
            let text = case
                .file("input.oui")
                .expect("program cases have input.oui");
            compare(&expected, &canonical(&parse(library, text)))
        }
    }
}

/// The signatures the library derives match the ones `library.json`
/// carries, except where it names a component slot's type, as
/// prompt.md section 2's example does (`Card(children: Button[])`).
/// The fixture library gives its schemas no names, so it prints those
/// slots as `any[]`.
#[test]
fn derived_signatures_match_library_json() {
    const NAMED_SLOTS: &[(&str, &str)] = &[
        (
            "Select",
            "Select(name: string, items: SelectItem[], value?: \
             $binding<string>)",
        ),
        ("Table", "Table(columns: Col[])"),
    ];
    let spec = fixture_spec();
    let library = library();
    for component in library.components() {
        let expected = NAMED_SLOTS
            .iter()
            .find(|(name, _)| *name == component.name())
            .map(|(_, signature)| (*signature).to_owned())
            .unwrap_or_else(|| {
                spec["components"][component.name()]["signature"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            });
        assert_eq!(component.signature(), expected);
    }
    for function in library.functions() {
        assert_eq!(
            function.signature(),
            spec["functions"][function.name()]["signature"]
        );
    }
    for action in library.actions() {
        assert_eq!(
            action.signature(),
            spec["actions"][action.name()]["signature"]
        );
    }
}
