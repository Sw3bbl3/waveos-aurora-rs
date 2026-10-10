use super::*;
use transfer::{Choice, Manager};

pub struct Files {
    tabs: Vec<Folder>,
    active: usize,
    transfers: transfer::Shared,
    worker: SearchWorker,
    cut: Arc<Mutex<Vec<String>>>,
    panel_focus: bool,
    panel_button: usize,
    conflict_serial: u64,
    progress: Option<(u64, usize, usize, bool, bool, bool, u64, String)>,
    last_tick: u64,
    close_prompt: bool,
    closing: bool,
    error_index: usize,
}
impl Files {
    pub fn new(start: &str) -> Self {
        let transfers = Manager::new();
        let worker = Arc::new(Mutex::new(None));
        let cut = Arc::new(Mutex::new(Vec::new()));
        Self {
            tabs: alloc::vec![Folder::new(start, transfers.clone(), worker.clone(), cut.clone())],
            active: 0,
            transfers,
            worker,
            cut,
            panel_focus: false,
            panel_button: 1,
            conflict_serial: 0,
            progress: None,
            last_tick: 0,
            close_prompt: false,
            closing: false,
            error_index: 0,
        }
    }
    fn pane(&self, r: Rect) -> Rect {
        let s = self.transfers.lock();
        let s = s.state.lock();
        let footer = if s.running || !s.summary.is_empty() || self.close_prompt || self.closing {
            if r.w < 700 {
                166
            } else {
                126
            }
        } else {
            0
        };
        Rect::new(r.x, r.y + 38, r.w, (r.h - 38 - footer).max(100))
    }
    fn tab_rect(&self, area: Rect, i: usize) -> Rect {
        let w = ((area.w - 48) / self.tabs.len() as i32).min(210);
        Rect::new(area.x + 6 + i as i32 * w, area.y + 4, w - 4, 30)
    }
    fn switch(&mut self, next: usize) {
        if next == self.active || next >= self.tabs.len() {
            return;
        }
        let old = &mut self.tabs[self.active];
        let resume = !old.search_done() || old.search_at.is_some();
        old.stop_search();
        old.resume_search = resume;
        old.thumbs.clear();
        old.quicklook = None;
        old.menu = None;
        old.finish_rename(true);
        self.active = next;
        self.panel_focus = false;
        if self.tabs[next].resume_search {
            self.tabs[next].start_search();
            self.tabs[next].resume_search = false;
        } else {
            self.tabs[next].refresh();
        }
    }
    fn add(&mut self) {
        if self.tabs.len() == 8 {
            self.tabs[self.active].flash(String::from("Eight tabs are already open"));
            return;
        }
        let path = self.tabs[self.active].cwd.clone();
        self.tabs.push(Folder::new(&path, self.transfers.clone(), self.worker.clone(), self.cut.clone()));
        self.switch(self.tabs.len() - 1);
    }
    fn buttons(area: Rect) -> [Rect; 5] {
        let cols = if area.w < 700 { 3 } else { 5 };
        let width = ((area.w - 24) / cols - 8).min(124);
        core::array::from_fn(|i| {
            Rect::new(
                area.x + 12 + (i as i32 % cols) * (width + 8),
                area.bottom() - 42 - (if cols == 3 { 1 - i as i32 / cols } else { 0 }) * 36,
                width,
                30,
            )
        })
    }
    fn cancel_close(&mut self) {
        self.close_prompt = false;
        self.closing = true;
        self.transfers.lock().state.lock().cancel = true;
        for tab in &mut self.tabs {
            tab.stop_search();
        }
    }
    fn decision(&mut self, index: usize) {
        let manager = self.transfers.lock();
        let mut s = manager.state.lock();
        match index {
            0 if s.conflict.as_ref().is_some_and(|c| c.replace) => s.decision = Some(Choice::Replace),
            1 => s.decision = Some(Choice::KeepBoth),
            2 => s.decision = Some(Choice::Skip),
            3 => s.apply_all = !s.apply_all,
            4 => s.cancel = true,
            _ => {}
        }
    }
}
impl App for Files {
    fn title(&self) -> String {
        self.tabs[self.active].title()
    }
    fn size(&self) -> (i32, i32) {
        (880, 580)
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let pane = self.pane(area);
        self.tabs[self.active].draw(cv, pane, env);
        let t = theme::current();
        cv.fill_rect(Rect::new(area.x, area.y, area.w, 38), t.window_bg_alt);
        for (i, tab) in self.tabs.iter().enumerate() {
            let r = self.tab_rect(area, i);
            if i == self.active {
                cv.fill_round_rect(r, 7, t.control_bg);
                cv.fill_rect(Rect::new(r.x + 8, r.bottom() - 2, r.w - 16, 2), theme::accent());
            }
            let label = if tab.cwd == "/" { "Aurora HD" } else { fs::file_name(&tab.cwd) };
            cv.text_clipped(r.x + 9, r.y + 20, label, theme::ui(12), t.text, r.w - 35);
            cv.text(r.right() - 20, r.y + 20, "×", theme::ui(14), t.text_secondary);
        }
        button(cv, Rect::new(area.right() - 36, area.y + 4, 30, 30), "+", ButtonStyle::Secondary, false);
        if pane.bottom() == area.bottom() {
            return;
        }
        let p = Rect::new(area.x, pane.bottom(), area.w, area.bottom() - pane.bottom());
        cv.fill_rect(p, t.window_bg_alt);
        cv.fill_rect(Rect::new(p.x, p.y, p.w, 1), t.separator);
        let buttons = Self::buttons(area);
        if self.panel_focus || self.close_prompt {
            cv.stroke_round_rect(buttons[self.panel_button].inset(-2), 7, theme::accent());
        }
        if self.close_prompt {
            cv.text(
                p.x + 14,
                p.y + 28,
                "A transfer is running. Keep this window open or cancel before closing.",
                theme::ui(14),
                t.text,
            );
            button(cv, buttons[0], "Keep Open", ButtonStyle::Primary, false);
            button(cv, buttons[1], "Cancel & Close", ButtonStyle::Secondary, false);
            return;
        }
        let manager = self.transfers.lock();
        let s = manager.state.lock();
        let heading = if self.closing {
            "Cleaning up before closing..."
        } else if s.running {
            "File transfer · Ctrl+J to focus controls"
        } else {
            &s.summary
        };
        cv.text_clipped(p.x + 14, p.y + 24, heading, theme::ui_bold(13), t.text, p.w - 28);
        if let Some(c) = &s.conflict {
            cv.text_clipped(
                p.x + 14,
                p.y + 48,
                &format!("Already exists: {}", c.name),
                theme::ui(13),
                t.text,
                p.w - 28,
            );
            let labels = [
                if c.replace { "Replace (R)" } else { "Replace unavailable" },
                "Keep Both (K)",
                "Skip (S)",
                if s.apply_all { "[x] Apply all (A)" } else { "[ ] Apply all (A)" },
                "Cancel (Esc)",
            ];
            for (i, label) in labels.iter().enumerate() {
                button(cv, buttons[i], label, ButtonStyle::Secondary, false);
            }
        } else {
            let line = if s.running {
                format!("{} / {} items · {} copied · {}", s.completed, s.total, human_size(s.bytes), s.current)
            } else if s.errors.is_empty() {
                format!("{} copied", human_size(s.bytes))
            } else {
                format!(
                    "Error {}/{}: {}",
                    self.error_index % s.errors.len() + 1,
                    s.errors.len(),
                    s.errors[self.error_index % s.errors.len()]
                )
            };
            cv.text_clipped(p.x + 14, p.y + 48, &line, theme::ui(12), t.text_secondary, p.w - 28);
            if s.running {
                button(
                    cv,
                    buttons[0],
                    if s.committing {
                        "Finishing..."
                    } else if s.cancel {
                        "Cancelling..."
                    } else {
                        "Cancel"
                    },
                    ButtonStyle::Secondary,
                    false,
                );
            } else {
                button(cv, buttons[0], "Dismiss", ButtonStyle::Secondary, false);
                if !s.errors.is_empty() {
                    button(cv, buttons[1], "Next error", ButtonStyle::Secondary, false);
                }
            }
        }
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if self.closing {
            return false;
        }
        if y < area.y + 38 {
            if x >= area.right() - 40 {
                self.add();
                return true;
            }
            for i in 0..self.tabs.len() {
                let r = self.tab_rect(area, i);
                if r.contains(x, y) {
                    self.switch(i);
                    if x >= r.right() - 27 && self.close_tab_requested(env) {
                        env.requests.push(Request::Close);
                    }
                    return true;
                }
            }
        }
        let pane = self.pane(area);
        if y >= pane.bottom() {
            if let Some(i) = Self::buttons(area).iter().position(|r| r.contains(x, y)) {
                if self.close_prompt {
                    if i == 0 {
                        self.close_prompt = false;
                    }
                    if i == 1 {
                        self.cancel_close();
                    }
                    return true;
                }
                self.panel_focus = true;
                self.panel_button = i;
                let conflict = self.transfers.lock().state.lock().conflict.is_some();
                if conflict {
                    self.decision(i);
                } else {
                    let manager = self.transfers.lock();
                    let mut s = manager.state.lock();
                    if i == 0 {
                        if s.running {
                            s.cancel = true;
                        } else {
                            s.summary.clear();
                        }
                    }
                    if i == 1 && !s.running {
                        self.error_index += 1;
                    }
                }
            }
            return true;
        }
        if self.close_prompt {
            return true;
        }
        self.panel_focus = false;
        self.tabs[self.active].click(x, y, pane, env)
    }
    fn key(&mut self, e: &KeyEvent, env: &mut Env) -> bool {
        if !e.pressed {
            return false;
        }
        if self.closing {
            return true;
        }
        if self.close_prompt {
            match e.code {
                KeyCode::Escape => self.close_prompt = false,
                KeyCode::Tab => self.panel_button = 1 - self.panel_button.min(1),
                KeyCode::Enter => {
                    if self.panel_button == 1 {
                        self.cancel_close();
                    } else {
                        self.close_prompt = false;
                    }
                }
                _ => {}
            }
            return true;
        }
        if (e.mods.ctrl || e.mods.super_key) && e.ch.is_some_and(|c| c.eq_ignore_ascii_case(&'j')) {
            self.panel_focus = true;
            self.panel_button = if self.transfers.lock().state.lock().conflict.is_some() { 1 } else { 0 };
            return true;
        }
        let conflict = self.transfers.lock().state.lock().conflict.is_some();
        if self.panel_focus && conflict && !e.mods.ctrl && !e.mods.super_key {
            if e.code == KeyCode::Tab {
                self.panel_button = (self.panel_button + if e.mods.shift { 4 } else { 1 }) % 5;
                return true;
            }
            if e.code == KeyCode::Enter || e.ch == Some(' ') {
                self.decision(self.panel_button);
                return true;
            }
            let i = match e.ch.map(|c| c.to_ascii_lowercase()) {
                Some('r') => Some(0),
                Some('k') => Some(1),
                Some('s') => Some(2),
                Some('a') => Some(3),
                _ => None,
            };
            if let Some(i) = i {
                self.decision(i);
                return true;
            }
            if e.code == KeyCode::Escape {
                self.decision(4);
                return true;
            }
        }
        if self.panel_focus && !conflict && !e.mods.ctrl && !e.mods.super_key {
            let manager = self.transfers.lock();
            let mut state = manager.state.lock();
            if e.code == KeyCode::Tab {
                self.panel_button =
                    if !state.running && !state.errors.is_empty() { 1 - self.panel_button.min(1) } else { 0 };
                return true;
            }
            if e.code == KeyCode::Enter || e.ch == Some(' ') || e.code == KeyCode::Escape {
                if state.running {
                    state.cancel = true;
                } else if self.panel_button == 1 && e.code != KeyCode::Escape {
                    self.error_index += 1;
                } else {
                    state.summary.clear();
                    self.panel_focus = false;
                }
                return true;
            }
        }
        if e.mods.ctrl || e.mods.super_key {
            if e.ch.is_some_and(|c| c.eq_ignore_ascii_case(&'t')) {
                self.add();
                return true;
            }
            if e.code == KeyCode::Tab {
                let n = self.tabs.len();
                self.switch((self.active + if e.mods.shift { n - 1 } else { 1 }) % n);
                return true;
            }
            if !e.mods.shift {
                if let Some(n) = e.ch.and_then(|c| c.to_digit(10)).filter(|n| *n >= 1 && *n <= 8) {
                    self.switch(n as usize - 1);
                    return true;
                }
            }
        }
        self.tabs[self.active].key(e, env)
    }
    fn tick(&mut self, env: &mut Env) -> bool {
        self.last_tick = env.now_ms;
        let finished = self.transfers.lock().reap();
        let progress_changed;
        {
            let m = self.transfers.lock();
            let s = m.state.lock();
            let progress = (
                s.bytes,
                s.completed,
                s.errors.len(),
                s.running,
                s.cancel,
                s.committing,
                s.conflict_serial,
                s.current.clone(),
            );
            progress_changed = self.progress.as_ref() != Some(&progress);
            self.progress = Some(progress);
            if s.conflict.is_some() && s.conflict_serial != self.conflict_serial {
                self.conflict_serial = s.conflict_serial;
                self.panel_focus = true;
                self.panel_button = 1;
                self.tabs[self.active].search_focused = false;
            }
        }
        if finished {
            self.panel_button = 0;
            let manager = self.transfers.lock();
            let s = manager.state.lock();
            {
                let mut cut = self.cut.lock();
                let owns_clipboard = corekit::clipboard::files().is_some_and(|paths| paths == *cut);
                cut.retain(|p| !s.moved.contains(p));
                if owns_clipboard {
                    corekit::clipboard::set_files(&cut);
                }
            }
            for e in &s.errors {
                corekit::eprintln!("Files transfer: {}", e);
            }
            drop(s);
            drop(manager);
            for tab in &mut self.tabs {
                tab.refresh();
            }
        }
        let changed = self.tabs[self.active].tick(env);
        if self.closing && !self.transfers.lock().busy() && self.worker.lock().is_none() {
            if self.transfers.lock().state.lock().errors.is_empty() {
                env.requests.push(Request::Close);
            } else {
                self.closing = false;
            }
        }
        changed || finished || progress_changed
    }
    fn next_wakeup(&self, env: &Env) -> Option<u64> {
        let tab = &self.tabs[self.active];
        let busy = self.transfers.lock().busy() || self.worker.lock().is_some() || self.closing;
        let editing = env.focused && (tab.search_focused || tab.rename.is_some());
        let thumbnails = tab.view == View::Icons
            && tab.items.iter().enumerate().any(|(i, e)| {
                let r = tab.item_rect(tab.area, i);
                let c = tab.content(tab.area);
                !e.is_dir
                    && is_image(&e.name)
                    && e.size < 12 << 20
                    && r.bottom() >= c.y
                    && r.y <= c.bottom()
                    && !tab.thumbs.contains_key(&e.path)
            });
        let interval = if busy || thumbnails {
            100
        } else if editing {
            500
        } else {
            2000
        };
        Some(tab.search_at.unwrap_or(u64::MAX).max(self.last_tick + 1).min(self.last_tick + interval))
    }
    fn close_requested(&mut self, _env: &mut Env) -> bool {
        if self.transfers.lock().busy() {
            self.close_prompt = true;
            self.panel_button = 0;
            return false;
        }
        for tab in &mut self.tabs {
            tab.stop_search();
        }
        if self.worker.lock().is_some() {
            self.closing = true;
            return false;
        }
        true
    }
    fn close_tab_requested(&mut self, env: &mut Env) -> bool {
        if self.tabs.len() == 1 {
            return self.close_requested(env);
        }
        self.tabs[self.active].stop_search();
        self.tabs.remove(self.active);
        self.active = self.active.min(self.tabs.len() - 1);
        if self.tabs[self.active].resume_search {
            self.tabs[self.active].start_search();
            self.tabs[self.active].resume_search = false;
        }
        false
    }
    fn double_click(&mut self, x: i32, y: i32, a: Rect, e: &mut Env) -> bool {
        let p = self.pane(a);
        if p.contains(x, y) {
            self.tabs[self.active].double_click(x, y, p, e)
        } else {
            self.click(x, y, a, e)
        }
    }
    fn right_click(&mut self, x: i32, y: i32, a: Rect, e: &mut Env) -> bool {
        let p = self.pane(a);
        p.contains(x, y) && self.tabs[self.active].right_click(x, y, p, e)
    }
    fn drag(&mut self, x: i32, y: i32, a: Rect, e: &mut Env) -> bool {
        let p = self.pane(a);
        self.tabs[self.active].drag(x, y, p, e)
    }
    fn release(&mut self, x: i32, y: i32, a: Rect, e: &mut Env) -> bool {
        let p = self.pane(a);
        self.tabs[self.active].release(x, y, p, e)
    }
    fn hover(&mut self, x: i32, y: i32, a: Rect) -> bool {
        let p = self.pane(a);
        self.tabs[self.active].hover(x, y, p)
    }
    fn scroll(&mut self, d: i32, a: Rect) -> bool {
        let p = self.pane(a);
        self.tabs[self.active].scroll(d, p)
    }
    fn drag_over(&mut self, x: i32, y: i32, a: Rect, e: &mut Env) -> bool {
        let p = self.pane(a);
        self.tabs[self.active].drag_over(x, y, p, e)
    }
    fn drop(&mut self, x: i32, y: i32, k: u32, a: Rect, e: &mut Env) -> bool {
        let p = self.pane(a);
        self.tabs[self.active].drop(x, y, k, p, e)
    }
    fn drag_end(&mut self, d: bool, e: &mut Env) -> bool {
        self.tabs[self.active].drag_end(d, e)
    }
}

mod checks;
