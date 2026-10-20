# OpenUI Lang

[![License](https://img.shields.io/badge/license-MIT%2FApache-blue.svg)](https://github.com/voxell-tech/openui-rs#license)
[![Crates.io](https://img.shields.io/crates/v/openui_lang.svg)](https://crates.io/crates/openui_lang)
[![Downloads](https://img.shields.io/crates/d/openui_lang.svg)](https://crates.io/crates/openui_lang)
[![Docs](https://docs.rs/openui_lang/badge.svg)](https://docs.rs/openui_lang/latest/openui_lang/)
[![CI](https://github.com/voxell-tech/openui-rs/workflows/CI/badge.svg)](https://github.com/voxell-tech/openui-rs/actions)
[![Discord](https://img.shields.io/discord/442334985471655946.svg?label=&logo=discord&logoColor=ffffff&color=7389D8&labelColor=6A7EC2)](https://discord.gg/Mhnyp6VYEQ)

**`openui_lang`** is a renderer-neutral runtime for [OpenUI Lang]
1.0. It parses a program, in one go or as the text streams, checks
it against a library of components you define, and evaluates it
into a tree that any renderer can draw, with diagnostics. It has no
UI, network, browser or model dependency, and it never acts on its
own: actions come back as plans and queries as requests for the
host to carry out.

**Status: milestone 1, not a full 1.0 client.** 53 of the 59
upstream conformance fixtures run and pass; editing and validation
rules are not implemented, and forms, action execution, query
scheduling, serialization and the stored-message protocol are not
either. See the [compatibility report](docs/compatibility.md) for
the support matrix and the known divergences.

[OpenUI Lang]: https://github.com/thesysdev/openui/blob/8d9c4c3b108a7b5352192a6a9d7327bdf7ea19b5/spec/language.md

## Use

Define the components a model may use, in argument order:

```rust
use openui_lang::{ComponentDef, Inputs, Library, Param, StreamParser, Type};

let library = Library::builder()
    .component(
        ComponentDef::new("Stack", "Lays out children in a column")
            .param(Param::required("children", Type::list(Type::component()))),
    )
    .component(
        ComponentDef::new("Text", "A run of text")
            .param(Param::required("text", Type::String))
            .param(Param::optional("variant", Type::one_of(["body", "heading"]))),
    )
    .root("Stack")
    .build()?;

// The same definitions give the signatures for a prompt.
assert_eq!(library.components()[1].signature(), r#"Text(text: string, variant?: "body" | "heading")"#);

let mut parser = StreamParser::new(&library);
for chunk in ["root = Stack([tit", "le])\ntitle = Text(\"Hi\", \"head", "ing\")"] {
    let document = parser.push(chunk);
    // Every intermediate document renders what has arrived so far.
    let _ = document.evaluate(&Inputs::new()).root();
}
let document = parser.finish(); // the same result as `openui_lang::parse`
assert!(document.diagnostics().is_empty());
```

- `Document::evaluate(&Inputs)` gives the `Element` tree, runtime
  diagnostics, and the query requests. `Inputs` carries the host's
  state values and query and mutation results.
- An action prop evaluates to an `ActionPlan` of steps (`Set`,
  `Reset`, `Run`, `ToAssistant`, `OpenUrl`, custom). The host
  decides whether to run it; `Document::resolve` evaluates a `@Set`
  value at that time.
- Every recovery is a `Diagnostic` with the spec's code, statement,
  component, JSON path, message and hint.
- Model output is untrusted. `Limits` bounds nesting, inlined size,
  evaluation steps and string growth, and anything beyond is
  dropped with a `limit-exceeded` diagnostic.

## Run

```sh
cargo run --example stream   # replays a response in uneven chunks, offline
cargo run --release --example playground -- 8790   # then open http://127.0.0.1:8790
cargo test                   # unit, API, streaming, limits and conformance tests
cargo test --test conformance -- --nocapture   # per-fixture report
```

The [stream example](examples/stream.rs) prints the page after
every chunk, then plays the host: it answers the query and shows
what a click on the first button would do.

The [playground](examples/playground.rs) is a local web page for
the same things, interactively. Edit a program or load an example
or any fixture case, and see the rendered tree, the diagnostics,
the statements and each streaming frame. Typing in a bound input
sets state, and clicking a button runs its action plan the way a
host would. A fixture case is compared with its `expected.json`. It
listens on 127.0.0.1 only and calls no model or tool.

Requires Rust 1.85 or later. The fixtures are vendored under
[`tests/fixtures/openui-1.0`](tests/fixtures/openui-1.0/PROVENANCE.md),
pinned to thesysdev/openui `8d9c4c3`, so nothing needs the network.
