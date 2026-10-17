//! Replays a model response in uneven chunks, the way it would arrive over a
//! network, and prints the page after every chunk. Then it acts as the host:
//! it supplies the query result and inspects one button's action plan.
//!
//! Everything is offline: the response is a constant and nothing runs actions.
//!
//! ```text
//! cargo run --example stream
//! ```

use openui_lang::{
    ActionStep, ComponentDef, Document, Element, Inputs, Library, LibraryError, Param,
    StreamParser, Type, Value,
};

const RESPONSE: &str = r#"Here is your ticket dashboard:

```openui
root = Stack([header, kpis, list])
header = Text("Tickets", "heading")
kpis = Stack([open, closed])
open = Metric("Open", @Count(@Filter(tickets.rows, "status", "==", "open")))
closed = Metric("Closed", @Count(@Filter(tickets.rows, "status", "==", "closed")))
list = Stack(@Each(tickets.rows, "t", Button(t.title, Action([@Set($selected, t.id), @ToAssistant("Tell me about " + t.title)]))))
tickets = Query("listTickets", {status: $filter}, {rows: []})
$filter = "all"
```

Ask me if you want a chart."#;

/// Chunk sizes in bytes, cycled. Model streams split text anywhere.
const CHUNK_SIZES: [usize; 8] = [7, 23, 3, 41, 12, 1, 64, 18];

fn library() -> Result<Library, LibraryError> {
    Library::builder()
        .component(
            ComponentDef::new("Stack", "Lays out children in a column")
                .param(Param::required("children", Type::list(Type::component()))),
        )
        .component(
            ComponentDef::new("Text", "A run of text")
                .param(Param::required("text", Type::String))
                .param(Param::optional(
                    "variant",
                    Type::one_of(["body", "heading"]),
                )),
        )
        .component(
            ComponentDef::new("Metric", "A labeled number")
                .param(Param::required("label", Type::String))
                .param(Param::required("value", Type::Number)),
        )
        .component(
            ComponentDef::new("Button", "A clickable button")
                .param(Param::required("label", Type::String))
                .param(Param::optional("action", Type::action())),
        )
        .root("Stack")
        .build()
}

fn main() -> Result<(), LibraryError> {
    let library = library()?;
    println!("Library:");
    for component in library.components() {
        println!("  {} — {}", component.signature(), component.description());
    }

    let mut parser = StreamParser::new(&library);
    let mut rest = RESPONSE;
    for (i, &size) in CHUNK_SIZES.iter().cycle().enumerate() {
        if rest.is_empty() {
            break;
        }
        let mut at = size.min(rest.len());
        while !rest.is_char_boundary(at) {
            at += 1;
        }
        let (chunk, tail) = rest.split_at(at);
        rest = tail;
        let document = parser.push(chunk);
        println!("\n── chunk {} {:?}", i + 1, chunk);
        show(&document, &Inputs::new());
    }

    let document = parser.finish();
    println!("\n── stream ended");
    show(&document, &Inputs::new());

    // The host runs the queries now that the stream has ended. Here it
    // answers from a fixture instead of calling a tool.
    let mut inputs = Inputs::new();
    for request in document.evaluate(&inputs).queries() {
        println!(
            "\nquery `{}` asks the host to call {} with {}",
            request.statement(),
            request.tool(),
            json(request.args())
        );
        inputs.set_query_result(request.statement(), tickets());
    }
    println!("\n── after the host returns listTickets");
    show(&document, &inputs);

    // A click on the first ticket button: the host reads the plan and decides.
    let evaluation = document.evaluate(&inputs);
    let button = evaluation.root().and_then(|root| {
        root.prop("children")?
            .as_array()?
            .get(2)?
            .as_element()?
            .prop("children")?
            .as_array()?
            .first()
            .cloned()
    });
    if let Some(Value::Element(button)) = button {
        println!(
            "\nclicking {:?} would:",
            button
                .prop("label")
                .map(Value::to_js_string)
                .unwrap_or_default()
        );
        let plan = button
            .prop("action")
            .and_then(Value::as_action)
            .map(|p| p.steps().to_vec())
            .unwrap_or_default();
        for step in plan {
            match step {
                ActionStep::Set { target, value } => {
                    println!(
                        "  set {target} to {}",
                        json(&document.resolve(&value, &inputs))
                    );
                }
                ActionStep::ToAssistant { message, .. } => println!("  send the model {message:?}"),
                other => println!("  {other:?}"),
            }
        }
    }
    Ok(())
}

fn tickets() -> Value {
    let ticket = |id: &str, title: &str, status: &str| {
        Value::object([
            ("id", Value::from(id)),
            ("title", Value::from(title)),
            ("status", Value::from(status)),
        ])
    };
    Value::object([(
        "rows",
        Value::array([
            ticket("T-1", "Login fails", "open"),
            ticket("T-2", "Slow search", "open"),
            ticket("T-3", "Typo on home page", "closed"),
        ]),
    )])
}

fn show(document: &Document, inputs: &Inputs) {
    let evaluation = document.evaluate(inputs);
    match evaluation.root() {
        Some(root) => outline(root, 1),
        None => println!("  (nothing renders yet)"),
    }
    for diagnostic in document
        .diagnostics()
        .iter()
        .chain(evaluation.diagnostics())
    {
        println!("  ! {diagnostic}");
    }
    if !document.unresolved().is_empty() {
        println!("  waiting for: {}", document.unresolved().join(", "));
    }
}

fn outline(element: &Element, depth: usize) {
    let indent = "  ".repeat(depth);
    let mut line = format!("{indent}{}", element.component());
    let mut children = Vec::new();
    for prop in element.props() {
        match prop.value() {
            Value::Array(items) if items.iter().all(|i| i.as_element().is_some()) => {
                children.extend(items.iter().filter_map(Value::as_element));
            }
            Value::Action(plan) => {
                line.push_str(&format!(" {}=<{} steps>", prop.name(), plan.steps().len()))
            }
            value => line.push_str(&format!(" {}={}", prop.name(), json(value))),
        }
    }
    println!("{line}");
    for child in children {
        outline(child, depth + 1);
    }
}

/// A compact JSON-like rendering for the outline.
fn json(value: &Value) -> String {
    match value {
        Value::String(s) => format!("{s:?}"),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", json(v)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Element(element) => format!("<{}>", element.component()),
        Value::Action(plan) => format!("<{} steps>", plan.steps().len()),
        other => other.to_js_string(),
    }
}
