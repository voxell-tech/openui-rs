//! An interactive playground for the runtime. Edit a program and see what
//! the crate makes of it: the rendered tree, the diagnostics, the
//! statements, and the frames a stream goes through. Then play the host:
//! type into bound inputs, click buttons to run their action plans, and
//! supply query results.
//!
//! ```text
//! cargo run --release --example playground -- 8790
//! ```
//!
//! Then open <http://127.0.0.1:8790>. The page uses the component library of
//! the conformance fixtures, and every fixture case can be loaded and
//! compared with its expected result. Nothing leaves the machine: actions
//! only describe what a host would do, and no query calls a tool.
//!
//! The server is a `std::net` loop with a thread per connection, enough for
//! one person on localhost. Every request parses the program again.

#[path = "../tests/common/mod.rs"]
mod common;

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use openui_lang::{
    ActionStep, Diagnostic, Document, Element, Inputs, Library, Limits, StreamParser, Value,
    parse_with_limits,
};
use serde_json::{Map as JsonMap, Value as Json, json};

const PAGE: &str = include_str!("playground/index.html");
/// Request bodies larger than this are refused.
const MAX_BODY: usize = 4 << 20;
/// A streaming replay keeps at most this many frames.
const MAX_FRAMES: usize = 400;

fn main() -> io::Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(8790);
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let playground = Arc::new(Playground::new());
    println!("playground at http://127.0.0.1:{port}");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let playground = Arc::clone(&playground);
        // A browser may open a connection before it has a request to send, so
        // one idle connection must not hold up the others.
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            if let Err(error) = playground.serve(&mut stream) {
                eprintln!("request failed: {error}");
            }
        });
    }
    Ok(())
}

struct Playground {
    library: Library,
    /// The `/api/setup` response: library, examples and fixture cases.
    setup: String,
}

impl Playground {
    fn new() -> Self {
        let library = common::library();
        let signatures = |items: Vec<(String, String)>| -> Vec<Json> {
            items
                .into_iter()
                .map(|(signature, description)| json!({ "signature": signature, "description": description }))
                .collect()
        };
        let fixtures: Vec<Json> = common::cases()
            .iter()
            .filter_map(|case| {
                Some(json!({
                    "id": case.id,
                    "text": case.text()?,
                    "chunks": case.chunks(),
                    "expected": case.expected(),
                    "interactions": case.file("interactions.json"),
                }))
            })
            .collect();
        let setup = json!({
            "root": library.root(),
            "components": signatures(library.components().iter().map(|c| (c.signature(), c.description().to_owned())).collect()),
            "functions": signatures(library.functions().iter().map(|f| (f.signature(), f.description().to_owned())).collect()),
            "actions": signatures(library.actions().iter().map(|a| (a.signature(), a.description().to_owned())).collect()),
            "limits": limits_json(&Limits::DEFAULT),
            "examples": examples(),
            "fixtures": fixtures,
        });
        Self {
            library,
            setup: setup.to_string(),
        }
    }

    fn serve(&self, stream: &mut TcpStream) -> io::Result<()> {
        let request = match read_request(stream) {
            Ok(Some(request)) => request,
            // A browser opens spare connections and may close them unused. An
            // answer written to one would be read as the reply to whatever the
            // browser sends on it later, so close it silently.
            Ok(None) => return Ok(()),
            Err(error) => {
                return respond(stream, 400, "text/plain", error.to_string().as_bytes());
            }
        };
        let json_type = "application/json";
        let (status, kind, body) = match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => (200, "text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
            ("GET", "/api/setup") => (200, json_type, self.setup.clone().into_bytes()),
            ("POST", "/api/run") => self.api(&request.body, |body| self.run(body)),
            ("POST", "/api/act") => self.api(&request.body, |body| self.act(body)),
            _ => (404, "text/plain", b"not found".to_vec()),
        };
        respond(stream, status, kind, &body)
    }

    fn api(
        &self,
        body: &[u8],
        handler: impl FnOnce(&Json) -> Result<Json, String>,
    ) -> (u16, &'static str, Vec<u8>) {
        let result = serde_json::from_slice(body)
            .map_err(|e| format!("the request is not JSON: {e}"))
            .and_then(|body| handler(&body));
        match result {
            Ok(json) => (200, "application/json", json.to_string().into_bytes()),
            Err(error) => (
                400,
                "application/json",
                json!({ "error": error }).to_string().into_bytes(),
            ),
        }
    }

    /// Parses the program, in chunks when asked, and reports every frame
    /// and the final document.
    fn run(&self, body: &Json) -> Result<Json, String> {
        let text = body["text"].as_str().ok_or("`text` is missing")?;
        let limits = limits(&body["limits"]);
        let inputs = inputs(body);
        let chunks: Vec<String> = match (&body["chunks"], body["chunkSize"].as_u64()) {
            (Json::Array(chunks), _) => chunks
                .iter()
                .map(|c| c.as_str().unwrap_or_default().to_owned())
                .collect(),
            (_, Some(size)) if size > 0 => split(text, size as usize),
            _ => Vec::new(),
        };
        let mut frames = Vec::new();
        let document = if chunks.is_empty() {
            parse_with_limits(&self.library, text, limits)
        } else {
            let mut parser = StreamParser::with_limits(&self.library, limits);
            let every = chunks.len().div_ceil(MAX_FRAMES);
            let mut received = 0;
            for (i, chunk) in chunks.iter().enumerate() {
                let document = parser.push(chunk);
                received += chunk.chars().count();
                if (i + 1) % every == 0 || i + 1 == chunks.len() {
                    frames.push(json!({
                        "chunk": i + 1,
                        "received": received,
                        "report": report(&document, &inputs),
                    }));
                }
            }
            parser.finish()
        };
        let canonical = common::canonical(&document);
        let fixture = match &body["expected"] {
            Json::Null => Json::Null,
            expected => json!({ "diffs": common::compare(expected, &canonical) }),
        };
        Ok(json!({
            "chunks": chunks.len(),
            "frames": frames,
            "final": report(&document, &inputs),
            "canonical": canonical,
            "fixture": fixture,
        }))
    }

    /// Runs the action plan at `path` the way a host would: `@Set` and
    /// `@Reset` change the state, and every other step becomes an event.
    fn act(&self, body: &Json) -> Result<Json, String> {
        let text = body["text"].as_str().ok_or("`text` is missing")?;
        let path = body["path"].as_array().ok_or("`path` is missing")?;
        let document = parse_with_limits(&self.library, text, limits(&body["limits"]));
        let mut inputs = inputs(body);
        let mut state = body["state"].as_object().cloned().unwrap_or_default();
        let evaluation = document.evaluate(&inputs);
        let root = evaluation.root().ok_or("nothing renders")?;
        let Some(Value::Action(plan)) = locate(root, path) else {
            return Err("no action at that path".to_owned());
        };
        let mut events = Vec::new();
        let mut event = |kind: &str, text: String, detail: Json| {
            events.push(json!({ "kind": kind, "text": text, "detail": detail }));
        };
        if plan.steps().is_empty() {
            event(
                "Nothing",
                "the plan is empty, so nothing happens".to_owned(),
                Json::Null,
            );
        }
        for step in plan.steps() {
            match step {
                ActionStep::Set { target, value } => {
                    // Evaluated now, with the state the earlier steps left.
                    let value = document.resolve(value, &inputs);
                    let shown = common::summarize(&value, &document, &inputs);
                    event("Set", format!("{target} = {shown}"), Json::Null);
                    state.insert(target.clone(), shown);
                    inputs.set_state(target.clone(), value);
                }
                ActionStep::Reset { targets } => {
                    for target in targets {
                        let value = document
                            .state_defaults()
                            .find(|(name, _)| name == target)
                            .map_or(Value::Null, |(_, value)| value.clone());
                        let shown = common::summarize(&value, &document, &inputs);
                        event("Reset", format!("{target} = {shown}"), Json::Null);
                        state.insert(target.clone(), shown);
                        inputs.set_state(target.clone(), value);
                    }
                }
                ActionStep::Run { statement, kind } => match document.request(statement, &inputs) {
                    Some(request) => event(
                        "Run",
                        format!(
                            "the host would call tool {} for {kind:?} {statement}",
                            request.tool()
                        ),
                        json!({
                            "statement": statement,
                            "runKind": format!("{kind:?}"),
                            "tool": request.tool(),
                            "args": common::summarize(request.args(), &document, &inputs),
                        }),
                    ),
                    None => event(
                        "Run",
                        format!("{statement} has no request within the limits"),
                        Json::Null,
                    ),
                },
                ActionStep::ToAssistant { message, context } => event(
                    "ToAssistant",
                    format!("send the model {message:?}"),
                    context.as_ref().map_or(
                        Json::Null,
                        |c| json!({ "context": common::summarize(c, &document, &inputs) }),
                    ),
                ),
                ActionStep::OpenUrl { url } => {
                    event("OpenUrl", format!("open {url}"), Json::Null);
                }
                ActionStep::Custom { name, params } => event(
                    "Custom",
                    format!("custom action @{name}"),
                    json!({ "params": common::summarize(&Value::from(params.clone()), &document, &inputs) }),
                ),
                other => event("Other", format!("{other:?}"), Json::Null),
            }
        }
        Ok(json!({ "state": state, "events": events }))
    }
}

/// What the page shows for one document: the tree, the statements and every
/// list the crate reports.
fn report(document: &Document, inputs: &Inputs) -> Json {
    let evaluation = document.evaluate(inputs);
    let diagnostics: Vec<Json> = document
        .diagnostics()
        .iter()
        .map(|d| diagnostic(d, "parse"))
        .chain(
            evaluation
                .diagnostics()
                .iter()
                .map(|d| diagnostic(d, "evaluate")),
        )
        .collect();
    let state: JsonMap<String, Json> = document
        .state_defaults()
        .map(|(name, value)| (name.to_owned(), common::summarize(value, document, inputs)))
        .collect();
    let queries: Vec<Json> = evaluation
        .queries()
        .iter()
        .map(|q| {
            json!({
                "statement": q.statement(),
                "tool": q.tool(),
                "args": common::summarize(q.args(), document, inputs),
                "refreshSeconds": q.refresh_seconds(),
            })
        })
        .collect();
    json!({
        "statements": document.statements().iter().map(|s| json!({ "name": s.name(), "kind": format!("{:?}", s.kind()) })).collect::<Vec<_>>(),
        "entry": document.entry().map(|e| json!({ "statement": e.statement(), "fallback": e.is_fallback() })),
        "complete": document.is_complete(),
        "incomplete": document.is_incomplete(),
        "unresolved": document.unresolved(),
        "orphans": document.orphans(),
        "stateDefaults": state,
        "diagnostics": diagnostics,
        "tree": evaluation.root().map(|root| element(root, document, inputs, &mut Vec::new())),
        "queries": queries,
    })
}

fn diagnostic(d: &Diagnostic, phase: &str) -> Json {
    json!({
        "phase": phase,
        "source": d.source.as_str(),
        "code": d.code.as_str(),
        "severity": format!("{:?}", d.severity).to_lowercase(),
        "fatal": d.is_fatal(),
        "statement": d.statement,
        "component": d.component,
        "path": d.path,
        "message": d.message,
        "hint": d.hint,
    })
}

/// The tree for the page. Each action carries its path from the root, so
/// a click can name the plan to run.
fn element(element: &Element, document: &Document, inputs: &Inputs, path: &mut Vec<Json>) -> Json {
    let props: Vec<Json> = element
        .props()
        .iter()
        .map(|prop| {
            path.push(json!(prop.name()));
            let value = value(prop.value(), document, inputs, path);
            path.pop();
            json!({ "name": prop.name(), "value": value, "binding": prop.binding() })
        })
        .collect();
    json!({ "$type": element.component(), "statement": element.statement(), "props": props })
}

fn value(value: &Value, document: &Document, inputs: &Inputs, path: &mut Vec<Json>) -> Json {
    match value {
        Value::Element(e) => element(e, document, inputs, path),
        Value::Array(items) => Json::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    path.push(json!(i));
                    let item = self::value(item, document, inputs, path);
                    path.pop();
                    item
                })
                .collect(),
        ),
        Value::Object(map) => Json::Object(
            map.iter()
                .map(|(key, item)| {
                    path.push(json!(key));
                    let item = self::value(item, document, inputs, path);
                    path.pop();
                    (key.to_owned(), item)
                })
                .collect(),
        ),
        Value::Action(plan) => {
            let steps: Vec<Json> = plan.steps().iter().map(step).collect();
            json!({ "$action": steps, "$path": path.clone() })
        }
        scalar => common::summarize(scalar, document, inputs),
    }
}

/// A step as the page lists it. `@Set` values wait for the click.
fn step(step: &ActionStep) -> Json {
    match step {
        ActionStep::ToAssistant { message, .. } => json!(format!("@ToAssistant({message:?})")),
        ActionStep::OpenUrl { url } => json!(format!("@OpenUrl({url:?})")),
        ActionStep::Set { target, .. } => json!(format!("@Set({target}, …)")),
        ActionStep::Reset { targets } => json!(format!("@Reset({})", targets.join(", "))),
        ActionStep::Run { statement, .. } => json!(format!("@Run({statement})")),
        ActionStep::Custom { name, .. } => json!(format!("@{name}(…)")),
        other => json!(format!("{other:?}")),
    }
}

/// Follows a path of prop names, array indices and object keys from the root.
fn locate(root: &Element, path: &[Json]) -> Option<Value> {
    let (first, rest) = path.split_first()?;
    let mut current = root.prop(first.as_str()?)?.clone();
    for key in rest {
        current = match (&current, key) {
            (Value::Element(e), Json::String(name)) => e.prop(name)?.clone(),
            (Value::Array(items), Json::Number(i)) => items.get(i.as_u64()? as usize)?.clone(),
            (Value::Object(map), Json::String(name)) => map.get(name)?.clone(),
            _ => return None,
        };
    }
    Some(current)
}

/// The host's side: state values and query and mutation results, as JSON.
fn inputs(body: &Json) -> Inputs {
    let mut inputs = Inputs::new();
    let entries = |key: &str| body[key].as_object().cloned().unwrap_or_default();
    for (name, value) in entries("state") {
        inputs.set_state(name, common::to_value(&value));
    }
    for (name, value) in entries("queries") {
        inputs.set_query_result(name, common::to_value(&value));
    }
    for (name, value) in entries("mutations") {
        inputs.set_mutation_result(name, common::to_value(&value));
    }
    inputs
}

/// The page's limits. Depth stays at or below the default so the stack
/// stays bounded; the others may be raised tenfold.
fn limits(json: &Json) -> Limits {
    let mut limits = Limits::DEFAULT;
    let get = |key: &str, default: usize, max: usize| {
        json[key]
            .as_u64()
            .map_or(default, |n| (n as usize).clamp(1, max))
    };
    limits.max_depth = get("max_depth", limits.max_depth, Limits::DEFAULT.max_depth);
    limits.max_nodes = get(
        "max_nodes",
        limits.max_nodes,
        Limits::DEFAULT.max_nodes * 10,
    );
    limits.max_steps = get(
        "max_steps",
        limits.max_steps,
        Limits::DEFAULT.max_steps * 10,
    );
    limits.max_string_bytes = get(
        "max_string_bytes",
        limits.max_string_bytes,
        Limits::DEFAULT.max_string_bytes * 10,
    );
    limits
}

fn limits_json(limits: &Limits) -> Json {
    json!({
        "max_depth": limits.max_depth,
        "max_nodes": limits.max_nodes,
        "max_steps": limits.max_steps,
        "max_string_bytes": limits.max_string_bytes,
    })
}

/// Splits `text` into chunks of `size` characters.
fn split(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Reads one request, or `None` when the connection ends or idles before a
/// byte of it arrives.
fn read_request(stream: &mut TcpStream) -> io::Result<Option<Request>> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_owned());
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let header_end = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) if buffer.is_empty() => return Ok(None),
            result => result?,
        };
        if n == 0 {
            return Err(invalid("the connection closed mid-request"));
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buffer.len() > 64 * 1024 {
            return Err(invalid("the headers are too long"));
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let target = request_line.next().unwrap_or_default();
    let path = target.split('?').next().unwrap_or_default().to_owned();
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err(invalid("the body is too large"));
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(invalid("the connection closed mid-body"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(Some(Request { method, path, body }))
}

fn respond(stream: &mut TcpStream, status: u16, kind: &str, body: &[u8]) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Programs that each show one part of the runtime, with a host state to start from.
fn examples() -> Json {
    json!([
        {
            "id": "tour",
            "title": "Tour: a ticket dashboard",
            "about": "Forward references, a query with host data, built-ins, @Each and buttons. Click a Details button: it sets $selected and asks the model, and Host shows both. Clear the query result in Host to see the defaults.",
            "text": "root = Stack([header, kpis, list, selection])\nheader = Text(\"Tickets\", \"heading\")\nkpis = Stack([open, closed], \"row\")\nopen = Metric(\"Open\", @Count(@Filter(tickets.rows, \"status\", \"==\", \"open\")))\nclosed = Metric(\"Closed\", @Count(@Filter(tickets.rows, \"status\", \"==\", \"closed\")))\nlist = Card(\"Latest\", @Each(tickets.rows, \"t\", Stack([Text(t.title), Tag(t.status, t.status == \"open\" ? \"green\" : \"gray\"), Button(\"Details\", Action([@Set($selected, t.id), @ToAssistant(\"Tell me about \" + t.title)]))], \"row\")))\nselection = Text($selected ? \"Selected \" + $selected : \"Nothing selected yet\")\ntickets = Query(\"listTickets\", {status: $filter}, {rows: []})\n$filter = \"all\"\n$selected = null",
            "queries": { "tickets": { "rows": [
                { "id": "T-1", "title": "Login fails", "status": "open" },
                { "id": "T-2", "title": "Slow search", "status": "open" },
                { "id": "T-3", "title": "Typo on the home page", "status": "closed" }
            ] } }
        },
        {
            "id": "stream",
            "title": "Streaming a response",
            "about": "A model reply arrives in chunks. Drag the frame slider or press Play: completed statements stay, the unfinished tail is closed for each frame, and unresolved references are left out until they arrive. A frame can report an error for the half-written tail, such as a Metric whose value has not arrived; it clears when the rest does.",
            "stream": 9,
            "text": "Here is the summary you asked for:\n\n```openui\nroot = Card(\"Today\", [kpis, note])\nkpis = Stack([Metric(\"Orders\", 128), Metric(\"Refunds\", 3)], \"row\")\nnote = Text(\"Refunds are down \" + @Round(100 * (5 - 3) / 5) + \"% from yesterday\")\n```\n\nAsk me for a breakdown by region."
        },
        {
            "id": "state",
            "title": "State and two-way binding",
            "about": "The Input binds $name. Typing writes the host's state, and the program evaluates again with it. Reset restores the declared default.",
            "text": "root = Card(\"Profile\", [name, greeting, Button(\"Reset\", @Reset($name), \"secondary\")])\nname = Input(\"name\", \"Your name\", $name)\ngreeting = Text(\"Hello, \" + (@Upper($name) || \"stranger\") + \"!\")\n$name = \"Ada\""
        },
        {
            "id": "actions",
            "title": "Actions are plans for the host",
            "about": "Evaluating never acts. Each button holds a plan; clicking it here plays the host. Save sets $saved first, so the mutation's arguments already see it.",
            "text": "root = Card(\"Order 42\", [Stack([save, ask, docs, copy], \"row\"), status])\nsave = Button(\"Save\", Action([@Set($saved, true), @Run(saveOrder)]), \"primary\")\nask = Button(\"Ask the model\", @ToAssistant(\"Summarize order 42\", {orderId: 42}))\ndocs = Button(\"Docs\", @OpenUrl(\"https://example.com/orders\"))\ncopy = Button(\"Copy ID\", @CopyToClipboard(\"42\"))\nstatus = Text($saved ? \"Saved; the mutation is \" + saveOrder.status : \"Not saved yet\")\nsaveOrder = Mutation(\"saveOrder\", {id: 42, saved: $saved})\n$saved = false"
        },
        {
            "id": "errors",
            "title": "Mistakes and recovery",
            "about": "Each mistake removes the smallest unit and is reported: an unknown component, an enum value outside the list, a missing required argument and an extra argument. The unresolved reference `missing` is not an error: it is left out and listed under Program. Everything else renders.",
            "text": "root = Stack([title, chart, tag, metric, missing, Text(\"Still renders\", \"body\", \"extra\")])\ntitle = Text(\"Recovery demo\", \"heading\")\nchart = BarChart([1, 2, 3])\ntag = Tag(\"beta\", \"blue\")\nmetric = Metric(\"Visitors\")"
        },
        {
            "id": "expressions",
            "title": "Expressions and built-ins",
            "about": "Pluck (sorted.name), @Sort, @Avg, @Round, @First and a ternary, with JavaScript's number formatting and coercion.",
            "text": "root = Card(\"Scores\", [Table([Col(\"Name\", sorted.name), Col(\"Score\", sorted.score)]), summary, Text(best)])\nscores = [{name: \"Ada\", score: 92.5}, {name: \"Grace\", score: 88}, {name: \"Linus\", score: 95.25}]\nsorted = @Sort(scores, \"score\", \"desc\")\nsummary = Text(\"Average \" + @Round(@Avg(scores.score), 1) + \" over \" + @Count(scores) + \" players\")\nbest = @First(sorted).name + \" leads\" + (@Max(scores.score) > 90 ? \" with more than 90\" : \"\")"
        },
        {
            "id": "forms",
            "title": "Selects and forms",
            "about": "A Select bound to $team inside a Form, and text that follows the choice. Form state and validation rules are not implemented yet; the rules object passes through as data.",
            "text": "root = Form(\"signup\", [team, email, Text(\"Team: \" + $team), Button(\"Join\", @ToAssistant(\"Join \" + $team))])\nteam = Select(\"team\", [SelectItem(\"red\", \"Red team\"), SelectItem(\"blue\", \"Blue team\")], $team)\nemail = Input(\"email\", \"you@example.com\", $email, {required: true, email: true})\n$team = \"red\""
        },
        {
            "id": "limits",
            "title": "Limits on hostile input",
            "about": "Six nested loops over ten items would take a million steps. Evaluation stops at the step limit with one limit-exceeded diagnostic, and the rest still renders. Lower max_steps under Limits to see it stop sooner.",
            "text": "r = [1, 1, 1, 1, 1, 1, 1, 1, 1, 1]\nroot = Stack([Text(\"Cells: \" + @Count(@Each(r, \"a\", @Each(r, \"b\", @Each(r, \"c\", @Each(r, \"d\", @Each(r, \"e\", @Each(r, \"f\", 1)))))))), Text(\"This line still renders\")])"
        }
    ])
}
