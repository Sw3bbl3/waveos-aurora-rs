//! Optional, non-modal introduction. Practice belongs to the shell and never
//! opens or writes a user document. Anchors are recomputed from live geometry.
use super::Desktop;
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::apps::{App, AppKind, Env};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::{prefs, theme};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use lumen::windowing::Placement;

const STEPS: [(&str, &str, &str); 6] = [
    (
        "Your apps, together",
        "Open Apps in Harbor below. Browse the collection, or start typing to find an app.",
        "Apps is also called Shoal. Press Super to open it.",
    ),
    (
        "Find it with Search",
        "Open Search at the top right. Try typing Notes, a file name, or a quick calculation.",
        "Search is also called Seek. Press Super + Space anytime.",
    ),
    (
        "A little room for controls",
        "Move over the practice window’s top strip. Its controls appear inside the window. Try moving it by the strip.",
        "Minimize, maximize, and close stay clear of your work. Alt + Space reaches them by keyboard.",
    ),
    (
        "Make room for your work",
        "Drag any corner of the practice window to resize it. Every edge works, too. The pointer shows the direction.",
        "Try a different corner. Your content stays below the control strip.",
    ),
    (
        "Two things at once",
        "Drag the practice window toward a screen edge to arrange it. Drag toward the top to explore layouts.",
        "Super + Left or Right arranges a window. Alt + Tab switches apps.",
    ),
    (
        "The essentials, close by",
        "Open Quick Settings using the status icon at the top right. Sound, appearance, and connection live here.",
        "Quick Settings is also called Current. Click the clock for Drift: calendar and notifications.",
    ),
];

pub(super) struct Tour {
    step: usize,
    entered: u64,
    done: bool,
    practice: Option<u32>,
    initial: Option<Rect>,
    focus: usize,
}

struct Practice;
impl App for Practice {
    fn kind(&self) -> AppKind {
        AppKind::Other
    }
    fn title(&self) -> String {
        String::from("Practice window")
    }
    fn size(&self) -> (i32, i32) {
        (540, 290)
    }
    fn min_size(&self) -> (i32, i32) {
        (300, 190)
    }
    fn draw(&mut self, cv: &mut Canvas, r: Rect, _: &Env) {
        let t = theme::current();
        cv.fill_round_rect(
            Rect::new(r.x, r.y - lumen::windowing::CHROME_H, r.w, r.h + lumen::windowing::CHROME_H),
            theme::WINDOW_RADIUS,
            t.window_bg,
        );
        let cx = r.x + 28;
        let compact = r.h < 250;
        let iy = r.y + if compact { 14 } else { 28 };
        cv.fill_round_rect(Rect::new(cx, iy, 44, 44), 12, with_alpha(theme::accent(), 25));
        cv.stroke_round_rect(Rect::new(cx + 11, iy + 11, 22, 20), 4, theme::accent_ink());
        cv.text_clipped(
            cx,
            r.y + if compact { 84 } else { 106 },
            "A space to try things",
            theme::ui_bold(if compact { 18 } else { 22 }),
            t.text,
            r.w - 56,
        );
        cv.text_clipped(
            cx,
            r.y + if compact { 112 } else { 140 },
            "Move me. Resize me. Try a layout.",
            theme::ui(14),
            t.text_secondary,
            r.w - 56,
        );
        if !compact {
            cv.text_clipped(
                cx,
                r.y + 166,
                "Your documents stay just as you left them.",
                theme::ui(13),
                t.text_secondary,
                r.w - 56,
            );
        }
        cv.fill_round_rect(Rect::new(cx, r.bottom() - 50, (r.w - 56).max(1), 24), 7, with_alpha(theme::accent(), 16));
        cv.text_clipped(
            cx + 10,
            r.bottom() - 33,
            "This practice window closes when the tour ends.",
            theme::ui(11),
            t.text_secondary,
            r.w - 76,
        );
    }
}

impl Desktop {
    pub(super) fn start_tour(&mut self) {
        if self.locked() {
            return;
        }
        if self.tour.is_some() {
            self.end_tour(false);
        }
        self.close_menu();
        self.close_current();
        self.launcher = None;
        self.spotlight = None;
        self.center = None;
        self.tour_keyboard = true;
        self.tour = Some(Tour { step: 0, entered: self.now_ms, done: false, practice: None, initial: None, focus: 2 });
        self.damage_all();
    }

    fn end_tour(&mut self, complete: bool) {
        if let Some(t) = self.tour.take() {
            // Save status before removing the temporary window; no user data is involved.
            if complete {
                let _ = prefs::set("tour_completed", "1");
            }
            let _ = prefs::set("tour_seen", "1");
            if let Some(id) = t.practice {
                self.close(id);
            }
        }
        self.damage_all();
    }

    fn tour_go(&mut self, forward: bool) {
        let Some(t) = &mut self.tour else {
            return;
        };
        if forward && t.step == STEPS.len() - 1 {
            self.end_tour(true);
            return;
        }
        t.step = if forward { (t.step + 1).min(STEPS.len() - 1) } else { t.step.saturating_sub(1) };
        t.done = false;
        t.entered = self.now_ms;
        let needs_practice =
            (2..=4).contains(&t.step) && t.practice.is_none_or(|id| !self.windows.iter().any(|w| w.id == id));
        self.close_menu();
        self.close_current();
        self.launcher = None;
        self.spotlight = None;
        self.center = None;
        if needs_practice {
            self.add_window(Box::new(Practice));
            let id = self.next_id - 1;
            let i = self.index_of(id).unwrap();
            let area = self.work_area();
            let r = &mut self.windows[i].rect;
            r.x = (area.x + 28).min(area.right() - r.w).max(area.x);
            r.y = (area.y + 34).min(area.bottom() - r.h).max(area.y);
            if let Some(t) = &mut self.tour {
                t.practice = Some(id);
            }
        }
        let r =
            self.tour.as_ref().and_then(|t| t.practice).and_then(|id| self.index_of(id)).map(|i| self.windows[i].rect);
        if let Some(t) = &mut self.tour {
            t.initial = r;
        }
        self.damage_all();
    }

    fn tour_card(&self) -> Rect {
        let width = 392.min(self.w - 32);
        let height = if self.h < 600 { 200 } else { 254 };
        let height = height.min(self.h - 80);
        // Stay in the lower right, clear of the top-right system panels.
        Rect::new(self.w - width - 24, self.h - height - theme::dock_height() - 28, width, height)
    }

    fn tour_buttons(&self) -> [Rect; 3] {
        let r = self.tour_card();
        [
            Rect::new(r.x + 18, r.bottom() - 48, 60, 30),
            Rect::new(r.right() - 174, r.bottom() - 48, 68, 30),
            Rect::new(r.right() - 98, r.bottom() - 48, 80, 30),
        ]
    }

    fn tour_anchor(&self, t: &Tour) -> Rect {
        let practice = t.practice.and_then(|id| self.index_of(id)).map(|i| &self.windows[i]);
        match t.step {
            0 => self.dock_layout().1.first().map(|(_, r)| *r).unwrap_or(Rect::new(self.w / 2, self.h - 50, 30, 30)),
            1 => self.right_bubble().2,
            2 | 4 => practice
                .map(|w| Rect::new(w.rect.x + 20, w.rect.y + 7, (w.rect.w - 40).min(340), 23))
                .unwrap_or(Rect::new(30, 90, 240, 30)),
            3 => practice
                .map(|w| Rect::new(w.rect.right() - 18, w.rect.bottom() - 18, 18, 18))
                .unwrap_or(Rect::new(30, 90, 24, 24)),
            _ => self.right_bubble().3,
        }
    }

    pub(super) fn tour_cursor(&self, x: i32, y: i32) -> Option<u32> {
        self.tour.as_ref()?;
        if self.tour_buttons().iter().any(|r| r.contains(x, y)) {
            Some(aurora_abi::cursor::POINTER)
        } else if self.tour_card().contains(x, y) {
            Some(aurora_abi::cursor::ARROW)
        } else {
            None
        }
    }

    pub(super) fn tour_press(&mut self, x: i32, y: i32) -> bool {
        if self.tour.is_none() {
            return false;
        }
        for (n, r) in self.tour_buttons().into_iter().enumerate() {
            if r.contains(x, y) {
                match n {
                    0 => self.end_tour(false),
                    1 => self.tour_go(false),
                    _ => self.tour_go(true),
                }
                return true;
            }
        }
        self.tour_card().contains(x, y)
    }

    pub(super) fn tour_key(&mut self, k: &KeyEvent) -> bool {
        let Some(t) = &mut self.tour else {
            return false;
        };
        if !k.pressed {
            return false;
        }
        if k.code == KeyCode::Escape {
            self.end_tour(false);
            return true;
        }
        // F1 returns keyboard focus to the tips without stealing typing from apps.
        if k.code == KeyCode::F1 {
            t.focus = 2;
            self.tour_keyboard = true;
            self.damage_all();
            return true;
        }
        if !self.tour_keyboard {
            return false;
        }
        match k.code {
            KeyCode::Tab => {
                t.focus = (t.focus + if k.mods.shift { 2 } else { 1 }) % 3;
                self.damage_all();
                true
            }
            KeyCode::Enter => {
                match t.focus {
                    0 => self.end_tour(false),
                    1 => self.tour_go(false),
                    _ => self.tour_go(true),
                }
                true
            }
            _ => {
                self.tour_keyboard = false;
                false
            }
        }
    }

    pub(super) fn tour_moving(&self) -> bool {
        self.tour.as_ref().is_some_and(|t| !theme::reduce_motion() && self.now_ms.saturating_sub(t.entered) < 3200)
    }

    pub(super) fn step_tour(&mut self) -> bool {
        let Some(t) = &self.tour else {
            return false;
        };
        let done = match t.step {
            0 => self.launcher.is_some(),
            1 => self.spotlight_open(),
            2 => t
                .practice
                .and_then(|id| self.index_of(id))
                .is_some_and(|i| self.windows[i].chrome.value(self.now_ms, theme::reduce_motion()) > 500),
            3 => t
                .practice
                .and_then(|id| self.index_of(id))
                .zip(t.initial)
                .is_some_and(|(i, r)| self.windows[i].rect.w != r.w || self.windows[i].rect.h != r.h),
            4 => t
                .practice
                .and_then(|id| self.index_of(id))
                .is_some_and(|i| self.windows[i].placement != Placement::Floating),
            _ => self.current_open(),
        };
        let changed = done && !t.done;
        if changed {
            if let Some(t) = &mut self.tour {
                t.done = true;
            }
        }
        let moving = self.tour_moving();
        if changed || moving {
            self.damage_all();
        }
        moving
    }

    pub(super) fn paint_tour(&self, cv: &mut Canvas) {
        let Some(t) = &self.tour else {
            return;
        };
        let r = self.tour_card();
        let theme = theme::current();
        let elapsed = self.now_ms.saturating_sub(t.entered);
        let k = if theme::reduce_motion() { 1000 } else { lumen::motion::ease(elapsed, lumen::motion::REVEAL) };
        let alpha = (255 * k / 1000) as u8;
        let anchor = self.tour_anchor(t);
        let pulse = if self.tour_moving() { lumen::math::sin((elapsed * 1024 / 1600) as i32) * 2 / 16384 } else { 0 };
        cv.stroke_round_rect(anchor.inset(-5 - pulse), 10, with_alpha(theme::accent_ink(), alpha));
        cv.stroke_round_rect(anchor.inset(-7 - pulse), 12, with_alpha(theme::accent(), alpha / 3));
        // A short arrow leads from the tip toward its live target, avoiding a
        // giant line through the user's workspace.
        let (tx, ty) = anchor.center();
        let (ax, ay) = if ty > r.bottom() { (tx, anchor.y - 28) } else { (tx, anchor.bottom() + 28) };
        let end_y = if ty > r.bottom() { anchor.y - 8 } else { anchor.bottom() + 8 };
        cv.line(ax, ay, tx, end_y, 2, theme::accent_ink());
        let dir = if ty > r.bottom() { -1 } else { 1 };
        cv.line(tx, end_y, tx - 5, end_y + dir * 5, 2, theme::accent_ink());
        cv.line(tx, end_y, tx + 5, end_y + dir * 5, 2, theme::accent_ink());
        cv.shadow(r, 16, 24, 6, theme.shadow);
        cv.fill_round_rect(r, 16, theme.window_bg);
        cv.stroke_round_rect(r, 16, theme.window_border);
        let left = r.x + 20;
        cv.text(
            left,
            r.y + 26,
            &format!("GET TO KNOW WAVEOS   {} / {}", t.step + 1, STEPS.len()),
            theme::ui_bold(10),
            theme.text_secondary,
        );
        let compact = r.h < 240;
        cv.text_clipped(
            left,
            r.y + if compact { 50 } else { 57 },
            STEPS[t.step].0,
            theme::ui_bold(if compact { 18 } else { 20 }),
            theme.text,
            r.w - 40,
        );
        draw_wrapped(
            cv,
            Rect::new(left, r.y + if compact { 75 } else { 83 }, r.w - 40, if compact { 51 } else { 57 }),
            STEPS[t.step].1,
            theme.text,
            13,
            if compact { 17 } else { 19 },
        );
        let detail = if t.done { "Nice — you’ve got it. Continue when you’re ready." } else { STEPS[t.step].2 };
        draw_wrapped(
            cv,
            Rect::new(left, r.y + if compact { 125 } else { 146 }, r.w - 40, if compact { 28 } else { 32 }),
            detail,
            if t.done { theme::accent_ink() } else { theme.text_secondary },
            11,
            if compact { 14 } else { 16 },
        );
        if !compact {
            cv.text(left, r.bottom() - 58, "F1 focuses tips · Esc skips", theme::ui(10), theme.text_secondary);
        }
        for (n, b) in self.tour_buttons().into_iter().enumerate() {
            let primary = n == 2;
            cv.fill_round_rect(b, 9, if primary { theme::accent() } else { theme.hover });
            if self.tour_keyboard && t.focus == n {
                cv.stroke_round_rect(b.inset(-2), 11, theme::accent());
            }
            let label = match n {
                0 => "Skip",
                1 => "Back",
                _ if t.step == STEPS.len() - 1 => "Finish",
                _ => "Next",
            };
            let f = theme::ui_bold(12);
            cv.text(
                b.x + (b.w - f.width(label)) / 2,
                b.y + 20,
                label,
                f,
                if primary { 0xFFFFFFFF } else { theme.text },
            );
        }
    }
}

fn draw_wrapped(cv: &mut Canvas, rect: Rect, text: &str, ink: u32, size: u16, line_height: i32) {
    let (x, y, width, max) = (rect.x, rect.y, rect.w, (rect.h / line_height) as usize);
    let f = theme::ui(size);
    let mut line = String::new();
    let mut row = 0;
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { String::from(word) } else { format!("{line} {word}") };
        if f.width(&candidate) > width && !line.is_empty() {
            cv.text_clipped(x, y + row as i32 * line_height, &line, f, ink, width);
            row += 1;
            if row == max {
                return;
            }
            line = String::from(word);
        } else {
            line = candidate;
        }
    }
    if row < max {
        cv.text_clipped(x, y + row as i32 * line_height, &line, f, ink, width);
    }
}
