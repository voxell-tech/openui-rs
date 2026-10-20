# OpenUI-rs

[![License](https://img.shields.io/badge/license-MIT%2FApache-blue.svg)](https://github.com/voxell-tech/openui-rs#license)
[![Crates.io](https://img.shields.io/crates/v/openui.svg)](https://crates.io/crates/openui)
[![Downloads](https://img.shields.io/crates/d/openui.svg)](https://crates.io/crates/openui)
[![Docs](https://docs.rs/openui/badge.svg)](https://docs.rs/openui/latest/openui/)
[![CI](https://github.com/voxell-tech/openui-rs/workflows/CI/badge.svg)](https://github.com/voxell-tech/openui-rs/actions)
[![Discord](https://img.shields.io/discord/442334985471655946.svg?label=&logo=discord&logoColor=ffffff&color=7389D8&labelColor=6A7EC2)](https://discord.gg/Mhnyp6VYEQ)

**OpenUI-rs** is an engine agnostic Rust runtime for
[OpenUI Lang], the compact language a model writes to describe a
user interface. Any renderer can draw its output: a game engine, a
native toolkit, a terminal or a web page.

This is an unofficial implementation, not affiliated with thesys,
the authors of OpenUI.

[OpenUI Lang]: https://github.com/thesysdev/openui/blob/8d9c4c3b108a7b5352192a6a9d7327bdf7ea19b5/spec/language.md

## Features

- Parses OpenUI Lang 1.0 in one go or as the text streams, and every
  partial document renders what has arrived so far.
- Checks a program against a library of components you define, and
  gives their signatures for a prompt.
- Evaluates into a renderer-neutral tree with diagnostics.
- Never acts on its own: actions come back as plans and queries as
  requests for the host to carry out.
- Treats model output as untrusted, with limits on nesting, size,
  evaluation steps and string growth.
- No UI, network, browser or model dependency.

**Status: milestone 1, not a full 1.0 client.** See the
[compatibility report](crates/openui/docs/compatibility.md).

## Example

```rust
// Feed a streamed response and render each partial page.
let mut parser = StreamParser::new(&library);
for chunk in response {
    let document = parser.push(chunk);
    let page = document.evaluate(&Inputs::new());
    draw(page.root());
}
let document = parser.finish();
```

## Where Next

- [`openui`](crates/openui): the runtime, with the full
  API walkthrough, examples and a local playground.

## Join the community!

You can join us on the [Voxell discord server](https://discord.gg/Mhnyp6VYEQ).

## License

`openui-rs` is dual-licensed under either:

- MIT License ([LICENSE-MIT](LICENSE-MIT) or
  [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

This means you can select the license you prefer! This dual-licensing
approach is the de-facto standard in the Rust ecosystem and there are
[very good reasons](https://github.com/bevyengine/bevy/issues/2373) to
include both.

The conformance fixtures vendored under
[`crates/openui/tests/fixtures/openui_1.0`](crates/openui/tests/fixtures/openui_1.0/PROVENANCE.md)
are MIT, © Thesys Inc.
