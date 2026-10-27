//! WaveUI — the declarative interface framework of the WaveSDK.
//!
//! Describe your interface as a function of your app's state, and WaveUI
//! keeps the window in step with it: it lays the views out, draws them in
//! the WaveOS look (light and dark, the accent colour, reduced motion),
//! and handles the pointer, the keyboard, focus, scrolling, menus and
//! sheets. Your app only says what changes:
//!
//! ```ignore
//! use waveui::prelude::*;
//!
//! struct Counter { count: i64 }
//!
//! #[derive(Clone)]
//! enum Msg { Add, Reset }
//!
//! impl App for Counter {
//!     type Message = Msg;
//!     fn title(&self) -> String { "Counter".into() }
//!
//!     fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
//!         match msg {
//!             Msg::Add => self.count += 1,
//!             Msg::Reset => self.count = 0,
//!         }
//!     }
//!
//!     fn view(&self, _env: &Env) -> View<'_, Msg> {
//!         VStack::new()
//!             .push(Text::new(format!("{}", self.count)).style(TextStyle::LargeTitle))
//!             .push(HStack::new()
//!                 .push(Button::new("Reset").action(Msg::Reset))
//!                 .push(Button::new("Add").action(Msg::Add).button_style(ButtonStyle::Prominent)))
//!             .spacing(16)
//!             .fill()
//!     }
//! }
//! ```
//!
//! The pieces:
//!
//! - [`App`]: your state, its messages, `update` and `view`.
//! - [`view`]: the views (text, images, controls, stacks, lists, …) and
//!   their modifiers.
//! - [`style`]: the design system (text styles, spacing, semantic colours).
//! - [`Context`]: what `update` can ask for (timers, opening apps and
//!   files, the clipboard, focus, scrolling).

#![no_std]

extern crate alloc;

mod layout;
mod render;
mod runtime;
pub mod style;
mod symbol;
pub mod view;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

pub use aurorakit::{KeyCode, KeyEvent, Modifiers, Request};
pub use lumen;
pub use style::{Align, Color, Insets, TextStyle};
pub use view::*;

/// Everything an app usually needs: `use waveui::prelude::*;`.
pub mod prelude {
    pub use crate::style::{radius, size, space, Align, Color, Insets, TextStyle};
    pub use crate::view::*;
    pub use crate::{App, Context, Env, KeyCode, KeyEvent};
    pub use alloc::format;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec;
    pub use alloc::vec::Vec;
}

/// What views can know about their surroundings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env {
    /// Milliseconds since the computer started.
    pub now_ms: u64,
    /// The window is the active one.
    pub active: bool,
    /// The window's content size.
    pub size: (i32, i32),
    /// The screen's size.
    pub screen: (i32, i32),
    /// Dark appearance.
    pub dark: bool,
    /// The person asked for less motion.
    pub reduce_motion: bool,
    /// Modifier keys held.
    pub mods: Modifiers,
}

impl Env {
    fn new(env: &aurorakit::Env, area: lumen::geom::Rect, now: u64) -> Env {
        Env {
            now_ms: now,
            active: env.focused,
            size: (area.w, area.h),
            screen: env.screen,
            dark: lumen::theme::current().dark,
            reduce_motion: lumen::theme::reduce_motion(),
            mods: env.mods,
        }
    }
}

/// An app: its state, the messages that change it, and its interface.
pub trait App: Sized {
    /// What happens in the app: a button pressed, text typed, a timer fired.
    type Message: Clone;

    /// The window title.
    fn title(&self) -> String;

    /// The window's initial content size.
    fn size(&self) -> (i32, i32) {
        (720, 480)
    }

    /// Can the window be resized?
    fn resizable(&self) -> bool {
        true
    }

    /// Changes the state for a message. The interface is redrawn after.
    fn update(&mut self, message: Self::Message, cx: &mut Context<Self::Message>);

    /// The interface for the current state.
    fn view(&self, env: &Env) -> View<'_, Self::Message>;

    /// A sheet over the window (a dialog, an alert), while there is one.
    /// The window behind it ignores input until it goes.
    fn sheet(&self, _env: &Env) -> Option<View<'_, Self::Message>> {
        None
    }

    /// A key nothing else used (no focused control, no shortcut).
    fn key(&self, _ev: &KeyEvent) -> Option<Self::Message> {
        None
    }

    /// Called once the window is open.
    fn started(&mut self, _cx: &mut Context<Self::Message>) {}

    /// Background work (reading a pipe, the network, the clock), every
    /// [`App::poll_interval`] milliseconds. Return true to redraw.
    fn poll(&mut self, _cx: &mut Context<Self::Message>) -> bool {
        false
    }

    /// How often to call [`App::poll`] (`None`: never).
    fn poll_interval(&self) -> Option<u64> {
        None
    }

    /// The close button (or Ctrl+W). Return false to keep the window open,
    /// e.g. to ask about unsaved changes, then call [`Context::close`].
    fn close_requested(&mut self, _cx: &mut Context<Self::Message>) -> bool {
        true
    }

    /// Ctrl+W. Closes the window unless the app has tabs to close instead.
    fn close_tab_requested(&mut self, cx: &mut Context<Self::Message>) -> bool {
        self.close_requested(cx)
    }
}

/// What `update` can ask of the system and of the interface.
pub struct Context<'r, M> {
    pub(crate) rt: &'r mut runtime::Rt,
    pub(crate) requests: &'r mut Vec<Request>,
    pub(crate) timers: &'r mut Vec<(u64, M)>,
    pub(crate) queue: &'r mut VecDeque<M>,
    pub(crate) now: u64,
    pub(crate) mods: Modifiers,
}

impl<M> Context<'_, M> {
    /// Milliseconds since the computer started.
    pub fn now(&self) -> u64 {
        self.now
    }

    /// The modifier keys held during the input that led here (Shift- and
    /// Ctrl-click to extend a selection, say).
    pub fn mods(&self) -> Modifiers {
        self.mods
    }

    /// Sends another message after this one.
    pub fn send(&mut self, m: M) {
        self.queue.push_back(m);
    }

    /// Sends `m` in `ms` milliseconds.
    pub fn after(&mut self, ms: u64, m: M) {
        self.timers.push((self.now + ms, m));
    }

    /// Cancels pending timers whose message matches.
    pub fn cancel(&mut self, f: impl Fn(&M) -> bool) {
        self.timers.retain(|(_, m)| !f(m));
    }

    /// Opens an app by name ("Notes") or program path.
    pub fn open_app(&mut self, name: &str) {
        self.requests.push(Request::OpenApp(name.into()));
    }

    /// Opens a document with its default app.
    pub fn open_file(&mut self, path: &str) {
        self.requests.push(Request::OpenFile(path.into()));
    }

    /// Closes the window (and ends the app).
    pub fn close(&mut self) {
        self.requests.push(Request::Close);
    }

    /// Asks the system for something else (appearance, wallpaper, power).
    pub fn request(&mut self, r: Request) {
        self.requests.push(r);
    }

    /// Puts text on the clipboard.
    pub fn copy(&mut self, text: &str) {
        corekit::clipboard::set_text(text);
    }

    /// The clipboard's text.
    pub fn paste(&self) -> Option<String> {
        corekit::clipboard::text()
    }

    /// Shows a notification.
    pub fn notify(&mut self, title: &str, body: &str) {
        corekit::notify::post(title, body);
    }
}

/// Runs `app` in a window until it closes. Returns the exit code.
pub fn run<A: App>(app: A) -> i32 {
    aurorakit::run(runtime::Host::new(app))
}
