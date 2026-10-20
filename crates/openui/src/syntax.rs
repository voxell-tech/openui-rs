//! Statements and expressions (spec sections 1.4 to 1.6).
//!
//! [`scan`] finds statement boundaries. The batch parser, the stream
//! parser and any later edit merge must agree on them byte for byte,
//! so this one function serves them all.

use std::sync::Arc;

use crate::lexer::{Tok, Token};

/// A statement's tokens, not yet parsed.
#[derive(Debug)]
pub(crate) struct RawStatement<'t, 'a> {
    pub(crate) name: &'a str,
    pub(crate) tokens: &'t [Token<'a>],
    /// The statement ended inside an unclosed bracket (or string,
    /// see [`Scan`]).
    pub(crate) unclosed: bool,
}

/// The statements found in a token list.
#[derive(Debug)]
pub(crate) struct Scan<'t, 'a> {
    /// Statements whose end is decided; later text cannot change
    /// them.
    pub(crate) completed: Vec<RawStatement<'t, 'a>>,
    /// Byte offset where the undecided rest of the text starts.
    pub(crate) resume_at: usize,
    /// Byte offset through which the text decided the completed
    /// statements, including the look-ahead token after a
    /// boundary. If this prefix of the text ever changes, every
    /// completed statement must be found again.
    pub(crate) decided_through: usize,
    /// The statement still being written, if the rest of the text
    /// starts one.
    pub(crate) pending: Option<RawStatement<'t, 'a>>,
}

/// Splits tokens into statements. A statement is `name = expression`;
/// a line without that shape is skipped. A newline ends a statement
/// only outside brackets and open ternaries, and only once the next
/// line's first token has arrived and is not `?`.
pub(crate) fn scan<'t, 'a>(tokens: &'t [Token<'a>]) -> Scan<'t, 'a> {
    let mut out = Scan {
        completed: Vec::new(),
        resume_at: 0,
        decided_through: 0,
        pending: None,
    };
    let mut pos = 0;
    loop {
        while matches!(
            tokens.get(pos),
            Some(Token {
                tok: Tok::Newline,
                ..
            })
        ) {
            pos += 1;
        }
        let Some(head) = tokens.get(pos) else {
            if let Some(last) = tokens.last() {
                out.resume_at = last.end;
                out.decided_through =
                    out.decided_through.max(last.end);
            }
            return out;
        };
        let start = head.start;
        let name = match head.tok {
            Tok::Ident(name) | Tok::Type(name) | Tok::State(name) => {
                Some(name)
            }
            _ => None,
        };
        let shaped = name.is_some()
            && matches!(tokens.get(pos + 1), Some(t) if t.tok == Tok::Assign);
        let Some(name) = name.filter(|_| shaped) else {
            // A name whose `=` has not arrived yet may still become a
            // statement.
            let undecided = name.is_some() && pos + 1 == tokens.len();
            match next_newline(tokens, pos) {
                Some(newline) if !undecided => {
                    pos = newline;
                    out.resume_at = tokens[newline].end;
                    out.decided_through =
                        out.decided_through.max(out.resume_at);
                    continue;
                }
                _ => {
                    out.resume_at = start;
                    return out;
                }
            }
        };
        let body = pos + 2;
        let (end, unclosed) = statement_end(tokens, body);
        let raw = RawStatement {
            name,
            tokens: &tokens
                [body..end.map_or(tokens.len(), |e| e.newline)],
            unclosed,
        };
        match end {
            Some(end) => {
                out.completed.push(raw);
                pos = end.newline;
                out.resume_at = tokens[end.newline].end;
                out.decided_through = tokens[end.lookahead].end;
            }
            None => {
                out.resume_at = start;
                out.pending = Some(raw);
                return out;
            }
        }
    }
}

fn next_newline(tokens: &[Token<'_>], from: usize) -> Option<usize> {
    tokens[from..]
        .iter()
        .position(|t| t.tok == Tok::Newline)
        .map(|n| from + n)
}

#[derive(Clone, Copy)]
struct End {
    newline: usize,
    lookahead: usize,
}

/// Finds the newline that ends the statement whose expression starts
/// at `from`. Also reports whether the tokens leave a bracket open.
///
/// Boundaries count bracket depth, so any closer closes the innermost
/// open bracket, as the reference implementation does: one mistyped
/// closer in `Stack([a, b)]` does not swallow the statements after
/// it. Whether a bracket is left open follows the bracket stack of
/// implicit closing (spec section 4, rule 2), where a closer that
/// does not match is skipped.
fn statement_end(
    tokens: &[Token<'_>],
    from: usize,
) -> (Option<End>, bool) {
    let mut brackets: Vec<Tok<'_>> = Vec::new();
    let mut depth = 0usize;
    // `?`s at depth zero still waiting for their `:`.
    let mut open_questions = 0usize;
    // A `:` at depth zero whose else-branch has no operand yet.
    let mut awaiting_else = false;
    let mut i = from;
    while i < tokens.len() {
        let tok = &tokens[i].tok;
        let top_level = depth == 0;
        match tok {
            Tok::Newline
                if top_level
                    && open_questions == 0
                    && !awaiting_else =>
            {
                let next = (i + 1..tokens.len())
                    .find(|&k| tokens[k].tok != Tok::Newline);
                match next {
                    // The next line's first token decides; it has not
                    // arrived yet.
                    None => return (None, false),
                    // A `?` continues the statement; only newlines
                    // lie between.
                    Some(k) if tokens[k].tok == Tok::Question => {
                        i = k;
                        continue;
                    }
                    Some(k) => {
                        return (
                            Some(End {
                                newline: i,
                                lookahead: k,
                            }),
                            false,
                        );
                    }
                }
            }
            Tok::LParen | Tok::LBracket | Tok::LBrace => {
                if top_level {
                    awaiting_else = false;
                }
                depth += 1;
                brackets.push(tok.clone());
            }
            Tok::RParen | Tok::RBracket | Tok::RBrace => {
                depth = depth.saturating_sub(1);
                // A closer that does not match is skipped and leaves
                // the stack alone.
                let matches = matches!(
                    (brackets.last(), tok),
                    (Some(Tok::LParen), Tok::RParen)
                        | (Some(Tok::LBracket), Tok::RBracket)
                        | (Some(Tok::LBrace), Tok::RBrace)
                );
                if matches {
                    brackets.pop();
                }
            }
            Tok::Question if top_level => open_questions += 1,
            Tok::Colon if top_level && open_questions > 0 => {
                open_questions -= 1;
                awaiting_else = true;
            }
            Tok::Str(_)
            | Tok::Num(_)
            | Tok::True
            | Tok::False
            | Tok::Null
            | Tok::Ident(_)
            | Tok::Type(_)
            | Tok::State(_)
            | Tok::At(_)
                if top_level =>
            {
                awaiting_else = false;
            }
            _ => {}
        }
        i += 1;
    }
    (None, !brackets.is_empty())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// An expression as written.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Str(Arc<str>),
    Num(f64),
    Bool(bool),
    Null,
    Array(Vec<Expr>),
    Object(Vec<(Arc<str>, Expr)>),
    /// A bare name: a statement, or a loop variable inside `@Each`.
    Ref(Arc<str>),
    /// `$name`.
    State(Arc<str>),
    /// `Name(...)` (`at` false) or `@Name(...)` (`at` true).
    Call {
        name: Arc<str>,
        at: bool,
        args: Vec<Expr>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Member(Box<Expr>, Arc<str>),
    Index(Box<Expr>, Box<Expr>),
    /// `$name = expression` inside an expression: a binding
    /// assignment.
    Assign(Arc<str>, Box<Expr>),
}

impl Expr {
    /// Calls every `$name` read or written in the expression.
    pub(crate) fn visit_state(&self, f: &mut impl FnMut(&Arc<str>)) {
        match self {
            Expr::State(name) => f(name),
            Expr::Assign(name, value) => {
                f(name);
                value.visit_state(f);
            }
            Expr::Array(items) | Expr::Call { args: items, .. } => {
                items.iter().for_each(|e| e.visit_state(f));
            }
            Expr::Object(entries) => {
                entries.iter().for_each(|(_, e)| e.visit_state(f))
            }
            Expr::Unary(_, e) | Expr::Member(e, _) => {
                e.visit_state(f)
            }
            Expr::Binary(_, a, b) | Expr::Index(a, b) => {
                a.visit_state(f);
                b.visit_state(f);
            }
            Expr::Ternary(a, b, c) => {
                a.visit_state(f);
                b.visit_state(f);
                c.visit_state(f);
            }
            Expr::Str(_)
            | Expr::Num(_)
            | Expr::Bool(_)
            | Expr::Null
            | Expr::Ref(_) => {}
        }
    }
}

/// Names that need `@` to be called. Written as `Name(...)` they are
/// a reference and the arguments are lost; only `Action` works either
/// way.
pub(crate) fn is_builtin_call(name: &str) -> bool {
    matches!(
        name,
        "Count"
            | "First"
            | "Last"
            | "Sum"
            | "Avg"
            | "Min"
            | "Max"
            | "Filter"
            | "Sort"
            | "Round"
            | "Abs"
            | "Floor"
            | "Ceil"
            | "Each"
            | "Set"
            | "Reset"
            | "Run"
            | "ToAssistant"
            | "OpenUrl"
            | "Action"
    )
}

/// The expression nested deeper than the limit allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TooDeep;

/// Parses a statement's expression tokens. Never fails on malformed
/// input: unexpected tokens read as `null` and missing closers are
/// implied.
pub(crate) fn parse_expression(
    tokens: &[Token<'_>],
    max_depth: usize,
) -> Result<Expr, TooDeep> {
    let tokens: Vec<&Tok<'_>> = tokens
        .iter()
        .map(|t| &t.tok)
        .filter(|t| **t != Tok::Newline)
        .collect();
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
        max_depth,
    };
    parser.expression(0).map(|(expr, _)| expr)
}

const PREC_TERNARY: u8 = 1;
const PREC_OR: u8 = 2;
const PREC_AND: u8 = 3;
const PREC_EQUALITY: u8 = 4;
const PREC_COMPARISON: u8 = 5;
const PREC_ADDITIVE: u8 = 6;
const PREC_MULTIPLICATIVE: u8 = 7;
const PREC_UNARY: u8 = 8;
const PREC_POSTFIX: u8 = 9;

/// An expression and its height, so chains built by loops are bounded
/// too.
type Parsed = (Expr, usize);

struct Parser<'t, 'a> {
    tokens: Vec<&'t Tok<'a>>,
    pos: usize,
    depth: usize,
    max_depth: usize,
}

impl<'t, 'a> Parser<'t, 'a> {
    fn peek(&self) -> Option<&'t Tok<'a>> {
        self.tokens.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<&'t Tok<'a>> {
        let tok = self.tokens.get(self.pos).copied();
        self.pos += 1;
        tok
    }

    fn eat(&mut self, tok: &Tok<'_>) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn node(
        &self,
        expr: Expr,
        child_height: usize,
    ) -> Result<Parsed, TooDeep> {
        let height = child_height + 1;
        if height > self.max_depth {
            Err(TooDeep)
        } else {
            Ok((expr, height))
        }
    }

    fn expression(
        &mut self,
        min_prec: u8,
    ) -> Result<Parsed, TooDeep> {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(TooDeep);
        }
        let mut left = self.prefix()?;
        while let Some(prec) = self.peek().and_then(infix_precedence)
        {
            if prec <= min_prec {
                break;
            }
            left = self.infix(left)?;
        }
        self.depth -= 1;
        Ok(left)
    }

    fn prefix(&mut self) -> Result<Parsed, TooDeep> {
        let Some(tok) = self.bump() else {
            return self.node(Expr::Null, 0);
        };
        match tok {
            Tok::Str(s) => self.node(Expr::Str(Arc::from(&**s)), 0),
            Tok::Num(n) => self.node(Expr::Num(*n), 0),
            Tok::True => self.node(Expr::Bool(true), 0),
            Tok::False => self.node(Expr::Bool(false), 0),
            Tok::Null => self.node(Expr::Null, 0),
            Tok::LBracket => {
                let (items, height) = self.list(&Tok::RBracket)?;
                self.node(Expr::Array(items), height)
            }
            Tok::LBrace => self.object(),
            Tok::State(name) => {
                let name: Arc<str> = Arc::from(*name);
                if self.eat(&Tok::Assign) {
                    let (value, height) = self.expression(0)?;
                    self.node(
                        Expr::Assign(name, Box::new(value)),
                        height,
                    )
                } else {
                    self.node(Expr::State(name), 0)
                }
            }
            Tok::Type(name) => {
                let name = *name;
                let callable =
                    !is_builtin_call(name) || name == "Action";
                if callable && self.peek() == Some(&Tok::LParen) {
                    self.call(name, false)
                } else {
                    self.node(Expr::Ref(Arc::from(name)), 0)
                }
            }
            Tok::At(name) => {
                let name = *name;
                if self.peek() == Some(&Tok::LParen) {
                    self.call(name, true)
                } else {
                    self.node(Expr::Ref(Arc::from(name)), 0)
                }
            }
            Tok::Ident(name) => {
                self.node(Expr::Ref(Arc::from(*name)), 0)
            }
            Tok::Not | Tok::Minus => {
                let op = if *tok == Tok::Not {
                    UnaryOp::Not
                } else {
                    UnaryOp::Neg
                };
                let (operand, height) =
                    self.expression(PREC_UNARY)?;
                self.node(Expr::Unary(op, Box::new(operand)), height)
            }
            Tok::LParen => {
                let inner = self.expression(0)?;
                self.eat(&Tok::RParen);
                Ok(inner)
            }
            // Anything else is skipped and reads as null.
            _ => self.node(Expr::Null, 0),
        }
    }

    fn infix(
        &mut self,
        (left, left_height): Parsed,
    ) -> Result<Parsed, TooDeep> {
        let tok = self
            .bump()
            .cloned()
            .expect("infix is only called on a token");
        let binary = |op| -> Option<(BinaryOp, u8)> {
            Some((op, infix_precedence(&tok)?))
        };
        let op = match tok {
            Tok::Or => binary(BinaryOp::Or),
            Tok::And => binary(BinaryOp::And),
            Tok::EqEq => binary(BinaryOp::Eq),
            Tok::NotEq => binary(BinaryOp::Ne),
            Tok::Gt => binary(BinaryOp::Gt),
            Tok::Lt => binary(BinaryOp::Lt),
            Tok::Ge => binary(BinaryOp::Ge),
            Tok::Le => binary(BinaryOp::Le),
            Tok::Plus => binary(BinaryOp::Add),
            Tok::Minus => binary(BinaryOp::Sub),
            Tok::Star => binary(BinaryOp::Mul),
            Tok::Slash => binary(BinaryOp::Div),
            Tok::Percent => binary(BinaryOp::Rem),
            _ => None,
        };
        if let Some((op, prec)) = op {
            let (right, right_height) = self.expression(prec)?;
            return self.node(
                Expr::Binary(op, Box::new(left), Box::new(right)),
                left_height.max(right_height),
            );
        }
        match tok {
            Tok::Question => {
                let (then, then_height) = self.expression(0)?;
                self.eat(&Tok::Colon);
                let (otherwise, else_height) = self.expression(0)?;
                self.node(
                    Expr::Ternary(
                        Box::new(left),
                        Box::new(then),
                        Box::new(otherwise),
                    ),
                    left_height.max(then_height).max(else_height),
                )
            }
            Tok::Dot => {
                let field = self
                    .bump()
                    .map_or_else(|| Arc::from("?"), key_text);
                self.node(
                    Expr::Member(Box::new(left), field),
                    left_height,
                )
            }
            Tok::LBracket => {
                let (index, index_height) = self.expression(0)?;
                self.eat(&Tok::RBracket);
                self.node(
                    Expr::Index(Box::new(left), Box::new(index)),
                    left_height.max(index_height),
                )
            }
            _ => unreachable!(
                "infix_precedence only admits infix tokens"
            ),
        }
    }

    /// `name(args)`; the cursor is on the `(`.
    fn call(
        &mut self,
        name: &str,
        at: bool,
    ) -> Result<Parsed, TooDeep> {
        self.bump();
        let (args, height) = self.list(&Tok::RParen)?;
        self.node(
            Expr::Call {
                name: Arc::from(name),
                at,
                args,
            },
            height,
        )
    }

    /// Expressions up to `close` or the end. Commas are optional.
    fn list(
        &mut self,
        close: &Tok<'_>,
    ) -> Result<(Vec<Expr>, usize), TooDeep> {
        let mut items = Vec::new();
        let mut height = 0;
        while self.peek().is_some_and(|t| t != close) {
            let (item, item_height) = self.expression(0)?;
            items.push(item);
            height = height.max(item_height);
            self.eat(&Tok::Comma);
        }
        self.eat(close);
        Ok((items, height))
    }

    /// `{ key: value ... }`; the cursor is past the `{`.
    fn object(&mut self) -> Result<Parsed, TooDeep> {
        let mut entries = Vec::new();
        let mut height = 0;
        while self.peek().is_some_and(|t| *t != Tok::RBrace) {
            let key =
                self.bump().map_or_else(|| Arc::from("?"), key_text);
            self.eat(&Tok::Colon);
            let (value, value_height) = self.expression(0)?;
            entries.push((key, value));
            height = height.max(value_height);
            self.eat(&Tok::Comma);
        }
        self.eat(&Tok::RBrace);
        self.node(Expr::Object(entries), height)
    }
}

/// An object key or member field. Names, strings, numbers, component
/// names and `$` names (without the `$`) count; any other token reads
/// as `?`.
fn key_text(tok: &Tok<'_>) -> Arc<str> {
    match tok {
        Tok::Ident(s) | Tok::Type(s) => Arc::from(*s),
        Tok::Str(s) => Arc::from(&**s),
        Tok::Num(n) => Arc::from(crate::js::number_to_string(*n)),
        Tok::State(s) => Arc::from(&s[1..]),
        _ => Arc::from("?"),
    }
}

fn infix_precedence(tok: &Tok<'_>) -> Option<u8> {
    Some(match tok {
        Tok::Question => PREC_TERNARY,
        Tok::Or => PREC_OR,
        Tok::And => PREC_AND,
        Tok::EqEq | Tok::NotEq => PREC_EQUALITY,
        Tok::Gt | Tok::Lt | Tok::Ge | Tok::Le => PREC_COMPARISON,
        Tok::Plus | Tok::Minus => PREC_ADDITIVE,
        Tok::Star | Tok::Slash | Tok::Percent => PREC_MULTIPLICATIVE,
        Tok::Dot | Tok::LBracket => PREC_POSTFIX,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn statements(src: &str) -> Vec<(String, usize)> {
        let lexed = lex(src);
        let scan = scan(&lexed.tokens);
        let mut out: Vec<(String, usize)> = scan
            .completed
            .iter()
            .map(|s| (s.name.to_owned(), s.tokens.len()))
            .collect();
        out.extend(
            scan.pending
                .map(|s| (format!("{}?", s.name), s.tokens.len())),
        );
        out
    }

    fn parse(src: &str) -> Expr {
        parse_expression(&lex(src).tokens, 64).unwrap()
    }

    #[test]
    fn a_multiline_ternary_stays_one_statement() {
        let src =
            "s = $n > 0\n  ? \"some\"\n  :\n \"none\"\nt = 1\nu = 2";
        let names: Vec<String> =
            statements(src).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["s", "t", "u?"]);
    }

    #[test]
    fn a_boundary_waits_for_the_next_line() {
        assert_eq!(statements("a = 1\n"), [("a?".to_owned(), 2)]);
        assert_eq!(statements("a = 1\nb"), [("a".to_owned(), 1)]);
    }

    #[test]
    fn junk_lines_are_skipped_up_to_the_newline() {
        let names: Vec<String> =
            statements("Here is (my UI\nroot = Stack([]))\nx = 1")
                .into_iter()
                .map(|(n, _)| n)
                .collect();
        assert_eq!(names, ["root", "x?"]);
    }

    #[test]
    fn a_mistyped_closer_still_ends_its_bracket_for_boundaries() {
        let lexed = lex("a = (]\nb = [1,\n2]\nc = Stack([x)]");
        let scan = scan(&lexed.tokens);
        let names: Vec<&str> =
            scan.completed.iter().map(|s| s.name).collect();
        assert_eq!(names, ["a", "b"]);
        // Implicit closing still sees the `(` that the mistyped `)`
        // left open.
        let pending = scan.pending.unwrap();
        assert_eq!(pending.name, "c");
        assert!(pending.unclosed);
    }

    #[test]
    fn precedence_and_associativity() {
        use BinaryOp::{Add, Mul, Sub};
        let n = |v| Box::new(Expr::Num(v));
        assert_eq!(
            parse("1 + 2 * 3 - 4"),
            Expr::Binary(
                Sub,
                Box::new(Expr::Binary(
                    Add,
                    n(1.0),
                    Box::new(Expr::Binary(Mul, n(2.0), n(3.0)))
                )),
                n(4.0)
            )
        );
        assert!(
            matches!(parse("a ? b : c ? d : e"), Expr::Ternary(_, _, e) if matches!(*e, Expr::Ternary(..)))
        );
    }

    #[test]
    fn builtins_without_at_are_references() {
        assert_eq!(parse("Count(x)"), Expr::Ref("Count".into()));
        assert!(matches!(
            parse("Action([])"),
            Expr::Call { at: false, .. }
        ));
        assert!(matches!(
            parse("@count(x)"),
            Expr::Call { at: true, .. }
        ));
    }

    #[test]
    fn member_fields_accept_key_tokens() {
        assert_eq!(
            parse("m.0.1"),
            Expr::Member(
                Box::new(Expr::Ref("m".into())),
                "0.1".into()
            )
        );
        assert_eq!(
            parse("o.$s"),
            Expr::Member(Box::new(Expr::Ref("o".into())), "s".into())
        );
    }

    #[test]
    fn deep_nesting_is_rejected_not_overflowed() {
        let deep =
            format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000));
        assert_eq!(
            parse_expression(&lex(&deep).tokens, 64),
            Err(TooDeep)
        );
        let chain = format!("a{}", ".b".repeat(10_000));
        assert_eq!(
            parse_expression(&lex(&chain).tokens, 64),
            Err(TooDeep)
        );
    }
}
