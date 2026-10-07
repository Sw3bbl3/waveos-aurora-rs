//! A recursive-descent parser for ECMAScript 2023 scripts.

use crate::ast::*;
use crate::lexer::{tokenize, Name, SyntaxError, Tok, Token};
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

type R<T> = Result<T, SyntaxError>;

const RESERVED: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
];
const STRICT_RESERVED: &[&str] =
    &["implements", "interface", "let", "package", "private", "protected", "public", "static", "yield"];

const MAX_DEPTH: u32 = 300;

#[derive(Default)]
struct FnCtx {
    refs: BTreeSet<Name>,
    inner: BTreeSet<Name>,
    is_arrow: bool,
    uses_arguments: bool,
    has_eval: bool,
}

pub struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
    strict: bool,
    in_function: bool,
    in_generator: bool,
    in_async: bool,
    /// Inside a class body or method: `super.x` is allowed.
    super_prop: bool,
    /// Inside a derived constructor: `super()` is allowed.
    super_call: bool,
    in_class_field: bool,
    fns: Vec<FnCtx>,
    next_site: u32,
    depth: u32,
}

pub fn parse_script(src: &str) -> R<Script> {
    let toks = tokenize(src)?;
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        strict: false,
        in_function: false,
        in_generator: false,
        in_async: false,
        super_prop: false,
        super_call: false,
        in_class_field: false,
        fns: vec![FnCtx::default()],
        next_site: 0,
        depth: 0,
    };
    let body = p.body_with_directives(true)?;
    if !p.at_eof() {
        return Err(p.unexpected());
    }
    let ctx = p.fns.pop().unwrap();
    Ok(Script { body, strict: p.strict, inner_refs: ctx.inner, has_eval: ctx.has_eval })
}

/// Parses the parameters and body given to `new Function(...)`.
pub fn parse_function_parts(params: &str, body: &str, is_async: bool, is_generator: bool) -> R<Script> {
    let kw = match (is_async, is_generator) {
        (false, false) => "function",
        (false, true) => "function*",
        (true, false) => "async function",
        (true, true) => "async function*",
    };
    let src = format!("({kw} anonymous({params}\n) {{\n{body}\n}})");
    parse_script(&src)
}

impl<'a> Parser<'a> {
    // ------------------------------------------------------------ tokens

    fn tok(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn token(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn peek_tok(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }

    fn at_eof(&self) -> bool {
        matches!(self.tok(), Tok::Eof)
    }

    fn advance(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.tok(), Tok::Punct(q) if *q == p)
    }

    fn is_at(&self, n: usize, p: &str) -> bool {
        matches!(self.peek_tok(n), Tok::Punct(q) if *q == p)
    }

    fn eat(&mut self, p: &str) -> bool {
        if self.is(p) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(self.err(&format!("expected '{p}' but found {}", self.describe())))
        }
    }

    /// A keyword (unescaped identifier) check.
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.tok(), Tok::Ident(n, false) if &**n == kw)
    }

    fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.peek_tok(n), Tok::Ident(x, false) if &**x == kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &str) -> R<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.err(&format!("expected '{kw}' but found {}", self.describe())))
        }
    }

    fn describe(&self) -> String {
        match self.tok() {
            Tok::Eof => String::from("end of input"),
            Tok::Punct(p) => format!("'{p}'"),
            Tok::Ident(n, _) => format!("'{n}'"),
            Tok::Num(_) => String::from("a number"),
            Tok::Str(_) => String::from("a string"),
            Tok::Template { .. } => String::from("a template"),
            Tok::Regex { .. } => String::from("a regular expression"),
            Tok::Private(n) => format!("'#{n}'"),
        }
    }

    fn err(&self, msg: &str) -> SyntaxError {
        let t = self.token();
        SyntaxError { message: String::from(msg), line: t.line, col: t.col }
    }

    fn unexpected(&self) -> SyntaxError {
        self.err(&format!("unexpected {}", self.describe()))
    }

    fn semicolon(&mut self) -> R<()> {
        if self.eat(";") {
            return Ok(());
        }
        if self.is("}") || self.at_eof() || self.token().nl_before {
            return Ok(());
        }
        Err(self.err(&format!("expected ';' but found {}", self.describe())))
    }

    fn line(&self) -> u32 {
        self.token().line
    }

    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err("too much nesting"));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // ------------------------------------------------------------ names

    fn reference(&mut self, name: &Name) {
        let ctx = self.fns.last_mut().unwrap();
        ctx.refs.insert(name.clone());
        if &**name == "arguments" {
            for f in self.fns.iter_mut().rev() {
                f.uses_arguments = true;
                if !f.is_arrow {
                    break;
                }
            }
        }
    }

    fn check_binding_name(&self, name: &str) -> R<()> {
        if RESERVED.contains(&name) {
            return Err(self.err(&format!("'{name}' is a reserved word")));
        }
        if self.strict && (STRICT_RESERVED.contains(&name) || name == "eval" || name == "arguments") {
            return Err(self.err(&format!("'{name}' can't be used as a name in strict mode")));
        }
        if (self.in_generator && name == "yield") || (self.in_async && name == "await") {
            return Err(self.err(&format!("'{name}' can't be used as a name here")));
        }
        Ok(())
    }

    /// An identifier usable as a reference or binding.
    fn identifier(&mut self) -> R<Name> {
        match self.tok().clone() {
            Tok::Ident(n, _) => {
                if RESERVED.contains(&&*n) {
                    return Err(self.unexpected());
                }
                if self.strict && STRICT_RESERVED.contains(&&*n) {
                    return Err(self.err(&format!("'{n}' is reserved in strict mode")));
                }
                if (self.in_generator && &*n == "yield") || (self.in_async && &*n == "await") {
                    return Err(self.unexpected());
                }
                self.advance();
                self.reference(&n);
                Ok(n)
            }
            _ => Err(self.err(&format!("expected an identifier but found {}", self.describe()))),
        }
    }

    fn binding_identifier(&mut self) -> R<Name> {
        let n = self.identifier()?;
        self.check_binding_name(&n)?;
        Ok(n)
    }

    /// Any IdentifierName (after `.`, in property keys).
    fn identifier_name(&mut self) -> R<Name> {
        match self.tok().clone() {
            Tok::Ident(n, _) => {
                self.advance();
                Ok(n)
            }
            _ => Err(self.err(&format!("expected a property name but found {}", self.describe()))),
        }
    }

    fn is_identifier(&self) -> bool {
        match self.tok() {
            Tok::Ident(n, _) => {
                !RESERVED.contains(&&**n)
                    && !(self.in_generator && &**n == "yield")
                    && !(self.in_async && &**n == "await")
                    && !(self.strict && STRICT_RESERVED.contains(&&**n))
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------ statements

    fn directive_is_use_strict(&self, t: &Token) -> bool {
        let raw = &self.src[t.start as usize..t.end as usize];
        raw == "'use strict'" || raw == "\"use strict\""
    }

    /// A statement list with an optional directive prologue.
    fn body_with_directives(&mut self, top: bool) -> R<Vec<Stmt>> {
        let mut body = Vec::new();
        let mut prologue = true;
        while !self.at_eof() && (top || !self.is("}")) {
            if prologue {
                if let Tok::Str(_) = self.tok() {
                    let t = self.token().clone();
                    let next = self.peek_tok(1);
                    let ends = matches!(next, Tok::Punct(";" | "}") | Tok::Eof) || self.toks[self.pos + 1].nl_before;
                    if ends {
                        if self.directive_is_use_strict(&t) {
                            self.strict = true;
                        } else if t.legacy_octal && self.strict {
                            return Err(self.err("octal escapes are not allowed in strict mode"));
                        }
                    } else {
                        prologue = false;
                    }
                } else {
                    prologue = false;
                }
            }
            body.push(self.statement_list_item()?);
        }
        Ok(body)
    }

    fn statement_list_item(&mut self) -> R<Stmt> {
        if self.is_kw("function") {
            return Ok(Stmt::Function(self.function_declaration(false)?));
        }
        if self.is_kw("async") && self.is_kw_at(1, "function") && !self.toks[self.pos + 1].nl_before {
            self.advance();
            return Ok(Stmt::Function(self.function_declaration(true)?));
        }
        if self.is_kw("class") {
            return Ok(Stmt::Class(self.class(true)?));
        }
        if self.is_kw("const") || self.is_let_declaration() {
            let kind = if self.is_kw("const") { VarKind::Const } else { VarKind::Let };
            self.advance();
            let decls = self.var_declarations(kind, false)?;
            self.semicolon()?;
            return Ok(Stmt::Var(kind, decls));
        }
        self.statement()
    }

    fn is_let_declaration(&self) -> bool {
        if !self.is_kw("let") {
            return false;
        }
        match self.peek_tok(1) {
            Tok::Punct("[" | "{") => true,
            Tok::Ident(n, _) => !matches!(&**n, "in" | "instanceof"),
            _ => false,
        }
    }

    fn statement(&mut self) -> R<Stmt> {
        self.enter()?;
        let s = self.statement_inner();
        self.leave();
        s
    }

    fn statement_inner(&mut self) -> R<Stmt> {
        match self.tok().clone() {
            Tok::Punct("{") => Ok(Stmt::Block(self.block()?)),
            Tok::Punct(";") => {
                self.advance();
                Ok(Stmt::Empty)
            }
            Tok::Ident(kw, false) => match &*kw {
                "var" => {
                    self.advance();
                    let decls = self.var_declarations(VarKind::Var, false)?;
                    self.semicolon()?;
                    Ok(Stmt::Var(VarKind::Var, decls))
                }
                "if" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression(false)?;
                    self.expect(")")?;
                    let cons = self.if_body()?;
                    let alt = if self.eat_kw("else") { Some(Box::new(self.if_body()?)) } else { None };
                    Ok(Stmt::If(test, Box::new(cons), alt))
                }
                "for" => self.for_statement(),
                "while" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression(false)?;
                    self.expect(")")?;
                    Ok(Stmt::While(test, Box::new(self.substatement()?)))
                }
                "do" => {
                    self.advance();
                    let body = self.substatement()?;
                    self.expect_kw("while")?;
                    self.expect("(")?;
                    let test = self.expression(false)?;
                    self.expect(")")?;
                    self.eat(";");
                    Ok(Stmt::DoWhile(Box::new(body), test))
                }
                "continue" | "break" => {
                    self.advance();
                    let label =
                        if !self.token().nl_before && matches!(self.tok(), Tok::Ident(..)) && !self.is_kw("case") {
                            Some(self.identifier_name()?)
                        } else {
                            None
                        };
                    self.semicolon()?;
                    Ok(if &*kw == "break" { Stmt::Break(label) } else { Stmt::Continue(label) })
                }
                "return" => {
                    if !self.in_function {
                        return Err(self.err("'return' outside of a function"));
                    }
                    self.advance();
                    let arg = if self.is(";") || self.is("}") || self.at_eof() || self.token().nl_before {
                        None
                    } else {
                        Some(self.expression(false)?)
                    };
                    self.semicolon()?;
                    Ok(Stmt::Return(arg))
                }
                "throw" => {
                    self.advance();
                    if self.token().nl_before {
                        return Err(self.err("no line break is allowed after 'throw'"));
                    }
                    let e = self.expression(false)?;
                    self.semicolon()?;
                    Ok(Stmt::Throw(e))
                }
                "try" => self.try_statement(),
                "switch" => self.switch_statement(),
                "debugger" => {
                    self.advance();
                    self.semicolon()?;
                    Ok(Stmt::Debugger)
                }
                "with" => {
                    if self.strict {
                        return Err(self.err("Strict mode code may not include a with statement"));
                    }
                    self.advance();
                    self.expect("(")?;
                    let obj = self.expression(false)?;
                    self.expect(")")?;
                    // Names inside may refer to the object's properties, so
                    // (as with eval) every binding stays reachable at run time.
                    for f in self.fns.iter_mut() {
                        f.has_eval = true;
                    }
                    let body = self.substatement()?;
                    Ok(Stmt::With(obj, Box::new(body)))
                }
                "function" => {
                    // Annex B: a function declaration as an if body, etc.
                    if self.strict {
                        return Err(self.err("functions can only be declared at the top level or inside a block"));
                    }
                    Ok(Stmt::Function(self.function_declaration(false)?))
                }
                "class" | "const" => Err(self.err(&format!("a '{kw}' declaration is not allowed here"))),
                "import" | "export" => Err(self.err("modules are not supported")),
                _ => {
                    if self.is_identifier() && self.is_at(1, ":") {
                        let label = self.identifier_name()?;
                        self.advance();
                        if self.is_kw("function") {
                            if self.strict {
                                return Err(self.err("labelled functions are not allowed in strict mode"));
                            }
                            return Ok(Stmt::Labeled(
                                label,
                                Box::new(Stmt::Function(self.function_declaration(false)?)),
                            ));
                        }
                        return Ok(Stmt::Labeled(label, Box::new(self.statement()?)));
                    }
                    self.expression_statement()
                }
            },
            _ => self.expression_statement(),
        }
    }

    /// The body of if/while/for: a statement, but not a declaration.
    /// An `if` branch: Annex B also allows a plain function declaration.
    fn if_body(&mut self) -> R<Stmt> {
        if self.is_kw("function") {
            return self.statement();
        }
        self.substatement()
    }

    /// The body of a loop or `with`, where no declaration may appear
    /// (labelled functions included).
    fn substatement(&mut self) -> R<Stmt> {
        let mut i = 0;
        while matches!(self.peek_tok(i), Tok::Ident(..)) && self.is_at(i + 1, ":") {
            i += 2;
        }
        let async_fn = self.is_kw_at(i, "async")
            && self.is_kw_at(i + 1, "function")
            && self.toks.get(self.pos + i + 1).is_some_and(|t| !t.nl_before);
        if self.is_kw_at(i, "function") || async_fn {
            return Err(self.err("a function declaration is not allowed here"));
        }
        if self.is_let_declaration() && self.is_at(1, "[") {
            return Err(self.err("a lexical declaration is not allowed here"));
        }
        self.statement()
    }

    fn expression_statement(&mut self) -> R<Stmt> {
        let e = self.expression(false)?;
        self.semicolon()?;
        Ok(Stmt::Expr(e))
    }

    fn block(&mut self) -> R<Vec<Stmt>> {
        self.expect("{")?;
        let mut body = Vec::new();
        while !self.is("}") {
            if self.at_eof() {
                return Err(self.err("expected '}'"));
            }
            body.push(self.statement_list_item()?);
        }
        self.advance();
        Ok(body)
    }

    fn var_declarations(&mut self, kind: VarKind, no_in: bool) -> R<Vec<VarDecl>> {
        let mut decls = Vec::new();
        loop {
            let pat = self.binding_target()?;
            if kind != VarKind::Var {
                if let Pat::Ident(n) = &pat {
                    if &**n == "let" {
                        return Err(self.err("'let' can't be a lexically bound name"));
                    }
                }
            }
            let init = if self.eat("=") { Some(self.assignment(no_in)?) } else { None };
            decls.push(VarDecl { pat, init });
            if !self.eat(",") {
                break;
            }
        }
        Ok(decls)
    }

    /// A binding identifier or destructuring pattern.
    fn binding_target(&mut self) -> R<Pat> {
        if self.is("[") {
            return self.array_binding();
        }
        if self.is("{") {
            return self.object_binding();
        }
        Ok(Pat::Ident(self.binding_identifier()?))
    }

    fn binding_element(&mut self) -> R<(Pat, Option<Expr>)> {
        let pat = self.binding_target()?;
        let init = if self.eat("=") { Some(self.assignment(false)?) } else { None };
        Ok((pat, init))
    }

    fn array_binding(&mut self) -> R<Pat> {
        self.expect("[")?;
        let mut elems = Vec::new();
        let mut rest = None;
        loop {
            if self.eat("]") {
                break;
            }
            if self.eat(",") {
                elems.push(None);
                continue;
            }
            if self.eat("...") {
                rest = Some(Box::new(self.binding_target()?));
                self.expect("]")?;
                break;
            }
            let (pat, init) = self.binding_element()?;
            elems.push(Some(PatElem { pat, init }));
            if !self.is("]") {
                self.expect(",")?;
            }
        }
        Ok(Pat::Array { elems, rest })
    }

    fn object_binding(&mut self) -> R<Pat> {
        self.expect("{")?;
        let mut props = Vec::new();
        let mut rest = None;
        loop {
            if self.eat("}") {
                break;
            }
            if self.eat("...") {
                rest = Some(Box::new(Pat::Ident(self.binding_identifier()?)));
                self.expect("}")?;
                break;
            }
            let shorthand = matches!(self.tok(), Tok::Ident(..)) && !self.is_at(1, ":") && !self.is_at(1, "(");
            if shorthand {
                let name = self.binding_identifier()?;
                let init = if self.eat("=") { Some(self.assignment(false)?) } else { None };
                props.push(PatProp { key: PropName::Ident(name.clone()), value: Pat::Ident(name), init });
            } else {
                let key = self.property_name()?;
                self.expect(":")?;
                let (value, init) = self.binding_element()?;
                props.push(PatProp { key, value, init });
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        Ok(Pat::Object { props, rest })
    }

    fn for_statement(&mut self) -> R<Stmt> {
        self.advance();
        let is_await = self.eat_kw("await");
        if is_await && !self.in_async {
            return Err(self.err("'for await' is only valid in async functions"));
        }
        self.expect("(")?;
        let mut init = None;
        if self.is(";") {
            // no init
        } else {
            let decl_kind = if self.is_kw("var") {
                Some(VarKind::Var)
            } else if self.is_kw("const") {
                Some(VarKind::Const)
            } else if self.is_let_declaration() {
                Some(VarKind::Let)
            } else {
                None
            };
            if let Some(kind) = decl_kind {
                self.advance();
                let decls = self.var_declarations(kind, true)?;
                if decls.len() == 1 && (self.is_kw("of") || self.is_kw("in")) {
                    let d = decls.into_iter().next().unwrap();
                    let of = self.is_kw("of");
                    if d.init.is_some()
                        && (of || self.strict || kind != VarKind::Var || !matches!(d.pat, Pat::Ident(_)))
                    {
                        return Err(self.err("for-in/of declarations can't have initialisers"));
                    }
                    return self.for_in_of_rest(ForHead::Var(kind, d.pat), of, is_await);
                }
                for d in &decls {
                    if d.init.is_none() && (kind == VarKind::Const || !matches!(d.pat, Pat::Ident(_))) {
                        return Err(self.err("missing initialiser in declaration"));
                    }
                }
                init = Some(ForInit::Var(kind, decls));
            } else {
                let start_is_let = self.is_kw("let");
                let start_is_async = self.is_kw("async") && !self.is_at(1, "=>");
                let e = self.expression(true)?;
                if self.is_kw("of") || self.is_kw("in") {
                    let of = self.is_kw("of");
                    if of && (start_is_let || (start_is_async && matches!(e, Expr::Ident(_)) && !is_await)) {
                        return Err(self.err("invalid left-hand side in for-of"));
                    }
                    let pat = self.to_pattern(e)?;
                    return self.for_in_of_rest(ForHead::Pat(pat), of, is_await);
                }
                init = Some(ForInit::Expr(e));
            }
        }
        if is_await {
            return Err(self.err("'for await' requires 'of'"));
        }
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expression(false)?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expression(false)?) };
        self.expect(")")?;
        let body = self.substatement()?;
        Ok(Stmt::For { init, test, update, body: Box::new(body) })
    }

    fn for_in_of_rest(&mut self, head: ForHead, of: bool, is_await: bool) -> R<Stmt> {
        self.advance();
        let rhs = if of { self.assignment(false)? } else { self.expression(false)? };
        self.expect(")")?;
        let body = Box::new(self.substatement()?);
        Ok(if of { Stmt::ForOf { head, iter: rhs, body, is_await } } else { Stmt::ForIn { head, obj: rhs, body } })
    }

    fn try_statement(&mut self) -> R<Stmt> {
        self.advance();
        let block = self.block()?;
        let mut param = None;
        let mut handler = None;
        if self.eat_kw("catch") {
            if self.eat("(") {
                param = Some(self.binding_target()?);
                self.expect(")")?;
            }
            handler = Some(self.block()?);
        }
        let finalizer = if self.eat_kw("finally") { Some(self.block()?) } else { None };
        if handler.is_none() && finalizer.is_none() {
            return Err(self.err("'try' needs 'catch' or 'finally'"));
        }
        Ok(Stmt::Try { block, param, handler, finalizer })
    }

    fn switch_statement(&mut self) -> R<Stmt> {
        self.advance();
        self.expect("(")?;
        let disc = self.expression(false)?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        let mut seen_default = false;
        while !self.eat("}") {
            let test = if self.eat_kw("default") {
                if seen_default {
                    return Err(self.err("more than one 'default' in a switch"));
                }
                seen_default = true;
                None
            } else {
                self.expect_kw("case")?;
                Some(self.expression(false)?)
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.is("}") && !self.is_kw("case") && !self.is_kw("default") {
                if self.at_eof() {
                    return Err(self.err("expected '}'"));
                }
                body.push(self.statement_list_item()?);
            }
            cases.push(Case { test, body });
        }
        Ok(Stmt::Switch(disc, cases))
    }

    // ------------------------------------------------------------ functions

    fn function_declaration(&mut self, is_async: bool) -> R<Rc<Function>> {
        let start = if is_async { self.toks[self.pos - 1].start } else { self.token().start };
        self.expect_kw("function")?;
        let is_generator = self.eat("*");
        let name = Some(self.binding_identifier()?);
        self.function_rest(name, FnKind::Normal, is_async, is_generator, start, false)
    }

    fn function_expression(&mut self, is_async: bool, start: u32) -> R<Expr> {
        self.expect_kw("function")?;
        let is_generator = self.eat("*");
        let name = if self.is("(") {
            None
        } else {
            // The name is bound inside the function, with its own async/generator-ness.
            let (g, a) = (self.in_generator, self.in_async);
            self.in_generator = is_generator;
            self.in_async = is_async;
            let n = self.binding_identifier();
            self.in_generator = g;
            self.in_async = a;
            Some(n?)
        };
        let f = self.function_rest(name, FnKind::Normal, is_async, is_generator, start, true)?;
        Ok(Expr::Function(f))
    }

    /// Parameters and body. The current token is `(`.
    fn function_rest(
        &mut self,
        name: Option<Name>,
        kind: FnKind,
        is_async: bool,
        is_generator: bool,
        start: u32,
        is_expr: bool,
    ) -> R<Rc<Function>> {
        let line = self.line();
        let saved = (self.strict, self.in_function, self.in_generator, self.in_async, self.super_prop, self.super_call);
        let saved_field = self.in_class_field;
        self.in_class_field = false;
        self.in_function = true;
        self.in_generator = is_generator;
        self.in_async = is_async;
        match kind {
            FnKind::Method | FnKind::Getter | FnKind::Setter | FnKind::ClassConstructor => {
                self.super_prop = true;
                self.super_call = false;
            }
            FnKind::DerivedConstructor => {
                self.super_prop = true;
                self.super_call = true;
            }
            FnKind::Normal => {
                self.super_prop = false;
                self.super_call = false;
            }
            _ => {}
        }
        self.fns.push(FnCtx::default());
        let result = (|| {
            self.expect("(")?;
            let (params, rest) = self.formal_parameters()?;
            self.expect("{")?;
            let body = self.body_with_directives(false)?;
            self.expect("}")?;
            Ok((params, rest, body))
        })();
        let ctx = self.fns.pop().unwrap();
        let strict = self.strict;
        (self.strict, self.in_function, self.in_generator, self.in_async, self.super_prop, self.super_call) = saved;
        self.in_class_field = saved_field;
        let (params, rest, body) = result?;
        let simple = rest.is_none() && params.iter().all(|p| p.init.is_none() && matches!(p.pat, Pat::Ident(_)));
        if strict && !saved.0 && !simple {
            return Err(self.err("'use strict' isn't allowed in a function with non-simple parameters"));
        }
        if strict {
            if let Some(n) = &name {
                if &**n == "eval" || &**n == "arguments" {
                    return Err(self.err("invalid function name in strict mode"));
                }
            }
            let mut seen = BTreeSet::new();
            for p in &params {
                if let Pat::Ident(n) = &p.pat {
                    if &**n == "eval" || &**n == "arguments" || !seen.insert(n.clone()) {
                        return Err(self.err("invalid parameter name in strict mode"));
                    }
                }
            }
        }
        let end = self.toks[self.pos - 1].end;
        Ok(self.finish_function(
            Function {
                name,
                length: params.iter().take_while(|p| p.init.is_none()).count() as u32,
                params,
                rest,
                body,
                kind,
                is_async,
                is_generator,
                strict,
                is_expr,
                inner_refs: BTreeSet::new(),
                has_eval: false,
                uses_arguments: false,
                span: (start, end),
                line,
            },
            ctx,
        ))
    }

    fn finish_function(&mut self, mut f: Function, ctx: FnCtx) -> Rc<Function> {
        f.inner_refs = ctx.inner;
        f.has_eval = ctx.has_eval;
        f.uses_arguments = ctx.uses_arguments;
        let parent = self.fns.last_mut().unwrap();
        let mut all = ctx.refs;
        all.extend(f.inner_refs.iter().cloned());
        if f.kind != FnKind::Arrow {
            all.remove("arguments");
        }
        parent.inner.extend(all);
        if ctx.has_eval {
            parent.has_eval = true;
        }
        Rc::new(f)
    }

    fn formal_parameters(&mut self) -> R<(Vec<Param>, Option<Pat>)> {
        let mut params = Vec::new();
        let mut rest = None;
        while !self.is(")") {
            if self.eat("...") {
                rest = Some(self.binding_target()?);
                break;
            }
            let (pat, init) = self.binding_element()?;
            params.push(Param { pat, init });
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.expect(")")?;
        Ok((params, rest))
    }

    /// An arrow function; the current token begins its parameters.
    fn arrow_function(&mut self, is_async: bool, start: u32) -> R<Expr> {
        let line = self.line();
        let saved = (self.in_generator, self.in_async, self.strict);
        self.fns.push(FnCtx { is_arrow: true, ..FnCtx::default() });
        self.in_generator = false;
        self.in_async = is_async;
        let result = (|| {
            let (params, rest) = if self.is("(") {
                self.advance();
                self.formal_parameters()?
            } else {
                let n = self.binding_identifier()?;
                (vec![Param { pat: Pat::Ident(n), init: None }], None)
            };
            if self.token().nl_before {
                return Err(self.err("no line break is allowed before '=>'"));
            }
            self.expect("=>")?;
            let saved_fn = self.in_function;
            self.in_function = true;
            let body = if self.is("{") {
                self.advance();
                let b = self.body_with_directives(false);
                self.in_function = saved_fn;
                let b = b?;
                self.expect("}")?;
                b
            } else {
                let e = self.assignment(false);
                self.in_function = saved_fn;
                vec![Stmt::Return(Some(e?))]
            };
            Ok((params, rest, body))
        })();
        let ctx = self.fns.pop().unwrap();
        let strict = self.strict;
        (self.in_generator, self.in_async, self.strict) = saved;
        let (params, rest, body) = result?;
        let end = self.toks[self.pos - 1].end;
        let f = Function {
            name: None,
            length: params.iter().take_while(|p| p.init.is_none()).count() as u32,
            params,
            rest,
            body,
            kind: FnKind::Arrow,
            is_async,
            is_generator: false,
            strict,
            is_expr: true,
            inner_refs: BTreeSet::new(),
            has_eval: false,
            uses_arguments: false,
            span: (start, end),
            line,
        };
        Ok(Expr::Function(self.finish_function(f, ctx)))
    }

    /// Whether the tokens from the current `(` form arrow parameters.
    fn arrow_ahead(&self) -> bool {
        if !self.is("(") {
            return false;
        }
        let mut depth = 0;
        let mut i = self.pos;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::Punct("(" | "[" | "{") => depth += 1,
                Tok::Punct(")" | "]" | "}") => {
                    depth -= 1;
                    if depth == 0 {
                        let next = &self.toks[(i + 1).min(self.toks.len() - 1)];
                        return matches!(next.tok, Tok::Punct("=>")) && !next.nl_before;
                    }
                }
                Tok::Template { tail: false, .. } => depth += 1,
                Tok::Template { cont: true, tail: true, .. } => depth -= 1,
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    // ------------------------------------------------------------ classes

    fn class(&mut self, declaration: bool) -> R<Rc<Class>> {
        let start = self.token().start;
        self.expect_kw("class")?;
        let saved_strict = self.strict;
        self.strict = true;
        let result = self.class_inner(declaration, start);
        self.strict = saved_strict;
        result
    }

    fn class_inner(&mut self, declaration: bool, start: u32) -> R<Rc<Class>> {
        let name = if self.is_identifier() && !self.is_kw("extends") {
            Some(self.binding_identifier()?)
        } else if declaration {
            return Err(self.err("a class declaration needs a name"));
        } else {
            None
        };
        let extends = if self.eat_kw("extends") { Some(Box::new(self.left_hand_side()?)) } else { None };
        self.expect("{")?;
        let mut constructor = None;
        let mut members = Vec::new();
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let member_start = self.token().start;
            let mut is_static = false;
            if self.is_kw("static")
                && !self.is_at(1, "(")
                && !self.is_at(1, "=")
                && !self.is_at(1, ";")
                && !self.is_at(1, "}")
            {
                self.advance();
                is_static = true;
                if self.is("{") {
                    // A static initialisation block.
                    let f = self.class_init_function(|p| {
                        p.advance();
                        let saved = (p.in_function, p.in_async);
                        p.in_function = false;
                        p.in_async = false;
                        let mut body = Vec::new();
                        while !p.is("}") {
                            if p.at_eof() {
                                return Err(p.err("expected '}'"));
                            }
                            body.push(p.statement_list_item()?);
                        }
                        p.advance();
                        (p.in_function, p.in_async) = saved;
                        Ok(body)
                    })?;
                    members.push(ClassMember {
                        key: PropName::Ident(Name::from("")),
                        kind: ClassMemberKind::StaticBlock,
                        is_static: true,
                        value: Some(f),
                    });
                    continue;
                }
            }
            let mut is_async = false;
            let mut is_generator = false;
            let mut accessor = None;
            if self.is_kw("async")
                && !self.is_at(1, "(")
                && !self.is_at(1, "=")
                && !self.toks[self.pos + 1].nl_before
                && !self.is_at(1, ";")
                && !self.is_at(1, "}")
            {
                self.advance();
                is_async = true;
            }
            if self.eat("*") {
                is_generator = true;
            }
            if !is_async
                && !is_generator
                && (self.is_kw("get") || self.is_kw("set"))
                && !self.is_at(1, "(")
                && !self.is_at(1, "=")
                && !self.is_at(1, ";")
                && !self.is_at(1, "}")
            {
                accessor = Some(self.is_kw("get"));
                self.advance();
            }
            let key = self.class_element_name()?;
            let is_ctor_name = !is_static && matches!(&key, PropName::Ident(n) if &**n == "constructor")
                || matches!(&key, PropName::Str(s) if !is_static && s.eq_str("constructor"));
            if self.is("(") {
                let kind = if is_ctor_name && accessor.is_none() && !is_async && !is_generator {
                    if extends.is_some() {
                        FnKind::DerivedConstructor
                    } else {
                        FnKind::ClassConstructor
                    }
                } else {
                    match accessor {
                        Some(true) => FnKind::Getter,
                        Some(false) => FnKind::Setter,
                        None => FnKind::Method,
                    }
                };
                if is_ctor_name && kind == FnKind::Method {
                    return Err(self.err("the class constructor can't be a special method"));
                }
                let fname = match &key {
                    PropName::Ident(n) => Some(n.clone()),
                    PropName::Str(s) => Some(Name::from(s.to_rust())),
                    PropName::Private(n) => Some(Name::from(format!("#{n}"))),
                    _ => None,
                };
                let f = self.function_rest(fname, kind, is_async, is_generator, member_start, true)?;
                if matches!(kind, FnKind::ClassConstructor | FnKind::DerivedConstructor) {
                    if constructor.is_some() {
                        return Err(self.err("a class may only have one constructor"));
                    }
                    constructor = Some(f);
                    continue;
                }
                let kind = match accessor {
                    Some(true) => ClassMemberKind::Getter,
                    Some(false) => ClassMemberKind::Setter,
                    None => ClassMemberKind::Method,
                };
                members.push(ClassMember { key, kind, is_static, value: Some(f) });
            } else {
                // A field.
                if is_async || is_generator || accessor.is_some() {
                    return Err(self.unexpected());
                }
                if is_ctor_name || matches!(&key, PropName::Ident(n) if &**n == "constructor") {
                    return Err(self.err("a class field can't be named 'constructor'"));
                }
                let value = if self.is("=") {
                    Some(self.class_init_function(|p| {
                        p.advance();
                        let saved = (p.in_function, p.in_async, p.in_generator, p.in_class_field);
                        p.in_function = false;
                        p.in_async = false;
                        p.in_generator = false;
                        p.in_class_field = true;
                        let e = p.assignment(false);
                        (p.in_function, p.in_async, p.in_generator, p.in_class_field) = saved;
                        Ok(vec![Stmt::Return(Some(e?))])
                    })?)
                } else {
                    None
                };
                self.semicolon()?;
                members.push(ClassMember { key, kind: ClassMemberKind::Field, is_static, value });
            }
        }
        let end = self.toks[self.pos - 1].end;
        let constructor = Some(match constructor {
            Some(c) => c,
            None => default_constructor(name.clone(), extends.is_some(), (start, end)),
        });
        Ok(Rc::new(Class { name, extends, constructor, members, span: (start, end) }))
    }

    /// Wraps a field initialiser or static block as a function.
    fn class_init_function(&mut self, body: impl FnOnce(&mut Self) -> R<Vec<Stmt>>) -> R<Rc<Function>> {
        let start = self.token().start;
        let line = self.line();
        let saved = (self.super_prop, self.super_call);
        self.super_prop = true;
        self.super_call = false;
        self.fns.push(FnCtx::default());
        let result = body(self);
        let ctx = self.fns.pop().unwrap();
        (self.super_prop, self.super_call) = saved;
        let body = result?;
        let end = self.toks[self.pos - 1].end;
        let f = Function {
            name: None,
            params: Vec::new(),
            rest: None,
            body,
            kind: FnKind::ClassInit,
            is_async: false,
            is_generator: false,
            strict: true,
            is_expr: true,
            inner_refs: BTreeSet::new(),
            has_eval: false,
            uses_arguments: false,
            span: (start, end),
            length: 0,
            line,
        };
        Ok(self.finish_function(f, ctx))
    }

    fn class_element_name(&mut self) -> R<PropName> {
        if let Tok::Private(n) = self.tok().clone() {
            if &*n == "constructor" {
                return Err(self.err("'#constructor' is not allowed"));
            }
            self.advance();
            let key = Name::from(format!("#{n}"));
            self.reference(&key);
            return Ok(PropName::Private(n));
        }
        self.property_name()
    }

    // ------------------------------------------------------------ expressions

    pub fn expression(&mut self, no_in: bool) -> R<Expr> {
        let first = self.assignment(no_in)?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut list = vec![first];
        while self.eat(",") {
            list.push(self.assignment(no_in)?);
        }
        Ok(Expr::Seq(list))
    }

    fn assignment(&mut self, no_in: bool) -> R<Expr> {
        self.enter()?;
        let r = self.assignment_inner(no_in);
        self.leave();
        r
    }

    fn assignment_inner(&mut self, no_in: bool) -> R<Expr> {
        // Arrow functions.
        let start = self.token().start;
        if self.is_identifier() && self.is_at(1, "=>") && !self.toks[self.pos + 1].nl_before {
            return self.arrow_function(false, start);
        }
        if self.is_kw("async") && !self.toks[self.pos + 1].nl_before {
            if matches!(self.peek_tok(1), Tok::Ident(..)) && self.is_at(2, "=>") && !self.is_kw_at(1, "function") {
                self.advance();
                return self.arrow_function(true, start);
            }
            if self.is_at(1, "(") {
                self.advance();
                if self.arrow_ahead() {
                    return self.arrow_function(true, start);
                }
                self.pos -= 1;
            }
        }
        if self.arrow_ahead() {
            return self.arrow_function(false, start);
        }
        if self.in_generator && self.is_kw("yield") {
            if self.in_class_field {
                return Err(self.err("'yield' is not allowed here"));
            }
            self.advance();
            let ends = self.token().nl_before
                || matches!(self.tok(), Tok::Punct(")" | "]" | "}" | "," | ";" | ":") | Tok::Eof)
                || self.is_kw("in");
            if ends {
                return Ok(Expr::Yield { arg: None, delegate: false });
            }
            let delegate = self.eat("*");
            let arg = self.assignment(no_in)?;
            return Ok(Expr::Yield { arg: Some(Box::new(arg)), delegate });
        }
        let lhs = self.conditional(no_in)?;
        let op = match self.tok() {
            Tok::Punct(p) => match *p {
                "=" => Some(AssignOp::Assign),
                "+=" => Some(AssignOp::Bin(BinOp::Add)),
                "-=" => Some(AssignOp::Bin(BinOp::Sub)),
                "*=" => Some(AssignOp::Bin(BinOp::Mul)),
                "/=" => Some(AssignOp::Bin(BinOp::Div)),
                "%=" => Some(AssignOp::Bin(BinOp::Mod)),
                "**=" => Some(AssignOp::Bin(BinOp::Exp)),
                "<<=" => Some(AssignOp::Bin(BinOp::Shl)),
                ">>=" => Some(AssignOp::Bin(BinOp::Shr)),
                ">>>=" => Some(AssignOp::Bin(BinOp::UShr)),
                "&=" => Some(AssignOp::Bin(BinOp::BitAnd)),
                "|=" => Some(AssignOp::Bin(BinOp::BitOr)),
                "^=" => Some(AssignOp::Bin(BinOp::BitXor)),
                "&&=" => Some(AssignOp::Logic(LogicOp::And)),
                "||=" => Some(AssignOp::Logic(LogicOp::Or)),
                "??=" => Some(AssignOp::Logic(LogicOp::Nullish)),
                _ => None,
            },
            _ => None,
        };
        let Some(op) = op else { return Ok(lhs) };
        self.advance();
        let target = if op == AssignOp::Assign { self.to_pattern(lhs)? } else { self.simple_target(lhs)? };
        let value = self.assignment(no_in)?;
        Ok(Expr::Assign { op, target: Box::new(target), value: Box::new(value) })
    }

    /// An identifier or member expression (for compound assignment and ++/--).
    fn simple_target(&self, e: Expr) -> R<Pat> {
        match e {
            Expr::Ident(n) => {
                if self.strict && (&*n == "eval" || &*n == "arguments") {
                    return Err(self.err("invalid assignment target in strict mode"));
                }
                Ok(Pat::Ident(n))
            }
            Expr::Member { optional: false, .. } | Expr::SuperMember(_) => Ok(Pat::Expr(Box::new(e))),
            Expr::Paren(inner) => self.simple_target(*inner),
            Expr::Call { .. } if !self.strict => Ok(Pat::Expr(Box::new(e))),
            _ => Err(self.err("invalid assignment target")),
        }
    }

    /// Reinterprets an expression as an assignment pattern.
    fn to_pattern(&self, e: Expr) -> R<Pat> {
        match e {
            Expr::Array(elems) => {
                let mut out = Vec::new();
                let mut rest = None;
                let n = elems.len();
                for (i, el) in elems.into_iter().enumerate() {
                    match el {
                        ArrayElem::Hole => out.push(None),
                        ArrayElem::Spread(e) => {
                            if i != n - 1 {
                                return Err(self.err("a rest element must be last"));
                            }
                            rest = Some(Box::new(self.to_pattern(e)?));
                        }
                        ArrayElem::Expr(Expr::Assign { op: AssignOp::Assign, target, value }) => {
                            out.push(Some(PatElem { pat: *target, init: Some(*value) }))
                        }
                        ArrayElem::Expr(e) => out.push(Some(PatElem { pat: self.to_pattern(e)?, init: None })),
                    }
                }
                Ok(Pat::Array { elems: out, rest })
            }
            Expr::Object(props) => {
                let mut out = Vec::new();
                let mut rest = None;
                let n = props.len();
                for (i, p) in props.into_iter().enumerate() {
                    match p {
                        ObjProp::Spread(e) => {
                            if i != n - 1 {
                                return Err(self.err("a rest element must be last"));
                            }
                            rest = Some(Box::new(self.simple_target(e)?));
                        }
                        ObjProp::Prop { key, value, kind, method } => {
                            if method || matches!(kind, PropKind::Get | PropKind::Set) {
                                return Err(self.err("invalid destructuring target"));
                            }
                            match kind {
                                PropKind::Shorthand => {
                                    let pat = self.simple_target(value)?;
                                    out.push(PatProp { key, value: pat, init: None });
                                }
                                PropKind::ShorthandInit(init) => {
                                    let pat = self.simple_target(value)?;
                                    out.push(PatProp { key, value: pat, init: Some(*init) });
                                }
                                _ => {
                                    let (pat, init) = match value {
                                        Expr::Assign { op: AssignOp::Assign, target, value } => (*target, Some(*value)),
                                        v => (self.to_pattern(v)?, None),
                                    };
                                    out.push(PatProp { key, value: pat, init });
                                }
                            }
                        }
                    }
                }
                Ok(Pat::Object { props: out, rest })
            }
            Expr::Assign { .. } => Err(self.err("invalid destructuring target")),
            Expr::Paren(inner) if !matches!(*inner, Expr::Object(_) | Expr::Array(_)) => self.simple_target(*inner),
            e => self.simple_target(e),
        }
    }

    fn conditional(&mut self, no_in: bool) -> R<Expr> {
        let test = self.binary(0, no_in)?;
        if !self.eat("?") {
            return Ok(test);
        }
        let cons = self.assignment(false)?;
        self.expect(":")?;
        let alt = self.assignment(no_in)?;
        Ok(Expr::Cond(Box::new(test), Box::new(cons), Box::new(alt)))
    }

    fn binary_op(&self, no_in: bool) -> Option<(u8, Result<BinOp, LogicOp>)> {
        let p = match self.tok() {
            Tok::Punct(p) => *p,
            Tok::Ident(n, false) if &**n == "instanceof" => return Some((8, Ok(BinOp::InstanceOf))),
            Tok::Ident(n, false) if &**n == "in" && !no_in => return Some((8, Ok(BinOp::In))),
            _ => return None,
        };
        Some(match p {
            "??" => (1, Err(LogicOp::Nullish)),
            "||" => (2, Err(LogicOp::Or)),
            "&&" => (3, Err(LogicOp::And)),
            "|" => (4, Ok(BinOp::BitOr)),
            "^" => (5, Ok(BinOp::BitXor)),
            "&" => (6, Ok(BinOp::BitAnd)),
            "==" => (7, Ok(BinOp::Eq)),
            "!=" => (7, Ok(BinOp::Ne)),
            "===" => (7, Ok(BinOp::StrictEq)),
            "!==" => (7, Ok(BinOp::StrictNe)),
            "<" => (8, Ok(BinOp::Lt)),
            ">" => (8, Ok(BinOp::Gt)),
            "<=" => (8, Ok(BinOp::Le)),
            ">=" => (8, Ok(BinOp::Ge)),
            "<<" => (9, Ok(BinOp::Shl)),
            ">>" => (9, Ok(BinOp::Shr)),
            ">>>" => (9, Ok(BinOp::UShr)),
            "+" => (10, Ok(BinOp::Add)),
            "-" => (10, Ok(BinOp::Sub)),
            "*" => (11, Ok(BinOp::Mul)),
            "/" => (11, Ok(BinOp::Div)),
            "%" => (11, Ok(BinOp::Mod)),
            "**" => (12, Ok(BinOp::Exp)),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8, no_in: bool) -> R<Expr> {
        // `#x in obj`.
        let mut left = if let (Tok::Private(n), true) = (self.tok().clone(), self.is_kw_at(1, "in")) {
            self.advance();
            self.advance();
            let key = Name::from(format!("#{n}"));
            self.reference(&key);
            let right = self.binary(9, no_in)?;
            Expr::PrivateIn(n, Box::new(right))
        } else {
            self.unary()?
        };
        let mut last_logic: Option<LogicOp> = None;
        while let Some((prec, op)) = self.binary_op(no_in) {
            if prec < min || (prec == min && prec != 12) {
                break;
            }
            if prec == 12 && matches!(left, Expr::Unary(..) | Expr::Await(_)) {
                return Err(self.err("unary operand of '**' must be parenthesised"));
            }
            self.advance();
            // `**` is right-associative.
            let right = if prec == 12 { self.binary(12, no_in)? } else { self.binary(prec, no_in)? };
            left = match op {
                Ok(b) => Expr::Binary(b, Box::new(left), Box::new(right)),
                Err(l) => {
                    let mixes = |a: LogicOp, b: LogicOp| (a == LogicOp::Nullish) != (b == LogicOp::Nullish);
                    if let Some(prev) = last_logic {
                        if mixes(prev, l) {
                            return Err(self.err("'??' can't be mixed with '&&' or '||' without parentheses"));
                        }
                    }
                    if let Expr::Logic(inner, ..) = &right {
                        if mixes(*inner, l) {
                            return Err(self.err("'??' can't be mixed with '&&' or '||' without parentheses"));
                        }
                    }
                    last_logic = Some(l);
                    Expr::Logic(l, Box::new(left), Box::new(right))
                }
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> R<Expr> {
        self.enter()?;
        let r = self.unary_inner();
        self.leave();
        r
    }

    fn unary_inner(&mut self) -> R<Expr> {
        let op = match self.tok() {
            Tok::Punct("!") => Some(UnaryOp::Not),
            Tok::Punct("-") => Some(UnaryOp::Neg),
            Tok::Punct("+") => Some(UnaryOp::Plus),
            Tok::Punct("~") => Some(UnaryOp::BitNot),
            Tok::Ident(n, false) if &**n == "typeof" => Some(UnaryOp::Typeof),
            Tok::Ident(n, false) if &**n == "void" => Some(UnaryOp::Void),
            Tok::Ident(n, false) if &**n == "delete" => Some(UnaryOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let arg = self.unary()?;
            if op == UnaryOp::Delete && self.strict {
                if let Expr::Ident(_) = strip_parens(&arg) {
                    return Err(self.err("deleting a variable is not allowed in strict mode"));
                }
            }
            if op == UnaryOp::Delete {
                if let Expr::Member { prop: MemberProp::Private(_), .. } = strip_parens(&arg) {
                    return Err(self.err("private fields can't be deleted"));
                }
            }
            return Ok(Expr::Unary(op, Box::new(arg)));
        }
        if self.is("++") || self.is("--") {
            let inc = self.is("++");
            self.advance();
            let arg = self.unary()?;
            let target = self.simple_target(arg)?;
            return Ok(Expr::Update { inc, prefix: true, target: Box::new(pat_to_expr(target)) });
        }
        if self.in_async && self.is_kw("await") {
            if self.in_class_field {
                return Err(self.err("'await' is not allowed here"));
            }
            self.advance();
            let arg = self.unary()?;
            return Ok(Expr::Await(Box::new(arg)));
        }
        let e = self.left_hand_side()?;
        if (self.is("++") || self.is("--")) && !self.token().nl_before {
            let inc = self.is("++");
            self.advance();
            let target = self.simple_target(e)?;
            return Ok(Expr::Update { inc, prefix: false, target: Box::new(pat_to_expr(target)) });
        }
        Ok(e)
    }

    fn arguments(&mut self) -> R<Vec<Arg>> {
        self.expect("(")?;
        let mut args = Vec::new();
        while !self.is(")") {
            if self.eat("...") {
                args.push(Arg::Spread(self.assignment(false)?));
            } else {
                args.push(Arg::Expr(self.assignment(false)?));
            }
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.advance();
        Ok(args)
    }

    /// Source text from byte `start` to the end of the previous token (capped).
    fn text_from(&self, start: u32) -> Rc<str> {
        let end = self.toks[self.pos.saturating_sub(1)].end.max(start) as usize;
        let t = &self.src[start as usize..end];
        if t.len() > 80 {
            let mut cut = 77;
            while !t.is_char_boundary(cut) {
                cut -= 1;
            }
            Rc::from(alloc::format!("{}...", &t[..cut]).as_str())
        } else {
            Rc::from(t)
        }
    }

    fn left_hand_side(&mut self) -> R<Expr> {
        let start = self.token().start;
        let mut e = if self.is_kw("new") {
            self.new_expression()?
        } else if self.is_kw("super") {
            self.super_expression()?
        } else if self.is_kw("import") {
            return Err(self.err("modules are not supported"));
        } else {
            self.primary()?
        };
        let mut in_chain = false;
        loop {
            let line = self.line();
            match self.tok().clone() {
                Tok::Punct(".") => {
                    self.advance();
                    let prop = self.member_name()?;
                    e = Expr::Member { obj: Box::new(e), prop, optional: false, line };
                }
                Tok::Punct("?.") => {
                    self.advance();
                    in_chain = true;
                    if self.is("(") {
                        let text = self.text_from(start);
                        let args = self.arguments()?;
                        e = Expr::Call { callee: Box::new(e), args, optional: true, line, text };
                    } else if self.eat("[") {
                        let p = self.expression(false)?;
                        self.expect("]")?;
                        e = Expr::Member {
                            obj: Box::new(e),
                            prop: MemberProp::Computed(Box::new(p)),
                            optional: true,
                            line,
                        };
                    } else if let Tok::Template { .. } = self.tok() {
                        return Err(self.err("tagged templates can't be used in optional chains"));
                    } else {
                        let prop = self.member_name()?;
                        e = Expr::Member { obj: Box::new(e), prop, optional: true, line };
                    }
                }
                Tok::Punct("[") => {
                    self.advance();
                    let p = self.expression(false)?;
                    self.expect("]")?;
                    e = Expr::Member {
                        obj: Box::new(e),
                        prop: MemberProp::Computed(Box::new(p)),
                        optional: false,
                        line,
                    };
                }
                Tok::Punct("(") => {
                    let text = self.text_from(start);
                    let args = self.arguments()?;
                    if let Expr::Ident(n) = &e {
                        if &**n == "eval" {
                            for f in self.fns.iter_mut() {
                                f.has_eval = true;
                            }
                        }
                    }
                    e = Expr::Call { callee: Box::new(e), args, optional: false, line, text };
                }
                Tok::Template { cont: false, .. } => {
                    if in_chain {
                        return Err(self.err("tagged templates can't be used in optional chains"));
                    }
                    let (quasis, exprs) = self.template_parts(true)?;
                    let site = self.next_site;
                    self.next_site += 1;
                    e = Expr::Tagged { tag: Box::new(e), quasis, exprs, site };
                }
                _ => break,
            }
        }
        if in_chain {
            e = Expr::OptChain(Box::new(e));
        }
        Ok(e)
    }

    fn member_name(&mut self) -> R<MemberProp> {
        if let Tok::Private(n) = self.tok().clone() {
            self.advance();
            let key = Name::from(format!("#{n}"));
            self.reference(&key);
            return Ok(MemberProp::Private(n));
        }
        Ok(MemberProp::Name(self.identifier_name()?))
    }

    fn super_expression(&mut self) -> R<Expr> {
        self.advance();
        if self.is("(") {
            if !self.super_call {
                return Err(self.err("'super()' is only valid in derived class constructors"));
            }
            let args = self.arguments()?;
            return Ok(Expr::SuperCall(args));
        }
        if !self.super_prop {
            return Err(self.err("'super' is only valid inside methods"));
        }
        if self.eat(".") {
            return Ok(Expr::SuperMember(MemberProp::Name(self.identifier_name()?)));
        }
        if self.eat("[") {
            let p = self.expression(false)?;
            self.expect("]")?;
            return Ok(Expr::SuperMember(MemberProp::Computed(Box::new(p))));
        }
        Err(self.err("unexpected 'super'"))
    }

    fn new_expression(&mut self) -> R<Expr> {
        let line = self.line();
        self.advance();
        if self.eat(".") {
            match self.tok() {
                Tok::Ident(n, false) if &**n == "target" => {
                    if !self.in_function && !self.in_class_field && !self.fns.iter().any(|f| !f.is_arrow) {
                        return Err(self.err("'new.target' is only valid inside functions"));
                    }
                    self.advance();
                    return Ok(Expr::NewTarget);
                }
                _ => return Err(self.unexpected()),
            }
        }
        // The callee: a member expression without calls.
        let callee_start = self.token().start;
        let mut callee = if self.is_kw("new") {
            self.new_expression()?
        } else if self.is_kw("super") {
            self.super_expression()?
        } else {
            self.primary()?
        };
        loop {
            let line = self.line();
            if self.eat(".") {
                let prop = self.member_name()?;
                callee = Expr::Member { obj: Box::new(callee), prop, optional: false, line };
            } else if self.eat("[") {
                let p = self.expression(false)?;
                self.expect("]")?;
                callee = Expr::Member {
                    obj: Box::new(callee),
                    prop: MemberProp::Computed(Box::new(p)),
                    optional: false,
                    line,
                };
            } else if let Tok::Template { cont: false, .. } = self.tok() {
                let (quasis, exprs) = self.template_parts(true)?;
                let site = self.next_site;
                self.next_site += 1;
                callee = Expr::Tagged { tag: Box::new(callee), quasis, exprs, site };
            } else if self.is("?.") {
                return Err(self.err("optional chains are not allowed in 'new' expressions"));
            } else {
                break;
            }
        }
        let text = self.text_from(callee_start);
        let args = if self.is("(") { self.arguments()? } else { Vec::new() };
        Ok(Expr::New { callee: Box::new(callee), args, line, text })
    }

    fn template_parts(&mut self, tagged: bool) -> R<(Vec<TemplatePart>, Vec<Expr>)> {
        let mut quasis = Vec::new();
        let mut exprs = Vec::new();
        loop {
            let Tok::Template { cooked, raw, tail, .. } = self.tok().clone() else {
                return Err(self.err("unterminated template literal"));
            };
            if cooked.is_none() && !tagged {
                return Err(self.err("invalid escape sequence in template"));
            }
            self.advance();
            quasis.push(TemplatePart { cooked, raw });
            if tail {
                break;
            }
            exprs.push(self.expression(false)?);
            if !matches!(self.tok(), Tok::Template { cont: true, .. }) {
                return Err(self.err("expected '}' to close a template substitution"));
            }
        }
        Ok((quasis, exprs))
    }

    fn primary(&mut self) -> R<Expr> {
        let start = self.token().start;
        match self.tok().clone() {
            Tok::Num(n) => {
                if self.strict && self.token().legacy_octal {
                    return Err(self.err("octal literals are not allowed in strict mode"));
                }
                let raw = &self.src[self.token().start as usize..self.token().end as usize];
                if self.strict && raw.len() > 1 && raw.starts_with('0') && raw.as_bytes()[1].is_ascii_digit() {
                    return Err(self.err("legacy octal literals are not allowed in strict mode"));
                }
                self.advance();
                Ok(Expr::Num(n))
            }
            Tok::Str(s) => {
                if self.strict && self.token().legacy_octal {
                    return Err(self.err("octal escapes are not allowed in strict mode"));
                }
                self.advance();
                Ok(Expr::Str(s))
            }
            Tok::Template { cont: false, .. } => {
                let (quasis, exprs) = self.template_parts(false)?;
                Ok(Expr::Template { quasis, exprs })
            }
            Tok::Regex { pattern, flags } => {
                self.advance();
                Ok(Expr::Regex { pattern, flags })
            }
            Tok::Punct("(") => {
                self.advance();
                let e = self.expression(false)?;
                self.expect(")")?;
                Ok(Expr::Paren(Box::new(e)))
            }
            Tok::Punct("[") => self.array_literal(),
            Tok::Punct("{") => self.object_literal(),
            Tok::Ident(n, escaped) => {
                if !escaped {
                    match &*n {
                        "this" => {
                            self.advance();
                            return Ok(Expr::This);
                        }
                        "null" => {
                            self.advance();
                            return Ok(Expr::Null);
                        }
                        "true" | "false" => {
                            self.advance();
                            return Ok(Expr::Bool(&*n == "true"));
                        }
                        "function" => return self.function_expression(false, start),
                        "async" if self.is_kw_at(1, "function") && !self.toks[self.pos + 1].nl_before => {
                            self.advance();
                            return self.function_expression(true, start);
                        }
                        "class" => return Ok(Expr::Class(self.class(false)?)),
                        _ => {}
                    }
                }
                if self.in_class_field && &*n == "arguments" {
                    return Err(self.err("'arguments' is not allowed in class field initialisers"));
                }
                Ok(Expr::Ident(self.identifier()?))
            }
            Tok::Private(_) => Err(self.err("private names are only valid in member expressions")),
            _ => Err(self.unexpected()),
        }
    }

    fn array_literal(&mut self) -> R<Expr> {
        self.advance();
        let mut elems = Vec::new();
        loop {
            if self.eat("]") {
                break;
            }
            if self.eat(",") {
                elems.push(ArrayElem::Hole);
                continue;
            }
            if self.eat("...") {
                elems.push(ArrayElem::Spread(self.assignment(false)?));
            } else {
                elems.push(ArrayElem::Expr(self.assignment(false)?));
            }
            if !self.is("]") {
                self.expect(",")?;
            }
        }
        Ok(Expr::Array(elems))
    }

    fn property_name(&mut self) -> R<PropName> {
        match self.tok().clone() {
            Tok::Ident(n, _) => {
                self.advance();
                Ok(PropName::Ident(n))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(PropName::Str(s))
            }
            Tok::Num(n) => {
                self.advance();
                Ok(PropName::Num(n))
            }
            Tok::Punct("[") => {
                self.advance();
                let e = self.assignment(false)?;
                self.expect("]")?;
                Ok(PropName::Computed(Box::new(e)))
            }
            _ => Err(self.err(&format!("expected a property name but found {}", self.describe()))),
        }
    }

    fn object_literal(&mut self) -> R<Expr> {
        self.advance();
        let mut props = Vec::new();
        while !self.eat("}") {
            if self.eat("...") {
                props.push(ObjProp::Spread(self.assignment(false)?));
            } else {
                props.push(self.object_property()?);
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        Ok(Expr::Object(props))
    }

    fn object_property(&mut self) -> R<ObjProp> {
        let start = self.token().start;
        let next_ends = |p: &Self| matches!(p.peek_tok(1), Tok::Punct(":" | "(" | "," | "}" | "="));
        // get/set/async/*
        let mut is_async = false;
        let mut is_generator = false;
        let mut accessor = None;
        if self.is_kw("async") && !next_ends(self) && !self.toks[self.pos + 1].nl_before {
            self.advance();
            is_async = true;
        }
        if self.eat("*") {
            is_generator = true;
        }
        if !is_async && !is_generator && (self.is_kw("get") || self.is_kw("set")) && !next_ends(self) {
            accessor = Some(self.is_kw("get"));
            self.advance();
        }
        // Shorthand `{ a }` / `{ a = 1 }`.
        if accessor.is_none() && !is_async && !is_generator {
            if let Tok::Ident(n, _) = self.tok().clone() {
                if matches!(self.peek_tok(1), Tok::Punct("," | "}" | "=")) {
                    let name = self.identifier()?;
                    let _ = n;
                    if self.eat("=") {
                        let init = self.assignment(false)?;
                        return Ok(ObjProp::Prop {
                            key: PropName::Ident(name.clone()),
                            value: Expr::Ident(name),
                            kind: PropKind::ShorthandInit(Box::new(init)),
                            method: false,
                        });
                    }
                    return Ok(ObjProp::Prop {
                        key: PropName::Ident(name.clone()),
                        value: Expr::Ident(name),
                        kind: PropKind::Shorthand,
                        method: false,
                    });
                }
            }
        }
        let key = self.property_name()?;
        if self.is("(") {
            let kind = match accessor {
                Some(true) => FnKind::Getter,
                Some(false) => FnKind::Setter,
                None => FnKind::Method,
            };
            let fname = match &key {
                PropName::Ident(n) => Some(n.clone()),
                PropName::Str(s) => Some(Name::from(s.to_rust())),
                _ => None,
            };
            let f = self.function_rest(fname, kind, is_async, is_generator, start, true)?;
            if kind == FnKind::Getter && (!f.params.is_empty() || f.rest.is_some()) {
                return Err(self.err("getters take no parameters"));
            }
            if kind == FnKind::Setter && (f.params.len() != 1 || f.rest.is_some()) {
                return Err(self.err("setters take exactly one parameter"));
            }
            let pk = match accessor {
                Some(true) => PropKind::Get,
                Some(false) => PropKind::Set,
                None => PropKind::Init,
            };
            return Ok(ObjProp::Prop { key, value: Expr::Function(f), kind: pk, method: true });
        }
        if accessor.is_some() || is_async || is_generator {
            return Err(self.unexpected());
        }
        self.expect(":")?;
        let value = self.assignment(false)?;
        Ok(ObjProp::Prop { key, value, kind: PropKind::Init, method: false })
    }
}

/// `constructor() {}` or `constructor(...args) { super(...args) }`.
fn default_constructor(name: Option<Name>, derived: bool, span: (u32, u32)) -> Rc<Function> {
    let args = Name::from("args");
    Rc::new(Function {
        name,
        params: Vec::new(),
        rest: if derived { Some(Pat::Ident(args.clone())) } else { None },
        body: if derived {
            vec![Stmt::Expr(Expr::SuperCall(vec![Arg::Spread(Expr::Ident(args))]))]
        } else {
            Vec::new()
        },
        kind: if derived { FnKind::DerivedConstructor } else { FnKind::ClassConstructor },
        is_async: false,
        is_generator: false,
        strict: true,
        is_expr: false,
        inner_refs: BTreeSet::new(),
        has_eval: false,
        uses_arguments: false,
        span,
        length: 0,
        line: 0,
    })
}

fn strip_parens(e: &Expr) -> &Expr {
    match e {
        Expr::Paren(inner) => strip_parens(inner),
        e => e,
    }
}

fn pat_to_expr(p: Pat) -> Expr {
    match p {
        Pat::Ident(n) => Expr::Ident(n),
        Pat::Expr(e) => *e,
        _ => unreachable!(),
    }
}

impl From<SyntaxError> for String {
    fn from(e: SyntaxError) -> String {
        format!("SyntaxError: {} (line {}, column {})", e.message, e.line, e.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) {
        if let Err(e) = parse_script(src) {
            panic!("{src}: {} at {}:{}", e.message, e.line, e.col);
        }
    }

    fn bad(src: &str) {
        assert!(parse_script(src).is_err(), "should fail: {src}");
    }

    #[test]
    fn parses() {
        ok("var a = 1, b = [1, , 2, ...c], d = {x, y: 2, [z]: 3, get g() { return 1 }, set s(v) {}, m() {}, ...e};");
        ok("function f(a, b = 2, ...c) { 'use strict'.length; return a + b * c ** 2 ** 2; }");
        ok("const f = (a, {b, c: [d]}) => a + b, g = async x => await x, h = async (y) => { yield: 1 };");
        ok("for (let i = 0; i < 10; i++) { if (i % 2) continue; else break; }");
        ok("for (const [k, v] of Object.entries(o)) {} for (k in o) {} for (var x = 0 in {}) {}");
        ok("label: for (;;) { while (true) { break label; } }");
        ok("try { throw new Error('x') } catch ({message}) { } finally { }  try {} catch {}");
        ok("switch (x) { case 1: case 2: y(); break; default: z() }");
        ok("class A extends B { #p = 1; static s = 2; static { this.t = 3 } constructor() { super(); this.#p++ } get x() { return this.#p } static async *gen() { yield* other() } #m() { return #p in this } }");
        ok("a?.b?.[c]?.(d).e; x ??= y || z; ({a, b} = {a: 1}); [a, b] = [b, a];");
        ok("tag`a${b}c`; `x${`y${z}`}`; new Foo; new Foo.Bar(1); new.target; let async = 1; async\nfunction f(){}");
        ok("function* g() { const x = yield; yield x; } async function h() { for await (const x of y) {} }");
        ok("x = a ? b : c ? d : e; y = typeof a === 'undefined' && !b; delete o[k]; void 0;");
        ok("if (a) function f() {}");
        ok("a\n++b");
        ok("let x = 1\nlet y = 2\n[1].map(x => x)");
        bad("let let = 1");
        bad("'use strict'; with (a) {}");
        bad("a ?? b || c");
        bad("-2 ** 2");
        bad("({a: 1}) = 1");
        bad("for (let x = 1 of y) {}");
        bad("return 1");
        bad("class A { constructor() { super() } }");
        bad("x = { get a(b) {} }");
        bad("'use strict'; var yield = 1");
        bad("function f() { 'use strict'; 010 }");
    }

    #[test]
    fn captured_names() {
        let s = parse_script("function f(a, b) { let c = 1; return () => a + c; }").unwrap();
        let Stmt::Function(f) = &s.body[0] else { panic!() };
        assert!(f.inner_refs.contains("a") && f.inner_refs.contains("c") && !f.inner_refs.contains("b"));
        let s = parse_script("function f() { return () => arguments[0]; }").unwrap();
        let Stmt::Function(f) = &s.body[0] else { panic!() };
        assert!(f.uses_arguments);
    }
}
