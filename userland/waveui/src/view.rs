//! Views: what an app's interface is made of.
//!
//! A view is a value describing part of the interface — a text, a button,
//! a stack of other views — built fresh from the app's state each time it
//! changes. Modifiers (`.padding()`, `.foreground()`, `.on_tap()`, …)
//! adjust a view and return it, so interfaces read top to bottom:
//!
//! ```ignore
//! VStack::new()
//!     .push(Text::new("Volume").style(TextStyle::Headline))
//!     .push(Slider::new(self.volume, 0.0..=1.0).on_change(Msg::Volume))
//!     .spacing(8)
//!     .padding(16)
//! ```
//!
//! Interaction produces the app's own messages (`M`); callbacks only build
//! messages (`Msg::Volume` is one), they don't touch the app.

use crate::style::{Align, Color, Insets, TextStyle};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use lumen::canvas::Canvas as Surface;
use lumen::geom::Rect;

/// A callback turning a control's new value into a message.
pub type Handler<T, M> = Rc<dyn Fn(T) -> M>;

/// Horizontal or vertical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Small built-in symbols, drawn as vectors in the current colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    Check,
    Plus,
    Minus,
    Close,
    Search,
    More,
    Star,
    StarFill,
    Info,
    Warning,
    Trash,
    Share,
    Refresh,
    Play,
    Pause,
    Stop,
    Flag,
    Gear,
    Home,
    Grid,
    List,
    Folder,
    Document,
    Globe,
    Lock,
    Person,
    Bell,
    Moon,
    Sun,
    Wifi,
    Speaker,
    Keyboard,
    Mouse,
    Gamepad,
    Display,
    Battery,
    Clock,
    Download,
    Sidebar,
    Pencil,
    Eraser,
    Circle,
    Square,
    Line,
    Text,
    Undo,
    Redo,
    Copy,
    Eject,
    Calendar,
    Timer,
    Stopwatch,
}

/// A keyboard shortcut (Ctrl stands in for ⌘).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Delete,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

impl Shortcut {
    /// Ctrl + a key (⌘ on other systems).
    pub const fn ctrl(c: char) -> Shortcut {
        Shortcut { key: Key::Char(c), ctrl: true, shift: false, alt: false }
    }
    pub const fn ctrl_shift(c: char) -> Shortcut {
        Shortcut { key: Key::Char(c), ctrl: true, shift: true, alt: false }
    }
    pub const fn key(key: Key) -> Shortcut {
        Shortcut { key, ctrl: false, shift: false, alt: false }
    }
    /// Return: the default button of a sheet or form.
    pub const fn default_action() -> Shortcut {
        Shortcut::key(Key::Enter)
    }
    /// Escape: Cancel.
    pub const fn cancel() -> Shortcut {
        Shortcut::key(Key::Escape)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonStyle {
    /// A raised button (the default).
    #[default]
    Bordered,
    /// The main action: accent-filled.
    Prominent,
    /// Destructive: red.
    Destructive,
    /// Just the label, accent-coloured (links, toolbar actions).
    Plain,
    /// A borderless icon or label that shows a fill on hover (toolbars).
    Toolbar,
    /// A row of a list or a sidebar: full width, highlighted when hovered.
    Row,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ControlSize {
    Small,
    #[default]
    Regular,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PickerStyle {
    /// Side by side segments.
    #[default]
    Segmented,
    /// A button showing the choice, with a menu of the others.
    Menu,
    /// One per line, with a check mark.
    Inline,
}

/// What a canvas view hears (coordinates relative to its top left).
#[derive(Clone, Copy, Debug)]
pub enum CanvasEvent {
    Press {
        x: i32,
        y: i32,
        button: u8,
        clicks: u32,
    },
    Drag {
        x: i32,
        y: i32,
    },
    Release {
        x: i32,
        y: i32,
    },
    Hover {
        x: i32,
        y: i32,
    },
    Leave,
    /// Scrolling, in pixels (positive: down, right).
    Scroll {
        dx: i32,
        dy: i32,
    },
    Key(aurorakit::KeyEvent),
    /// Its size, when it changes.
    Resize {
        w: i32,
        h: i32,
    },
}

/// A context menu entry.
#[derive(Clone)]
pub enum MenuItem<M> {
    Action { label: String, message: M, enabled: bool, destructive: bool, shortcut: Option<Shortcut> },
    Separator,
}

impl<M> MenuItem<M> {
    pub fn new(label: impl Into<String>, message: M) -> Self {
        MenuItem::Action { label: label.into(), message, enabled: true, destructive: false, shortcut: None }
    }
    pub fn destructive(label: impl Into<String>, message: M) -> Self {
        MenuItem::Action { label: label.into(), message, enabled: true, destructive: true, shortcut: None }
    }
    pub fn enabled(self, on: bool) -> Self {
        match self {
            MenuItem::Action { label, message, destructive, shortcut, .. } => {
                MenuItem::Action { label, message, enabled: on, destructive, shortcut }
            }
            s => s,
        }
    }
    pub fn shortcut(self, s: Shortcut) -> Self {
        match self {
            MenuItem::Action { label, message, enabled, destructive, .. } => {
                MenuItem::Action { label, message, enabled, destructive, shortcut: Some(s) }
            }
            s => s,
        }
    }
}

/// Decoded pixels an [`Image`] can show (shared, so views can hold it cheaply).
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl Pixmap {
    /// Decodes a PNG, JPEG or BMP file.
    pub fn load(path: &str) -> Option<Rc<Pixmap>> {
        let data = corekit::fs::read(path).ok()?;
        let img = aurora_image::decode(&data).ok()?;
        Some(Rc::new(Pixmap { width: img.width, height: img.height, pixels: img.pixels }))
    }
}

/// How an image fits its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ContentMode {
    /// Whole image visible.
    #[default]
    Fit,
    /// Frame filled, edges cropped.
    Fill,
}

pub(crate) enum ImageSource {
    Pixmap(Rc<Pixmap>),
    AppIcon(lumen::icons::Icon),
    Symbol(Symbol),
}

/// What a view hands over when it is dragged out.
#[derive(Clone, Debug)]
pub enum DragItem {
    /// Files, with an icon from `corekit::abi::drag`.
    Files(Vec<String>, usize),
    Text(String),
}

/// A drop landing on a view (position relative to the view).
pub struct DropInfo {
    pub x: i32,
    pub y: i32,
    pub items: corekit::dnd::Dropped,
}

pub(crate) type DrawFn<'a> = Box<dyn Fn(&mut Surface, Rect) + 'a>;
pub(crate) type RowFn<'a, M> = Box<dyn Fn(usize) -> View<'a, M> + 'a>;

pub(crate) enum Node<'a, M> {
    Empty,
    Spacer {
        min: i32,
    },
    Divider,
    Text {
        text: String,
        lines: Option<u32>,
        align: Align,
    },
    Image {
        source: ImageSource,
        mode: ContentMode,
        size: Option<(i32, i32)>,
    },
    Button {
        label: Box<View<'a, M>>,
        action: Option<M>,
        style: ButtonStyle,
        size: ControlSize,
        menu: Vec<MenuItem<M>>,
    },
    Toggle {
        label: String,
        on: bool,
        action: Option<Handler<bool, M>>,
    },
    Checkbox {
        label: String,
        on: bool,
        action: Option<Handler<bool, M>>,
    },
    Slider {
        value: f32,
        min: f32,
        max: f32,
        step: Option<f32>,
        action: Option<Handler<f32, M>>,
    },
    Stepper {
        label: String,
        value: i64,
        min: i64,
        max: i64,
        step: i64,
        action: Option<Handler<i64, M>>,
    },
    Picker {
        options: Vec<String>,
        selected: usize,
        style: PickerStyle,
        action: Option<Handler<usize, M>>,
    },
    TextField {
        value: String,
        placeholder: String,
        secure: bool,
        action: Option<Handler<String, M>>,
        submit: Option<M>,
        search: bool,
    },
    TextEditor {
        value: String,
        mono: bool,
        action: Option<Handler<String, M>>,
        caret: Option<Handler<(usize, usize), M>>,
        highlights: Vec<(usize, usize)>,
        current: Option<usize>,
        placeholder: String,
        rust: bool,
    },
    Progress {
        value: Option<f32>,
        circular: bool,
    },
    Stack {
        axis: Axis,
        spacing: i32,
        align: Align,
        children: Vec<View<'a, M>>,
    },
    ZStack {
        align: Align,
        children: Vec<View<'a, M>>,
    },
    Scroll {
        axis: Axis,
        child: Box<View<'a, M>>,
        stick_to_end: bool,
    },
    List {
        count: usize,
        row_height: i32,
        rows: RowFn<'a, M>,
        selection: Option<usize>,
        on_select: Option<Handler<usize, M>>,
        on_activate: Option<Handler<usize, M>>,
    },
    Grid {
        min_width: i32,
        spacing: i32,
        children: Vec<View<'a, M>>,
    },
    Card {
        children: Vec<View<'a, M>>,
        separators: bool,
        inset: i32,
    },
    Split {
        sidebar: Box<View<'a, M>>,
        detail: Box<View<'a, M>>,
        width: i32,
    },
    Canvas {
        draw: DrawFn<'a>,
        input: Option<Handler<CanvasEvent, Option<M>>>,
        focusable: bool,
    },
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Frame {
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub min_width: Option<i32>,
    pub min_height: Option<i32>,
    pub max_width: Option<i32>,
    pub max_height: Option<i32>,
    pub align: Align,
}

pub(crate) struct Mods<'a, M> {
    pub padding: Insets,
    pub frame: Frame,
    /// Takes extra space along a stack (Spacer-like), weighted.
    pub grow: u16,
    pub background: Option<(Color, i32)>,
    pub material: bool,
    pub border: Option<(Color, i32)>,
    pub shadow: bool,
    pub foreground: Option<Color>,
    pub tint: Option<Color>,
    pub corner: Option<i32>,
    pub style: Option<TextStyle>,
    pub opacity: Option<u8>,
    pub disabled: bool,
    pub hidden: bool,
    pub selected: bool,
    pub on_tap: Option<M>,
    pub on_double_tap: Option<M>,
    pub on_hover: Option<Handler<bool, M>>,
    pub context_menu: Vec<MenuItem<M>>,
    pub help: Option<String>,
    pub id: Option<u64>,
    pub shortcut: Option<Shortcut>,
    pub on_drop: Option<Handler<DropInfo, Option<M>>>,
    pub drag: Option<DragItem>,
    pub focus_on_appear: bool,
    pub clip: bool,
    pub custom: Option<DrawFn<'a>>,
    pub label: Option<String>,
}

impl<M> Default for Mods<'_, M> {
    fn default() -> Self {
        Mods {
            padding: Insets::default(),
            frame: Frame::default(),
            grow: 0,
            background: None,
            material: false,
            border: None,
            shadow: false,
            foreground: None,
            tint: None,
            corner: None,
            style: None,
            opacity: None,
            disabled: false,
            hidden: false,
            selected: false,
            on_tap: None,
            on_double_tap: None,
            on_hover: None,
            context_menu: Vec::new(),
            help: None,
            id: None,
            shortcut: None,
            on_drop: None,
            drag: None,
            focus_on_appear: false,
            clip: false,
            custom: None,
            label: None,
        }
    }
}

/// A piece of interface. Build one with the constructors in this module
/// ([`Text`], [`Button`], [`VStack`], …) and adjust it with modifiers.
pub struct View<'a, M> {
    pub(crate) node: Node<'a, M>,
    pub(crate) mods: Mods<'a, M>,
}

impl<'a, M> View<'a, M> {
    pub(crate) fn of(node: Node<'a, M>) -> Self {
        View { node, mods: Mods::default() }
    }

    /// Nothing (takes no space).
    pub fn empty() -> Self {
        View::of(Node::Empty)
    }

    // ------------------------------------------------------- modifiers

    /// Space around the view, on every side.
    pub fn padding(mut self, v: i32) -> Self {
        self.mods.padding = Insets::all(v);
        self
    }
    pub fn padding_xy(mut self, x: i32, y: i32) -> Self {
        self.mods.padding = Insets::xy(x, y);
        self
    }
    pub fn padding_insets(mut self, i: Insets) -> Self {
        self.mods.padding = i;
        self
    }
    /// A fixed width.
    pub fn width(mut self, w: i32) -> Self {
        self.mods.frame.width = Some(w);
        self
    }
    /// A fixed height.
    pub fn height(mut self, h: i32) -> Self {
        self.mods.frame.height = Some(h);
        self
    }
    pub fn size(self, w: i32, h: i32) -> Self {
        self.width(w).height(h)
    }
    pub fn min_width(mut self, w: i32) -> Self {
        self.mods.frame.min_width = Some(w);
        self
    }
    pub fn max_width(mut self, w: i32) -> Self {
        self.mods.frame.max_width = Some(w);
        self
    }
    pub fn min_height(mut self, h: i32) -> Self {
        self.mods.frame.min_height = Some(h);
        self
    }
    pub fn max_height(mut self, h: i32) -> Self {
        self.mods.frame.max_height = Some(h);
        self
    }
    /// Takes all the width it is offered.
    pub fn fill_width(mut self) -> Self {
        self.mods.frame.max_width = Some(i32::MAX);
        self
    }
    /// Takes all the height it is offered.
    pub fn fill_height(mut self) -> Self {
        self.mods.frame.max_height = Some(i32::MAX);
        self
    }
    /// Takes all the space it is offered.
    pub fn fill(self) -> Self {
        self.fill_width().fill_height()
    }
    /// Where the content sits when the frame is larger than it.
    pub fn align(mut self, a: Align) -> Self {
        self.mods.frame.align = a;
        self
    }
    /// Shares the extra space along its stack with other growing views.
    pub fn grow(mut self, weight: u16) -> Self {
        self.mods.grow = weight;
        self
    }
    pub fn background(mut self, c: Color) -> Self {
        self.mods.background = Some((c, 0));
        self
    }
    /// A rounded background.
    pub fn background_rounded(mut self, c: Color, radius: i32) -> Self {
        self.mods.background = Some((c, radius));
        self
    }
    /// See-through glass showing what is behind the window (sidebars).
    pub fn material(mut self) -> Self {
        self.mods.material = true;
        self
    }
    pub fn border(mut self, c: Color, radius: i32) -> Self {
        self.mods.border = Some((c, radius));
        self
    }
    /// A soft drop shadow (cards that float).
    pub fn shadow(mut self) -> Self {
        self.mods.shadow = true;
        self
    }
    /// The colour of text and symbols inside.
    pub fn foreground(mut self, c: Color) -> Self {
        self.mods.foreground = Some(c);
        self
    }
    /// The colour of a control (a button's fill, say) in place of the accent.
    pub fn tint(mut self, c: Color) -> Self {
        self.mods.tint = Some(c);
        self
    }
    /// Rounds the corners of the view's background, selection and border
    /// (and of a button) to `r` pixels.
    pub fn corner_radius(mut self, r: i32) -> Self {
        self.mods.corner = Some(r);
        self
    }
    /// The text style of text inside.
    pub fn style(mut self, s: TextStyle) -> Self {
        self.mods.style = Some(s);
        self
    }
    /// 0 (invisible) to 255.
    pub fn opacity(mut self, a: u8) -> Self {
        self.mods.opacity = Some(a);
        self
    }
    /// Greyed out and not interactive.
    pub fn disabled(mut self, d: bool) -> Self {
        self.mods.disabled = d;
        self
    }
    /// Not shown, taking no space.
    pub fn hidden(mut self, h: bool) -> Self {
        self.mods.hidden = h;
        self
    }
    /// Highlighted as the selection (list rows, sidebar items).
    pub fn selected(mut self, s: bool) -> Self {
        self.mods.selected = s;
        self
    }
    /// Sends `m` when clicked (or activated from the keyboard).
    pub fn on_tap(mut self, m: M) -> Self {
        self.mods.on_tap = Some(m);
        self
    }
    pub fn on_double_tap(mut self, m: M) -> Self {
        self.mods.on_double_tap = Some(m);
        self
    }
    /// Told when the pointer enters (`true`) and leaves.
    pub fn on_hover(mut self, f: impl Fn(bool) -> M + 'static) -> Self {
        self.mods.on_hover = Some(Rc::new(f));
        self
    }
    /// A menu on secondary click.
    pub fn context_menu(mut self, items: Vec<MenuItem<M>>) -> Self {
        self.mods.context_menu = items;
        self
    }
    /// A tooltip.
    pub fn help(mut self, text: impl Into<String>) -> Self {
        self.mods.help = Some(text.into());
        self
    }
    /// Keeps the view's state (scroll position, text being edited) across
    /// rebuilds when its place in the tree may change.
    pub fn id(mut self, key: impl core::hash::Hash) -> Self {
        self.mods.id = Some(crate::layout::key_of(&key));
        self
    }
    /// A keyboard shortcut that activates it (buttons, `on_tap` views).
    pub fn shortcut(mut self, s: Shortcut) -> Self {
        self.mods.shortcut = Some(s);
        self
    }
    /// Accepts dropped items.
    pub fn on_drop(mut self, f: impl Fn(DropInfo) -> Option<M> + 'static) -> Self {
        self.mods.on_drop = Some(Rc::new(f));
        self
    }
    /// Can be dragged out (to other views or apps).
    pub fn draggable(mut self, item: DragItem) -> Self {
        self.mods.drag = Some(item);
        self
    }
    /// Takes keyboard focus when it first appears (the first field of a sheet).
    pub fn focused(mut self) -> Self {
        self.mods.focus_on_appear = true;
        self
    }
    /// Clips what it draws to its frame.
    pub fn clipped(mut self) -> Self {
        self.mods.clip = true;
        self
    }
    /// Draws extra decoration over the view.
    pub fn overlay_draw(mut self, f: impl Fn(&mut Surface, Rect) + 'a) -> Self {
        self.mods.custom = Some(Box::new(f));
        self
    }
    /// What a screen reader says.
    pub fn accessibility_label(mut self, l: impl Into<String>) -> Self {
        self.mods.label = Some(l.into());
        self
    }
}

// ------------------------------------------------------------ constructors

/// Text, one line by default; `.wrap()` for paragraphs.
pub struct Text;
impl Text {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(s: impl Into<String>) -> View<'a, M> {
        View::of(Node::Text { text: s.into(), lines: Some(1), align: Align::Start })
    }
    /// A paragraph that wraps onto as many lines as it needs.
    pub fn paragraph<'a, M>(s: impl Into<String>) -> View<'a, M> {
        View::of(Node::Text { text: s.into(), lines: None, align: Align::Start })
    }
    /// A paragraph aligned in the centre.
    pub fn centered<'a, M>(s: impl Into<String>) -> View<'a, M> {
        View::of(Node::Text { text: s.into(), lines: None, align: Align::Center })
    }
}

impl<'a, M> View<'a, M> {
    /// For text: wrap onto at most `n` lines (0: as many as needed).
    pub fn lines(mut self, n: u32) -> Self {
        if let Node::Text { lines, .. } = &mut self.node {
            *lines = if n == 0 { None } else { Some(n) };
        }
        self
    }
    /// For text: align its lines.
    pub fn text_align(mut self, a: Align) -> Self {
        if let Node::Text { align, .. } = &mut self.node {
            *align = a;
        }
        self
    }
}

/// An image: a picture, an app icon or a symbol.
pub struct Image;
impl Image {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(p: Rc<Pixmap>) -> View<'a, M> {
        View::of(Node::Image { source: ImageSource::Pixmap(p), mode: ContentMode::Fit, size: None })
    }
    /// An app's icon at `size` pixels.
    pub fn app_icon<'a, M>(icon: lumen::icons::Icon, size: i32) -> View<'a, M> {
        View::of(Node::Image { source: ImageSource::AppIcon(icon), mode: ContentMode::Fit, size: Some((size, size)) })
    }
    /// A symbol at the text size (16 px unless framed otherwise).
    pub fn symbol<'a, M>(s: Symbol) -> View<'a, M> {
        View::of(Node::Image { source: ImageSource::Symbol(s), mode: ContentMode::Fit, size: Some((16, 16)) })
    }
}

impl<'a, M> View<'a, M> {
    /// For images: fit or fill the frame.
    pub fn content_mode(mut self, m: ContentMode) -> Self {
        if let Node::Image { mode, .. } = &mut self.node {
            *mode = m;
        }
        self
    }
    /// For symbols and icons: their size.
    pub fn symbol_size(mut self, s: i32) -> Self {
        if let Node::Image { size, .. } = &mut self.node {
            *size = Some((s, s));
        }
        self
    }
}

/// A symbol and a text side by side.
pub fn label<'a, M>(symbol: Symbol, text: impl Into<String>) -> View<'a, M> {
    HStack::new().push(Image::symbol(symbol)).push(Text::new(text)).spacing(6)
}

/// A clickable button.
pub struct Button;
impl Button {
    /// A button with a text label.
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(title: impl Into<String>) -> View<'a, M> {
        Button::with(Text::new(title))
    }
    /// A button showing any view.
    pub fn with<'a, M>(label: View<'a, M>) -> View<'a, M> {
        View::of(Node::Button {
            label: Box::new(label),
            action: None,
            style: ButtonStyle::Bordered,
            size: ControlSize::Regular,
            menu: Vec::new(),
        })
    }
    /// A symbol-only button (toolbars). Give it `.help()` text.
    pub fn symbol<'a, M>(s: Symbol) -> View<'a, M> {
        let mut b = Button::with(Image::symbol(s));
        if let Node::Button { style, .. } = &mut b.node {
            *style = ButtonStyle::Toolbar;
        }
        b
    }
}

impl<'a, M> View<'a, M> {
    /// For buttons: what clicking sends.
    pub fn action(mut self, m: M) -> Self {
        if let Node::Button { action, .. } = &mut self.node {
            *action = Some(m);
        } else {
            self.mods.on_tap = Some(m);
        }
        self
    }
    /// For buttons: their look.
    pub fn button_style(mut self, s: ButtonStyle) -> Self {
        if let Node::Button { style, .. } = &mut self.node {
            *style = s;
        }
        self
    }
    /// For controls: their size.
    pub fn control_size(mut self, s: ControlSize) -> Self {
        if let Node::Button { size, .. } = &mut self.node {
            *size = s;
        }
        self
    }
    /// For buttons: a menu that opens on click instead of an action.
    pub fn menu(mut self, items: Vec<MenuItem<M>>) -> Self {
        if let Node::Button { menu, .. } = &mut self.node {
            *menu = items;
        }
        self
    }
}

/// An on/off switch with a label on its left.
pub struct Toggle;
impl Toggle {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(label: impl Into<String>, on: bool) -> View<'a, M> {
        View::of(Node::Toggle { label: label.into(), on, action: None })
    }
    /// A check box with a label on its right.
    pub fn checkbox<'a, M>(label: impl Into<String>, on: bool) -> View<'a, M> {
        View::of(Node::Checkbox { label: label.into(), on, action: None })
    }
}

/// A value along a track.
pub struct Slider;
impl Slider {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(value: f32, range: core::ops::RangeInclusive<f32>) -> View<'a, M> {
        View::of(Node::Slider { value, min: *range.start(), max: *range.end(), step: None, action: None })
    }
}

impl<'a, M> View<'a, M> {
    /// For sliders: snap to multiples of `step`.
    pub fn step(mut self, s: f32) -> Self {
        if let Node::Slider { step, .. } = &mut self.node {
            *step = Some(s);
        }
        self
    }
}

/// A number with − and + buttons.
pub struct Stepper;
impl Stepper {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(label: impl Into<String>, value: i64, range: core::ops::RangeInclusive<i64>) -> View<'a, M> {
        View::of(Node::Stepper {
            label: label.into(),
            value,
            min: *range.start(),
            max: *range.end(),
            step: 1,
            action: None,
        })
    }
}

/// One choice among several.
pub struct Picker;
impl Picker {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M, S: Into<String>>(options: impl IntoIterator<Item = S>, selected: usize) -> View<'a, M> {
        View::of(Node::Picker {
            options: options.into_iter().map(Into::into).collect(),
            selected,
            style: PickerStyle::Segmented,
            action: None,
        })
    }
}

impl<'a, M> View<'a, M> {
    pub fn picker_style(mut self, s: PickerStyle) -> Self {
        if let Node::Picker { style, .. } = &mut self.node {
            *style = s;
        }
        self
    }
}

/// Editable text, one line.
pub struct TextField;
impl TextField {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(placeholder: impl Into<String>, value: impl Into<String>) -> View<'a, M> {
        View::of(Node::TextField {
            value: value.into(),
            placeholder: placeholder.into(),
            secure: false,
            action: None,
            submit: None,
            search: false,
        })
    }
    /// For passwords: shows dots.
    pub fn secure<'a, M>(placeholder: impl Into<String>, value: impl Into<String>) -> View<'a, M> {
        let mut v = TextField::new(placeholder, value);
        if let Node::TextField { secure, .. } = &mut v.node {
            *secure = true;
        }
        v
    }
    /// A rounded search field with a magnifying glass.
    pub fn search<'a, M>(placeholder: impl Into<String>, value: impl Into<String>) -> View<'a, M> {
        let mut v = TextField::new(placeholder, value);
        if let Node::TextField { search, .. } = &mut v.node {
            *search = true;
        }
        v
    }
}

/// Editable text, many lines (scrolls, wraps).
pub struct TextEditor;
impl TextEditor {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(value: impl Into<String>) -> View<'a, M> {
        View::of(Node::TextEditor {
            value: value.into(),
            mono: false,
            action: None,
            caret: None,
            highlights: Vec::new(),
            current: None,
            placeholder: String::new(),
            rust: false,
        })
    }
    pub fn monospaced<'a, M>(value: impl Into<String>) -> View<'a, M> {
        let mut v = TextEditor::new(value);
        if let Node::TextEditor { mono, .. } = &mut v.node {
            *mono = true;
        }
        v
    }
}

impl<'a, M> View<'a, M> {
    /// For text editors: told the caret's line and column (1-based) when it moves.
    pub fn on_caret(mut self, f: impl Fn((usize, usize)) -> M + 'static) -> Self {
        if let Node::TextEditor { caret, .. } = &mut self.node {
            *caret = Some(Rc::new(f));
        }
        self
    }
    /// For text editors: byte ranges to highlight (find matches), `current` stronger.
    pub fn highlights(mut self, ranges: Vec<(usize, usize)>, current: Option<usize>) -> Self {
        if let Node::TextEditor { highlights, current: c, .. } = &mut self.node {
            *highlights = ranges;
            *c = current;
        }
        self
    }
    /// For text editors: what shows while empty.
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        if let Node::TextEditor { placeholder, .. } = &mut self.node {
            *placeholder = text.into();
        }
        self
    }
    /// For text editors: colours Rust keywords, strings and comments.
    pub fn rust_syntax(mut self) -> Self {
        if let Node::TextEditor { rust, .. } = &mut self.node {
            *rust = true;
        }
        self
    }
}

impl<'a, M> View<'a, M> {
    /// For controls with a value: what a new value sends.
    pub fn on_change<T: 'static>(mut self, f: impl Fn(T) -> M + 'static) -> Self
    where
        Self: SetHandler<T, M>,
    {
        self.set_handler(Rc::new(f));
        self
    }
    /// For text fields: what Return sends.
    pub fn on_submit(mut self, m: M) -> Self {
        if let Node::TextField { submit, .. } = &mut self.node {
            *submit = Some(m);
        }
        self
    }
    /// For lists: what selecting a row sends.
    pub fn on_select(mut self, f: impl Fn(usize) -> M + 'static) -> Self {
        if let Node::List { on_select, .. } = &mut self.node {
            *on_select = Some(Rc::new(f));
        }
        self
    }
    /// For lists: what double-clicking (or Return on) a row sends.
    pub fn on_activate(mut self, f: impl Fn(usize) -> M + 'static) -> Self {
        if let Node::List { on_activate, .. } = &mut self.node {
            *on_activate = Some(Rc::new(f));
        }
        self
    }
}

/// Controls that report a value of type `T`.
pub trait SetHandler<T, M> {
    fn set_handler(&mut self, h: Handler<T, M>);
}

impl<M> SetHandler<bool, M> for View<'_, M> {
    fn set_handler(&mut self, h: Handler<bool, M>) {
        match &mut self.node {
            Node::Toggle { action, .. } | Node::Checkbox { action, .. } => *action = Some(h),
            _ => {}
        }
    }
}
impl<M> SetHandler<f32, M> for View<'_, M> {
    fn set_handler(&mut self, h: Handler<f32, M>) {
        if let Node::Slider { action, .. } = &mut self.node {
            *action = Some(h);
        }
    }
}
impl<M> SetHandler<i64, M> for View<'_, M> {
    fn set_handler(&mut self, h: Handler<i64, M>) {
        if let Node::Stepper { action, .. } = &mut self.node {
            *action = Some(h);
        }
    }
}
impl<M> SetHandler<usize, M> for View<'_, M> {
    fn set_handler(&mut self, h: Handler<usize, M>) {
        if let Node::Picker { action, .. } = &mut self.node {
            *action = Some(h);
        }
    }
}
impl<M> SetHandler<String, M> for View<'_, M> {
    fn set_handler(&mut self, h: Handler<String, M>) {
        match &mut self.node {
            Node::TextField { action, .. } | Node::TextEditor { action, .. } => *action = Some(h),
            _ => {}
        }
    }
}

/// A progress bar (`None`: busy, without a known amount).
pub struct ProgressView;
impl ProgressView {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(value: Option<f32>) -> View<'a, M> {
        View::of(Node::Progress { value, circular: false })
    }
    /// A small spinning indicator.
    pub fn spinner<'a, M>() -> View<'a, M> {
        View::of(Node::Progress { value: None, circular: true })
    }
}

/// Flexible empty space along a stack.
pub struct Spacer;
impl Spacer {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>() -> View<'a, M> {
        View::of(Node::Spacer { min: 0 })
    }
    pub fn min<'a, M>(min: i32) -> View<'a, M> {
        View::of(Node::Spacer { min })
    }
}

/// A hairline across a stack.
pub struct Divider;
impl Divider {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>() -> View<'a, M> {
        View::of(Node::Divider)
    }
}

macro_rules! stack {
    ($name:ident, $axis:expr, $doc:literal) => {
        #[doc = $doc]
        pub struct $name;
        impl $name {
            #[allow(clippy::new_ret_no_self)]
            pub fn new<'a, M>() -> View<'a, M> {
                View::of(Node::Stack { axis: $axis, spacing: 8, align: Align::Center, children: Vec::new() })
            }
            pub fn with<'a, M>(children: Vec<View<'a, M>>) -> View<'a, M> {
                View::of(Node::Stack { axis: $axis, spacing: 8, align: Align::Center, children })
            }
        }
    };
}

stack!(VStack, Axis::Vertical, "Views one above the other.");
stack!(HStack, Axis::Horizontal, "Views side by side.");

/// Views on top of each other, the first at the back.
pub struct ZStack;
impl ZStack {
    #[allow(clippy::new_ret_no_self)]
    pub fn with<'a, M>(children: Vec<View<'a, M>>) -> View<'a, M> {
        View::of(Node::ZStack { align: Align::Center, children })
    }
}

impl<'a, M> View<'a, M> {
    /// For stacks and groups: adds a child.
    pub fn push(mut self, child: View<'a, M>) -> Self {
        match &mut self.node {
            Node::Stack { children, .. }
            | Node::ZStack { children, .. }
            | Node::Grid { children, .. }
            | Node::Card { children, .. } => children.push(child),
            _ => {}
        }
        self
    }
    /// For stacks and groups: adds a child if there is one.
    pub fn push_if(self, child: Option<View<'a, M>>) -> Self {
        match child {
            Some(c) => self.push(c),
            None => self,
        }
    }
    /// For stacks and groups: adds children.
    pub fn extend(mut self, more: impl IntoIterator<Item = View<'a, M>>) -> Self {
        for c in more {
            self = self.push(c);
        }
        self
    }
    /// For stacks and grids: space between children.
    pub fn spacing(mut self, s: i32) -> Self {
        match &mut self.node {
            Node::Stack { spacing, .. } | Node::Grid { spacing, .. } => *spacing = s,
            _ => {}
        }
        self
    }
    /// For stacks: how children line up across the stack.
    pub fn alignment(mut self, a: Align) -> Self {
        match &mut self.node {
            Node::Stack { align, .. } | Node::ZStack { align, .. } => *align = a,
            _ => self.mods.frame.align = a,
        }
        self
    }
}

/// Scrolls its content when it is larger than the space it has.
pub struct ScrollView;
impl ScrollView {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(content: View<'a, M>) -> View<'a, M> {
        View::of(Node::Scroll { axis: Axis::Vertical, child: Box::new(content), stick_to_end: false })
    }
    pub fn horizontal<'a, M>(content: View<'a, M>) -> View<'a, M> {
        View::of(Node::Scroll { axis: Axis::Horizontal, child: Box::new(content), stick_to_end: false })
    }
}

impl<'a, M> View<'a, M> {
    /// For scroll views: stay scrolled to the end as content grows (logs, chats).
    pub fn stick_to_end(mut self) -> Self {
        if let Node::Scroll { stick_to_end, .. } = &mut self.node {
            *stick_to_end = true;
        }
        self
    }
}

/// Many rows of equal height, built only when visible, with a selection
/// that follows the arrow keys.
pub struct List;
impl List {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(count: usize, row_height: i32, rows: impl Fn(usize) -> View<'a, M> + 'a) -> View<'a, M> {
        View::of(Node::List {
            count,
            row_height,
            rows: Box::new(rows),
            selection: None,
            on_select: None,
            on_activate: None,
        })
    }
}

impl<'a, M> View<'a, M> {
    /// For lists: the selected row.
    pub fn selection(mut self, s: Option<usize>) -> Self {
        if let Node::List { selection, .. } = &mut self.node {
            *selection = s;
        }
        self
    }
}

/// Tiles in as many columns as fit, each at least `min_width` wide.
pub struct Grid;
impl Grid {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(min_width: i32) -> View<'a, M> {
        View::of(Node::Grid { min_width, spacing: 12, children: Vec::new() })
    }
}

/// Grouped rows on a rounded card, as in Settings.
pub struct Section;
impl Section {
    /// A card of rows, with hairlines between them.
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>() -> View<'a, M> {
        View::of(Node::Card { children: Vec::new(), separators: true, inset: 14 })
    }
    /// A titled section: the title above the card.
    pub fn titled<'a, M>(title: impl Into<String>, card: View<'a, M>) -> View<'a, M> {
        VStack::new()
            .push(Text::new(title).style(TextStyle::Headline).foreground(Color::SecondaryLabel).padding_xy(6, 0))
            .push(card.fill_width())
            .spacing(6)
            .alignment(Align::Start)
    }
    /// A plain card without separators.
    pub fn card<'a, M>(content: View<'a, M>) -> View<'a, M> {
        View::of(Node::Card { children: alloc::vec![content], separators: false, inset: 0 })
    }
}

/// One row of a section: a title (and subtitle) on the left, an accessory
/// (a control, a value) on the right.
pub fn row<'a, M>(title: impl Into<String>, accessory: View<'a, M>) -> View<'a, M> {
    HStack::new()
        .push(Text::new(title))
        .push(Spacer::new())
        .push(accessory)
        .min_height(crate::style::size::ROW)
        .fill_width()
}

/// A row with a subtitle under the title.
pub fn row_detail<'a, M>(title: impl Into<String>, detail: impl Into<String>, accessory: View<'a, M>) -> View<'a, M> {
    HStack::new()
        .push(
            VStack::new()
                .push(Text::new(title))
                .push(Text::paragraph(detail).style(TextStyle::Subheadline).foreground(Color::SecondaryLabel))
                .spacing(2)
                .alignment(Align::Start)
                .grow(1),
        )
        .push(accessory)
        .spacing(12)
        .min_height(crate::style::size::ROW + 8)
        .fill_width()
}

/// A sidebar beside the content, as in Settings and Files.
pub struct SplitView;
impl SplitView {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(sidebar: View<'a, M>, detail: View<'a, M>) -> View<'a, M> {
        View::of(Node::Split { sidebar: Box::new(sidebar), detail: Box::new(detail), width: 220 })
    }
}

impl<'a, M> View<'a, M> {
    /// For split views: the sidebar's width.
    pub fn sidebar_width(mut self, w: i32) -> Self {
        if let Node::Split { width, .. } = &mut self.node {
            *width = w;
        }
        self
    }
}

/// An item of a sidebar: a symbol and a title, highlighted when selected.
pub fn sidebar_item<'a, M>(symbol: Symbol, title: impl Into<String>, selected: bool) -> View<'a, M> {
    HStack::new()
        .push(Image::symbol(symbol).foreground(Color::Accent))
        .push(Text::new(title).grow(1))
        .spacing(10)
        .padding_xy(10, 0)
        .height(crate::style::size::SIDEBAR_ROW)
        .fill_width()
        .selected(selected)
}

/// A bar of tools at the top of a window's content.
pub fn toolbar<'a, M>(items: Vec<View<'a, M>>) -> View<'a, M> {
    HStack::with(items).spacing(6).padding_xy(12, 0).height(crate::style::size::TOOLBAR).fill_width()
}

/// Draws anything, and hears the pointer and keys if asked (paintings,
/// web pages, terminals, charts).
pub struct Canvas;
impl Canvas {
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, M>(draw: impl Fn(&mut Surface, Rect) + 'a) -> View<'a, M> {
        View::of(Node::Canvas { draw: Box::new(draw), input: None, focusable: false })
    }
}

impl<'a, M> View<'a, M> {
    /// For canvases: what pointer, scroll and key events send.
    pub fn on_input(mut self, f: impl Fn(CanvasEvent) -> Option<M> + 'static) -> Self {
        if let Node::Canvas { input, .. } = &mut self.node {
            *input = Some(Rc::new(f));
        }
        self
    }
    /// For canvases: takes keyboard focus when clicked (and from Tab).
    pub fn focusable(mut self) -> Self {
        if let Node::Canvas { focusable, .. } = &mut self.node {
            *focusable = true;
        }
        self
    }
}

/// Calls `f` with the view at `path` below `v` (child indices; list rows
/// by row number, built on the way).
pub(crate) fn find<'a, M, R>(v: &View<'a, M>, path: &[u32], f: &mut dyn FnMut(&View<'a, M>) -> R) -> Option<R> {
    let Some((&i, rest)) = path.split_first() else { return Some(f(v)) };
    let i = i as usize;
    match &v.node {
        Node::Stack { children, .. }
        | Node::ZStack { children, .. }
        | Node::Grid { children, .. }
        | Node::Card { children, .. } => find(children.get(i)?, rest, f),
        Node::Scroll { child, .. } => find(child, rest, f),
        Node::Split { sidebar, detail, .. } => find(if i == 0 { sidebar } else { detail }, rest, f),
        Node::Button { label, .. } => find(label, rest, f),
        Node::List { count, rows, .. } if i < *count => {
            let row = rows(i);
            find(&row, rest, f)
        }
        _ => None,
    }
}

/// An alert: a title, a message and buttons, over the window.
pub struct Alert<M> {
    pub title: String,
    pub message: String,
    pub buttons: Vec<AlertButton<M>>,
}

pub struct AlertButton<M> {
    pub label: String,
    pub message: M,
    pub role: AlertRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertRole {
    /// The default (Return).
    Default,
    /// Escape.
    Cancel,
    Destructive,
    Other,
}

impl<M: Clone> Alert<M> {
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Alert { title: title.into(), message: message.into(), buttons: Vec::new() }
    }
    pub fn button(mut self, label: impl Into<String>, message: M, role: AlertRole) -> Self {
        self.buttons.push(AlertButton { label: label.into(), message, role });
        self
    }

    /// The alert as a view (a sheet).
    pub fn view<'a>(&self) -> View<'a, M> {
        let mut buttons = HStack::new().push(Spacer::new()).spacing(8);
        for b in &self.buttons {
            let style = match b.role {
                AlertRole::Default => ButtonStyle::Prominent,
                AlertRole::Destructive => ButtonStyle::Destructive,
                _ => ButtonStyle::Bordered,
            };
            let mut btn = Button::new(b.label.clone()).action(b.message.clone()).button_style(style);
            match b.role {
                AlertRole::Default => btn = btn.shortcut(Shortcut::default_action()),
                AlertRole::Cancel => btn = btn.shortcut(Shortcut::cancel()),
                _ => {}
            }
            buttons = buttons.push(btn.min_width(84));
        }
        VStack::new()
            .push(Text::paragraph(self.title.clone()).style(TextStyle::Title2))
            .push(Text::paragraph(self.message.clone()).foreground(Color::SecondaryLabel))
            .push(buttons.padding_insets(Insets { top: 8, ..Default::default() }).fill_width())
            .spacing(10)
            .alignment(Align::Start)
            .width(380)
    }
}
