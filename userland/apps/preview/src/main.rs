//! Lens — views PNG, JPEG, GIF and BMP pictures.
//!
//! Fit to window or zoom (wheel, +/−, double-click for actual size), drag to
//! pan, rotate, step through the pictures in the same folder with ← and →,
//! and set a picture as the desktop wallpaper.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use aurora_image::Image as Picture;
use core::cell::RefCell;
use corekit::fs;
use waveui::lumen::geom::Rect;
use waveui::lumen::icons::Icon;
use waveui::prelude::*;
use waveui::Request;

corekit::entry!(main);

const MIN_ZOOM: i32 = 5; // percent
const MAX_ZOOM: i32 = 3200;

struct Preview {
    path: Option<String>,
    image: Option<Picture>,
    error: Option<String>,
    /// Percent; `None` = fit to window.
    zoom: Option<i32>,
    /// Pan offset of the image centre from the viewport centre, in screen pixels.
    pan: (i32, i32),
    drag_from: Option<(i32, i32, (i32, i32))>,
    /// A reduced copy for drawing at small zoom levels (quality and speed).
    reduced: RefCell<Option<Picture>>,
    status: Option<String>,
    /// The size of the picture area.
    view: (i32, i32),
    /// The name card (top left) and the filmstrip (bottom), shown or not.
    show_info: bool,
    show_strip: bool,
    /// Small copies of the pictures beside this one, by path.
    thumbs: RefCell<BTreeMap<String, Option<Picture>>>,
}

#[derive(Clone)]
enum Msg {
    Step(i32),
    Zoom(i32),
    Fit,
    Actual,
    Rotate,
    Wallpaper,
    Resize(i32, i32),
    Grab(i32, i32),
    Pan(i32, i32),
    Release,
    ToggleActual,
    ClearStatus,
    Open(String),
    ToggleInfo,
    ToggleStrip,
    Share,
}

impl Preview {
    fn new(path: Option<String>) -> Self {
        let mut p = Preview {
            path: None,
            image: None,
            error: None,
            zoom: None,
            pan: (0, 0),
            drag_from: None,
            reduced: RefCell::new(None),
            status: None,
            view: (640, 414),
            show_info: true,
            show_strip: true,
            thumbs: RefCell::new(BTreeMap::new()),
        };
        if let Some(path) = path {
            p.load(path);
        }
        p
    }

    fn load(&mut self, path: String) {
        self.image = None;
        *self.reduced.borrow_mut() = None;
        self.zoom = None;
        self.pan = (0, 0);
        self.error = match fs::read(&path) {
            Ok(data) => match aurora_image::decode(&data) {
                Ok(img) => {
                    self.image = Some(img);
                    None
                }
                Err(aurora_image::Error::Unsupported) => Some(String::from("This file isn't a PNG or BMP image.")),
                Err(aurora_image::Error::TooLarge) => Some(String::from("This image is too large to open.")),
                Err(aurora_image::Error::Corrupt) => Some(String::from("This image is damaged.")),
            },
            Err(e) => Some(format!("Couldn't open the file: {e}")),
        };
        self.path = Some(path);
    }

    /// Other images in the same folder, sorted by name.
    fn siblings(&self) -> Vec<String> {
        let Some(path) = &self.path else { return Vec::new() };
        let dir = fs::parent(path);
        let mut v: Vec<String> = fs::read_dir(dir)
            .map(|es| {
                es.into_iter()
                    .filter(|e| !e.is_dir && aurora_image::is_image_name(&e.name))
                    .map(|e| fs::join(dir, &e.name))
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by_key(|a| a.to_lowercase());
        v
    }

    fn step(&mut self, delta: i32) {
        let list = self.siblings();
        let Some(cur) = self.path.as_ref().and_then(|p| list.iter().position(|s| s == p)) else { return };
        let next = (cur as i32 + delta).rem_euclid(list.len() as i32) as usize;
        if next != cur {
            self.load(list[next].clone());
        }
    }

    fn viewport(&self) -> Rect {
        Rect::new(0, 0, self.view.0, self.view.1)
    }

    /// Current zoom in percent (fit computes it from the viewport).
    fn effective_zoom(&self) -> i32 {
        let Some(img) = &self.image else { return 100 };
        match self.zoom {
            Some(z) => z,
            None => {
                let v = self.viewport().inset(24);
                let zx = v.w * 100 / img.width.max(1) as i32;
                let zy = v.h * 100 / img.height.max(1) as i32;
                zx.min(zy).clamp(MIN_ZOOM, 100)
            }
        }
    }

    /// Where the picture goes in the picture area.
    fn image_rect(&self) -> Option<Rect> {
        let img = self.image.as_ref()?;
        let z = self.effective_zoom();
        let (w, h) =
            ((img.width as i64 * z as i64 / 100).max(1) as i32, (img.height as i64 * z as i64 / 100).max(1) as i32);
        let (cx, cy) = self.viewport().center();
        Some(Rect::new(cx - w / 2 + self.pan.0, cy - h / 2 + self.pan.1, w, h))
    }

    fn set_zoom(&mut self, z: i32) {
        let old = self.effective_zoom();
        let z = z.clamp(MIN_ZOOM, MAX_ZOOM);
        // Keep the viewport centre on the same image point.
        self.pan = (self.pan.0 * z / old.max(1), self.pan.1 * z / old.max(1));
        self.zoom = Some(z);
    }

    fn zoom_by(&mut self, steps: i32) {
        let z = self.effective_zoom();
        let next = if steps > 0 { z * 5 / 4 + 1 } else { z * 4 / 5 };
        self.set_zoom(next);
    }

    fn rotate(&mut self) {
        let Some(img) = &self.image else { return };
        let (w, h) = (img.width, img.height);
        let mut px = alloc::vec![0u32; img.pixels.len()];
        for y in 0..h {
            for x in 0..w {
                // 90° clockwise: (x, y) → (h - 1 - y, x) in a h×w image.
                px[(x * h + (h - 1 - y)) as usize] = img.pixels[(y * w + x) as usize];
            }
        }
        self.image = Some(Picture { width: h, height: w, pixels: px });
        *self.reduced.borrow_mut() = None;
        self.pan = (0, 0);
    }

    fn picture(&self, cv: &mut waveui::lumen::canvas::Canvas, view: Rect) {
        let Some(img) = &self.image else { return };
        let Some(r) = self.image_rect() else { return };
        let r = r.offset(view.x, view.y);
        if !img.is_opaque() {
            cv.checkerboard(r, 8, 0xFFFF_FFFF, 0xFFDD_DDE2);
        }
        // Draw from a reduced copy when shrinking a lot (quality and speed).
        if r.w * 2 < img.width as i32 {
            let mut reduced = self.reduced.borrow_mut();
            if reduced.as_ref().is_none_or(|t| t.width != r.w as u32) {
                *reduced = Some(img.thumbnail(r.w as u32, r.h as u32));
            }
            let small = reduced.as_ref().unwrap();
            cv.draw_image(&small.pixels, small.width, small.height, r, 255);
        } else {
            cv.draw_image(&img.pixels, img.width, img.height, r, 255);
        }
    }

    /// A small copy of `path` for the filmstrip (made once).
    fn thumb(&self, path: &str) -> Option<Picture> {
        let mut thumbs = self.thumbs.borrow_mut();
        thumbs
            .entry(String::from(path))
            .or_insert_with(|| {
                let data = fs::read(path).ok()?;
                let img = aurora_image::decode(&data).ok()?;
                Some(img.thumbnail(144, 104))
            })
            .clone()
    }

    fn filmstrip(&self) -> Option<View<'_, Msg>> {
        let list = self.siblings();
        let cur = self.path.as_ref().and_then(|p| list.iter().position(|s| s == p))?;
        if list.len() < 2 {
            return None;
        }
        let from = cur.saturating_sub(3).min(list.len().saturating_sub(7));
        let mut strip = HStack::new().spacing(8);
        for path in list.iter().skip(from).take(7) {
            let on = Some(path) == self.path.as_ref();
            let t = self.thumb(path);
            strip = strip.push(
                Canvas::new(move |cv, r| {
                    let inner = r.inset(3);
                    match &t {
                        Some(t) => {
                            let (w, h) = aurora_image::fit(t.width, t.height, inner.w as u32, inner.h as u32);
                            let (w, h) = (w as i32, h as i32);
                            let dst = Rect::new(inner.x + (inner.w - w) / 2, inner.y + (inner.h - h) / 2, w, h);
                            cv.draw_image_rounded(&t.pixels, t.width, t.height, dst, dst, 7);
                        }
                        None => cv.fill_round_rect(inner, 7, 0x30FF_FFFF),
                    }
                    if on {
                        cv.stroke_round_rect(r, 10, waveui::lumen::theme::accent());
                        cv.stroke_round_rect(r.inset(1), 9, waveui::lumen::theme::accent());
                    }
                })
                .size(80, 58)
                .on_tap(Msg::Open(path.clone()))
                .help(String::from(fs::file_name(path))),
            );
        }
        Some(strip.padding(6).background_rounded(Color::Custom(panel()), 14).border(Color::Separator, 14))
    }

    fn placeholder<'a>(title: &str, detail: &str) -> View<'a, Msg> {
        VStack::new()
            .push(Image::app_icon(Icon::Preview, 64))
            .push(Text::new(title).style(TextStyle::Title2))
            .push(Text::centered(detail).foreground(Color::SecondaryLabel))
            .spacing(8)
            .fill()
    }
}

impl App for Preview {
    type Message = Msg;

    fn title(&self) -> String {
        match &self.path {
            Some(p) => String::from(fs::file_name(p)),
            None => String::from("Lens"),
        }
    }

    fn size(&self) -> (i32, i32) {
        match &self.image {
            Some(img) => {
                let (w, h) = aurora_image::fit(img.width, img.height, 960, 600);
                ((w as i32).max(640), (h as i32).max(460))
            }
            None => (640, 460),
        }
    }

    fn resizable(&self) -> bool {
        true
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Step(d) => self.step(d),
            Msg::Zoom(d) => self.zoom_by(d),
            Msg::Fit => {
                self.zoom = None;
                self.pan = (0, 0);
            }
            Msg::Actual => self.set_zoom(100),
            Msg::ToggleActual => {
                if self.zoom.is_none() {
                    self.set_zoom(100);
                } else {
                    self.zoom = None;
                    self.pan = (0, 0);
                }
            }
            Msg::Rotate => self.rotate(),
            Msg::Wallpaper => {
                if let (Some(p), Some(_)) = (&self.path, &self.image) {
                    cx.request(Request::SetWallpaperImage(p.clone()));
                    self.status = Some(String::from("Set as desktop wallpaper"));
                    cx.after(2500, Msg::ClearStatus);
                }
            }
            Msg::Resize(w, h) => self.view = (w, h),
            Msg::Grab(x, y) => self.drag_from = Some((x, y, self.pan)),
            Msg::Pan(x, y) => {
                if let Some((sx, sy, start)) = self.drag_from {
                    self.pan = (start.0 + x - sx, start.1 + y - sy);
                }
            }
            Msg::Release => self.drag_from = None,
            Msg::ClearStatus => self.status = None,
            Msg::Open(p) => self.load(p),
            Msg::ToggleInfo => self.show_info = !self.show_info,
            Msg::ToggleStrip => self.show_strip = !self.show_strip,
            Msg::Share => {
                if let Some(p) = &self.path {
                    cx.copy(p);
                    self.status = Some(String::from("Path copied"));
                    cx.after(2000, Msg::ClearStatus);
                }
            }
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        Some(match (ev.code, ev.ch) {
            (KeyCode::Left, _) => Msg::Step(-1),
            (KeyCode::Right, _) => Msg::Step(1),
            (_, Some('+' | '=')) => Msg::Zoom(1),
            (_, Some('-')) => Msg::Zoom(-1),
            (_, Some('0')) => Msg::Fit,
            (_, Some('1')) => Msg::Actual,
            (_, Some('r' | 'R')) => Msg::Rotate,
            _ => return None,
        })
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let has = self.image.is_some();
        let backdrop = if env.dark { 0xFF00_0000 } else { 0xFFF4_F5FA };
        let content = match (&self.image, &self.error) {
            (Some(_), _) => Canvas::new(move |cv, r| {
                cv.fill_rect(r, backdrop);
                self.picture(cv, r);
            })
            .on_input(|e| match e {
                CanvasEvent::Press { clicks: 2, .. } => Some(Msg::ToggleActual),
                CanvasEvent::Press { x, y, button: 1, .. } => Some(Msg::Grab(x, y)),
                CanvasEvent::Drag { x, y } => Some(Msg::Pan(x, y)),
                CanvasEvent::Release { .. } => Some(Msg::Release),
                CanvasEvent::Scroll { dy, .. } if dy != 0 => Some(Msg::Zoom(-dy.signum())),
                CanvasEvent::Resize { w, h } => Some(Msg::Resize(w, h)),
                _ => None,
            })
            .fill(),
            (None, Some(err)) => Self::placeholder("Can't show this picture", err).background(Color::Custom(backdrop)),
            (None, None) => Self::placeholder("No picture open", "Double-click a picture in Files to open it here.")
                .background(Color::Custom(backdrop)),
        };
        // The name card.
        let card = (self.show_info && self.path.is_some()).then(|| {
            let path = self.path.as_deref().unwrap_or("");
            let list = self.siblings();
            let mut detail = Vec::new();
            if let Some(i) = list.iter().position(|p| p == path) {
                detail.push(format!("{} of {}", i + 1, list.len()));
            }
            if let Some(img) = &self.image {
                detail.push(format!("{} × {}", img.width, img.height));
            }
            if let Ok(st) = fs::stat(path) {
                detail.push(human_size(st.size));
            }
            if has && self.zoom.is_some() {
                detail.push(format!("{}%", self.effective_zoom()));
            }
            VStack::new()
                .push(
                    VStack::new()
                        .push(
                            Text::new(String::from(fs::file_name(path)))
                                .style(TextStyle::Sized(15, waveui::style::Weight::Bold)),
                        )
                        .push(
                            Text::new(detail.join(" · "))
                                .style(TextStyle::Sized(11, waveui::style::Weight::Bold))
                                .foreground(Color::SecondaryLabel),
                        )
                        .spacing(2)
                        .alignment(Align::Start)
                        .padding_xy(14, 10)
                        .background_rounded(Color::Custom(panel()), 12)
                        .border(Color::Separator, 12),
                )
                .push(Spacer::new())
                .alignment(Align::Start)
                .padding(22)
                .fill()
        });
        let tool = |sym: Symbol, m: Msg, tip: &str| Button::symbol(sym).action(m).disabled(!has).help(tip.to_string());
        let sep = || Divider::new().height(20);
        let bar = HStack::new()
            .push(Button::symbol(Symbol::ChevronLeft).action(Msg::Step(-1)).help("Previous picture (←)"))
            .push(Button::symbol(Symbol::ChevronRight).action(Msg::Step(1)).help("Next picture (→)"))
            .push(sep())
            .push(tool(Symbol::Search, Msg::Zoom(1), "Zoom in (+)"))
            .push(
                Button::new(if self.zoom.is_none() { "Fit" } else { "1:1" })
                    .action(if self.zoom.is_none() { Msg::Actual } else { Msg::Fit })
                    .button_style(ButtonStyle::Toolbar)
                    .disabled(!has)
                    .help("Fit the window, or actual size (0, 1)"),
            )
            .push(tool(Symbol::Refresh, Msg::Rotate, "Rotate (R)"))
            .push(tool(Symbol::Photo, Msg::Wallpaper, "Set as Wallpaper"))
            .push(
                Button::symbol(Symbol::Info)
                    .action(Msg::ToggleInfo)
                    .background_rounded(if self.show_info { Color::SecondaryFill } else { Color::Clear }, 15)
                    .help("Show the name"),
            )
            .push(sep())
            .push(tool(Symbol::Share, Msg::Share, "Copy Path"))
            .push(
                Button::symbol(Symbol::Grid)
                    .action(Msg::ToggleStrip)
                    .background_rounded(if self.show_strip { Color::SecondaryFill } else { Color::Clear }, 15)
                    .help("Show the other pictures here"),
            )
            .spacing(6)
            .padding_xy(10, 6)
            .background_rounded(Color::Custom(panel()), 22)
            .border(Color::Separator, 22);
        let strip = if self.show_strip { self.filmstrip() } else { None };
        let mut bottom = VStack::new().push(Spacer::new());
        if let Some(s) = &self.status {
            bottom = bottom.push(
                Text::new(s.clone())
                    .style(TextStyle::Headline)
                    .padding_xy(16, 8)
                    .background_rounded(Color::Custom(panel()), 17),
            );
        }
        let bottom = bottom.push(bar).push_if(strip).spacing(14).padding(22).fill();
        ZStack::with(vec![content]).push_if(card).push(bottom).fill()
    }
}

/// The floating panels' glass.
fn panel() -> u32 {
    if waveui::lumen::theme::current().dark {
        0xF00B_0E18
    } else {
        0xF0FF_FFFF
    }
}

fn human_size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{} KB", n / 1024)
    } else {
        format!("{n} B")
    }
}

fn main(args: corekit::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    waveui::run(Preview::new(path))
}
