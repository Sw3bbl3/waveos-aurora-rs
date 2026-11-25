//! Sleep where the architecture has no suspend-to-RAM path yet.

/// Whether this machine can sleep: not here.
pub fn available() -> bool {
    false
}

/// Never sleeps.
pub fn suspend() -> Result<(), &'static str> {
    Err("sleep is not supported on this processor architecture yet")
}
