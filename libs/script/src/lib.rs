//! Pulsar, the JavaScript engine of the Nebula web browser.
//!
//! A `no_std` engine: source is parsed to a syntax tree, compiled to
//! bytecode and run on a stack machine with a mark-and-sweep heap. The
//! embedder supplies time and console output through [`Host`], and adds
//! its own objects (the DOM, timers) as native functions.
//!
//! ```ignore
//! let mut realm = Realm::new(Box::new(MyHost));
//! let v = realm.eval("[1, 2, 3].map(x => x * 2).join()", "example.js")?;
//! realm.run_jobs(); // promise callbacks
//! ```

#![no_std]

extern crate alloc;

pub mod ast;
pub mod builtins;
pub mod bytecode;
pub mod compiler;
pub mod heap;
pub mod lexer;
pub mod numconv;
pub mod object;
pub mod parser;
pub mod realm;
pub mod regexp;
pub mod value;
pub mod vm;

pub use builtins::console::inspect;
pub use heap::ObjRef;
pub use object::{NativeFn, DEFAULT, HIDDEN};
pub use realm::{Call, Embedder, ErrorKind, Host, JsResult, PropDesc, Realm, Root};
pub use value::{JsStr, PropKey, Sym, Value};
