//! The syntax tree.

use crate::lexer::Name;
use crate::value::JsStr;
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

pub type P<T> = Box<T>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    InstanceOf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogicOp {
    And,
    Or,
    Nullish,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssignOp {
    Assign,
    Bin(BinOp),
    Logic(LogicOp),
}

#[derive(Debug, Clone)]
pub enum PropName {
    Ident(Name),
    Str(JsStr),
    Num(f64),
    Computed(P<Expr>),
    Private(Name),
}

#[derive(Debug, Clone)]
pub enum MemberProp {
    Name(Name),
    Computed(P<Expr>),
    Private(Name),
}

#[derive(Debug, Clone)]
pub enum ArrayElem {
    Expr(Expr),
    Spread(Expr),
    Hole,
}

#[derive(Debug, Clone)]
pub enum Arg {
    Expr(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum PropKind {
    Init,
    Get,
    Set,
    /// `{ a }`.
    Shorthand,
    /// `{ a = 1 }`: only valid once the object becomes a pattern.
    ShorthandInit(P<Expr>),
}

#[derive(Debug, Clone)]
pub enum ObjProp {
    Prop { key: PropName, value: Expr, kind: PropKind, method: bool },
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub struct TemplatePart {
    pub cooked: Option<JsStr>,
    pub raw: JsStr,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Str(JsStr),
    Bool(bool),
    Null,
    Ident(Name),
    This,
    Template {
        quasis: Vec<TemplatePart>,
        exprs: Vec<Expr>,
    },
    /// `site`: a unique id, so each call site gets one frozen strings array.
    Tagged {
        tag: P<Expr>,
        quasis: Vec<TemplatePart>,
        exprs: Vec<Expr>,
        site: u32,
    },
    Regex {
        pattern: JsStr,
        flags: String,
    },
    Array(Vec<ArrayElem>),
    Object(Vec<ObjProp>),
    Function(Rc<Function>),
    Class(Rc<Class>),
    Unary(UnaryOp, P<Expr>),
    Update {
        inc: bool,
        prefix: bool,
        target: P<Expr>,
    },
    Binary(BinOp, P<Expr>, P<Expr>),
    Logic(LogicOp, P<Expr>, P<Expr>),
    Assign {
        op: AssignOp,
        target: P<Pat>,
        value: P<Expr>,
    },
    Cond(P<Expr>, P<Expr>, P<Expr>),
    /// `optional`: `f?.()`.
    /// `text`: the callee's source, for "x.y is not a function" errors.
    Call {
        callee: P<Expr>,
        args: Vec<Arg>,
        optional: bool,
        line: u32,
        text: Rc<str>,
    },
    New {
        callee: P<Expr>,
        args: Vec<Arg>,
        line: u32,
        text: Rc<str>,
    },
    /// `optional`: `a?.b`.
    Member {
        obj: P<Expr>,
        prop: MemberProp,
        optional: bool,
        line: u32,
    },
    /// The end of an optional chain: short-circuits to `undefined` here.
    OptChain(P<Expr>),
    SuperMember(MemberProp),
    SuperCall(Vec<Arg>),
    NewTarget,
    Seq(Vec<Expr>),
    Yield {
        arg: Option<P<Expr>>,
        delegate: bool,
    },
    Await(P<Expr>),
    /// `#x in obj`.
    PrivateIn(Name, P<Expr>),
    /// Parenthesised, kept so `(a) = 1` is allowed but `({a}) = 1` is not.
    Paren(P<Expr>),
}

#[derive(Debug, Clone)]
pub enum Pat {
    Ident(Name),
    /// A member expression target (assignment patterns only).
    Expr(P<Expr>),
    Object {
        props: Vec<PatProp>,
        rest: Option<P<Pat>>,
    },
    Array {
        elems: Vec<Option<PatElem>>,
        rest: Option<P<Pat>>,
    },
}

#[derive(Debug, Clone)]
pub struct PatProp {
    pub key: PropName,
    pub value: Pat,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct PatElem {
    pub pat: Pat,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub pat: Pat,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub enum ForInit {
    Var(VarKind, Vec<VarDecl>),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum ForHead {
    Var(VarKind, Pat),
    Pat(Pat),
}

#[derive(Debug, Clone)]
pub struct Case {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    Var(VarKind, Vec<VarDecl>),
    Function(Rc<Function>),
    Class(Rc<Class>),
    Return(Option<Expr>),
    If(Expr, P<Stmt>, Option<P<Stmt>>),
    For { init: Option<ForInit>, test: Option<Expr>, update: Option<Expr>, body: P<Stmt> },
    ForIn { head: ForHead, obj: Expr, body: P<Stmt> },
    ForOf { head: ForHead, iter: Expr, body: P<Stmt>, is_await: bool },
    While(Expr, P<Stmt>),
    DoWhile(P<Stmt>, Expr),
    Break(Option<Name>),
    Continue(Option<Name>),
    Throw(Expr),
    Try { block: Vec<Stmt>, param: Option<Pat>, handler: Option<Vec<Stmt>>, finalizer: Option<Vec<Stmt>> },
    Switch(Expr, Vec<Case>),
    Labeled(Name, P<Stmt>),
    Block(Vec<Stmt>),
    Empty,
    Debugger,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FnKind {
    Normal,
    Arrow,
    Method,
    Getter,
    Setter,
    ClassConstructor,
    DerivedConstructor,
    /// Class field initialisers and static blocks, run with the class or
    /// instance as `this`.
    ClassInit,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub pat: Pat,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: Option<Name>,
    pub params: Vec<Param>,
    pub rest: Option<Pat>,
    pub body: Vec<Stmt>,
    pub kind: FnKind,
    pub is_async: bool,
    pub is_generator: bool,
    pub strict: bool,
    /// A named function expression (its name is bound inside it).
    pub is_expr: bool,
    /// Names referenced anywhere inside nested functions: bindings with
    /// these names must live in heap scopes so closures can see them.
    pub inner_refs: BTreeSet<Name>,
    /// Calls `eval(...)` directly: every binding is captured.
    pub has_eval: bool,
    /// Uses `arguments` (itself or through arrows).
    pub uses_arguments: bool,
    /// Byte range in the source, for Function.prototype.toString.
    pub span: (u32, u32),
    /// `length`: parameters before the first default or rest.
    pub length: u32,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum ClassMemberKind {
    Method,
    Getter,
    Setter,
    Field,
    StaticBlock,
}

#[derive(Debug, Clone)]
pub struct ClassMember {
    pub key: PropName,
    pub kind: ClassMemberKind,
    pub is_static: bool,
    /// Methods, accessors and static blocks: the function. Fields: their
    /// initialiser wrapped as a ClassInit arrow-like function (or None).
    pub value: Option<Rc<Function>>,
}

#[derive(Debug, Clone)]
pub struct Class {
    pub name: Option<Name>,
    pub extends: Option<P<Expr>>,
    /// Always present: the parser supplies the default constructor.
    pub constructor: Option<Rc<Function>>,
    pub members: Vec<ClassMember>,
    pub span: (u32, u32),
}

#[derive(Debug, Clone)]
pub struct Script {
    pub body: Vec<Stmt>,
    pub strict: bool,
    pub inner_refs: BTreeSet<Name>,
    pub has_eval: bool,
}
