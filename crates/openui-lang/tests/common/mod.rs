//! Shared helpers: the fixture library and the canonical result of
//! `spec/fixtures/1.0/README.md`, "How the tree is summarized".

#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use openui_lang::{
    ActionDef, ActionStep, ComponentDef, Document, Element, FunctionDef, Inputs, Library, Param,
    Type, Value,
};
use serde_json::{Map as JsonMap, Value as Json, json};

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openui-1.0")
}

/// A fixture case: its id, such as `grammar/003-fence-multiple-joined`, and its files.
pub struct Case {
    pub id: String,
    pub files: BTreeMap<String, String>,
}

impl Case {
    pub fn file(&self, name: &str) -> Option<&str> {
        self.files.get(name).map(String::as_str)
    }

    /// The program text: `input.oui`, or the joined `chunks.json`.
    pub fn text(&self) -> Option<String> {
        if let Some(chunks) = self.chunks() {
            return Some(chunks.concat());
        }
        self.file("input.oui").map(str::to_owned)
    }

    pub fn chunks(&self) -> Option<Vec<String>> {
        let chunks = self.file("chunks.json")?;
        Some(serde_json::from_str(chunks).expect("chunks.json is a list of strings"))
    }

    pub fn expected(&self) -> Option<Json> {
        let expected = self.file("expected.json")?;
        Some(serde_json::from_str(expected).expect("expected.json is JSON"))
    }
}

/// Every case folder, sorted by id.
pub fn cases() -> Vec<Case> {
    let root = fixtures_dir();
    let mut cases = Vec::new();
    for area in sorted_dirs(&root) {
        for case in sorted_dirs(&area) {
            let mut files = BTreeMap::new();
            for entry in fs::read_dir(&case).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                files.insert(name, fs::read_to_string(&path).unwrap());
            }
            let id = format!(
                "{}/{}",
                area.file_name().unwrap().to_string_lossy(),
                case.file_name().unwrap().to_string_lossy()
            );
            cases.push(Case { id, files });
        }
    }
    cases
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// `library.json` as a [`Library`], with `@Upper` implemented as the fixture README asks.
pub fn library() -> Library {
    static LIBRARY: OnceLock<Library> = OnceLock::new();
    LIBRARY
        .get_or_init(|| load_library(&fixture_spec()))
        .clone()
}

pub fn fixture_spec() -> Json {
    let text = fs::read_to_string(fixtures_dir().join("library.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn load_library(spec: &Json) -> Library {
    let schema = &spec["schema"];
    let components: HashSet<String> = object(&schema["properties"]).keys().cloned().collect();
    let mut actions: HashSet<String> = object(&schema["actions"]).keys().cloned().collect();
    actions.extend(["ActionExpression", "OpenUrl", "ToAssistant"].map(String::from));
    let reader = SchemaReader {
        components: &components,
        actions: &actions,
    };

    let mut builder = Library::builder();
    for name in object(&schema["properties"]).keys() {
        let def = &schema["$defs"][name];
        let signature = spec["components"][name]["signature"]
            .as_str()
            .unwrap_or_default();
        let mut component =
            ComponentDef::new(name, def["description"].as_str().unwrap_or_default());
        for param in reader.params(def) {
            // The schema has no binding marker; the signature prints `$binding<`.
            let bindable = signature.contains(&format!("{}: $binding<", param.name()))
                || signature.contains(&format!("{}?: $binding<", param.name()));
            component = component.param(if bindable { param.bindable() } else { param });
        }
        builder = builder.component(component);
    }
    for (name, def) in object(&schema["functions"]) {
        let mut function =
            FunctionDef::new(name, def["description"].as_str().unwrap_or_default(), upper);
        for param in reader.params(&def["params"]) {
            function = function.param(param);
        }
        if def.get("returns").is_some() {
            function = function.returns(reader.ty(&def["returns"]));
        }
        builder = builder.function(function);
    }
    for (name, def) in object(&schema["actions"]) {
        let mut action = ActionDef::new(name, def["description"].as_str().unwrap_or_default());
        for param in reader.params(&def["params"]) {
            action = action.param(param);
        }
        builder = builder.action(action);
    }
    if let Some(root) = spec["root"].as_str() {
        builder = builder.root(root);
    }
    builder.build().expect("library.json is a valid library")
}

/// `@Upper(text)`: the text in upper case.
fn upper(args: &openui_lang::Map) -> Result<Value, openui_lang::FunctionError> {
    let text = args
        .get("text")
        .map(Value::to_js_string)
        .unwrap_or_default();
    Ok(Value::from(text.to_uppercase()))
}

fn object(json: &Json) -> JsonMap<String, Json> {
    json.as_object().cloned().unwrap_or_default()
}

/// Reads JSON Schema the way prompt.md section 2.3 describes.
struct SchemaReader<'a> {
    components: &'a HashSet<String>,
    actions: &'a HashSet<String>,
}

impl SchemaReader<'_> {
    fn params(&self, object_schema: &Json) -> Vec<Param> {
        let required: Vec<&str> = object_schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        object(&object_schema["properties"])
            .iter()
            .map(|(name, schema)| {
                let ty = self.ty(schema);
                let mut param = if required.contains(&name.as_str()) {
                    Param::required(name, ty)
                } else {
                    Param::optional(name, ty)
                };
                if let Some(default) = schema.get("default") {
                    param = param.with_default(to_value(default));
                }
                param
            })
            .collect()
    }

    fn ty(&self, schema: &Json) -> Type {
        let refs: Option<Vec<String>> = match (schema.get("$ref"), schema.get("anyOf")) {
            (Some(r), _) => Some(vec![ref_name(r)]),
            (None, Some(Json::Array(options)))
                if options.iter().all(|o| o.get("$ref").is_some()) =>
            {
                Some(options.iter().map(|o| ref_name(&o["$ref"])).collect())
            }
            _ => None,
        };
        if let Some(names) = refs {
            if names.iter().all(|n| self.components.contains(n)) {
                // A slot naming every component takes any component.
                return if names.len() == self.components.len() {
                    Type::component()
                } else {
                    Type::components(names)
                };
            }
            if names.iter().all(|n| self.actions.contains(n)) {
                return if names == ["ActionExpression"] {
                    Type::action()
                } else {
                    Type::actions(names)
                };
            }
            return Type::Any;
        }
        if let Some(Json::Array(options)) = schema.get("anyOf") {
            return Type::union(options.iter().map(|o| self.ty(o)));
        }
        if let Some(Json::Array(values)) = schema.get("enum") {
            return Type::one_of(values.iter().map(to_value));
        }
        match schema["type"].as_str() {
            Some("string") => Type::String,
            Some("number" | "integer") => Type::Number,
            Some("boolean") => Type::Boolean,
            Some("array") => match schema.get("items") {
                Some(items) => Type::list(self.ty(items)),
                None => Type::list(Type::Any),
            },
            Some("object") => Type::object(self.params(schema)),
            _ => Type::Any,
        }
    }
}

fn ref_name(r: &Json) -> String {
    r.as_str()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

pub fn to_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        Json::String(s) => Value::from(s.as_str()),
        Json::Array(items) => Value::array(items.iter().map(to_value)),
        Json::Object(map) => Value::object(map.iter().map(|(k, v)| (k.as_str(), to_value(v)))),
    }
}

/// The canonical result of a document at stream end: `root`, `errors`,
/// `unresolved`, `orphans`, `incomplete`, `state`, and `errorObjects`.
pub fn canonical(document: &Document) -> Json {
    let inputs = Inputs::new();
    let evaluation = document.evaluate(&inputs);
    let diagnostics: Vec<_> = document
        .diagnostics()
        .iter()
        .chain(evaluation.diagnostics())
        .collect();
    let errors: Vec<Json> = diagnostics
        .iter()
        .map(|d| json!({ "code": d.code.as_str(), "statementId": d.statement }))
        .collect();
    let error_objects: Vec<Json> = diagnostics
        .iter()
        .map(|d| {
            json!({
                "source": d.source.as_str(),
                "code": d.code.as_str(),
                "statementId": d.statement,
                "component": d.component,
                "path": d.path,
            })
        })
        .collect();
    let state: JsonMap<String, Json> = document
        .state_defaults()
        .map(|(name, value)| (name.to_owned(), summarize(value, document, &inputs)))
        .collect();
    json!({
        "root": evaluation.root().map_or(Json::Null, |root| summarize_element(root, document, &inputs)),
        "errors": errors,
        "unresolved": document.unresolved(),
        "orphans": document.orphans(),
        "incomplete": document.is_incomplete(),
        "state": state,
        "errorObjects": error_objects,
    })
}

pub fn summarize(value: &Value, document: &Document, inputs: &Inputs) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => number(*n),
        Value::String(s) => Json::String(s.to_string()),
        Value::Array(items) => Json::Array(
            items
                .iter()
                .map(|v| summarize(v, document, inputs))
                .collect(),
        ),
        Value::Object(map) => Json::Object(
            map.iter()
                .map(|(k, v)| (k.to_owned(), summarize(v, document, inputs)))
                .collect(),
        ),
        Value::Element(element) => summarize_element(element, document, inputs),
        Value::Action(plan) => {
            let actions: Vec<Json> = plan
                .steps()
                .iter()
                .map(|step| summarize_step(step, document, inputs))
                .collect();
            json!({ "actions": actions })
        }
    }
}

fn summarize_element(element: &Element, document: &Document, inputs: &Inputs) -> Json {
    let mut props = JsonMap::new();
    for prop in element.props() {
        let summary = match prop.binding() {
            Some(binding) => json!({ "$binding": binding }),
            None => summarize(prop.value(), document, inputs),
        };
        if !summary.is_null() {
            props.insert(prop.name().to_owned(), summary);
        }
    }
    json!({ "type": element.component(), "props": props })
}

fn summarize_step(step: &ActionStep, document: &Document, inputs: &Inputs) -> Json {
    let (action, args) = match step {
        ActionStep::ToAssistant { message, context } => {
            let mut args = vec![json!(message)];
            if let Some(context) = context {
                args.push(summarize(context, document, inputs));
            }
            ("ToAssistant".to_owned(), args)
        }
        ActionStep::OpenUrl { url } => ("OpenUrl".to_owned(), vec![json!(url)]),
        ActionStep::Set { target, value } => {
            let value = document.resolve(value, inputs);
            (
                "Set".to_owned(),
                vec![json!(target), summarize(&value, document, inputs)],
            )
        }
        ActionStep::Reset { targets } => (
            "Reset".to_owned(),
            targets.iter().map(|t| json!(t)).collect(),
        ),
        ActionStep::Run { statement, .. } => ("Run".to_owned(), vec![json!(statement)]),
        ActionStep::Custom { name, params } => {
            let def = document
                .library()
                .action(name)
                .expect("custom steps name library actions");
            let args = def
                .params()
                .iter()
                .map(|p| {
                    params
                        .get(p.name())
                        .map_or(Json::Null, |v| summarize(v, document, inputs))
                })
                .collect();
            (name.clone(), args)
        }
        other => panic!("unexpected action step {other:?}"),
    };
    json!({ "action": action, "args": args })
}

/// A number as `JSON.stringify` writes it: integers without a fraction, and
/// NaN and infinities as null.
fn number(n: f64) -> Json {
    if !n.is_finite() {
        Json::Null
    } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

/// The README's comparison: `errors` as a multiset where an entry without
/// `statementId` matches any statement, `unresolved` and `orphans` as sets,
/// optional keys only when expected, everything else exactly.
pub fn compare(expected: &Json, actual: &Json) -> Vec<String> {
    let mut diffs = Vec::new();
    let mut check = |key: &str, ok: bool| {
        if !ok {
            diffs.push(format!(
                "{key}: expected {}, got {}",
                expected[key], actual[key]
            ));
        }
    };
    check("root", expected["root"] == actual["root"]);
    let (want, got) = (list(&expected["errors"]), list(&actual["errors"]));
    check("errors", want.len() == got.len() && claims(&want, &got));
    check(
        "unresolved",
        as_set(&expected["unresolved"]) == as_set(&actual["unresolved"]),
    );
    check(
        "orphans",
        as_set(&expected["orphans"]) == as_set(&actual["orphans"]),
    );
    check("incomplete", expected["incomplete"] == actual["incomplete"]);
    if expected.get("state").is_some() {
        check("state", expected["state"] == actual["state"]);
    }
    if expected.get("errorObjects").is_some() {
        let (want, got) = (
            list(&expected["errorObjects"]),
            list(&actual["errorObjects"]),
        );
        check("errorObjects", claims(&want, &got));
    }
    diffs
}

fn list(json: &Json) -> Vec<Json> {
    json.as_array().cloned().unwrap_or_default()
}

fn as_set(json: &Json) -> Vec<String> {
    let mut items: Vec<String> = list(json).iter().map(|v| v.to_string()).collect();
    items.sort();
    items.dedup();
    items
}

/// Each expected entry claims its own actual entry that agrees on the keys it
/// lists. Entries listing more keys claim first.
fn claims(expected: &[Json], actual: &[Json]) -> bool {
    let mut pool: Vec<&Json> = actual.iter().collect();
    let mut expected: Vec<&Json> = expected.iter().collect();
    expected.sort_by_key(|e| std::cmp::Reverse(e.as_object().map_or(0, |o| o.len())));
    expected.into_iter().all(|e| {
        let agrees = |a: &&Json| {
            e.as_object()
                .is_some_and(|o| o.iter().all(|(k, v)| a.get(k).unwrap_or(&Json::Null) == v))
        };
        match pool.iter().position(agrees) {
            Some(i) => {
                pool.remove(i);
                true
            }
            None => false,
        }
    })
}
