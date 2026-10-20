//! What a host relies on beyond the fixtures: inputs, deferred action
//! values, tool requests, and runtime diagnostics.

use openui::{
    ActionDef, ActionStep, Code, ComponentDef, Document, FunctionDef,
    Inputs, Library, Limits, Param, RunKind, Severity, Source, Type,
    Value, parse, parse_with_limits,
};

fn library() -> Library {
    let text = ComponentDef::new("Text", "Text")
        .param(Param::required("text", Type::String));
    let stack = ComponentDef::new("Stack", "A column").param(
        Param::required("children", Type::list(Type::component())),
    );
    let button = ComponentDef::new("Button", "A button")
        .param(Param::required("label", Type::String))
        .param(Param::optional("action", Type::action()));
    let input = ComponentDef::new("Input", "A field")
        .param(Param::required("name", Type::String))
        .param(Param::optional("value", Type::String).bindable())
        .param(
            Param::optional("size", Type::one_of(["s", "m"]))
                .with_default("m"),
        );
    let label =
        FunctionDef::new("Label", "Formats a count", |args| {
            let n =
                args.get("n").and_then(Value::as_f64).unwrap_or(0.0);
            if n < 0.0 {
                return Err("negative count".into());
            }
            Ok(if n > 99.0 {
                Value::from(n)
            } else {
                Value::from(format!("{n} items"))
            })
        })
        .param(Param::required("n", Type::Number))
        .returns(Type::String);
    let copy = ActionDef::new("Copy", "Copies text")
        .param(Param::required("text", Type::String));
    Library::builder()
        .component(stack)
        .component(text)
        .component(button)
        .component(input)
        .function(label)
        .action(copy)
        .root("Stack")
        .build()
        .unwrap()
}

fn children(document: &Document, inputs: &Inputs) -> Vec<Value> {
    let evaluation = document.evaluate(inputs);
    let root = evaluation.root().expect("renders");
    root.prop("children").unwrap().as_array().unwrap().to_vec()
}

fn steps(value: &Value) -> Vec<ActionStep> {
    value
        .as_element()
        .unwrap()
        .prop("action")
        .unwrap()
        .as_action()
        .unwrap()
        .steps()
        .to_vec()
}

#[test]
fn state_and_query_results_come_from_the_host() {
    let document = parse(
        &library(),
        "root = Stack([Text(\"Hi \" + $name), Text(\"\" + \
         @Count(q.rows)), Input(\"n\", $name)])\n$name = \"Ada\"\nq \
         = Query(\"list\", {who: $name}, {rows: []}, 30)",
    );
    let text = |v: &Value| {
        v.as_element().unwrap().prop("text").unwrap().to_js_string()
    };
    let defaults = children(&document, &Inputs::new());
    assert_eq!(
        (text(&defaults[0]), text(&defaults[1])),
        ("Hi Ada".into(), "0".into())
    );

    let mut inputs = Inputs::new();
    inputs
        .set_state("$name", Value::from("Grace"))
        .set_query_result(
            "q",
            Value::object([(
                "rows",
                Value::array([Value::from(1), Value::from(2)]),
            )]),
        );
    let updated = children(&document, &inputs);
    assert_eq!(
        (text(&updated[0]), text(&updated[1])),
        ("Hi Grace".into(), "2".into())
    );
    let field = updated[2].as_element().unwrap();
    let value =
        field.props().iter().find(|p| p.name() == "value").unwrap();
    assert_eq!(
        (value.binding(), value.value()),
        (Some("$name"), &Value::from("Grace"))
    );
    // An absent optional prop stays absent, even with a schema
    // default; only required ones are filled.
    assert_eq!(field.prop("size"), None);

    let queries = document.evaluate(&inputs).queries().to_vec();
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].tool(), "list");
    assert_eq!(
        queries[0].args(),
        &Value::object([("who", Value::from("Grace"))])
    );
    assert_eq!(queries[0].refresh_seconds(), Some(30.0));
}

#[test]
fn set_values_wait_for_the_action_and_capture_the_loop_element() {
    let document = parse(
        &library(),
        "rows = [{id: \"a\"}, {id: \"b\"}]\nroot = \
         Stack(@Each(rows, \"r\", Button(r.id, \
         Action([@Set($picked, r.id + $suffix), \
         @Run(save)]))))\n$suffix = \"!\"\nsave = \
         Mutation(\"save\", {picked: $picked})",
    );
    let buttons = children(&document, &Inputs::new());
    assert_eq!(buttons.len(), 2);
    let mut inputs = Inputs::new();
    inputs.set_state("$suffix", Value::from("?"));
    for (button, expected) in buttons.iter().zip(["a?", "b?"]) {
        let plan = steps(button);
        let [
            ActionStep::Set { target, value },
            ActionStep::Run { statement, kind },
        ] = plan.as_slice()
        else {
            panic!("unexpected steps {plan:?}");
        };
        assert_eq!(target, "$picked");
        // Evaluated when the action runs, with the state at that
        // time.
        assert_eq!(
            document.resolve(value, &inputs),
            Value::from(expected)
        );
        assert_eq!(
            (statement.as_str(), *kind),
            ("save", RunKind::Mutation)
        );
    }
    inputs.set_state("$picked", Value::from("a?"));
    let request = document.request("save", &inputs).unwrap();
    assert_eq!(
        request.args(),
        &Value::object([("picked", Value::from("a?"))])
    );
    // A deferred value from a document with another library does not
    // resolve.
    let other = parse(&library(), "root = Stack([])");
    let plan = steps(&buttons[0]);
    let [ActionStep::Set { value, .. }, _] = plan.as_slice() else {
        unreachable!()
    };
    assert_eq!(other.resolve(value, &inputs), Value::Null);
}

#[test]
fn library_functions_and_actions_are_checked_when_they_run() {
    let document = parse(
        &library(),
        "root = Stack([Text(@Label($n)), Text(@Label($m)), \
         Text(@Label($big)), Button(\"c\", @Copy($n)), \
         Button(\"d\", @Copy(\"ok\"))])\n$n = 3\n$m = -1\n$big = 120",
    );
    let evaluation = document.evaluate(&Inputs::new());
    let root = evaluation.root().unwrap();
    let items = root.prop("children").unwrap().as_array().unwrap();
    assert_eq!(
        items[0].as_element().unwrap().prop("text"),
        Some(&Value::from("3 items"))
    );
    // A thrown error or a wrong return type makes the call null, so
    // the required prop drops out.
    assert_eq!(items[1].as_element().unwrap().prop("text"), None);
    assert_eq!(items[2].as_element().unwrap().prop("text"), None);
    // An invalid argument makes a custom action do nothing.
    assert_eq!(steps(&items[3]), []);
    assert!(
        matches!(&steps(&items[4])[..], [ActionStep::Custom { name, params }] if name == "Copy" && params.get("text") == Some(&Value::from("ok")))
    );
    let runtime: Vec<_> = evaluation
        .diagnostics()
        .iter()
        .map(|d| {
            (
                d.source,
                d.code,
                d.component.as_deref(),
                d.path.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        runtime,
        [
            (
                Source::Runtime,
                Code::RuntimeError,
                Some("Text"),
                Some("/text")
            ),
            (
                Source::Runtime,
                Code::RuntimeError,
                Some("Text"),
                Some("/text")
            ),
            (
                Source::Runtime,
                Code::RuntimeError,
                Some("Button"),
                Some("/action")
            ),
        ]
    );
}

#[test]
fn entry_problems_are_reported_at_stream_end() {
    let library = library();
    let prose = parse(&library, "Sorry, I can't build that.");
    assert_eq!(prose.diagnostics()[0].code, Code::ParseFailed);
    assert!(prose.diagnostics()[0].is_fatal());
    assert!(parse(&library, "  \n").diagnostics().is_empty());

    let fallback = parse(&library, "page = Stack([])");
    assert_eq!(
        fallback.entry().map(|e| (e.statement(), e.is_fallback())),
        Some(("page", true))
    );
    assert_eq!(fallback.diagnostics()[0].severity, Severity::Warning);

    let ternary =
        parse(&library, "$on = false\nroot = $on ? Stack([]) : null");
    let evaluation = ternary.evaluate(&Inputs::new());
    assert!(evaluation.root().is_none());
    assert_eq!(evaluation.diagnostics()[0].code, Code::NoRoot);
    assert_eq!(evaluation.diagnostics()[0].source, Source::Runtime);
    let mut on = Inputs::new();
    on.set_state("$on", Value::Bool(true));
    assert!(ternary.evaluate(&on).root().is_some());
}

#[test]
fn a_broken_line_drops_only_itself() {
    let library = library();
    let texts = |document: &Document| -> Vec<String> {
        children(document, &Inputs::new())
            .iter()
            .filter_map(|c| {
                Some(c.as_element()?.prop("text")?.to_js_string())
            })
            .collect()
    };
    // `a =` with nothing after it is skipped; it does not set `a` to
    // null.
    let empty =
        parse(&library, "root = Stack([a])\na = Text(\"kept\")\na =");
    assert_eq!(texts(&empty), ["kept"]);
    let fallback =
        parse(&library, "page = Stack([Text(\"hi\")])\nroot =");
    assert_eq!(fallback.entry().map(|e| e.statement()), Some("page"));
    // A mistyped closer ends its bracket, so the next lines are
    // statements.
    let typo = parse(
        &library,
        "root = Stack([a, b)]\na = Text(\"A\")\nb = Text(\"B\")",
    );
    assert_eq!(texts(&typo), ["A", "B"]);
    assert!(typo.unresolved().is_empty());
}

#[test]
fn a_query_can_read_its_own_result() {
    let document = parse(
        &library(),
        "root = Stack([Text(\"Page \" + q.page)])\nq = \
         Query(\"list\", {cursor: q.next}, {page: 1, next: null})",
    );
    assert!(document.unresolved().is_empty());
    let mut inputs = Inputs::new();
    inputs.set_query_result(
        "q",
        Value::object([
            ("page", Value::from(2)),
            ("next", Value::from("c3")),
        ]),
    );
    let evaluation = document.evaluate(&inputs);
    assert_eq!(
        evaluation.queries()[0].args(),
        &Value::object([("cursor", Value::from("c3"))])
    );
}

#[test]
fn requests_are_whole_or_left_out() {
    let mut limits = Limits::DEFAULT;
    limits.max_steps = 60;
    let rows = vec!["1"; 30].join(", ");
    let document = parse_with_limits(
        &library(),
        &format!(
            "root = Stack(@Each(r, \"i\", Text(\"row\")))\nr = \
             [{rows}]\nq = Query(\"list\", {{who: $name, page: \
             2}})\n$name = \"Ada\"\nbig = Query(\"all\", {{rows: \
             @Each(r, \"i\", @Each(r, \"j\", 1))}})"
        ),
        limits,
    );
    let evaluation = document.evaluate(&Inputs::new());
    // Rendering ran out of steps, but that does not cut the requests
    // short.
    assert!(
        evaluation
            .diagnostics()
            .iter()
            .any(|d| d.code == Code::LimitExceeded)
    );
    let requests: Vec<_> = evaluation
        .queries()
        .iter()
        .map(|q| (q.tool(), q.args().clone()))
        .collect();
    let args = Value::object([
        ("who", Value::from("Ada")),
        ("page", Value::from(2)),
    ]);
    // `big` needs more steps than the limit allows, so it is left
    // out.
    assert_eq!(requests, [("list", args)]);
    assert_eq!(document.request("big", &Inputs::new()), None);
}
