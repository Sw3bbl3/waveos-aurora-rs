//! Coves: several desktops, side by side. Each window lives in one Cove
//! and only the Cove in front shows; Lagoon's strip adds, removes and
//! switches them, and takes windows from one to another.
//!
//! Ctrl+Super+Left/Right moves to the next Cove with a slide (the
//! wallpaper, bar and Harbor stay; the windows glide aside on a spring, so
//! pressing again mid-way carries on smoothly), and with Shift the focused
//! window comes along. Crest names the Cove. Bringing forward a window of
//! another Cove (Harbor, Glide, Seek, a notification) goes there first.
//! A Cove spans every display. Nothing about Coves is kept across a restart.

use super::bar::Activity;
use super::*;
use lumen::spring::{self, Spring};

/// At most this many Coves.
pub(super) const MAX: usize = 8;

pub(super) struct Coves {
    /// Their ids, in order (ids stay with a Cove as others come and go).
    pub ids: Vec<u16>,
    pub current: u16,
    next: u16,
    slide: Option<Slide>,
}

impl Default for Coves {
    fn default() -> Self {
        Coves { ids: alloc::vec![1], current: 1, next: 2, slide: None }
    }
}

/// Moving between Coves: a camera over the row of Coves (a Cove is 1000
/// wide), and pictures of the windows it passes.
struct Slide {
    camera: Spring,
    pictures: Vec<(u32, Vec<u32>, u32, u32)>,
    /// The window carried along, which stays where it is.
    carried: Option<u32>,
}

impl Coves {
    /// Stops a slide where it is (the windows show where they are).
    pub fn end_slide(&mut self) {
        self.slide = None;
    }

    pub fn index_of(&self, cove: u16) -> Option<usize> {
        self.ids.iter().position(|c| *c == cove)
    }

    /// Adds a Cove at the end; its index (None when there are already [`MAX`]).
    pub fn add(&mut self) -> Option<usize> {
        if self.ids.len() >= MAX {
            return None;
        }
        self.ids.push(self.next);
        self.next += 1;
        Some(self.ids.len() - 1)
    }

    /// Removes the Cove at `index` (never the last one): (its id, the
    /// neighbour that takes its windows: the one before it, or after the first).
    pub fn remove(&mut self, index: usize) -> Option<(u16, u16)> {
        if self.ids.len() < 2 || index >= self.ids.len() {
            return None;
        }
        let gone = self.ids.remove(index);
        let heir = self.ids[index.saturating_sub(1).min(self.ids.len() - 1)];
        if self.current == gone {
            self.current = heir;
        }
        Some((gone, heir))
    }
}

impl Desktop {
    /// The index of the Cove in front.
    pub(super) fn cove_index(&self) -> usize {
        self.coves.index_of(self.coves.current).unwrap_or(0)
    }

    pub(super) fn cove_name(&self, index: usize) -> String {
        alloc::format!("Cove {}", index + 1)
    }

    /// Each window knows whether its Cove is the one in front.
    pub(super) fn sync_coves(&mut self) {
        let current = self.coves.current;
        for w in &mut self.windows {
            w.elsewhere = w.cove != current;
        }
    }

    /// Brings Cove `index` forward (with `carry`, that window comes along).
    pub(super) fn switch_cove(&mut self, index: usize, carry: Option<u32>) {
        let Some(&to) = self.coves.ids.get(index) else { return };
        let from = self.cove_index();
        if to == self.coves.current {
            return;
        }
        let carry = carry.filter(|id| self.index_of(*id).is_some());
        if !theme::reduce_motion() && self.lagoon.is_none() && !self.play_covers() && self.shore.is_none() {
            self.begin_slide(from, index, carry);
        }
        if let Some(i) = carry.and_then(|id| self.index_of(id)) {
            self.windows[i].cove = to;
        }
        self.coves.current = to;
        self.sync_coves();
        if let Some(id) = carry {
            self.raise(id);
        }
        // What belonged to the Cove left behind.
        self.close_menu();
        self.harbor.peek = None;
        self.grab = None;
        if self.drag.is_some_and(|d| Some(d.window) != carry) {
            self.drag = None;
            self.snap_preview = None;
            self.moorings_open = false;
        }
        self.show_activity(Activity::Cove);
        self.lagoon_cove_changed();
        self.damage_all();
        self.damage_shell();
    }

    /// The next Cove to the left (`step` -1) or right (+1), taking the
    /// focused window along with `carry`. Past either end it bumps.
    pub(super) fn step_cove(&mut self, step: i32, carry: bool) {
        let at = self.cove_index() as i32 + step;
        let carried = if carry {
            // A window being dragged comes along; otherwise the focused one.
            self.drag.filter(|d| d.started && matches!(d.kind, DragKind::Move)).map(|d| d.window).or(self.focused_id())
        } else {
            self.drag.filter(|d| d.started && matches!(d.kind, DragKind::Move)).map(|d| d.window)
        };
        if at < 0 || at as usize >= self.coves.ids.len() {
            // Nothing that way: a nudge, and the name of where you are.
            if let Some(s) = self.coves.slide.as_mut() {
                s.camera.kick(-step * 1600, self.now_ms);
            } else if !theme::reduce_motion() && self.lagoon.is_none() && !self.play_covers() {
                let here = self.cove_index();
                self.begin_slide(here, here, None);
                if let Some(s) = self.coves.slide.as_mut() {
                    s.camera.kick(step * 1600, self.now_ms);
                }
            }
            self.show_activity(Activity::Cove);
            return;
        }
        self.switch_cove(at as usize, carried);
    }

    /// Ctrl+Super+Left/Right (with Shift: the focused window along),
    /// Ctrl+Super+D (a new Cove, at the end, brought forward) and
    /// Ctrl+Super+F4 (removes the Cove in front; its windows join the neighbour).
    pub(super) fn cove_key(&mut self, k: &KeyEvent) -> bool {
        if !(k.pressed && k.mods.ctrl && k.mods.super_key) || self.shore.is_some() {
            return false;
        }
        match k.code {
            KeyCode::Left | KeyCode::Right => {
                self.step_cove(if k.code == KeyCode::Left { -1 } else { 1 }, k.mods.shift);
            }
            KeyCode::F4 => self.remove_cove(self.cove_index()),
            _ if matches!(k.ch, Some('d' | 'D')) => match self.add_cove() {
                Some(i) => self.switch_cove(i, None),
                None => self.show_activity(Activity::Cove),
            },
            _ => return false,
        }
        self.super_alone = false;
        true
    }

    /// A new Cove at the end (None: there are as many as there can be).
    pub(super) fn add_cove(&mut self) -> Option<usize> {
        let i = self.coves.add()?;
        self.lagoon_coves_changed();
        Some(i)
    }

    /// Removes Cove `index`; its windows join the neighbour.
    pub(super) fn remove_cove(&mut self, index: usize) {
        let showing = self.coves.current;
        let Some((gone, heir)) = self.coves.remove(index) else { return };
        for w in &mut self.windows {
            if w.cove == gone {
                w.cove = heir;
            }
        }
        self.sync_coves();
        if showing == gone {
            self.show_activity(Activity::Cove);
            self.lagoon_cove_changed();
        }
        self.lagoon_coves_changed();
        self.damage_all();
        self.damage_shell();
    }

    /// Puts window `id` in Cove `index` (it leaves the screen if that Cove isn't in front).
    pub(super) fn move_to_cove(&mut self, id: u32, index: usize) {
        let (Some(i), Some(&cove)) = (self.index_of(id), self.coves.ids.get(index)) else { return };
        if self.windows[i].cove == cove {
            return;
        }
        let b = self.windows[i].bounds();
        self.windows[i].cove = cove;
        self.sync_coves();
        self.damage(b);
        if let Some(f) = self.focused_id() {
            self.damage_window(f);
        }
        self.damage_shell();
    }

    // ------------------------------------------------------------- the slide

    fn begin_slide(&mut self, from: usize, to: usize, carried: Option<u32>) {
        let now = self.now_ms;
        let mut slide = self.coves.slide.take().unwrap_or_else(|| Slide {
            camera: Spring::new(from as i32 * 1000, spring::SMOOTH),
            pictures: Vec::new(),
            carried: None,
        });
        slide.camera.set_target(to as i32 * 1000, now);
        slide.carried = carried.or(slide.carried);
        // Pictures of every window between where the camera is and where it goes.
        let at = (slide.camera.value() / 1000).max(0) as usize;
        let (lo, hi) = (from.min(to).min(at), from.max(to).max(at + 1).min(self.coves.ids.len() - 1));
        let ids: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| !w.minimized && w.awaiting_frame.is_none())
            .filter(|w| self.coves.index_of(w.cove).is_some_and(|c| (lo..=hi).contains(&c)))
            .map(|w| w.id)
            .filter(|id| !slide.pictures.iter().any(|p| p.0 == *id))
            .collect();
        for id in ids {
            if let Some((px, w, h)) = self.snapshot(id) {
                slide.pictures.push((id, px, w, h));
            }
        }
        self.ghosts.clear();
        for w in &mut self.windows {
            w.animating = w.awaiting_frame.is_some();
        }
        self.coves.slide = Some(slide);
        self.damage_all();
    }

    pub(super) fn sliding(&self) -> bool {
        self.coves.slide.is_some()
    }

    /// Advances the slide; true while it runs.
    pub(super) fn step_slide(&mut self) -> bool {
        let now = self.now_ms;
        let Some(s) = self.coves.slide.as_mut() else { return false };
        if s.camera.advance(now) && !theme::reduce_motion() {
            self.damage_all();
            return true;
        }
        self.coves.slide = None;
        self.damage_all();
        false
    }

    /// The windows passing by while the camera moves (instead of the windows themselves).
    pub(super) fn paint_slide(&self, cv: &mut Canvas) {
        let Some(s) = &self.coves.slide else { return };
        let t = theme::current();
        let camera = s.camera.value();
        let rects = self.display_rects();
        for w in &self.windows {
            if w.minimized || w.awaiting_frame.is_some() {
                continue;
            }
            let Some((_, px, pw, ph)) = s.pictures.iter().find(|p| p.0 == w.id) else { continue };
            let Some(cove) = self.coves.index_of(w.cove) else { continue };
            let display = rects[lumen::displays::home(&rects, w.rect)];
            let dx = if s.carried == Some(w.id) {
                0
            } else {
                ((cove as i64 * 1000 - camera as i64) * display.w as i64 / 1000) as i32
            };
            if dx.abs() >= display.w + SHADOW_BLUR {
                continue;
            }
            let r = w.rect.offset(dx, 0);
            let clip = display.intersect(&cv.clip);
            if clip.is_empty() || !r.inset(-SHADOW_BLUR).intersects(&clip) {
                continue;
            }
            cv.with_clip(clip, |cv| {
                cv.shadow(r, WINDOW_RADIUS, SHADOW_BLUR, 6, t.shadow * 6 / 10);
                cv.draw_image(px, *pw, *ph, r, 255);
            });
        }
    }
}

#[cfg(feature = "ktest")]
impl Desktop {
    /// (the index of the Cove in front, how many there are, and each
    /// window: its id, its Cove's index, whether it is hidden) — self-tests.
    pub fn coves_state(&self) -> (usize, usize, Vec<(u32, usize, bool)>) {
        let windows = self
            .windows
            .iter()
            .map(|w| (w.id, self.coves.index_of(w.cove).unwrap_or(usize::MAX), w.hidden()))
            .collect();
        (self.cove_index(), self.coves.ids.len(), windows)
    }

    pub fn focused_window(&self) -> Option<u32> {
        self.focused_id()
    }

    pub fn test_add_cove(&mut self) -> Option<usize> {
        self.add_cove()
    }

    pub fn test_remove_cove(&mut self, index: usize) {
        self.remove_cove(index)
    }

    pub fn test_move_to_cove(&mut self, id: u32, index: usize) {
        self.move_to_cove(id, index)
    }

    /// Whether anything (Lagoon, a slide between Coves) is still moving.
    pub fn shell_moving(&self) -> bool {
        self.lagoon_moving() || self.sliding()
    }
}
