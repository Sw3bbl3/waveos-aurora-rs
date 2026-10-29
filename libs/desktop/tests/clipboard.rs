//! Exercise the kernel clipboard implementation used by Files and Desktop.
extern crate alloc;
mod sync {
    pub struct Mutex<T>(std::sync::Mutex<T>);
    impl<T> Mutex<T> {
        pub const fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap()
        }
    }
}
#[path = "../../../kernel/src/gui/clipboard.rs"]
mod clipboard;
#[test]
fn cross_window_cut_compatibility_and_confirmed_moves() {
    use aurora_abi::clip::*;
    let mut out = [0; 100];
    clipboard::set(CUT_FILES, b"/Desktop/a\n/Desktop/b").unwrap();
    for kind in [TEXT, FILES, CUT_FILES] {
        let n = clipboard::get(kind, &mut out).unwrap();
        assert_eq!(&out[..n], b"/Desktop/a\n/Desktop/b");
    }
    // Failed/cancelled transfers have no confirmed moves.
    clipboard::set(COMPLETE_MOVES, b"").unwrap();
    assert_eq!(clipboard::get(FILES, &mut out).unwrap(), 21);
    clipboard::set(COMPLETE_MOVES, b"/Desktop/a").unwrap();
    let n = clipboard::get(FILES, &mut out).unwrap();
    assert_eq!(&out[..n], b"/Desktop/b");
    clipboard::set(FILES, b"/Desktop/b").unwrap();
    clipboard::set(COMPLETE_MOVES, b"/Desktop/b").unwrap();
    assert_eq!(clipboard::get(FILES, &mut out).unwrap(), 10);
    assert!(clipboard::get(CUT_FILES, &mut out).is_err());
    clipboard::set(TEXT, b"a new clipboard").unwrap();
    clipboard::set(COMPLETE_MOVES, b"/Desktop/b").unwrap();
    let n = clipboard::get(TEXT, &mut out).unwrap();
    assert_eq!(&out[..n], b"a new clipboard");
}
