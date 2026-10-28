//! Every setting, by name and synonyms, for the search field and for
//! deep links from Seek (`Settings.elf keyboard/repeat-rate`).

use super::Pane;

#[derive(Clone, Copy)]
pub struct Setting {
    /// The deep link, and the `.id()` of the control it points at.
    pub id: &'static str,
    pub label: &'static str,
    pub keywords: &'static str,
    pub pane: Pane,
}

macro_rules! item {
    ($id:literal, $label:literal, $keys:literal, $pane:ident) => {
        Setting { id: $id, label: $label, keywords: $keys, pane: Pane::$pane }
    };
}

pub const SETTINGS: &[Setting] = &[
    item!("appearance/light", "Light appearance", "theme bright", Appearance),
    item!("appearance/dark", "Dark appearance", "theme night", Appearance),
    item!("appearance/accent", "Accent colour", "color highlight", Appearance),
    item!("appearance/wallpaper", "Wallpaper", "background picture", Appearance),
    item!("display/resolution", "Screen resolution", "display size pixels", Display),
    item!("keyboard/layout", "Keyboard layout", "language typing input", Keyboard),
    item!("keyboard/repeat-delay", "Key repeat delay", "keyboard typing wait", Keyboard),
    item!("keyboard/repeat-rate", "Key repeat speed", "keyboard typing rate", Keyboard),
    item!("keyboard/test", "Test your keyboard", "typing try layout", Keyboard),
    item!("trackpad/tap-to-click", "Tap to click", "touchpad trackpad tap click", Trackpad),
    item!("trackpad/natural-scrolling", "Natural scrolling", "touchpad trackpad scroll direction", Trackpad),
    item!("trackpad/tracking-speed", "Tracking speed", "touchpad trackpad pointer speed", Trackpad),
    item!("mouse/pointer-speed", "Pointer speed", "mouse cursor speed sensitivity", Mouse),
    item!("controllers/list", "Game controllers", "gamepad joystick xbox playstation switch", Controllers),
    item!("network/wifi", "Wi-Fi", "wireless network internet join", Network),
    item!("datetime/24-hour", "24-hour clock", "time format", DateTime),
    item!("datetime/seconds", "Show seconds", "clock time menu bar", DateTime),
    item!("datetime/date", "Show date", "clock menu bar", DateTime),
    item!("datetime/set-clock", "Set date and time", "clock calendar", DateTime),
    item!("datetime/time-zone", "Time zone", "timezone utc offset", DateTime),
    item!("sound/volume", "Output volume", "audio speaker loudness", Sound),
    item!("sound/mute", "Mute sound", "audio silent speaker", Sound),
    item!("notifications/do-not-disturb", "Calm", "do not disturb notifications silence focus", Notifications),
    item!("bar/crest", "Show Crest", "clock time top bar notch", Bar),
    item!("bar/battery", "Show battery level", "percent power status ring", Bar),
    item!("login/auto", "Log in automatically", "password account startup", Login),
    item!("desktop/dock-size", "Harbor size", "dock desktop icons magnification", Desktop),
    item!("desktop/dock-autohide", "Hide Harbor until pointed at", "dock autohide desktop space", Desktop),
    item!("desktop/glass", "Liquid glass intensity", "desktop blur transparency lumen", Desktop),
    item!("accessibility/transparency", "Reduce transparency", "solid surfaces glass blur", Accessibility),
    item!("accessibility/motion", "Reduce motion", "animations accessibility", Accessibility),
    item!("accessibility/contrast", "Increase contrast", "accessibility visibility text", Accessibility),
    item!("about/info", "System information", "version processor memory storage", About),
];

/// The settings matching every word of `query`.
pub fn matches(query: &str) -> alloc::vec::Vec<usize> {
    let query = query.to_ascii_lowercase();
    SETTINGS
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            let text = alloc::format!("{} {} {}", s.id, s.label, s.keywords).to_ascii_lowercase();
            query.split_whitespace().all(|word| text.contains(word))
        })
        .map(|(i, _)| i)
        .collect()
}
