use super::*;
#[derive(Clone, Copy)]
pub struct Setting {
    pub id: &'static str,
    pub label: &'static str,
    pub keywords: &'static str,
    pub pane: Pane,
    pub hit: Hit,
    pub control: Option<u64>,
}
macro_rules! item {
    ($id:literal,$label:literal,$keys:literal,$pane:ident,$hit:expr) => {
        Setting { id: $id, label: $label, keywords: $keys, pane: Pane::$pane, hit: $hit, control: None }
    };
    ($id:literal,$label:literal,$keys:literal,$pane:ident,$hit:expr,$control:literal) => {
        Setting { id: $id, label: $label, keywords: $keys, pane: Pane::$pane, hit: $hit, control: Some($control) }
    };
}
pub const SETTINGS: &[Setting] = &[
    item!("appearance/light", "Light appearance", "theme bright", Appearance, Hit::Light),
    item!("appearance/dark", "Dark appearance", "theme night", Appearance, Hit::Dark),
    item!("appearance/accent", "Accent colour", "color highlight", Appearance, Hit::Accent(0)),
    item!("appearance/wallpaper", "Wallpaper", "background picture", Appearance, Hit::Wallpaper(0)),
    item!("display/resolution", "Screen resolution", "display size pixels", Display, Hit::Mode(0)),
    item!("keyboard/layout", "Keyboard layout", "language typing input", Keyboard, Hit::Layout(0)),
    item!("keyboard/repeat-delay", "Key repeat delay", "keyboard typing wait", Keyboard, Hit::RepeatDelay),
    item!("keyboard/repeat-rate", "Key repeat speed", "keyboard typing rate", Keyboard, Hit::RepeatRate),
    item!("keyboard/test", "Test your keyboard", "typing try layout", Keyboard, Hit::TryField),
    item!("datetime/24-hour", "24-hour clock", "time format", DateTime, Hit::Toggle(pref::CLOCK_24H)),
    item!("datetime/seconds", "Show seconds", "clock time menu bar", DateTime, Hit::Toggle(pref::CLOCK_SECONDS)),
    item!("datetime/date", "Show date", "clock menu bar", DateTime, Hit::Toggle(pref::CLOCK_DATE)),
    item!("datetime/set-clock", "Set date and time", "clock calendar", DateTime, Hit::SetClock),
    item!("datetime/time-zone", "Time zone", "timezone utc offset", DateTime, Hit::Zone(30)),
    item!("sound/volume", "Output volume", "audio speaker loudness", Sound, Hit::Volume),
    item!("sound/mute", "Mute sound", "audio silent speaker", Sound, Hit::Toggle(pref::MUTED)),
    item!(
        "notifications/do-not-disturb",
        "Do Not Disturb",
        "notifications silence focus",
        Notifications,
        Hit::Toggle(pref::DND)
    ),
    item!("desktop/dock-size", "Dock size", "desktop icons magnification", Desktop, Hit::DockSize, 5),
    item!(
        "desktop/dock-autohide",
        "Automatically hide Dock",
        "desktop space",
        Desktop,
        Hit::Toggle(pref::DOCK_AUTOHIDE),
        4
    ),
    item!("desktop/glass", "Liquid glass intensity", "desktop blur transparency lumen", Desktop, Hit::Glass, 6),
    item!(
        "accessibility/transparency",
        "Reduce transparency",
        "solid surfaces glass blur",
        Accessibility,
        Hit::Toggle(pref::REDUCE_TRANSPARENCY),
        1
    ),
    item!(
        "accessibility/motion",
        "Reduce motion",
        "animations accessibility",
        Accessibility,
        Hit::Toggle(pref::REDUCE_MOTION),
        2
    ),
    item!(
        "accessibility/contrast",
        "Increase contrast",
        "accessibility visibility text",
        Accessibility,
        Hit::Toggle(pref::HIGH_CONTRAST),
        3
    ),
    item!("about/info", "System information", "version processor memory storage", About, Hit::AboutMore),
];
impl Settings {
    pub fn matches(&self) -> Vec<usize> {
        let query = self.search.text().to_ascii_lowercase();
        SETTINGS
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let text = format!("{} {} {}", s.id, s.label, s.keywords).to_ascii_lowercase();
                query.split_whitespace().all(|word| text.contains(word))
            })
            .map(|(i, _)| i)
            .collect()
    }
    pub fn jump_setting(&mut self, i: usize, now: u64) {
        let s = SETTINGS[i];
        self.pane = s.pane;
        self.scroll = 0;
        self.search.edit.set_text("");
        self.search_focused = false;
        self.try_focused = false;
        self.controls.focused = None;
        self.focus = if s.control.is_some() { None } else { Some(s.hit) };
        self.target = Some(i);
        self.highlight_until = now + 1800;
        self.redraw = true;
    }
    pub fn result_rect(&self, area: Rect, row: usize) -> Rect {
        Rect::new(area.x + SIDEBAR_W + 20, area.y + 65 + row as i32 * 54, area.w - SIDEBAR_W - 40, 50)
    }
    pub fn result_capacity(&self) -> usize {
        ((self.area.h - 80) / 54).max(1) as usize
    }
    pub fn result_offset(&self) -> usize {
        self.search_selected.saturating_sub(self.result_capacity() - 1)
    }
    pub fn draw_results(&self, cv: &mut Canvas, area: Rect) {
        let t = theme::current();
        let matches = self.matches();
        let x = area.x + SIDEBAR_W + 24;
        cv.text(x, area.y + 36, &format!("{} matching settings", matches.len()), theme::ui_bold(18), t.text);
        if matches.is_empty() {
            cv.text(
                x,
                area.y + 88,
                "Try a setting name, such as motion, volume, or keyboard.",
                theme::ui(13),
                t.text_secondary,
            );
        }
        for (row, &i) in matches.iter().skip(self.result_offset()).take(self.result_capacity()).enumerate() {
            let s = SETTINGS[i];
            let r = self.result_rect(area, row);
            if row + self.result_offset() == self.search_selected {
                cv.fill_round_rect(r, 8, t.control_bg);
                cv.stroke_round_rect(r, 8, theme::accent());
            }
            cv.text(r.x + 12, r.y + 21, s.label, theme::ui_bold(13), t.text);
            let pane = PANES.iter().find(|p| p.0 == s.pane).unwrap().1;
            cv.text(r.x + 12, r.y + 40, pane, theme::ui(11), t.text_secondary);
        }
    }
    pub fn result_key(&mut self, e: &KeyEvent, env: &Env) -> bool {
        use aurorakit::KeyCode;
        if !e.pressed {
            return false;
        }
        match e.code {
            KeyCode::Escape => {
                self.search.edit.set_text("");
                self.search_selected = 0;
            }
            KeyCode::Down => {
                self.search_selected = (self.search_selected + 1).min(self.matches().len().saturating_sub(1));
            }
            KeyCode::Up => {
                self.search_selected = self.search_selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(&i) = self.matches().get(self.search_selected) {
                    self.jump_setting(i, env.now_ms);
                }
            }
            _ => {
                let r = self.search.key(e, env.now_ms);
                if r.edited {
                    self.search_selected = 0;
                }
                return r.handled;
            }
        }
        true
    }
    pub fn reveal_target(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let Some(i) = self.target else {
            return;
        };
        let target = SETTINGS[i];
        let rect = if let Some(id) = target.control {
            self.controls.focused = Some(id);
            self.controls.layout_rect(id)
        } else {
            self.hits(area).into_iter().find(|(h, _)| *h == target.hit).map(|(_, r)| r)
        };
        if let Some(r) = rect {
            let delta = if r.bottom() > area.bottom() - 18 {
                r.bottom() - area.bottom() + 18
            } else if r.y < area.y + 18 {
                r.y - area.y - 18
            } else {
                0
            };
            if delta != 0 {
                self.scroll = (self.scroll + delta).max(0);
                self.redraw = true;
            } else if env.now_ms < self.highlight_until {
                cv.stroke_round_rect(r.inset(-5), 9, theme::accent());
            }
        }
        if env.now_ms >= self.highlight_until {
            self.target = None;
        }
    }
}
