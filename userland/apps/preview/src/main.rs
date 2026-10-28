//! Lens — views PNG, JPEG, GIF and BMP pictures.
//!
//! Fit to window or zoom (wheel, +/−, double-click for actual size), drag to
//! pan, rotate, step through the pictures in the same folder with ← and →,
//! and set a picture as the desktop wallpaper.

#![no_std]
#![no_main]

extern crate alloc;

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
                ((w as i32).max(560), h as i32 + size::TOOLBAR)
            }
            None => (640, 460),
        }
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
        let info = self.image.as_ref().map(|img| format!("{} × {}", img.width, img.height)).unwrap_or_default();
        let zoom = if has { format!("{}%", self.effective_zoom()) } else { String::from("—") };
        let bar = toolbar(vec![
            Button::symbol(Symbol::ChevronLeft).action(Msg::Step(-1)).help("Previous picture"),
            Button::symbol(Symbol::ChevronRight).action(Msg::Step(1)).help("Next picture"),
            Text::new(info).foreground(Color::SecondaryLabel).padding_xy(6, 0),
            Spacer::new(),
            Button::symbol(Symbol::Minus).action(Msg::Zoom(-1)).disabled(!has).help("Zoom out"),
            Text::centered(zoom).foreground(Color::SecondaryLabel).width(56),
            Button::symbol(Symbol::Plus).action(Msg::Zoom(1)).disabled(!has).help("Zoom in"),
            Button::new("Fit").action(Msg::Fit).disabled(!has).button_style(if self.zoom.is_none() && has {
                ButtonStyle::Prominent
            } else {
                ButtonStyle::Bordered
            }),
            Button::symbol(Symbol::Refresh).action(Msg::Rotate).disabled(!has).help("Rotate"),
            Button::new("Set as Wallpaper").action(Msg::Wallpaper).disabled(!has),
        ]);
        let backdrop = if env.dark { 0xFF18_181E } else { 0xFFE9_E9EE };
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
        let status = self.status.as_ref().map(|s| {
            VStack::new()
                .push(Spacer::new())
                .push(
                    Text::new(s.clone())
                        .style(TextStyle::Headline)
                        .foreground(Color::Custom(0xFFFF_FFFF))
                        .padding_xy(16, 8)
                        .background_rounded(Color::Custom(0xE025_2530), 17),
                )
                .padding(18)
                .fill()
        });
        VStack::new()
            .push(bar)
            .push(Divider::new())
            .push(ZStack::with(vec![content]).push_if(status).fill())
            .spacing(0)
            .fill()
    }
}

fn main(args: corekit::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    waveui::run(Preview::new(path))
}
