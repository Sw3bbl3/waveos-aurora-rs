//! libaurora — the WaveOS Aurora user-space runtime.
//!
//! Provides the program entry point, a heap, `print!`, files, processes and
//! raw system calls. A program looks like:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! aurora::entry!(main);
//! fn main(args: aurora::Args) -> i32 { aurora::println!("hi"); 0 }
//! ```

#![no_std]

extern crate alloc;

pub mod audio;
pub mod clipboard;
pub mod dnd;
pub mod fs;
pub mod heap;
pub mod io;
pub mod notify;
pub mod prefs;
pub mod process;
pub mod rt;
pub mod sync;
pub mod sys;
pub mod thread;
pub mod time;

pub use aurora_abi as abi;
pub use rt::Args;

/// An error returned by a system call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error(pub isize);

impl Error {
    pub fn is(&self, e: isize) -> bool {
        self.0.abs() == e
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str(abi::err::name(self.0))
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Declares the program's `main(args) -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        #[link_section = ".text._start"]
        pub extern "C" fn _start(block: *const u64) -> ! {
            $crate::rt::start(block, $main)
        }
    };
}
