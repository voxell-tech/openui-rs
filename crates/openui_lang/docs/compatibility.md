# OpenUI Lang compatibility

`openui_lang` 0.1.0 is milestone 1 of the runtime: parsing, streaming,
references, validation, evaluation into a renderer-neutral tree, and
diagnostics. It is **not** a full OpenUI Lang 1.0 client. This report records
what is supported, what is not, and where the crate knowingly differs from the
pinned reference implementation.

## Target

| | |
| --- | --- |
| Language version | OpenUI Lang 1.0 (`openui_lang::SPEC_VERSION`) |
| Specification | [`spec/language.md`] and [`spec/prompt.md`] at thesysdev/openui `8d9c4c3b108a7b5352192a6a9d7327bdf7ea19b5` |
| Oracle | `spec/fixtures/1.0/` at the same commit, copied unchanged into [`tests/fixtures/openui-1.0/`](../tests/fixtures/openui-1.0/PROVENANCE.md) |
| Reference implementation | `packages/lang-core` at the same commit, read as a development aid only. Nothing in the crate or its tests runs JavaScript. |
| Rust | 1.85 or later (edition 2024) |

[`spec/language.md`]: https://github.com/thesysdev/openui/blob/8d9c4c3b108a7b5352192a6a9d7327bdf7ea19b5/spec/language.md
[`spec/prompt.md`]: https://github.com/thesysdev/openui/blob/8d9c4c3b108a7b5352192a6a9d7327bdf7ea19b5/spec/prompt.md

## Fixture results

59 cases in the fixture set. 53 run and pass; 6 are deferred.

| Result | Cases | Meaning |
| --- | --- | --- |
| pass | 40 | Every check of the case passes. |
| pass (L1) | 13 | The program check (`expected.json`) passes. The case also has a check this milestone cannot run: 12 have an `interactions.json`, which needs a host that runs actions, and `grammar/010-roundtrip-tree` checks serialization. |
| deferred | 6 | The feature is not implemented: `editing/001`, `002`, `003`, `005`, `006` (incremental editing) and `state/005-validation-rules` (validation rules). |

Pass (L1) cases: `actions/001`–`004`, `006`–`010`, `state/002`–`004`, `grammar/010`.

The harness compares results with the rules in the fixtures' `README.md`:
errors as a multiset where an entry claims the actual error that agrees on the
keys it lists, `unresolved` and `orphans` as sets, `state` and `errorObjects`
only when expected, everything else exactly. Each streaming case runs twice,
pushed chunk by chunk and as a batch parse of the joined chunks, and both must
match. The harness reads `library.json` into a `Library` the way prompt.md
section 2.3 describes, and checks that the derived signatures match the ones
the file carries.

Because some cases are deferred and others only partly checked, the crate does
not claim any conformance level.

## Support matrix

Supported means implemented and tested. Partial means the listed part works
and the rest is missing. Host means the spec assigns the work to a host that
this crate leaves to its caller, with the data the caller needs.

### Language (`spec/language.md`)

| Section | Status | Notes |
| --- | --- | --- |
| 1.2 Source text | Supported | Fences open at line start and join; comments are stripped outside strings; the result is trimmed. Stored-message marker lines are not recognized (see prompt.md 7). |
| 1.3 Lexical elements | Supported | Identifiers, keywords, reserved names, all operators, string escapes, numbers, and the minus rule. |
| 1.4 Statements | Supported | Statement boundaries, including a ternary that continues on the next line. |
| 1.5–1.6 Expressions and grammar | Supported | The complete grammar, with the spec's precedence, member access, indexing and digit fields. |
| 2.1 Statement kinds | Supported | Value, state, query and mutation statements. A `Query` or `Mutation` right side wins over a `$` name. |
| 2.2 Entry | Supported | `root`, else the first statement that calls the library root, with the `no-root` warning. A `root` that gives no component reports a runtime `no-root`. |
| 2.3 References and hoisting | Supported | Forward references, unresolved names, orphans, and cycle detection. |
| 2.4 Duplicate names | Supported | The last definition wins, at the first one's position. |
| 3.1–3.3 Values, operators, member access | Supported | The spec's `toNumber`, with ECMA-262 string parsing, `Number::toString`, `ToBoolean` and `IsLooselyEqual` written in Rust, plus pluck and indexing. |
| 3.4 Built-in functions | Supported | All 13 built-ins. `@Sort` uses an approximation of locale collation (see divergences). |
| 3.5 `@Each` | Supported | Templates are evaluated per element, and actions in them capture the loop variable. |
| 3.6 Custom functions | Supported | `FunctionDef` with ordered params, defaults, return type, and literal arguments checked at parse time. |
| 4 Streaming | Supported | `StreamParser` keeps completed statements, closes the pending tail, and gives the same result as a batch parse once the stream ends. |
| 5.1 State store | Partial, host | Auto-declaration and defaults (`Document::state_defaults`). Evaluation is pure: the host keeps the store and passes it as `Inputs`, so keeping user values across parses is the host's job. |
| 5.2 Two-way binding | Partial, host | A bindable prop's `Prop::binding` names the `$variable`. Writing user changes back is the host's job. |
| 5.3 Form state | Not implemented | No form scopes or form names. |
| 5.4 Validation rules | Not implemented | A rules object reaches the component as a plain value; nothing checks it. |
| 6.1 Query lifecycle | Partial, host | Defaults until the host supplies a result; `Evaluation::queries` returns a `ToolRequest` for each query once the stream is complete. Not implemented: which `$variables` trigger a refetch, stale results, the `__openui_*` keys, and refresh timers (`refresh_seconds` is reported). |
| 6.2 Mutation lifecycle | Partial, host | Mutations read `{status: "idle", data: null, error: null}` until the host supplies a result. `Document::request` evaluates the args when a mutation runs. The host runs it and rejects a second run while loading. |
| 6.3 Actions | Partial, host | Every action becomes an `ActionPlan` of `ActionStep`s, and `@Set` values stay `Deferred` until `Document::resolve`. Nothing runs. Not implemented: the event shape with `formName` and `formState`, and component default actions. Stopping a plan after a failed mutation is the host's job. |
| 6.4 Tool resolution | Host | The crate calls no tools, so it has no `tool-not-found`, `tool-error` or `mcp-error`. |
| 7 Incremental editing | Not implemented | No patch merge. |
| 8.1 Drop and render | Supported | |
| 8.2 Error codes | Partial | All parser codes, plus runtime `no-root` and `runtime-error`. Not implemented: `parse-exception` (the crate does not catch panics; one would be a bug) and `render-error` (renderers are outside the crate). Added: `limit-exceeded`. |
| 8.3 The error object | Partial | `Diagnostic` has every field of the error object. Not implemented: JSON output, `toolName`, and the emission protocol (clear at stream start, emit on change). |
| 8.4 Component validation | Supported | Positional mapping, list and component slots, array element drops, and unknown components. |
| 9.1 The tree | Supported | `Element`s with component names, props in parameter order, statement names and bindings. `Document::statements` gives each statement's kind. |
| 9.2 Serialization | Not implemented | |

### Prompt and LibrarySpec (`spec/prompt.md`)

| Section | Status | Notes |
| --- | --- | --- |
| 2 LibrarySpec | Partial | Libraries are built in Rust with ordered params (`LibraryBuilder`), with components, functions, actions and a root. Not implemented: reading a LibrarySpec or JSON Schema (only the test harness does), `id` and `version`, and extending a library (2.8). |
| 2.4, 5.1 Derived signatures | Supported | Signatures match `library.json`, except that a slot restricted to some components prints their names, as the example in prompt.md section 2 does. |
| 3 Rules for libraries | Not implemented | No conformance or backward-compatibility checks. |
| 4 Prompt generation | Not implemented | |
| 5.3 Error hints | Supported | `Signature: Name(a*, b). * marks required`, or the available names. |
| 6 The conversation | Not implemented | No history, error feedback, event messages or edit mode. |
| 7 The message protocol | Not implemented | The host must pass only the body of the last `content` section. Marker lines are not recognized: a marker line does not parse as a statement and is dropped without a diagnostic, and two `content` sections are parsed as one program. |

## Divergences from the reference implementation

These are deliberate. Most follow the spec text where the reference
implementation does something else; none changes a fixture result.

**Streaming**

- While the stream is open, enum and object checks wait for the end, and so
  does `missing-required` for a prop whose reference has not arrived. The
  reference waits only when the tail had to be closed.
- At the end of a truncated stream, enum and object checks run (spec 8.2). The
  reference skips them.
- A statement is complete only once the next line's first token arrives, so a
  ternary that continues on the next line is never cut early.

**Validation and evaluation**

- Components inside computed expressions are validated; the reference skips
  them.
- Unresolved references inside computed arrays are dropped (spec 8.4).
- Object values are computed positions, as the spec text says. The reference
  treats them as value positions.
- `parse-failed` is reported when text yields no statements; the reference
  never emits it.
- Empty input is complete and not `incomplete`; the reference reports it
  incomplete.
- State defaults are evaluated as expressions.
- Missing optional function arguments take their declared defaults.
- Data cannot pose as action steps: legacy `{type: ...}` objects are not
  actions.
- Member access on a component element reads null.
- Property reads ignore the JavaScript prototype chain, so `x.constructor` is
  null.
- `@Sort` compares strings with an approximation of ICU root collation (case,
  accents and common decompositions), not a full locale implementation. For
  arrays of 64 or more elements with an inconsistent comparison, the order may
  differ from V8's.
- A component slot restricted to some components names them in its signature.
- `runtime-error` messages read `evaluating prop "text" of Text failed: ...`,
  with `component`, `path` and `statement` set.

**Limits**

Model output is untrusted, so parsing and evaluation are bounded by `Limits`
(spec 8.1). The part beyond a bound is dropped and reported
once with the extension code `limit-exceeded`; the rest renders.

| Limit | Default | Bounds |
| --- | --- | --- |
| `max_depth` | 96 | Expression nesting and statements inlined through references. Evaluation has the same bound, counting query defaults that read other queries, so the defaults fit a 2 MiB thread stack in a debug build. |
| `max_nodes` | 100 000 | Nodes in the checked program after references are inlined, so reference fan-out cannot grow exponentially. |
| `max_steps` | 1 000 000 | Steps in one `evaluate` call, including `@Each` and built-ins. Reading, comparing or hashing a string costs a step per 64 bytes, and checking a library function's arguments costs a step per value. Query requests get a budget of their own; a request that goes past it is left out rather than sent incomplete. |
| `max_string_bytes` | 8 MiB | Bytes of strings built by `+` and conversions, or copied into sort keys and action plans, in one evaluation. |

Diagnostics stay small too: a message quotes at most 40 characters of a
value, and one evaluation reports at most 100 runtime errors, followed by one
`limit-exceeded` diagnostic for the rest.

Each push re-prepares and re-checks the whole text, which costs O(n) per push,
as in the reference. Wide inputs (50 000 keys, references or state names, or
200 000 newlines) stay linear. Each test in `tests/limits.rs` fails if it takes
more than five seconds, so work that grows faster than the input is caught.

## Reproducing

```sh
cargo fmt --check
cargo build --all-targets
cargo clippy --all-targets
cargo test
cargo test --test conformance -- --nocapture   # per-case report
cargo run --example stream                     # offline streaming replay
```

Nothing contacts a network or a model. To check the minimum Rust version, run
`cargo +1.85 test`.

## Remaining work toward full 1.0

In rough dependency order:

1. The message protocol (prompt.md 7): read marker lines and sections, keep
   the last `content`, and hold back partial markers while streaming.
2. Incremental editing (language.md 7), with the `editing/` fixtures.
3. Serialization (9.2) and the round-trip check of `grammar/010`.
4. A host runtime layer: a state store that keeps user values across parses,
   form scopes and form state, validation rules, action execution with event
   shapes, query scheduling and stale-result handling, tool resolution and its
   error codes, and the error-list emission protocol. The `interactions.json`
   checks then run.
5. LibrarySpec input and output: JSON Schema import, `id` and `version`,
   extension, and the conformance and compatibility rules.
6. Prompt generation and the conversation (prompt.md 4–6).
