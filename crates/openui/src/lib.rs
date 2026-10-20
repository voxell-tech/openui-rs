//! A renderer-neutral runtime for [OpenUI Lang] 1.0.
//!
//! OpenUI Lang is the compact language a model writes to describe a
//! user interface. This crate parses it, in one go or as it streams,
//! checks it against a [`Library`] of components, and evaluates it
//! into a tree of [`Element`]s that any renderer can draw. It has no
//! UI, network or model dependency, and it never acts on its own:
//! actions come back as [`ActionPlan`]s and queries as
//! [`ToolRequest`]s for the host to carry out.
//!
//! ```
//! use openui::{ComponentDef, Inputs, Library, Param, Type, parse};
//!
//! let library = Library::builder()
//!     .component(
//!         ComponentDef::new("Stack", "Lays out children").param(
//!             Param::required(
//!                 "children",
//!                 Type::list(Type::component()),
//!             ),
//!         ),
//!     )
//!     .component(
//!         ComponentDef::new("Text", "A run of text")
//!             .param(Param::required("text", Type::String)),
//!     )
//!     .root("Stack")
//!     .build()?;
//!
//! let document = parse(
//!     &library,
//!     "root = Stack([greeting])\ngreeting = Text(\"Hi \" + \
//!      $name)\n$name = \"Ada\"",
//! );
//! assert!(document.diagnostics().is_empty());
//!
//! let evaluation = document.evaluate(&Inputs::new());
//! let root = evaluation.root().expect("the program renders");
//! let children =
//!     root.prop("children").and_then(|c| c.as_array()).unwrap();
//! assert_eq!(
//!     children[0]
//!         .as_element()
//!         .unwrap()
//!         .prop("text")
//!         .unwrap()
//!         .as_str(),
//!     Some("Hi Ada")
//! );
//! # Ok::<(), openui::LibraryError>(())
//! ```
//!
//! Model output is untrusted. Parsing and evaluation are bounded by
//! [`Limits`], and every recovery is reported as a [`Diagnostic`].
//!
//! [OpenUI Lang]: https://github.com/thesysdev/openui/blob/8d9c4c3b108a7b5352192a6a9d7327bdf7ea19b5/spec/language.md

mod builtins;
mod collate;
mod diagnostic;
mod document;
mod eval;
mod js;
mod lexer;
mod library;
mod limits;
mod lower;
mod source;
mod stream;
mod syntax;
mod value;

pub use diagnostic::{Code, Diagnostic, Severity, Source};
pub use document::{
    Document, Entry, Evaluation, Inputs, StatementInfo,
    StatementKind, ToolRequest,
};
pub use library::{
    ActionDef, ComponentDef, FunctionDef, FunctionError, Library,
    LibraryBuilder, LibraryError, Param, Type,
};
pub use limits::Limits;
pub use stream::{StreamParser, parse, parse_with_limits};
pub use value::{
    ActionPlan, ActionStep, Deferred, Element, Map, Prop, RunKind,
    Value,
};

/// The OpenUI Lang version this crate implements.
pub const SPEC_VERSION: &str = "1.0";
