//! Web client building blocks shared by `fetch` and the Surf browser: URLs
//! and HTTP/1.1. No system calls: callers supply the connections, so this
//! also runs (and is tested) on the host.

#![no_std]

extern crate alloc;

pub mod http;
pub mod url;

pub use url::Url;
