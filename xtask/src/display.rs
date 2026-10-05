//! Host keyboard routing. Guest key mappings are deliberately unchanged.
pub fn backend(macos: bool, headless: bool, capture: bool) -> Option<&'static str> {
    if headless {
        Some("none")
    } else if macos && capture {
        Some("cocoa,full-grab=on,left-command-key=on,swap-opt-cmd=off")
    } else if macos {
        Some("cocoa,full-grab=off,left-command-key=on,swap-opt-cmd=off")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_is_host_specific_and_headless_wins() {
        assert_eq!(backend(true, true, true), Some("none"));
        assert_eq!(backend(false, false, true), None);
        assert!(backend(true, false, true).unwrap().contains("full-grab=on"));
        assert!(backend(true, false, false).unwrap().contains("full-grab=off"));
    }
}
