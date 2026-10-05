#![no_std]
#![no_main]
extern crate alloc;
use alloc::{format, string::String};
use aurorakit::{
    canvas::Canvas,
    geom::Rect,
    ui::{self, View},
    App, Env, KeyCode, KeyEvent,
};
struct Demo {
    ui: ui::State,
    count: u32,
    on: bool,
    value: i32,
}
impl Demo {
    fn action(&mut self, a: Option<ui::Action>) -> bool {
        match a {
            Some(ui::Action::Activate(1)) => {
                self.count += 1;
                let _ =
                    corekit::fs::write("/AppData/dev.example.gallery/count.txt", format!("{}", self.count).as_bytes());
                corekit::println!("Count: {}", self.count);
            }
            Some(ui::Action::Toggle(2, on)) => self.on = on,
            Some(ui::Action::Change(3, v)) => self.value = v,
            _ => return false,
        }
        true
    }
}
impl App for Demo {
    fn title(&self) -> String {
        "AuroraKit Gallery".into()
    }
    fn size(&self) -> (i32, i32) {
        (560, 480)
    }
    fn draw(&mut self, cv: &mut Canvas, r: Rect, _: &Env) {
        let view = View::column(alloc::vec![
            View::heading("Made with AuroraKit"),
            View::label("A real native app installed from a .gina package."),
            View::card(alloc::vec![
                View::button(1, &format!("Clicked {} times", self.count)),
                View::toggle(2, "A native switch", self.on),
                View::slider(3, "A native slider", self.value)
            ]),
            View::label("Click count is saved in this app's data folder.")
        ])
        .padding(28)
        .gap(20);
        self.ui.draw(cv, &view, r);
    }
    fn click(&mut self, x: i32, y: i32, _: Rect, _: &mut Env) -> bool {
        let a = self.ui.click(x, y);
        self.action(a)
    }
    fn hover(&mut self, x: i32, y: i32, _: Rect) -> bool {
        self.ui.hover(x, y)
    }
    fn drag(&mut self, x: i32, _: i32, _: Rect, _: &mut Env) -> bool {
        let a = self.ui.drag(x);
        self.action(a)
    }
    fn release(&mut self, _: i32, _: i32, _: Rect, _: &mut Env) -> bool {
        self.ui.release();
        false
    }
    fn key(&mut self, k: &KeyEvent, _: &mut Env) -> bool {
        if !k.pressed {
            return false;
        }
        let key = match k.code {
            KeyCode::Tab => {
                if k.mods.shift {
                    ui::Key::Previous
                } else {
                    ui::Key::Next
                }
            }
            KeyCode::Enter => ui::Key::Activate,
            KeyCode::Left => ui::Key::Left,
            KeyCode::Right => ui::Key::Right,
            _ => return false,
        };
        let a = self.ui.key(key);
        self.action(a);
        true
    }
}
corekit::entry!(main);
fn main(_: corekit::Args) -> i32 {
    let count = corekit::fs::read_to_string("/AppData/dev.example.gallery/count.txt")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    aurorakit::run(Demo { ui: ui::State::default(), count, on: false, value: 600 })
}
