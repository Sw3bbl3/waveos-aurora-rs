//! Compiles the syntax tree to bytecode.
//!
//! Scopes are resolved statically. A binding lives in a frame slot unless a
//! nested function might refer to it (its name appears in the function's
//! `inner_refs`), in which case it lives in a heap scope that closures
//! capture. Top-level script bindings are globals.
//!
//! `break`, `continue` and `return` that leave `finally` blocks, loops over
//! iterators, or block scopes run the cleanup code inline (finally bodies are
//! compiled again at each exit), so the VM needs no completion records.

use crate::ast::*;
use crate::bytecode::{Code, CodeKind, Const, Op, TemplateData};
use crate::lexer::{Name, SyntaxError};
use crate::numconv;
use crate::value::JsStr;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

type R<T> = Result<T, SyntaxError>;

static NEXT_SITE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Debug)]
enum BKind {
    Var,
    Param,
    Function,
    Let,
    Const,
    Class,
    /// A named function expression's own name: immutable, silently in sloppy mode.
    Callee,
    /// Private names and a class's inner name binding.
    Hidden,
}

impl BKind {
    fn lexical(self) -> bool {
        matches!(self, BKind::Let | BKind::Const | BKind::Class | BKind::Hidden)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Loc {
    Local(u32),
    Heap(u16),
}

#[derive(Clone, Debug)]
struct Binding {
    name: Name,
    kind: BKind,
    loc: Loc,
}

#[derive(Default)]
struct CScope {
    bindings: Vec<Binding>,
    heap: bool,
    heap_size: u16,
    /// The script's top level: names not found below here are globals.
    global: bool,
}

impl CScope {
    fn find(&self, name: &str) -> Option<&Binding> {
        self.bindings.iter().rev().find(|b| &*b.name == name)
    }
}

#[derive(Clone, Copy)]
enum Cleanup<'a> {
    PopScope,
    PopHandler,
    IterClose,
    PopForIn,
    /// A finally body, and the number of compile-time scopes when it began.
    Finally(&'a [Stmt], usize),
}

struct Target {
    labels: Vec<Name>,
    is_loop: bool,
    breaks: Vec<usize>,
    continues: Vec<usize>,
    break_cleanups: usize,
    continue_cleanups: usize,
}

enum Res {
    Local(u32, BKind),
    Scope(u16, u16, BKind),
    Global,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// An assignment expression (checks TDZ and const).
    Assign,
    /// A declaration's initialisation.
    Init,
    /// A `var` declaration's initialiser.
    Var,
}

struct FnState<'a> {
    ops: Vec<Op>,
    consts: Vec<Const>,
    lines: Vec<(u32, u32)>,
    callees: Vec<(u32, u32)>,
    n_locals: u32,
    scopes: Vec<CScope>,
    targets: Vec<Target>,
    cleanups: Vec<Cleanup<'a>>,
    strict: bool,
    kind: CodeKind,
    is_async: bool,
    is_generator: bool,
    captured: BTreeSet<Name>,
    capture_all: bool,
    strings: BTreeMap<JsStr, u32>,
    opt_ends: Vec<Vec<usize>>,
    completion: Option<u32>,
    last_line: u32,
}

pub struct Compiler<'a> {
    src: Rc<str>,
    file: Rc<str>,
    fns: Vec<FnState<'a>>,
}

pub fn compile_script(script: &Script, src: Rc<str>, file: Rc<str>) -> R<Rc<Code>> {
    let mut c = Compiler { src, file, fns: Vec::new() };
    c.script(script)
}

fn err(msg: &str) -> SyntaxError {
    SyntaxError { message: String::from(msg), line: 0, col: 0 }
}

fn pattern_names(p: &Pat, out: &mut Vec<Name>) {
    match p {
        Pat::Ident(n) => out.push(n.clone()),
        Pat::Expr(_) => {}
        Pat::Object { props, rest } => {
            for pr in props {
                pattern_names(&pr.value, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pat::Array { elems, rest } => {
            for e in elems.iter().flatten() {
                pattern_names(&e.pat, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
    }
}

/// `var` names declared in these statements (not in nested functions), and
/// Annex B function names (functions declared in blocks, sloppy mode).
fn var_names(stmts: &[Stmt], out: &mut Vec<Name>, annex_b: &mut Vec<Name>, nested: bool, strict: bool) {
    for s in stmts {
        var_names_stmt(s, out, annex_b, nested, strict);
    }
}

fn var_names_stmt(s: &Stmt, out: &mut Vec<Name>, annex_b: &mut Vec<Name>, nested: bool, strict: bool) {
    match s {
        Stmt::Var(VarKind::Var, decls) => {
            for d in decls {
                pattern_names(&d.pat, out);
            }
        }
        Stmt::Function(f) if nested && !strict && !f.is_async && !f.is_generator => {
            if let Some(n) = &f.name {
                annex_b.push(n.clone());
            }
        }
        Stmt::If(_, a, b) => {
            var_names_stmt(a, out, annex_b, true, strict);
            if let Some(b) = b {
                var_names_stmt(b, out, annex_b, true, strict);
            }
        }
        Stmt::For { init, body, .. } => {
            if let Some(ForInit::Var(VarKind::Var, decls)) = init {
                for d in decls {
                    pattern_names(&d.pat, out);
                }
            }
            var_names_stmt(body, out, annex_b, true, strict);
        }
        Stmt::ForIn { head, body, .. } | Stmt::ForOf { head, body, .. } => {
            if let ForHead::Var(VarKind::Var, p) = head {
                pattern_names(p, out);
            }
            var_names_stmt(body, out, annex_b, true, strict);
        }
        Stmt::While(_, b) | Stmt::DoWhile(b, _) => var_names_stmt(b, out, annex_b, true, strict),
        Stmt::Labeled(_, b) => var_names_stmt(b, out, annex_b, nested, strict),
        Stmt::Block(b) => var_names(b, out, annex_b, true, strict),
        Stmt::Try { block, handler, finalizer, .. } => {
            var_names(block, out, annex_b, true, strict);
            if let Some(h) = handler {
                var_names(h, out, annex_b, true, strict);
            }
            if let Some(f) = finalizer {
                var_names(f, out, annex_b, true, strict);
            }
        }
        Stmt::Switch(_, cases) => {
            for c in cases {
                var_names(&c.body, out, annex_b, true, strict);
            }
        }
        _ => {}
    }
}

/// The lexical declarations directly in a statement list: (name, kind).
fn lexical_names(stmts: &[Stmt], functions_too: bool) -> Vec<(Name, BKind)> {
    let mut out = Vec::new();
    for s in stmts {
        match s {
            Stmt::Var(k @ (VarKind::Let | VarKind::Const), decls) => {
                let mut names = Vec::new();
                for d in decls {
                    pattern_names(&d.pat, &mut names);
                }
                let kind = if *k == VarKind::Let { BKind::Let } else { BKind::Const };
                out.extend(names.into_iter().map(|n| (n, kind)));
            }
            Stmt::Class(c) => {
                if let Some(n) = &c.name {
                    out.push((n.clone(), BKind::Class));
                }
            }
            Stmt::Function(f) if functions_too => {
                if let Some(n) = &f.name {
                    out.push((n.clone(), BKind::Function));
                }
            }
            Stmt::Labeled(_, inner) if functions_too => {
                if let Stmt::Function(f) = &**inner {
                    if let Some(n) = &f.name {
                        out.push((n.clone(), BKind::Function));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Early errors for a block's declarations: a lexical name declared twice
/// (two plain functions are allowed in sloppy mode), or also declared with
/// `var` anywhere inside the block.
fn check_redeclarations(stmts: &[Stmt], strict: bool) -> R<()> {
    let lexical = lexical_names(stmts, true);
    for (i, (n, k)) in lexical.iter().enumerate() {
        for (m, k2) in &lexical[..i] {
            if m == n {
                let plain_functions = *k == BKind::Function
                    && *k2 == BKind::Function
                    && !strict
                    && stmts.iter().all(|s| match s {
                        Stmt::Function(f) if f.name.as_ref() == Some(n) => !f.is_async && !f.is_generator,
                        _ => true,
                    });
                if !plain_functions {
                    return Err(err(&format!("Identifier '{n}' has already been declared")));
                }
            }
        }
    }
    let mut vars = Vec::new();
    let mut annex_b = Vec::new();
    var_names(stmts, &mut vars, &mut annex_b, true, strict);
    for v in &vars {
        if lexical.iter().any(|(n, _)| n == v) {
            return Err(err(&format!("Identifier '{v}' has already been declared")));
        }
    }
    Ok(())
}

fn hoisted_functions(stmts: &[Stmt]) -> Vec<&Rc<Function>> {
    let mut out = Vec::new();
    for s in stmts {
        match s {
            Stmt::Function(f) => out.push(f),
            Stmt::Labeled(_, inner) => {
                if let Stmt::Function(f) = &**inner {
                    out.push(f);
                }
            }
            _ => {}
        }
    }
    out
}

fn is_anonymous_fn(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => f.name.is_none(),
        Expr::Class(c) => c.name.is_none(),
        Expr::Paren(inner) => is_anonymous_fn(inner),
        _ => false,
    }
}

fn prop_name_str(p: &PropName) -> Option<JsStr> {
    match p {
        PropName::Ident(n) => Some(JsStr::from(&**n)),
        PropName::Str(s) => Some(s.clone()),
        PropName::Num(n) => Some(JsStr::from(numconv::to_string(*n))),
        _ => None,
    }
}

impl<'a> Compiler<'a> {
    fn f(&mut self) -> &mut FnState<'a> {
        self.fns.last_mut().unwrap()
    }

    fn fr(&self) -> &FnState<'a> {
        self.fns.last().unwrap()
    }

    fn emit(&mut self, op: Op) -> usize {
        let f = self.f();
        f.ops.push(op);
        f.ops.len() - 1
    }

    fn here(&self) -> u32 {
        self.fr().ops.len() as u32
    }

    fn patch(&mut self, at: usize, target: u32) {
        let op = &mut self.f().ops[at];
        *op = match *op {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(target),
            Op::JumpIfFalseKeep(_) => Op::JumpIfFalseKeep(target),
            Op::JumpIfTrueKeep(_) => Op::JumpIfTrueKeep(target),
            Op::JumpIfNotNullishKeep(_) => Op::JumpIfNotNullishKeep(target),
            Op::JumpIfNullish(_, d) => Op::JumpIfNullish(target, d),
            Op::JumpIfUndefined(_) => Op::JumpIfUndefined(target),
            Op::JumpIfNotUndefined(_) => Op::JumpIfNotUndefined(target),
            Op::TryStart(_, fin) => Op::TryStart(target, fin),
            Op::IterNext(_) => Op::IterNext(target),
            Op::IterResult(_) => Op::IterResult(target),
            Op::ForInNext(_) => Op::ForInNext(target),
            Op::YieldDelegate(_) => Op::YieldDelegate(target),
            other => panic!("not a jump: {other:?}"),
        };
    }

    fn patch_here(&mut self, at: usize) {
        let h = self.here();
        self.patch(at, h);
    }

    fn line(&mut self, line: u32) {
        if line != 0 && line != self.fr().last_line {
            let pc = self.here();
            let f = self.f();
            f.last_line = line;
            if f.lines.last().is_some_and(|(p, _)| *p == pc) {
                f.lines.last_mut().unwrap().1 = line;
            } else {
                f.lines.push((pc, line));
            }
        }
    }

    fn str_const(&mut self, s: JsStr) -> u32 {
        if let Some(&i) = self.fr().strings.get(&s) {
            return i;
        }
        let f = self.f();
        let i = f.consts.len() as u32;
        f.consts.push(Const::Str(s.clone()));
        f.strings.insert(s, i);
        i
    }

    fn name_const(&mut self, n: &str) -> u32 {
        self.str_const(JsStr::from(n))
    }

    fn num(&mut self, n: f64) {
        if n == libm::trunc(n) && n.abs() < 1e9 && !(n == 0.0 && n.is_sign_negative()) {
            self.emit(Op::Int(n as i32));
        } else {
            let f = self.f();
            let i = f.consts.len() as u32;
            f.consts.push(Const::Num(n));
            self.emit(Op::Const(i));
        }
    }

    fn string(&mut self, s: JsStr) {
        let i = self.str_const(s);
        self.emit(Op::Const(i));
    }

    fn new_local(&mut self) -> u32 {
        let f = self.f();
        f.n_locals += 1;
        f.n_locals - 1
    }

    fn captured(&self, name: &str) -> bool {
        let f = self.fr();
        f.capture_all || f.captured.contains(name)
    }

    // ------------------------------------------------------------ scopes

    /// Opens a scope with these bindings; emits PushScope when any of them
    /// is captured, and puts lexical bindings in their TDZ.
    fn enter_scope(&mut self, names: &[(Name, BKind)]) -> bool {
        let mut scope = CScope::default();
        let mut tdz_locals = Vec::new();
        for (n, k) in names {
            if scope.bindings.iter().any(|b| b.name == *n) {
                continue;
            }
            let loc = if self.captured(n) {
                scope.heap_size += 1;
                Loc::Heap(scope.heap_size - 1)
            } else {
                let slot = self.new_local();
                if k.lexical() {
                    tdz_locals.push(slot);
                }
                Loc::Local(slot)
            };
            scope.bindings.push(Binding { name: n.clone(), kind: *k, loc });
        }
        scope.heap = scope.heap_size > 0;
        let heap = scope.heap;
        if heap {
            self.emit(Op::PushScope(scope.heap_size));
            // Heap slots start in the TDZ; non-lexical ones become undefined.
            for b in &scope.bindings {
                if let (Loc::Heap(i), false) = (b.loc, b.kind.lexical()) {
                    self.f().ops.push(Op::Undef);
                    self.f().ops.push(Op::SetScope(0, i));
                    self.f().ops.push(Op::Pop);
                }
            }
        }
        for slot in tdz_locals {
            self.emit(Op::Empty);
            self.emit(Op::SetLocal(slot));
            self.emit(Op::Pop);
        }
        self.f().scopes.push(scope);
        if heap {
            self.f().cleanups.push(Cleanup::PopScope);
        }
        heap
    }

    fn exit_scope(&mut self) {
        let s = self.f().scopes.pop().unwrap();
        if s.heap {
            self.emit(Op::PopScope);
            self.f().cleanups.pop();
        }
    }

    fn resolve(&self, name: &str) -> R<Res> {
        self.resolve_skip(name, 0)
    }

    /// Resolves, ignoring the innermost `skip` scopes of the current function.
    fn resolve_skip(&self, name: &str, skip: usize) -> R<Res> {
        let mut depth: u16 = 0;
        let last = self.fns.len() - 1;
        for (fi, f) in self.fns.iter().enumerate().rev() {
            let n = f.scopes.len() - if fi == last { skip.min(f.scopes.len()) } else { 0 };
            for (si, s) in f.scopes[..n].iter().enumerate().rev() {
                if let Some(b) = s.find(name) {
                    return match b.loc {
                        Loc::Local(slot) if fi == last => Ok(Res::Local(slot, b.kind)),
                        Loc::Local(_) => Err(err(&format!("internal error: '{name}' was not captured"))),
                        Loc::Heap(i) => Ok(Res::Scope(depth, i, b.kind)),
                    };
                }
                if s.global && si == 0 {
                    return Ok(Res::Global);
                }
                if s.heap {
                    depth += 1;
                }
            }
        }
        Ok(Res::Global)
    }

    fn load(&mut self, name: &Name) -> R<()> {
        match self.resolve(name)? {
            Res::Local(slot, k) => {
                if k.lexical() {
                    let n = self.name_const(name);
                    self.emit(Op::GetLocalChecked(slot, n));
                } else {
                    self.emit(Op::GetLocal(slot));
                }
            }
            Res::Scope(d, i, k) => {
                if k.lexical() {
                    let n = self.name_const(name);
                    self.emit(Op::GetScopeChecked(d, i, n));
                } else {
                    self.emit(Op::GetScope(d, i));
                }
            }
            Res::Global => {
                if &**name == "undefined" {
                    self.emit(Op::Undef);
                } else {
                    let n = self.name_const(name);
                    self.emit(Op::GetGlobal(n));
                }
            }
        }
        Ok(())
    }

    /// Stores the top of the stack in `name`, leaving it there.
    fn store(&mut self, name: &Name, mode: Mode) -> R<()> {
        let strict = self.fr().strict;
        match self.resolve(name)? {
            Res::Local(slot, k) => {
                if mode != Mode::Init {
                    match k {
                        BKind::Const | BKind::Hidden => {
                            let n = self.name_const(name);
                            self.emit(Op::ThrowConst(n));
                            return Ok(());
                        }
                        BKind::Callee => {
                            if strict {
                                let n = self.name_const(name);
                                self.emit(Op::ThrowConst(n));
                            }
                            return Ok(());
                        }
                        k if k.lexical() => {
                            let n = self.name_const(name);
                            self.emit(Op::CheckLocal(slot, n));
                        }
                        _ => {}
                    }
                }
                self.emit(Op::SetLocal(slot));
            }
            Res::Scope(d, i, k) => {
                if mode != Mode::Init {
                    match k {
                        BKind::Const | BKind::Hidden => {
                            let n = self.name_const(name);
                            self.emit(Op::CheckScope(d, i, n));
                            self.emit(Op::ThrowConst(n));
                            return Ok(());
                        }
                        BKind::Callee => {
                            if strict {
                                let n = self.name_const(name);
                                self.emit(Op::ThrowConst(n));
                            }
                            return Ok(());
                        }
                        k if k.lexical() => {
                            let n = self.name_const(name);
                            self.emit(Op::CheckScope(d, i, n));
                        }
                        _ => {}
                    }
                }
                self.emit(Op::SetScope(d, i));
            }
            Res::Global => {
                let n = self.name_const(name);
                if mode == Mode::Init && self.fr().scopes.len() == 1 && self.fr().kind == CodeKind::Script {
                    self.emit(Op::InitLexical(n));
                } else {
                    self.emit(Op::SetGlobal(n));
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ functions

    fn new_fn(&mut self, kind: CodeKind, strict: bool, captured: BTreeSet<Name>, capture_all: bool) {
        self.fns.push(FnState {
            ops: Vec::new(),
            consts: Vec::new(),
            lines: Vec::new(),
            callees: Vec::new(),
            n_locals: 0,
            scopes: Vec::new(),
            targets: Vec::new(),
            cleanups: Vec::new(),
            strict,
            kind,
            is_async: false,
            is_generator: false,
            captured,
            capture_all,
            strings: BTreeMap::new(),
            opt_ends: Vec::new(),
            completion: None,
            last_line: 0,
        });
    }

    fn finish_fn(
        &mut self,
        name: JsStr,
        n_params: u32,
        length: u32,
        keep_args: bool,
        span: Option<(u32, u32)>,
    ) -> Rc<Code> {
        let f = self.fns.pop().unwrap();
        Rc::new(Code {
            name,
            ops: f.ops,
            consts: f.consts,
            n_locals: f.n_locals.max(n_params),
            n_params,
            length,
            kind: f.kind,
            strict: f.strict,
            is_async: f.is_async,
            is_generator: f.is_generator,
            keep_args,
            lines: f.lines,
            callees: f.callees,
            source: span.map(|(a, b)| (self.src.clone(), a, b)),
            file: self.file.clone(),
        })
    }

    fn script(&mut self, s: &'a Script) -> R<Rc<Code>> {
        self.new_fn(CodeKind::Script, s.strict, s.inner_refs.clone(), s.has_eval);
        self.f().scopes.push(CScope { global: true, ..CScope::default() });
        let completion = self.new_local();
        self.f().completion = Some(completion);
        // Hoisting: vars, functions, then lexical declarations.
        let mut vars = Vec::new();
        let mut annex_b = Vec::new();
        var_names(&s.body, &mut vars, &mut annex_b, false, s.strict);
        let lexical = lexical_names(&s.body, false);
        for (i, (n, _)) in lexical.iter().enumerate() {
            if lexical[..i].iter().any(|(m, _)| m == n) {
                return Err(err(&format!("Identifier '{n}' has already been declared")));
            }
        }
        for (n, _) in &lexical {
            if vars.contains(n) {
                return Err(err(&format!("Identifier '{n}' has already been declared")));
            }
        }
        for n in vars.iter().chain(annex_b.iter()) {
            let c = self.name_const(n);
            self.emit(Op::DeclareVar(c));
        }
        for (n, k) in &lexical {
            let c = self.name_const(n);
            self.emit(Op::DeclareLexical(c, *k == BKind::Const));
        }
        for f in hoisted_functions(&s.body) {
            let name = f.name.clone().unwrap();
            self.function(f, None)?;
            let c = self.name_const(&name);
            self.emit(Op::DeclareFunction(c));
        }
        self.statements(&s.body)?;
        self.emit(Op::GetLocal(completion));
        self.emit(Op::Return);
        Ok(self.finish_fn(JsStr::empty(), 0, 0, false, None))
    }

    /// Compiles a function and emits `Closure` for it.
    fn function(&mut self, f: &'a Rc<Function>, name_hint: Option<JsStr>) -> R<()> {
        let code = self.function_code(f, name_hint)?;
        let fs = self.f();
        let i = fs.consts.len() as u32;
        fs.consts.push(Const::Code(code));
        self.emit(Op::Closure(i));
        Ok(())
    }

    fn function_code(&mut self, f: &'a Function, name_hint: Option<JsStr>) -> R<Rc<Code>> {
        let kind = match f.kind {
            FnKind::Normal => CodeKind::Normal,
            FnKind::Arrow => CodeKind::Arrow,
            FnKind::Method => CodeKind::Method,
            FnKind::Getter => CodeKind::Getter,
            FnKind::Setter => CodeKind::Setter,
            FnKind::ClassConstructor => CodeKind::ClassConstructor,
            FnKind::DerivedConstructor => CodeKind::DerivedConstructor,
            FnKind::ClassInit => CodeKind::ClassInit,
        };
        self.new_fn(kind, f.strict, f.inner_refs.clone(), f.has_eval);
        self.f().is_async = f.is_async;
        self.f().is_generator = f.is_generator;
        self.line(f.line);
        let result = self.function_body(f);
        if let Err(e) = result {
            self.fns.pop();
            return Err(e);
        }
        let name = name_hint.or_else(|| f.name.as_ref().map(|n| JsStr::from(&**n))).unwrap_or_else(JsStr::empty);
        let simple = f.rest.is_none() && f.params.iter().all(|p| p.init.is_none() && matches!(p.pat, Pat::Ident(_)));
        let keep_args = f.rest.is_some() || (f.uses_arguments && f.kind != FnKind::Arrow) || !simple;
        Ok(self.finish_fn(name, f.params.len() as u32, f.length, keep_args, Some(f.span)))
    }

    fn function_body(&mut self, f: &'a Function) -> R<()> {
        let n_params = f.params.len() as u32;
        self.f().n_locals = n_params;
        let simple = f.rest.is_none() && f.params.iter().all(|p| p.init.is_none() && matches!(p.pat, Pat::Ident(_)));
        // The function scope: parameters, `arguments`, vars, functions, lexicals.
        let mut param_names = Vec::new();
        for p in &f.params {
            pattern_names(&p.pat, &mut param_names);
        }
        if let Some(r) = &f.rest {
            pattern_names(r, &mut param_names);
        }
        let mut vars = Vec::new();
        let mut annex_b = Vec::new();
        var_names(&f.body, &mut vars, &mut annex_b, false, f.strict);
        let lexical = lexical_names(&f.body, false);
        let functions = hoisted_functions(&f.body);
        for (i, (n, _)) in lexical.iter().enumerate() {
            if lexical[..i].iter().any(|(m, _)| m == n) {
                return Err(err(&format!("Identifier '{n}' has already been declared")));
            }
        }
        for (n, _) in &lexical {
            if vars.contains(n) || param_names.contains(n) || functions.iter().any(|g| g.name.as_ref() == Some(n)) {
                return Err(err(&format!("Identifier '{n}' has already been declared")));
            }
        }
        let mut scope = CScope::default();
        let add = |c: &mut Self, scope: &mut CScope, name: &Name, kind: BKind, slot: Option<u32>| {
            if let Some(b) = scope.bindings.iter_mut().find(|b| b.name == *name) {
                // A repeated parameter: the last one wins.
                if let (BKind::Param, Some(s), Loc::Local(_)) = (kind, slot, b.loc) {
                    b.loc = Loc::Local(s);
                }
                return;
            }
            let loc = if c.captured(name) {
                scope.heap_size += 1;
                Loc::Heap(scope.heap_size - 1)
            } else {
                Loc::Local(slot.unwrap_or_else(|| c.new_local()))
            };
            scope.bindings.push(Binding { name: name.clone(), kind, loc });
        };
        if simple {
            for (i, p) in f.params.iter().enumerate() {
                if let Pat::Ident(n) = &p.pat {
                    add(self, &mut scope, n, BKind::Param, Some(i as u32));
                }
            }
        } else {
            for n in &param_names {
                add(self, &mut scope, n, BKind::Param, None);
            }
        }
        let wants_arguments =
            f.uses_arguments && f.kind != FnKind::Arrow && !param_names.iter().any(|n| &**n == "arguments");
        if wants_arguments {
            add(self, &mut scope, &Name::from("arguments"), BKind::Var, None);
        }
        for n in vars.iter().chain(annex_b.iter()) {
            add(self, &mut scope, n, BKind::Var, None);
        }
        for g in &functions {
            add(self, &mut scope, g.name.as_ref().unwrap(), BKind::Function, None);
        }
        let mut tdz = Vec::new();
        for (n, k) in &lexical {
            add(self, &mut scope, n, *k, None);
            tdz.push(n.clone());
        }
        let callee_name = f.name.clone().filter(|n| f.is_expr && !scope.bindings.iter().any(|b| b.name == *n));
        if let Some(n) = &callee_name {
            add(self, &mut scope, n, BKind::Callee, None);
        }
        scope.heap = scope.heap_size > 0;
        if scope.heap {
            self.emit(Op::PushScope(scope.heap_size));
        }
        let bindings = scope.bindings.clone();
        self.f().scopes.push(scope);
        // Non-lexical heap slots start as undefined; captured simple params
        // are copied from their argument slots.
        for b in &bindings {
            match (b.loc, b.kind) {
                (Loc::Heap(i), BKind::Param) if simple => {
                    let idx =
                        f.params.iter().rposition(|p| matches!(&p.pat, Pat::Ident(n) if *n == b.name)).unwrap() as u32;
                    self.emit(Op::GetLocal(idx));
                    self.emit(Op::SetScope(0, i));
                    self.emit(Op::Pop);
                }
                (Loc::Heap(i), BKind::Var | BKind::Function | BKind::Param) => {
                    self.emit(Op::Undef);
                    self.emit(Op::SetScope(0, i));
                    self.emit(Op::Pop);
                }
                (Loc::Local(s), k) if k.lexical() => {
                    self.emit(Op::Empty);
                    self.emit(Op::SetLocal(s));
                    self.emit(Op::Pop);
                }
                _ => {}
            }
        }
        if let Some(n) = &callee_name {
            self.emit(Op::Callee);
            self.store(n, Mode::Init)?;
            self.emit(Op::Pop);
        }
        if wants_arguments {
            self.emit(Op::Arguments);
            self.store(&Name::from("arguments"), Mode::Init)?;
            self.emit(Op::Pop);
        }
        if !simple {
            for (i, p) in f.params.iter().enumerate() {
                self.emit(Op::GetLocal(i as u32));
                if let Some(init) = &p.init {
                    let j = self.emit(Op::JumpIfNotUndefined(0));
                    self.emit(Op::Pop);
                    self.expr_named(init, &p.pat)?;
                    self.patch_here(j);
                }
                self.bind(&p.pat, Mode::Init)?;
            }
            if let Some(r) = &f.rest {
                self.emit(Op::Rest(n_params));
                self.bind(r, Mode::Init)?;
            }
        } else if let Some(r) = &f.rest {
            self.emit(Op::Rest(n_params));
            self.bind(r, Mode::Init)?;
        }
        for g in functions {
            self.function(g, None)?;
            self.store(g.name.as_ref().unwrap(), Mode::Init)?;
            self.emit(Op::Pop);
        }
        if f.kind == FnKind::ClassConstructor {
            self.emit(Op::InitFields);
        }
        if f.is_generator {
            self.emit(Op::InitialYield);
        }
        self.statements(&f.body)?;
        if f.kind == FnKind::Arrow && f.body.len() == 1 && matches!(f.body[0], Stmt::Return(_)) {
            // Concise body: already returned.
        } else {
            self.emit(Op::Undef);
            self.emit(Op::Return);
        }
        Ok(())
    }

    // ------------------------------------------------------------ statements

    fn statements(&mut self, stmts: &'a [Stmt]) -> R<()> {
        for s in stmts {
            self.statement(s)?;
        }
        Ok(())
    }

    /// A statement list with its own block scope.
    fn block(&mut self, stmts: &'a [Stmt]) -> R<()> {
        check_redeclarations(stmts, self.fr().strict)?;
        let lexical = lexical_names(stmts, true);
        if lexical.is_empty() {
            return self.statements(stmts);
        }
        self.enter_scope(&lexical);
        self.hoist_block_functions(stmts)?;
        self.statements(stmts)?;
        self.exit_scope();
        Ok(())
    }

    fn hoist_block_functions(&mut self, stmts: &'a [Stmt]) -> R<()> {
        for g in hoisted_functions(stmts) {
            let name = g.name.as_ref().unwrap();
            self.function(g, None)?;
            self.store(name, Mode::Init)?;
            // Annex B: also assign the function-scoped var of the same name.
            if !self.fr().strict && !g.is_async && !g.is_generator {
                self.store_skip(name, 1)?;
            }
            self.emit(Op::Pop);
        }
        Ok(())
    }

    fn store_skip(&mut self, name: &Name, skip: usize) -> R<()> {
        match self.resolve_skip(name, skip)? {
            Res::Local(slot, k) if !k.lexical() => {
                self.emit(Op::SetLocal(slot));
            }
            Res::Scope(d, i, k) if !k.lexical() => {
                // Depth counted from the inner scope we skipped.
                let inner_heap = self.fr().scopes.last().is_some_and(|s| s.heap);
                self.emit(Op::SetScope(d + inner_heap as u16, i));
            }
            Res::Global => {
                let n = self.name_const(name);
                self.emit(Op::SetGlobal(n));
            }
            _ => {}
        }
        Ok(())
    }

    fn statement(&mut self, s: &'a Stmt) -> R<()> {
        match s {
            Stmt::Expr(e) => {
                self.expr(e)?;
                match self.fr().completion {
                    Some(c) => {
                        self.emit(Op::SetLocal(c));
                        self.emit(Op::Pop);
                    }
                    None => {
                        self.emit(Op::Pop);
                    }
                }
            }
            Stmt::Var(kind, decls) => {
                for d in decls {
                    match &d.init {
                        Some(init) => {
                            self.expr_named(init, &d.pat)?;
                            let mode = if *kind == VarKind::Var { Mode::Var } else { Mode::Init };
                            self.bind(&d.pat, mode)?;
                        }
                        None if *kind != VarKind::Var => {
                            self.emit(Op::Undef);
                            self.bind(&d.pat, Mode::Init)?;
                        }
                        None => {}
                    }
                }
            }
            Stmt::Function(_) => {} // hoisted
            Stmt::Class(c) => {
                self.class(c, None)?;
                self.store(c.name.as_ref().unwrap(), Mode::Init)?;
                self.emit(Op::Pop);
            }
            Stmt::Return(e) => {
                match e {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                if self.fr().is_async && self.fr().is_generator {
                    self.emit(Op::Await);
                }
                if self.fr().cleanups.is_empty() {
                    self.emit(Op::Return);
                } else {
                    let tmp = self.new_local();
                    self.emit(Op::SetLocal(tmp));
                    self.emit(Op::Pop);
                    self.emit_cleanups(0)?;
                    self.emit(Op::GetLocal(tmp));
                    self.emit(Op::Return);
                }
            }
            Stmt::If(test, cons, alt) => {
                self.expr(test)?;
                let j = self.emit(Op::JumpIfFalse(0));
                self.statement_scoped(cons)?;
                match alt {
                    Some(alt) => {
                        let k = self.emit(Op::Jump(0));
                        self.patch_here(j);
                        self.statement_scoped(alt)?;
                        self.patch_here(k);
                    }
                    None => self.patch_here(j),
                }
            }
            Stmt::Block(b) => self.block(b)?,
            Stmt::Empty | Stmt::Debugger => {}
            Stmt::Throw(e) => {
                self.expr(e)?;
                self.emit(Op::Throw);
            }
            Stmt::While(..) | Stmt::DoWhile(..) | Stmt::For { .. } | Stmt::ForIn { .. } | Stmt::ForOf { .. } => {
                self.loop_statement(s, Vec::new())?
            }
            Stmt::Labeled(..) => {
                let mut labels = Vec::new();
                let mut inner = s;
                while let Stmt::Labeled(l, body) = inner {
                    labels.push(l.clone());
                    inner = body;
                }
                match inner {
                    Stmt::While(..)
                    | Stmt::DoWhile(..)
                    | Stmt::For { .. }
                    | Stmt::ForIn { .. }
                    | Stmt::ForOf { .. } => self.loop_statement(inner, labels)?,
                    _ => {
                        let n = self.fr().cleanups.len();
                        self.f().targets.push(Target {
                            labels,
                            is_loop: false,
                            breaks: Vec::new(),
                            continues: Vec::new(),
                            break_cleanups: n,
                            continue_cleanups: n,
                        });
                        self.statement_scoped(inner)?;
                        let t = self.f().targets.pop().unwrap();
                        for b in t.breaks {
                            self.patch_here(b);
                        }
                    }
                }
            }
            Stmt::Break(label) => {
                let i = self.find_target(label.as_ref(), false)?;
                let n = self.fr().targets[i].break_cleanups;
                self.emit_cleanups(n)?;
                let j = self.emit(Op::Jump(0));
                self.f().targets[i].breaks.push(j);
            }
            Stmt::Continue(label) => {
                let i = self.find_target(label.as_ref(), true)?;
                let n = self.fr().targets[i].continue_cleanups;
                self.emit_cleanups(n)?;
                let j = self.emit(Op::Jump(0));
                self.f().targets[i].continues.push(j);
            }
            Stmt::Switch(disc, cases) => self.switch(disc, cases)?,
            Stmt::Try { block, param, handler, finalizer } => {
                self.try_statement(block, param.as_ref(), handler.as_deref(), finalizer.as_deref())?
            }
        }
        Ok(())
    }

    /// The body of if/else/labels: function declarations there get a scope.
    fn statement_scoped(&mut self, s: &'a Stmt) -> R<()> {
        if let Stmt::Function(_) = s {
            return self.block(core::slice::from_ref(s));
        }
        self.statement(s)
    }

    fn find_target(&self, label: Option<&Name>, is_continue: bool) -> R<usize> {
        let targets = &self.fr().targets;
        for (i, t) in targets.iter().enumerate().rev() {
            match label {
                Some(l) => {
                    if t.labels.contains(l) {
                        if is_continue && !t.is_loop {
                            return Err(err(&format!("'continue {l}' does not name a loop")));
                        }
                        return Ok(i);
                    }
                }
                None => {
                    if t.is_loop || (!is_continue && t.labels.is_empty()) {
                        return Ok(i);
                    }
                }
            }
        }
        Err(err(match (label, is_continue) {
            (Some(_), _) => "undefined label",
            (None, true) => "'continue' outside of a loop",
            (None, false) => "'break' outside of a loop or switch",
        }))
    }

    fn emit_cleanups(&mut self, down_to: usize) -> R<()> {
        let mut i = self.fr().cleanups.len();
        while i > down_to {
            i -= 1;
            match self.fr().cleanups[i] {
                Cleanup::PopScope => {
                    self.emit(Op::PopScope);
                }
                Cleanup::PopHandler => {
                    self.emit(Op::TryEnd);
                }
                Cleanup::IterClose => {
                    self.emit(Op::IterClose(false));
                }
                Cleanup::PopForIn => {
                    self.emit(Op::Pop);
                }
                Cleanup::Finally(body, n_scopes) => {
                    // Compile the finally body as it is seen from its try.
                    let saved_cleanups = self.f().cleanups.split_off(i);
                    let saved_scopes = self.f().scopes.split_off(n_scopes);
                    let saved_targets_len = self.fr().targets.len();
                    let r = self.block(body);
                    self.f().scopes.extend(saved_scopes);
                    self.f().cleanups.extend(saved_cleanups);
                    debug_assert_eq!(saved_targets_len, self.fr().targets.len());
                    r?;
                }
            }
        }
        Ok(())
    }

    fn push_target(&mut self, labels: Vec<Name>, is_loop: bool, break_cleanups: usize, continue_cleanups: usize) {
        self.f().targets.push(Target {
            labels,
            is_loop,
            breaks: Vec::new(),
            continues: Vec::new(),
            break_cleanups,
            continue_cleanups,
        });
    }

    fn pop_target(&mut self, break_to: u32, continue_to: u32) {
        let t = self.f().targets.pop().unwrap();
        for b in t.breaks {
            self.patch(b, break_to);
        }
        for c in t.continues {
            self.patch(c, continue_to);
        }
    }

    fn loop_statement(&mut self, s: &'a Stmt, labels: Vec<Name>) -> R<()> {
        match s {
            Stmt::While(test, body) => {
                let top = self.here();
                self.expr(test)?;
                let exit = self.emit(Op::JumpIfFalse(0));
                let n = self.fr().cleanups.len();
                self.push_target(labels, true, n, n);
                self.statement_scoped(body)?;
                self.emit(Op::Jump(top));
                self.patch_here(exit);
                let end = self.here();
                self.pop_target(end, top);
            }
            Stmt::DoWhile(body, test) => {
                let top = self.here();
                let n = self.fr().cleanups.len();
                self.push_target(labels, true, n, n);
                self.statement_scoped(body)?;
                let cont = self.here();
                self.expr(test)?;
                self.emit(Op::JumpIfTrue(top));
                let end = self.here();
                self.pop_target(end, cont);
            }
            Stmt::For { init, test, update, body } => {
                let mut scoped = false;
                let mut per_iteration = false;
                match init {
                    Some(ForInit::Var(kind, decls)) if *kind != VarKind::Var => {
                        let mut names = Vec::new();
                        for d in decls {
                            pattern_names(&d.pat, &mut names);
                        }
                        let k = if *kind == VarKind::Let { BKind::Let } else { BKind::Const };
                        let bindings: Vec<(Name, BKind)> = names.into_iter().map(|n| (n, k)).collect();
                        per_iteration = self.enter_scope(&bindings);
                        scoped = true;
                        self.statement_var(*kind, decls)?;
                        if per_iteration {
                            self.emit(Op::CopyScope);
                        }
                    }
                    Some(ForInit::Var(kind, decls)) => self.statement_var(*kind, decls)?,
                    Some(ForInit::Expr(e)) => {
                        self.expr(e)?;
                        self.emit(Op::Pop);
                    }
                    None => {}
                }
                let top = self.here();
                let exit = match test {
                    Some(t) => {
                        self.expr(t)?;
                        Some(self.emit(Op::JumpIfFalse(0)))
                    }
                    None => None,
                };
                let n = self.fr().cleanups.len();
                self.push_target(labels, true, n, n);
                self.statement_scoped(body)?;
                let cont = self.here();
                if per_iteration {
                    self.emit(Op::CopyScope);
                }
                if let Some(u) = update {
                    self.expr(u)?;
                    self.emit(Op::Pop);
                }
                self.emit(Op::Jump(top));
                if let Some(e) = exit {
                    self.patch_here(e);
                }
                let end = self.here();
                self.pop_target(end, cont);
                if scoped {
                    self.exit_scope();
                }
            }
            Stmt::ForIn { head, obj, body } => {
                // TDZ scope for the expression when the head is lexical.
                self.for_head_expr(head, obj)?;
                self.emit(Op::ForInStart);
                self.f().cleanups.push(Cleanup::PopForIn);
                let n = self.fr().cleanups.len();
                let top = self.here();
                let next = self.emit(Op::ForInNext(0));
                self.push_target(labels, true, n - 1, n);
                self.for_body(head, body)?;
                self.emit(Op::Jump(top));
                self.patch_here(next);
                self.emit(Op::Pop);
                self.f().cleanups.pop();
                let end = self.here();
                self.pop_target(end, top);
            }
            Stmt::ForOf { head, iter, body, is_await } => {
                self.for_head_expr(head, iter)?;
                self.emit(if *is_await { Op::GetAsyncIterator } else { Op::GetIterator });
                self.f().cleanups.push(Cleanup::IterClose);
                let tmp = self.new_local();
                let top = self.here();
                let next = if *is_await {
                    self.emit(Op::IterNextRaw);
                    self.emit(Op::Await);
                    self.emit(Op::IterResult(0))
                } else {
                    self.emit(Op::IterNext(0))
                };
                self.emit(Op::SetLocal(tmp));
                self.emit(Op::Pop);
                let handler = self.emit(Op::TryStart(0, false));
                self.f().cleanups.push(Cleanup::PopHandler);
                let n = self.fr().cleanups.len();
                self.push_target(labels, true, n - 2, n - 1);
                self.emit(Op::GetLocal(tmp));
                self.for_bind_and_body(head, body)?;
                self.emit(Op::TryEnd);
                let cont = self.here();
                self.emit(Op::Jump(top));
                // A throw from the body closes the iterator.
                self.patch_here(handler);
                self.emit(Op::Swap);
                self.emit(Op::IterClose(true));
                self.emit(Op::Throw);
                self.patch_here(next);
                self.emit(Op::IterDrop);
                self.f().cleanups.pop();
                self.f().cleanups.pop();
                let end = self.here();
                self.pop_target(end, cont);
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn statement_var(&mut self, kind: VarKind, decls: &'a [VarDecl]) -> R<()> {
        for d in decls {
            match &d.init {
                Some(init) => {
                    self.expr_named(init, &d.pat)?;
                    self.bind(&d.pat, if kind == VarKind::Var { Mode::Var } else { Mode::Init })?;
                }
                None if kind != VarKind::Var => {
                    self.emit(Op::Undef);
                    self.bind(&d.pat, Mode::Init)?;
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Evaluates the object of a for-in/of, with lexical head names in TDZ.
    fn for_head_expr(&mut self, head: &'a ForHead, e: &'a Expr) -> R<()> {
        if let ForHead::Var(kind @ (VarKind::Let | VarKind::Const), pat) = head {
            let mut names = Vec::new();
            pattern_names(pat, &mut names);
            let k = if *kind == VarKind::Let { BKind::Let } else { BKind::Const };
            let b: Vec<(Name, BKind)> = names.into_iter().map(|n| (n, k)).collect();
            self.enter_scope(&b);
            self.expr(e)?;
            self.exit_scope();
            return Ok(());
        }
        self.expr(e)
    }

    /// The value is on the stack: binds it to the head, then runs the body
    /// (in a fresh scope per iteration for lexical heads).
    fn for_bind_and_body(&mut self, head: &'a ForHead, body: &'a Stmt) -> R<()> {
        match head {
            ForHead::Var(kind @ (VarKind::Let | VarKind::Const), pat) => {
                let mut names = Vec::new();
                pattern_names(pat, &mut names);
                let k = if *kind == VarKind::Let { BKind::Let } else { BKind::Const };
                let b: Vec<(Name, BKind)> = names.into_iter().map(|n| (n, k)).collect();
                // The value is on the stack; entering the scope only pushes
                // the scope (its TDZ stores are balanced).
                self.enter_scope(&b);
                self.bind(pat, Mode::Init)?;
                self.statement_scoped(body)?;
                self.exit_scope();
            }
            ForHead::Var(VarKind::Var, pat) => {
                self.bind(pat, Mode::Var)?;
                self.statement_scoped(body)?;
            }
            ForHead::Pat(pat) => {
                self.bind(pat, Mode::Assign)?;
                self.statement_scoped(body)?;
            }
        }
        Ok(())
    }

    fn for_body(&mut self, head: &'a ForHead, body: &'a Stmt) -> R<()> {
        self.for_bind_and_body(head, body)
    }

    fn switch(&mut self, disc: &'a Expr, cases: &'a [Case]) -> R<()> {
        self.expr(disc)?;
        let tmp = self.new_local();
        self.emit(Op::SetLocal(tmp));
        self.emit(Op::Pop);
        let all: Vec<Stmt> = cases.iter().flat_map(|c| c.body.iter().cloned()).collect();
        check_redeclarations(&all, self.fr().strict)?;
        let mut lexical = Vec::new();
        for c in cases {
            lexical.extend(lexical_names(&c.body, true));
        }
        let scoped = !lexical.is_empty();
        if scoped {
            self.enter_scope(&lexical);
            for c in cases {
                self.hoist_block_functions(&c.body)?;
            }
        }
        let mut jumps = Vec::new();
        for c in cases {
            if let Some(t) = &c.test {
                self.emit(Op::GetLocal(tmp));
                self.expr(t)?;
                self.emit(Op::StrictEq);
                jumps.push(Some(self.emit(Op::JumpIfTrue(0))));
            } else {
                jumps.push(None);
            }
        }
        let to_default = self.emit(Op::Jump(0));
        let n = self.fr().cleanups.len();
        self.push_target(Vec::new(), false, n, n);
        let mut default_at = None;
        for (c, j) in cases.iter().zip(jumps) {
            match j {
                Some(j) => self.patch_here(j),
                None => default_at = Some(self.here()),
            }
            self.statements(&c.body)?;
        }
        let end = self.here();
        self.patch(to_default, default_at.unwrap_or(end));
        self.pop_target(end, end);
        if scoped {
            self.exit_scope();
        }
        Ok(())
    }

    fn try_statement(
        &mut self,
        block: &'a [Stmt],
        param: Option<&'a Pat>,
        handler: Option<&'a [Stmt]>,
        finalizer: Option<&'a [Stmt]>,
    ) -> R<()> {
        let mut fin_handler = None;
        if let Some(fin) = finalizer {
            let n_scopes = self.fr().scopes.len();
            self.f().cleanups.push(Cleanup::Finally(fin, n_scopes));
            fin_handler = Some(self.emit(Op::TryStart(0, true)));
            self.f().cleanups.push(Cleanup::PopHandler);
        }
        match handler {
            Some(h) => {
                let catch = self.emit(Op::TryStart(0, false));
                self.f().cleanups.push(Cleanup::PopHandler);
                self.block(block)?;
                self.f().cleanups.pop();
                self.emit(Op::TryEnd);
                let skip = self.emit(Op::Jump(0));
                self.patch_here(catch);
                match param {
                    Some(p) => {
                        let mut names = Vec::new();
                        pattern_names(p, &mut names);
                        let b: Vec<(Name, BKind)> = names.into_iter().map(|n| (n, BKind::Let)).collect();
                        self.enter_scope(&b);
                        self.bind(p, Mode::Init)?;
                        self.block(h)?;
                        self.exit_scope();
                    }
                    None => {
                        self.emit(Op::Pop);
                        self.block(h)?;
                    }
                }
                self.patch_here(skip);
            }
            None => self.block(block)?,
        }
        if let (Some(fin), Some(fh)) = (finalizer, fin_handler) {
            self.f().cleanups.pop();
            self.emit(Op::TryEnd);
            self.f().cleanups.pop();
            self.block(fin)?;
            let skip = self.emit(Op::Jump(0));
            self.patch_here(fh);
            let tmp = self.new_local();
            self.emit(Op::SetLocal(tmp));
            self.emit(Op::Pop);
            self.block(fin)?;
            self.emit(Op::GetLocal(tmp));
            self.emit(Op::Rethrow);
            self.patch_here(skip);
        }
        Ok(())
    }

    // ------------------------------------------------------------ patterns

    /// Binds the value on top of the stack to `pat`, consuming it.
    fn bind(&mut self, pat: &'a Pat, mode: Mode) -> R<()> {
        match pat {
            Pat::Ident(n) => {
                self.store(n, mode)?;
                self.emit(Op::Pop);
            }
            Pat::Expr(e) => match strip(e) {
                Expr::Member { obj, prop, line, .. } => {
                    self.line(*line);
                    self.expr(obj)?;
                    match prop {
                        MemberProp::Name(n) => {
                            self.emit(Op::Swap);
                            let c = self.name_const(n);
                            self.emit(Op::SetProp(c));
                        }
                        MemberProp::Computed(k) => {
                            // value obj key → obj key value
                            self.expr(k)?;
                            self.emit(Op::Rot3);
                            self.emit(Op::Rot3);
                            self.emit(Op::SetElem);
                        }
                        MemberProp::Private(n) => {
                            self.load_private(n)?;
                            self.emit(Op::Rot3);
                            self.emit(Op::Rot3);
                            self.emit(Op::SetPrivate);
                        }
                    }
                    self.emit(Op::Pop);
                }
                Expr::SuperMember(prop) => {
                    match prop {
                        MemberProp::Name(n) => {
                            let c = self.name_const(n);
                            self.emit(Op::SetSuper(c));
                        }
                        MemberProp::Computed(k) => {
                            self.expr(k)?;
                            self.emit(Op::Swap);
                            self.emit(Op::SetSuperElem);
                        }
                        MemberProp::Private(_) => return Err(err("unexpected private name")),
                    }
                    self.emit(Op::Pop);
                }
                Expr::Call { .. } => {
                    // `f() = x` (sloppy mode): a ReferenceError at runtime.
                    self.expr(e)?;
                    self.emit(Op::Pop);
                    let c = self.name_const("Invalid assignment target");
                    self.emit(Op::Const(c));
                    self.emit(Op::Throw);
                }
                _ => return Err(err("invalid assignment target")),
            },
            Pat::Object { props, rest } => {
                self.emit(Op::RequireCoercible);
                let mut keys = Vec::new();
                for p in props {
                    self.emit(Op::Dup);
                    match &p.key {
                        PropName::Computed(k) => {
                            self.expr(k)?;
                            self.emit(Op::ToPropertyKey);
                            if rest.is_some() {
                                let t = self.new_local();
                                self.emit(Op::SetLocal(t));
                                keys.push(Err(t));
                            }
                            self.emit(Op::GetElem);
                        }
                        PropName::Private(_) => return Err(err("unexpected private name")),
                        k => {
                            let s = prop_name_str(k).unwrap();
                            keys.push(Ok(s.clone()));
                            let c = self.str_const(s);
                            self.emit(Op::GetProp(c));
                        }
                    }
                    if let Some(init) = &p.init {
                        let j = self.emit(Op::JumpIfNotUndefined(0));
                        self.emit(Op::Pop);
                        self.expr_named(init, &p.value)?;
                        self.patch_here(j);
                    }
                    self.bind(&p.value, mode)?;
                }
                if let Some(r) = rest {
                    let n = keys.len() as u32;
                    for k in keys {
                        match k {
                            Ok(s) => self.string(s),
                            Err(t) => {
                                self.emit(Op::GetLocal(t));
                            }
                        }
                    }
                    self.emit(Op::CopyRest(n));
                    self.bind(r, mode)?;
                } else {
                    self.emit(Op::Pop);
                }
            }
            Pat::Array { elems, rest } => {
                self.emit(Op::GetIterator);
                for e in elems {
                    match e {
                        None => {
                            self.emit(Op::IterNextOrUndef);
                            self.emit(Op::Pop);
                        }
                        Some(el) => {
                            self.emit(Op::IterNextOrUndef);
                            if let Some(init) = &el.init {
                                let j = self.emit(Op::JumpIfNotUndefined(0));
                                self.emit(Op::Pop);
                                self.expr_named(init, &el.pat)?;
                                self.patch_here(j);
                            }
                            self.bind(&el.pat, mode)?;
                        }
                    }
                }
                if let Some(r) = rest {
                    self.emit(Op::IterRest);
                    self.bind(r, mode)?;
                }
                self.emit(Op::IterClose(false));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ expressions

    /// Compiles `e`, naming it after `target` if it's an anonymous function.
    fn expr_named(&mut self, e: &'a Expr, target: &Pat) -> R<()> {
        if let (Pat::Ident(n), true) = (target, is_anonymous_fn(e)) {
            return self.expr_with_name(e, JsStr::from(&**n));
        }
        self.expr(e)
    }

    fn expr_with_name(&mut self, e: &'a Expr, name: JsStr) -> R<()> {
        match e {
            Expr::Function(f) if f.name.is_none() => self.function(f, Some(name)),
            Expr::Class(c) if c.name.is_none() => self.class(c, Some(name)),
            Expr::Paren(inner) => self.expr_with_name(inner, name),
            e => self.expr(e),
        }
    }

    fn load_private(&mut self, n: &Name) -> R<()> {
        let key = Name::from(format!("#{n}"));
        match self.resolve(&key)? {
            Res::Global => Err(err(&format!("Private field '#{n}' must be declared in an enclosing class"))),
            _ => self.load(&key),
        }
    }

    fn expr(&mut self, e: &'a Expr) -> R<()> {
        match e {
            Expr::Num(n) => self.num(*n),
            Expr::Str(s) => self.string(s.clone()),
            Expr::Bool(true) => {
                self.emit(Op::True);
            }
            Expr::Bool(false) => {
                self.emit(Op::False);
            }
            Expr::Null => {
                self.emit(Op::Null);
            }
            Expr::Ident(n) => self.load(n)?,
            Expr::This => {
                self.emit(Op::This);
            }
            Expr::NewTarget => {
                self.emit(Op::NewTarget);
            }
            Expr::Paren(inner) => self.expr(inner)?,
            Expr::Template { quasis, exprs } => {
                let first = quasis[0].cooked.clone().unwrap_or_else(JsStr::empty);
                self.string(first);
                for (i, x) in exprs.iter().enumerate() {
                    self.expr(x)?;
                    self.emit(Op::ToString);
                    self.emit(Op::Add);
                    let q = quasis[i + 1].cooked.clone().unwrap_or_else(JsStr::empty);
                    if !q.is_empty() {
                        self.string(q);
                        self.emit(Op::Add);
                    }
                }
            }
            Expr::Tagged { tag, quasis, exprs, .. } => {
                self.callee(tag)?;
                let site = NEXT_SITE.fetch_add(1, Ordering::Relaxed);
                let data = TemplateData {
                    site,
                    cooked: quasis.iter().map(|q| q.cooked.clone()).collect(),
                    raw: quasis.iter().map(|q| q.raw.clone()).collect(),
                };
                let f = self.f();
                let i = f.consts.len() as u32;
                f.consts.push(Const::Template(Rc::new(data)));
                self.emit(Op::Template(i));
                for x in exprs {
                    self.expr(x)?;
                }
                self.emit(Op::Call(1 + exprs.len() as u32));
            }
            Expr::Regex { pattern, flags } => {
                // An invalid literal is an early error, even if never evaluated.
                if let Err(e) = crate::regexp::compile(pattern.units(), flags) {
                    return Err(err(&format!("Invalid regular expression: /{pattern}/{flags}: {e}")));
                }
                let p = self.str_const(pattern.clone());
                let f = self.str_const(JsStr::from(flags.as_str()));
                self.emit(Op::RegExp(p, f));
            }
            Expr::Array(elems) => {
                self.emit(Op::NewArray);
                for el in elems {
                    match el {
                        ArrayElem::Expr(x) => {
                            self.expr(x)?;
                            self.emit(Op::ArrayPush);
                        }
                        ArrayElem::Spread(x) => {
                            self.expr(x)?;
                            self.emit(Op::ArraySpread);
                        }
                        ArrayElem::Hole => {
                            self.emit(Op::ArrayHole);
                        }
                    }
                }
            }
            Expr::Object(props) => self.object(props)?,
            Expr::Function(f) => self.function(f, None)?,
            Expr::Class(c) => self.class(c, None)?,
            Expr::Unary(op, arg) => self.unary(*op, arg)?,
            Expr::Update { inc, prefix, target } => self.update(*inc, *prefix, target)?,
            Expr::Binary(op, a, b) => {
                self.expr(a)?;
                self.expr(b)?;
                self.emit(match op {
                    BinOp::Add => Op::Add,
                    BinOp::Sub => Op::Sub,
                    BinOp::Mul => Op::Mul,
                    BinOp::Div => Op::Div,
                    BinOp::Mod => Op::Mod,
                    BinOp::Exp => Op::Exp,
                    BinOp::Shl => Op::Shl,
                    BinOp::Shr => Op::Shr,
                    BinOp::UShr => Op::UShr,
                    BinOp::BitAnd => Op::BitAnd,
                    BinOp::BitOr => Op::BitOr,
                    BinOp::BitXor => Op::BitXor,
                    BinOp::Eq => Op::Eq,
                    BinOp::Ne => Op::Ne,
                    BinOp::StrictEq => Op::StrictEq,
                    BinOp::StrictNe => Op::StrictNe,
                    BinOp::Lt => Op::Lt,
                    BinOp::Le => Op::Le,
                    BinOp::Gt => Op::Gt,
                    BinOp::Ge => Op::Ge,
                    BinOp::In => Op::In,
                    BinOp::InstanceOf => Op::InstanceOf,
                });
            }
            Expr::Logic(op, a, b) => {
                self.expr(a)?;
                let j = self.emit(match op {
                    LogicOp::And => Op::JumpIfFalseKeep(0),
                    LogicOp::Or => Op::JumpIfTrueKeep(0),
                    LogicOp::Nullish => Op::JumpIfNotNullishKeep(0),
                });
                self.expr(b)?;
                self.patch_here(j);
            }
            Expr::Cond(t, a, b) => {
                self.expr(t)?;
                let j = self.emit(Op::JumpIfFalse(0));
                self.expr(a)?;
                let k = self.emit(Op::Jump(0));
                self.patch_here(j);
                self.expr(b)?;
                self.patch_here(k);
            }
            Expr::Assign { op, target, value } => self.assign(*op, target, value)?,
            Expr::Seq(list) => {
                for (i, x) in list.iter().enumerate() {
                    self.expr(x)?;
                    if i + 1 < list.len() {
                        self.emit(Op::Pop);
                    }
                }
            }
            Expr::Member { obj, prop, optional, line } => {
                self.expr(obj)?;
                self.line(*line);
                if *optional {
                    self.opt_jump(0);
                }
                self.member_get(prop)?;
            }
            Expr::SuperMember(prop) => match prop {
                MemberProp::Name(n) => {
                    let c = self.name_const(n);
                    self.emit(Op::GetSuper(c));
                }
                MemberProp::Computed(k) => {
                    self.expr(k)?;
                    self.emit(Op::GetSuperElem);
                }
                MemberProp::Private(_) => return Err(err("unexpected private name")),
            },
            Expr::OptChain(inner) => {
                self.f().opt_ends.push(Vec::new());
                let r = self.expr(inner);
                let ends = self.f().opt_ends.pop().unwrap();
                r?;
                for j in ends {
                    self.patch_here(j);
                }
            }
            Expr::Call { callee, args, optional, line, text } => {
                self.callee(callee)?;
                self.line(*line);
                if *optional {
                    self.opt_jump(1);
                }
                self.call_args(args, false)?;
                self.record_callee(text);
            }
            Expr::New { callee, args, line, text } => {
                self.expr(callee)?;
                self.line(*line);
                if args.iter().any(|a| matches!(a, Arg::Spread(_))) {
                    self.spread_array(args)?;
                    self.emit(Op::NewSpread);
                } else {
                    for a in args {
                        if let Arg::Expr(x) = a {
                            self.expr(x)?;
                        }
                    }
                    self.emit(Op::New(args.len() as u32));
                }
                self.record_callee(text);
            }
            Expr::SuperCall(args) => {
                if args.iter().any(|a| matches!(a, Arg::Spread(_))) {
                    self.spread_array(args)?;
                    self.emit(Op::SuperCallSpread);
                } else {
                    for a in args {
                        if let Arg::Expr(x) = a {
                            self.expr(x)?;
                        }
                    }
                    self.emit(Op::SuperCall(args.len() as u32));
                }
            }
            Expr::Yield { arg, delegate } => {
                match arg {
                    Some(a) => self.expr(a)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                if *delegate {
                    if self.fr().is_async {
                        self.emit(Op::GetAsyncIterator);
                    } else {
                        self.emit(Op::GetIterator);
                    }
                    self.emit(Op::Undef);
                    let top = self.here();
                    let j = self.emit(Op::YieldDelegate(0));
                    self.emit(Op::Jump(top));
                    self.patch_here(j);
                } else if self.fr().is_async {
                    // Async generators await the operand before yielding it.
                    self.emit(Op::Await);
                    self.emit(Op::AsyncYield);
                } else {
                    self.emit(Op::Yield);
                }
            }
            Expr::Await(a) => {
                self.expr(a)?;
                self.emit(Op::Await);
            }
            Expr::PrivateIn(n, obj) => {
                self.expr(obj)?;
                self.load_private(n)?;
                self.emit(Op::HasPrivate);
            }
        }
        Ok(())
    }

    /// Remembers the callee text for the call op just emitted.
    fn record_callee(&mut self, text: &str) {
        let pc = self.here() - 1;
        let c = self.name_const(text);
        self.f().callees.push((pc, c));
    }

    fn opt_jump(&mut self, depth: u8) {
        let j = self.emit(Op::JumpIfNullish(0, depth));
        match self.f().opt_ends.last_mut() {
            Some(list) => list.push(j),
            None => {
                // Not inside OptChain (shouldn't happen): fall through.
            }
        }
    }

    /// obj → value
    fn member_get(&mut self, prop: &'a MemberProp) -> R<()> {
        match prop {
            MemberProp::Name(n) => {
                let c = self.name_const(n);
                self.emit(Op::GetProp(c));
            }
            MemberProp::Computed(k) => {
                self.expr(k)?;
                self.emit(Op::GetElem);
            }
            MemberProp::Private(n) => {
                self.load_private(n)?;
                self.emit(Op::GetPrivate);
            }
        }
        Ok(())
    }

    /// Pushes `func this` for a call.
    fn callee(&mut self, callee: &'a Expr) -> R<()> {
        match strip(callee) {
            Expr::Member { obj, prop, optional, line } => {
                self.expr(obj)?;
                self.line(*line);
                if *optional {
                    self.opt_jump(0);
                }
                match prop {
                    MemberProp::Name(n) => {
                        let c = self.name_const(n);
                        self.emit(Op::GetMethod(c));
                    }
                    MemberProp::Computed(k) => {
                        self.expr(k)?;
                        self.emit(Op::GetMethodElem);
                    }
                    MemberProp::Private(n) => {
                        self.emit(Op::Dup);
                        self.load_private(n)?;
                        self.emit(Op::GetPrivate);
                        self.emit(Op::Swap);
                    }
                }
            }
            Expr::SuperMember(prop) => {
                match prop {
                    MemberProp::Name(n) => {
                        let c = self.name_const(n);
                        self.emit(Op::GetSuper(c));
                    }
                    MemberProp::Computed(k) => {
                        self.expr(k)?;
                        self.emit(Op::GetSuperElem);
                    }
                    MemberProp::Private(_) => return Err(err("unexpected private name")),
                }
                self.emit(Op::This);
            }
            Expr::OptChain(inner) if matches!(&**inner, Expr::Member { .. }) => {
                // `(a?.b)()`: the chain yields both function and receiver.
                self.f().opt_ends.push(Vec::new());
                let r = self.callee(inner);
                let ends = self.f().opt_ends.pop().unwrap();
                r?;
                // On short-circuit only `undefined` is on the stack; add a receiver.
                let skip = self.emit(Op::Jump(0));
                for j in ends {
                    self.patch_here(j);
                }
                self.emit(Op::Undef);
                self.patch_here(skip);
            }
            other => {
                self.expr(other)?;
                self.emit(Op::Undef);
            }
        }
        Ok(())
    }

    fn call_args(&mut self, args: &'a [Arg], _new: bool) -> R<()> {
        if args.iter().any(|a| matches!(a, Arg::Spread(_))) {
            self.spread_array(args)?;
            self.emit(Op::CallSpread);
        } else {
            for a in args {
                if let Arg::Expr(x) = a {
                    self.expr(x)?;
                }
            }
            self.emit(Op::Call(args.len() as u32));
        }
        Ok(())
    }

    fn spread_array(&mut self, args: &'a [Arg]) -> R<()> {
        self.emit(Op::NewArray);
        for a in args {
            match a {
                Arg::Expr(x) => {
                    self.expr(x)?;
                    self.emit(Op::ArrayPush);
                }
                Arg::Spread(x) => {
                    self.expr(x)?;
                    self.emit(Op::ArraySpread);
                }
            }
        }
        Ok(())
    }

    fn unary(&mut self, op: UnaryOp, arg: &'a Expr) -> R<()> {
        match op {
            UnaryOp::Typeof => {
                if let Expr::Ident(n) = strip(arg) {
                    if let Res::Global = self.resolve(n)? {
                        let c = self.name_const(n);
                        self.emit(Op::TypeofGlobal(c));
                        return Ok(());
                    }
                }
                self.expr(arg)?;
                self.emit(Op::Typeof);
            }
            UnaryOp::Delete => match strip(arg) {
                Expr::Member { obj, prop, optional, line } => {
                    self.expr(obj)?;
                    self.line(*line);
                    if *optional {
                        self.opt_jump(0);
                    }
                    match prop {
                        MemberProp::Name(n) => {
                            let c = self.name_const(n);
                            self.emit(Op::DeleteProp(c));
                        }
                        MemberProp::Computed(k) => {
                            self.expr(k)?;
                            self.emit(Op::DeleteElem);
                        }
                        MemberProp::Private(_) => return Err(err("private fields can't be deleted")),
                    }
                }
                Expr::OptChain(inner) => {
                    self.f().opt_ends.push(Vec::new());
                    let r = self.unary(UnaryOp::Delete, inner);
                    let ends = self.f().opt_ends.pop().unwrap();
                    r?;
                    let skip = self.emit(Op::Jump(0));
                    for j in ends {
                        self.patch_here(j);
                    }
                    // A short-circuited delete is true.
                    self.emit(Op::Pop);
                    self.emit(Op::True);
                    self.patch_here(skip);
                }
                Expr::Ident(n) => match self.resolve(n)? {
                    Res::Global => {
                        let g = self.name_const("globalThis");
                        self.emit(Op::GetGlobal(g));
                        let c = self.name_const(n);
                        self.emit(Op::DeleteProp(c));
                    }
                    _ => {
                        self.emit(Op::False);
                    }
                },
                Expr::SuperMember(_) => {
                    let c = self.name_const("Unsupported reference to 'super'");
                    self.emit(Op::Const(c));
                    self.emit(Op::Throw);
                }
                other => {
                    self.expr(other)?;
                    self.emit(Op::Pop);
                    self.emit(Op::True);
                }
            },
            UnaryOp::Void => {
                self.expr(arg)?;
                self.emit(Op::Pop);
                self.emit(Op::Undef);
            }
            _ => {
                self.expr(arg)?;
                self.emit(match op {
                    UnaryOp::Neg => Op::Neg,
                    UnaryOp::Plus => Op::Plus,
                    UnaryOp::Not => Op::Not,
                    UnaryOp::BitNot => Op::BitNot,
                    _ => unreachable!(),
                });
            }
        }
        Ok(())
    }

    fn update(&mut self, inc: bool, prefix: bool, target: &'a Expr) -> R<()> {
        let step = if inc { Op::Inc } else { Op::Dec };
        match strip(target) {
            Expr::Ident(n) => {
                self.load(n)?;
                self.emit(Op::ToNumeric);
                if prefix {
                    self.emit(step);
                    self.store(n, Mode::Assign)?;
                } else {
                    self.emit(Op::Dup);
                    self.emit(step);
                    self.store(n, Mode::Assign)?;
                    self.emit(Op::Pop);
                }
            }
            Expr::Member { obj, prop, line, .. } => {
                self.expr(obj)?;
                self.line(*line);
                match prop {
                    MemberProp::Name(n) => {
                        let c = self.name_const(n);
                        self.emit(Op::Dup);
                        self.emit(Op::GetProp(c));
                        self.emit(Op::ToNumeric);
                        if prefix {
                            self.emit(step);
                            self.emit(Op::SetProp(c));
                        } else {
                            self.emit(Op::Dup);
                            self.emit(Op::Rot3);
                            self.emit(step);
                            self.emit(Op::SetProp(c));
                            self.emit(Op::Pop);
                        }
                    }
                    MemberProp::Computed(k) => {
                        self.expr(k)?;
                        self.emit(Op::ToPropertyKey);
                        self.emit(Op::Dup2);
                        self.emit(Op::GetElem);
                        self.emit(Op::ToNumeric);
                        if prefix {
                            self.emit(step);
                            self.emit(Op::SetElem);
                        } else {
                            self.emit(Op::Dup);
                            self.emit(Op::Rot4);
                            self.emit(step);
                            self.emit(Op::SetElem);
                            self.emit(Op::Pop);
                        }
                    }
                    MemberProp::Private(n) => {
                        self.load_private(n)?;
                        self.emit(Op::Dup2);
                        self.emit(Op::GetPrivate);
                        self.emit(Op::ToNumeric);
                        if prefix {
                            self.emit(step);
                            self.emit(Op::SetPrivate);
                        } else {
                            self.emit(Op::Dup);
                            self.emit(Op::Rot4);
                            self.emit(step);
                            self.emit(Op::SetPrivate);
                            self.emit(Op::Pop);
                        }
                    }
                }
            }
            Expr::SuperMember(MemberProp::Name(n)) => {
                let c = self.name_const(n);
                self.emit(Op::GetSuper(c));
                self.emit(Op::ToNumeric);
                if prefix {
                    self.emit(step);
                    self.emit(Op::SetSuper(c));
                } else {
                    self.emit(Op::Dup);
                    self.emit(step);
                    self.emit(Op::SetSuper(c));
                    self.emit(Op::Pop);
                }
            }
            _ => return Err(err("invalid update target")),
        }
        Ok(())
    }

    fn assign(&mut self, op: AssignOp, target: &'a Pat, value: &'a Expr) -> R<()> {
        if op == AssignOp::Assign {
            match target {
                Pat::Ident(n) => {
                    self.expr_named(value, target)?;
                    self.store(n, Mode::Assign)?;
                }
                Pat::Expr(e) => match strip(e) {
                    Expr::Member { obj, prop, line, .. } => {
                        self.expr(obj)?;
                        match prop {
                            MemberProp::Name(n) => {
                                self.expr(value)?;
                                self.line(*line);
                                let c = self.name_const(n);
                                self.emit(Op::SetProp(c));
                            }
                            MemberProp::Computed(k) => {
                                self.expr(k)?;
                                self.emit(Op::ToPropertyKey);
                                self.expr(value)?;
                                self.line(*line);
                                self.emit(Op::SetElem);
                            }
                            MemberProp::Private(n) => {
                                self.load_private(n)?;
                                self.expr(value)?;
                                self.emit(Op::SetPrivate);
                            }
                        }
                    }
                    _ => {
                        self.expr(value)?;
                        self.emit(Op::Dup);
                        self.bind(target, Mode::Assign)?;
                    }
                },
                _ => {
                    self.expr(value)?;
                    self.emit(Op::Dup);
                    self.bind(target, Mode::Assign)?;
                }
            }
            return Ok(());
        }
        // Compound and logical assignment: identifier or member targets.
        let binop = |b: BinOp| match b {
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Div => Op::Div,
            BinOp::Mod => Op::Mod,
            BinOp::Exp => Op::Exp,
            BinOp::Shl => Op::Shl,
            BinOp::Shr => Op::Shr,
            BinOp::UShr => Op::UShr,
            BinOp::BitAnd => Op::BitAnd,
            BinOp::BitOr => Op::BitOr,
            BinOp::BitXor => Op::BitXor,
            _ => unreachable!(),
        };
        let logic_jump = |l: LogicOp| match l {
            LogicOp::And => Op::JumpIfFalseKeep(0),
            LogicOp::Or => Op::JumpIfTrueKeep(0),
            LogicOp::Nullish => Op::JumpIfNotNullishKeep(0),
        };
        match target {
            Pat::Ident(n) => {
                self.load(n)?;
                match op {
                    AssignOp::Bin(b) => {
                        self.expr(value)?;
                        self.emit(binop(b));
                        self.store(n, Mode::Assign)?;
                    }
                    AssignOp::Logic(l) => {
                        let j = self.emit(logic_jump(l));
                        self.expr_named(value, target)?;
                        self.store(n, Mode::Assign)?;
                        self.patch_here(j);
                    }
                    AssignOp::Assign => unreachable!(),
                }
            }
            Pat::Expr(e) => match strip(e) {
                Expr::Member { obj, prop, line, .. } => {
                    self.expr(obj)?;
                    self.line(*line);
                    // Bring the target (obj, or obj key) and current value up.
                    let (get, set, keys) = match prop {
                        MemberProp::Name(n) => {
                            let c = self.name_const(n);
                            self.emit(Op::Dup);
                            (Op::GetProp(c), Op::SetProp(c), 1)
                        }
                        MemberProp::Computed(k) => {
                            self.expr(k)?;
                            self.emit(Op::ToPropertyKey);
                            self.emit(Op::Dup2);
                            (Op::GetElem, Op::SetElem, 2)
                        }
                        MemberProp::Private(n) => {
                            self.load_private(n)?;
                            self.emit(Op::Dup2);
                            (Op::GetPrivate, Op::SetPrivate, 2)
                        }
                    };
                    self.emit(get);
                    match op {
                        AssignOp::Bin(b) => {
                            self.expr(value)?;
                            self.emit(binop(b));
                            self.emit(set);
                        }
                        AssignOp::Logic(l) => {
                            let j = self.emit(logic_jump(l));
                            self.expr(value)?;
                            self.emit(set);
                            let k = self.emit(Op::Jump(0));
                            self.patch_here(j);
                            // Short-circuited: drop the target, keep the value.
                            for _ in 0..keys {
                                self.emit(Op::Nip);
                            }
                            self.patch_here(k);
                        }
                        AssignOp::Assign => unreachable!(),
                    }
                }
                Expr::SuperMember(MemberProp::Name(n)) => {
                    let c = self.name_const(n);
                    self.emit(Op::GetSuper(c));
                    match op {
                        AssignOp::Bin(b) => {
                            self.expr(value)?;
                            self.emit(binop(b));
                            self.emit(Op::SetSuper(c));
                        }
                        AssignOp::Logic(l) => {
                            let j = self.emit(logic_jump(l));
                            self.expr(value)?;
                            self.emit(Op::SetSuper(c));
                            self.patch_here(j);
                        }
                        AssignOp::Assign => unreachable!(),
                    }
                }
                _ => return Err(err("invalid assignment target")),
            },
            _ => return Err(err("invalid assignment target")),
        }
        Ok(())
    }

    fn prop_key(&mut self, key: &'a PropName) -> R<()> {
        match key {
            PropName::Computed(k) => {
                self.expr(k)?;
                self.emit(Op::ToPropertyKey);
            }
            PropName::Private(n) => self.load_private(n)?,
            k => {
                let s = prop_name_str(k).unwrap();
                self.string(s);
            }
        }
        Ok(())
    }

    fn object(&mut self, props: &'a [ObjProp]) -> R<()> {
        self.emit(Op::NewObject);
        for p in props {
            match p {
                ObjProp::Spread(e) => {
                    self.expr(e)?;
                    self.emit(Op::CopyDataProps);
                }
                ObjProp::Prop { key, value, kind, method } => match kind {
                    PropKind::ShorthandInit(_) => return Err(err("invalid shorthand property initializer")),
                    PropKind::Get | PropKind::Set => {
                        self.prop_key(key)?;
                        self.expr(value)?;
                        let flag = if matches!(kind, PropKind::Get) { 1 } else { 2 };
                        self.emit(Op::DefineMethod(flag | 4 | 8 | 16));
                    }
                    _ if *method => {
                        self.prop_key(key)?;
                        self.expr(value)?;
                        self.emit(Op::DefineMethod(4 | 8 | 16));
                    }
                    _ => {
                        let is_proto = matches!(kind, PropKind::Init)
                            && match key {
                                PropName::Ident(n) => &**n == "__proto__",
                                PropName::Str(s) => s.eq_str("__proto__"),
                                _ => false,
                            };
                        if is_proto {
                            self.expr(value)?;
                            self.emit(Op::SetProtoLiteral);
                            continue;
                        }
                        match prop_name_str(key) {
                            Some(s) => {
                                if is_anonymous_fn(value) {
                                    self.expr_with_name(value, s.clone())?;
                                } else {
                                    self.expr(value)?;
                                }
                                let c = self.str_const(s);
                                self.emit(Op::DefineField(c));
                            }
                            None => {
                                self.prop_key(key)?;
                                self.expr(value)?;
                                if is_anonymous_fn(value) {
                                    self.emit(Op::SetFunctionName(0));
                                }
                                self.emit(Op::DefineElem);
                            }
                        }
                    }
                },
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ classes

    fn class(&mut self, c: &'a Class, name_hint: Option<JsStr>) -> R<()> {
        let saved_strict = self.fr().strict;
        self.f().strict = true;
        let r = self.class_inner(c, name_hint);
        self.f().strict = saved_strict;
        r
    }

    fn class_inner(&mut self, c: &'a Class, name_hint: Option<JsStr>) -> R<()> {
        let mut bindings: Vec<(Name, BKind)> = Vec::new();
        if let Some(n) = &c.name {
            bindings.push((n.clone(), BKind::Hidden));
        }
        let mut privates: Vec<Name> = Vec::new();
        for m in &c.members {
            if let PropName::Private(n) = &m.key {
                let key = Name::from(format!("#{n}"));
                if !privates.contains(&key) {
                    privates.push(key.clone());
                    bindings.push((key, BKind::Hidden));
                }
            }
        }
        self.enter_scope(&bindings);
        for p in &privates {
            let d = self.name_const(p);
            self.emit(Op::PrivateName(d));
            self.store(p, Mode::Init)?;
            self.emit(Op::Pop);
        }
        if let Some(h) = &c.extends {
            self.expr(h)?;
        }
        let name = c.name.as_ref().map(|n| JsStr::from(&**n)).or(name_hint).unwrap_or_else(JsStr::empty);
        let code = self.function_code(c.constructor.as_ref().unwrap(), Some(name))?;
        let fs = self.f();
        let ci = fs.consts.len() as u32;
        fs.consts.push(Const::Code(code));
        self.emit(Op::Class(ci, c.extends.is_some()));
        // Stack: ctor proto.
        let mut deferred: Vec<(&'a ClassMember, Option<u32>)> = Vec::new();
        for m in &c.members {
            match m.kind {
                ClassMemberKind::Method | ClassMemberKind::Getter | ClassMemberKind::Setter => {
                    let f = m.value.as_ref().unwrap();
                    let accessor = match m.kind {
                        ClassMemberKind::Getter => 1,
                        ClassMemberKind::Setter => 2,
                        _ => 0,
                    };
                    if let (PropName::Private(_), false) = (&m.key, m.is_static) {
                        // Instance private methods are installed on each instance.
                        self.emit(Op::Pick(1));
                        self.prop_key(&m.key)?;
                        self.function(f, None)?;
                        self.emit(Op::AddField(1 | accessor << 1));
                        self.emit(Op::Pop);
                        continue;
                    }
                    self.emit(Op::Pick(if m.is_static { 1 } else { 0 }));
                    self.prop_key(&m.key)?;
                    self.function(f, None)?;
                    self.emit(Op::DefineMethod(accessor | 8 | 16));
                    self.emit(Op::Pop);
                }
                ClassMemberKind::Field if !m.is_static => {
                    self.emit(Op::Pick(1));
                    self.prop_key(&m.key)?;
                    match &m.value {
                        Some(f) => self.function(f, None)?,
                        None => {
                            self.emit(Op::Undef);
                        }
                    }
                    self.emit(Op::AddField(0));
                    self.emit(Op::Pop);
                }
                ClassMemberKind::Field => {
                    // Static: evaluate a computed key now, initialise later.
                    let slot = if let PropName::Computed(_) = &m.key {
                        self.prop_key(&m.key)?;
                        let t = self.new_local();
                        self.emit(Op::SetLocal(t));
                        self.emit(Op::Pop);
                        Some(t)
                    } else {
                        None
                    };
                    deferred.push((m, slot));
                }
                ClassMemberKind::StaticBlock => deferred.push((m, None)),
            }
        }
        self.emit(Op::Pop);
        if let Some(n) = &c.name {
            self.store(n, Mode::Init)?;
        }
        for (m, slot) in deferred {
            match m.kind {
                ClassMemberKind::StaticBlock => {
                    self.function(m.value.as_ref().unwrap(), None)?;
                    self.emit(Op::Pick(1));
                    self.emit(Op::Call(0));
                    self.emit(Op::Pop);
                }
                _ => {
                    self.emit(Op::Dup);
                    match slot {
                        Some(t) => {
                            self.emit(Op::GetLocal(t));
                        }
                        None => self.prop_key(&m.key)?,
                    }
                    match &m.value {
                        Some(f) => {
                            self.function(f, None)?;
                            self.emit(Op::Pick(2));
                            self.emit(Op::Call(0));
                            if m.value
                                .as_ref()
                                .is_some_and(|f| matches!(&f.body[..], [Stmt::Return(Some(e))] if is_anonymous_fn(e)))
                            {
                                self.emit(Op::SetFunctionName(0));
                            }
                        }
                        None => {
                            self.emit(Op::Undef);
                        }
                    }
                    self.emit(Op::DefineElem);
                    self.emit(Op::Pop);
                }
            }
        }
        self.exit_scope();
        Ok(())
    }
}

fn strip(e: &Expr) -> &Expr {
    match e {
        Expr::Paren(inner) => strip(inner),
        e => e,
    }
}
