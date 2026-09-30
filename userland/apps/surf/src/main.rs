//! Surf — the WaveOS Aurora web browser.
//!
//! Pages load on a background thread (the document and its stylesheets),
//! images on a few more; layout (`aurora-surf`) and painting happen here.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora::sync::Mutex;
use aurora::thread::JoinHandle;
use aurora_image::Image;
use aurora_surf::dom::{Document, NodeId};
use aurora_surf::layout::Item;
use aurora_surf::style::ListStyle;
use aurora_surf::{FontSpec, Host, Layout, Metrics, Page};
use aurora_web::http::{self, Response};
use aurora_web::Url;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use ripple::canvas::Canvas;
use ripple::font::{self, Face, Font};
use ripple::geom::Rect;
use ripple::math::sin;
use ripple::text::TextEdit;
use ripple::theme;
use ripple::{App, Env, KeyCode, KeyEvent};

aurora::entry!(main);

const TOOLBAR_H: i32 = 48;
const HOME: &str = "about:home";
const SEARCH: &str = "https://html.duckduckgo.com/html/?q=";
const IMAGE_WORKERS: usize = 3;
const MAX_IMAGES: usize = 80;
const MAX_STYLESHEETS: usize = 16;

// ------------------------------------------------------------------ loading

struct Loaded {
    page: Page,
    url: Url,
    /// Images that came with the document (an image opened directly).
    images: Vec<(String, Image)>,
}

struct LoadJob {
    cancel: AtomicBool,
    progress: Mutex<String>,
    result: Mutex<Option<Result<Loaded, String>>>,
}

fn describe(e: http::Error) -> String {
    match e {
        http::Error::Io(errno) => String::from(aurora::net::describe(aurora::Error(errno))),
        other => other.to_string(),
    }
}

fn content_type_for(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        "text/html"
    } else if aurora_image::is_image_name(&lower) {
        "image/png"
    } else {
        "text/plain"
    }
}

/// GETs a URL (http, https or file).
fn get(url: &Url) -> Result<Response, String> {
    match url.scheme.as_str() {
        "http" | "https" => http::fetch(url, &mut aurora::web::connect).map_err(describe),
        "file" => {
            let path = aurora_web::url::percent_decode(&url.path, false);
            let body = aurora::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            Ok(Response {
                status: 200,
                reason: String::from("OK"),
                headers: alloc::vec![(String::from("content-type"), String::from(content_type_for(&path)))],
                body,
                url: url.clone(),
            })
        }
        other => Err(format!("\"{other}:\" addresses aren't supported")),
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Loads a document and its stylesheets (on a worker thread).
fn load(url: Url, job: &LoadJob) -> Result<Loaded, String> {
    if url.scheme == "about" {
        let html = if url.path == "home" { HOME_PAGE } else { "<html><body></body></html>" };
        return Ok(Loaded { page: Page::parse(html), url, images: Vec::new() });
    }
    *job.progress.lock() = format!("Connecting to {}…", url.host);
    let resp = get(&url)?;
    let final_url = resp.url.clone();
    let ct = resp.content_type();
    let is_html = ct.contains("html") || (ct.is_empty() && resp.body.iter().take(512).any(|b| *b == b'<'));
    let mut images = Vec::new();
    let mut page = if ct.starts_with("image/") || (ct.is_empty() && aurora_image::is_image_name(&final_url.path)) {
        let img = aurora_image::decode(&resp.body).map_err(|_| String::from("this kind of picture can't be shown yet"))?;
        let src = final_url.to_string();
        let html = format!(
            "<html><head><title>{} ({}×{})</title></head><body style='margin:0;background:#1c1c20;text-align:center'><img src=\"{}\" style='margin:16px auto'></body></html>",
            escape(aurora::fs::file_name(&final_url.path)),
            img.width,
            img.height,
            escape(&src)
        );
        images.push((src, img));
        Page::parse(&html)
    } else if is_html {
        *job.progress.lock() = String::from("Reading the page…");
        Page::parse(&aurora_surf::decode(&resp.body, resp.header("content-type").unwrap_or("")))
    } else if ct.starts_with("text/") || ct.contains("json") || ct.contains("javascript") || ct.contains("xml") {
        let text = aurora_surf::decode(&resp.body, resp.header("content-type").unwrap_or(""));
        Page::parse(&format!(
            "<html><head><title>{}</title></head><body><pre style='white-space:pre-wrap'>{}</pre></body></html>",
            escape(aurora::fs::file_name(&final_url.path)),
            escape(&text)
        ))
    } else {
        // Anything else is a download.
        let mut name = String::from(aurora::fs::file_name(final_url.path.split('?').next().unwrap_or("")));
        if name.is_empty() {
            name = String::from("download");
        }
        let (stem, ext) = match name.rfind('.') {
            Some(p) => (String::from(&name[..p]), String::from(&name[p..])),
            None => (name.clone(), String::new()),
        };
        let path = aurora::fs::unique_name("/Downloads", &stem, &ext);
        aurora::fs::write(&path, &resp.body).map_err(|e| format!("couldn't save the download: {e}"))?;
        Page::parse(&format!(
            "<html><head><title>Downloaded</title></head><body style='font-family:sans-serif;margin:48px'><h2>Downloaded {}</h2><p>Saved {} to <b>{}</b>.</p></body></html>",
            escape(&name),
            human_size(resp.body.len() as u64),
            escape(&path)
        ))
    };
    if !resp.ok() && resp.body.is_empty() {
        return Err(format!("the server answered {} {}", resp.status, resp.reason));
    }
    // Stylesheets.
    let base = document_base(&page, &final_url);
    for (n, (slot, href)) in page.pending_stylesheets().into_iter().enumerate() {
        if n >= MAX_STYLESHEETS || job.cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(u) = base.join(&href) else { continue };
        *job.progress.lock() = format!("Loading styles from {}…", u.host);
        if let Ok(r) = get(&u) {
            if r.ok() {
                page.set_stylesheet(slot, &aurora_surf::decode(&r.body, ""));
            }
        }
    }
    Ok(Loaded { page, url: final_url, images })
}

/// The URL relative links resolve against (<base href>, else the document's).
fn document_base(page: &Page, url: &Url) -> Url {
    page.base.as_deref().and_then(|b| url.join(b)).unwrap_or_else(|| url.clone())
}

fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{}.{} MB", n >> 20, (n % (1 << 20)) * 10 >> 20),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} bytes"),
    }
}

// ------------------------------------------------------------------ images

struct ImageQueue {
    queue: VecDeque<(u64, String)>,
    done: Vec<(u64, String, Option<Image>)>,
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn load_image(url: &str) -> Option<Image> {
    let bytes = if let Some(data) = url.strip_prefix("data:") {
        let (meta, payload) = data.split_once(',')?;
        if meta.ends_with(";base64") {
            base64(payload)?
        } else {
            aurora_web::url::percent_decode(payload, false).into_bytes()
        }
    } else {
        let u = Url::parse(url)?;
        let r = get(&u).ok()?;
        if !r.ok() {
            return None;
        }
        r.body
    };
    let img = aurora_image::decode(&bytes).ok()?;
    Some(shrink(img, 1600))
}

/// Halves a picture until it is at most `max` pixels wide (saves memory).
fn shrink(mut img: Image, max: u32) -> Image {
    while img.width > max && img.height > 1 {
        let (w, h) = (img.width / 2, img.height / 2);
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let at = |dx: u32, dy: u32| img.pixels[((y * 2 + dy) * img.width + x * 2 + dx) as usize];
                let quad = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
                let mut c = 0u32;
                for shift in [0, 8, 16, 24] {
                    let sum: u32 = quad.iter().map(|p| (p >> shift) & 0xFF).sum();
                    c |= (sum / 4) << shift;
                }
                px.push(c);
            }
        }
        img = Image { width: w, height: h, pixels: px };
    }
    img
}

fn image_worker(q: Arc<Mutex<ImageQueue>>, generation: Arc<AtomicU64>) {
    loop {
        let next = q.lock().queue.pop_front();
        let Some((g, url)) = next else { return };
        if g != generation.load(Ordering::Relaxed) {
            continue;
        }
        let img = load_image(&url);
        q.lock().done.push((g, url, img));
    }
}

// ------------------------------------------------------------------ fonts

fn font_for(f: FontSpec) -> Font {
    let face = if f.mono {
        Face::Mono
    } else if f.bold {
        Face::SemiBold
    } else {
        Face::Regular
    };
    font::get(face, f.size)
}

struct Fonts<'a> {
    images: &'a BTreeMap<String, Option<Image>>,
    base: Option<Url>,
}

impl Host for Fonts<'_> {
    fn measure(&self, f: FontSpec, text: &str) -> i32 {
        let w = font_for(f).width(text);
        if f.italic {
            w + f.size as i32 / 8
        } else {
            w
        }
    }
    fn measure64(&self, f: FontSpec, text: &str) -> i32 {
        let w = font_for(f).width64(text);
        if f.italic {
            w + f.size as i32 * 8
        } else {
            w
        }
    }
    fn metrics(&self, f: FontSpec) -> Metrics {
        let font = font_for(f);
        Metrics { ascent: font.ascent as i32, descent: -(font.descent as i32) }
    }
    fn image_size(&self, src: &str) -> Option<(u32, u32)> {
        let key = resolve(self.base.as_ref(), src)?;
        match self.images.get(&key) {
            Some(Some(img)) => Some((img.width, img.height)),
            _ => None,
        }
    }
}

fn resolve(base: Option<&Url>, href: &str) -> Option<String> {
    let href = href.trim();
    if href.starts_with("data:") {
        return Some(String::from(href));
    }
    base?.join(href).map(|u| u.to_string())
}

// ------------------------------------------------------------------ the browser

struct Surf {
    address: TextEdit,
    editing: bool,
    history: Vec<String>,
    pos: usize,
    url: Option<Url>,
    page: Option<Page>,
    layout: Option<Layout>,
    laid_out_width: i32,
    scroll: i32,
    view: Rect,
    hover: Option<String>,
    job: Option<(Arc<LoadJob>, JoinHandle<()>)>,
    old_jobs: Vec<JoinHandle<()>>,
    loading_since: u64,
    images: BTreeMap<String, Option<Image>>,
    queue: Arc<Mutex<ImageQueue>>,
    generation: Arc<AtomicU64>,
    workers: Vec<JoinHandle<()>>,
    relayout_at: Option<u64>,
    fields: BTreeMap<NodeId, TextEdit>,
    focus: Option<NodeId>,
    fragment: Option<String>,
    press: Option<(i32, i32)>,
}

impl Surf {
    fn new(start: &str) -> Surf {
        let mut s = Surf {
            address: TextEdit::new("", false),
            editing: false,
            history: Vec::new(),
            pos: 0,
            url: None,
            page: None,
            layout: None,
            laid_out_width: 0,
            scroll: 0,
            view: Rect::new(0, TOOLBAR_H, 1100, 700),
            hover: None,
            job: None,
            old_jobs: Vec::new(),
            loading_since: 0,
            images: BTreeMap::new(),
            queue: Arc::new(Mutex::new(ImageQueue { queue: VecDeque::new(), done: Vec::new() })),
            generation: Arc::new(AtomicU64::new(0)),
            workers: Vec::new(),
            relayout_at: None,
            fields: BTreeMap::new(),
            focus: None,
            fragment: None,
            press: None,
        };
        s.go(start, true);
        s
    }

    fn loading(&self) -> bool {
        self.job.is_some()
    }

    /// Turns what was typed into an address: a URL, a path, or a search.
    fn normalize(input: &str) -> String {
        let s = input.trim();
        if s.is_empty() {
            return String::from(HOME);
        }
        if s.contains("://") || s.starts_with("about:") || s.starts_with("data:") {
            return String::from(s);
        }
        if s.starts_with('/') {
            return format!("file://{s}");
        }
        let hostlike = !s.contains(' ') && (s.contains('.') || s.starts_with("localhost")) && !s.ends_with('.');
        if hostlike {
            format!("https://{s}")
        } else {
            format!("{SEARCH}{}", form_encode(s))
        }
    }

    /// Navigates to `input`, adding a history entry when `push`.
    fn go(&mut self, input: &str, push: bool) {
        let target = Self::normalize(input);
        if push {
            self.history.truncate(if self.history.is_empty() { 0 } else { self.pos + 1 });
            self.history.push(target.clone());
            self.pos = self.history.len() - 1;
        }
        self.start(target);
    }

    fn start(&mut self, target: String) {
        self.stop();
        self.editing = false;
        self.address.set_text(&target);
        let Some(url) = Url::parse(&target) else {
            self.show_error(&target, "that isn't a web address");
            return;
        };
        // Same document, different fragment: just scroll.
        if let (Some(cur), Some(frag)) = (&self.url, &url.fragment) {
            let mut a = cur.clone();
            let mut b = url.clone();
            a.fragment = None;
            b.fragment = None;
            if a == b && self.page.is_some() {
                self.url = Some(url.clone());
                self.scroll_to_fragment(&frag.clone());
                return;
            }
        }
        let job = Arc::new(LoadJob { cancel: AtomicBool::new(false), progress: Mutex::new(String::new()), result: Mutex::new(None) });
        let j = job.clone();
        let u = url.clone();
        match aurora::thread::spawn(move || {
            let r = load(u, &j);
            *j.result.lock() = Some(r);
        }) {
            Ok(h) => {
                self.job = Some((job, h));
                self.loading_since = aurora::time::uptime_ms();
                self.fragment = url.fragment.clone();
            }
            Err(e) => self.show_error(&target, &format!("couldn't start loading ({e})")),
        }
    }

    fn stop(&mut self) {
        if let Some((job, h)) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            self.old_jobs.push(h);
        }
    }

    fn show_error(&mut self, url: &str, msg: &str) {
        let html = format!(
            "<html><head><title>Can't open the page</title></head><body style='margin:0;background:#f4f5f9;font-family:sans-serif'>\
             <div style='max-width:560px;margin:80px auto;padding:28px 32px;background:#fff;border:1px solid #e1e1e8;border-radius:14px'>\
             <h1 style='font-size:22px;margin:0 0 10px'>Surf can't open this page</h1>\
             <p style='color:#50505a;margin:0 0 14px'>{}.</p><p style='color:#8a8a96;font-size:13px;margin:0'>{}</p></div></body></html>",
            escape(&capitalize(msg)),
            escape(url)
        );
        self.install(Loaded { page: Page::parse(&html), url: Url::parse(url).unwrap_or_else(|| Url::parse(HOME).unwrap()), images: Vec::new() });
    }

    fn install(&mut self, loaded: Loaded) {
        let Loaded { page, url, images } = loaded;
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.queue.lock().queue.clear();
        self.images.clear();
        for (k, img) in images {
            self.images.insert(k, Some(img));
        }
        self.fields.clear();
        self.focus = None;
        self.scroll = 0;
        self.hover = None;
        let url_string = url.to_string();
        if !self.editing {
            self.address.set_text(&url_string);
        }
        if let Some(h) = self.history.get_mut(self.pos) {
            *h = url_string;
        }
        let base = document_base(&page, &url);
        let sources: Vec<String> = page.images().iter().filter_map(|s| resolve(Some(&base), s)).take(MAX_IMAGES).collect();
        let refresh = page.refresh();
        self.page = Some(page);
        self.url = Some(url);
        self.relayout();
        // Images.
        let g = self.generation.load(Ordering::Relaxed);
        {
            let mut q = self.queue.lock();
            for s in sources {
                if !self.images.contains_key(&s) {
                    q.queue.push_back((g, s));
                }
            }
        }
        self.spawn_workers();
        if let Some(frag) = self.fragment.take() {
            self.scroll_to_fragment(&frag);
        }
        if let Some((0, target)) = refresh {
            if !target.is_empty() {
                if let Some(u) = self.url.as_ref().and_then(|u| u.join(&target)) {
                    self.go(&u.to_string(), false);
                }
            }
        }
    }

    fn spawn_workers(&mut self) {
        self.workers.retain(|w| !w.is_finished());
        while self.workers.len() < IMAGE_WORKERS && !self.queue.lock().queue.is_empty() {
            let (q, g) = (self.queue.clone(), self.generation.clone());
            match aurora::thread::spawn(move || image_worker(q, g)) {
                Ok(h) => self.workers.push(h),
                Err(_) => break,
            }
        }
    }

    fn relayout(&mut self) {
        let Some(page) = &self.page else { return };
        let base = self.url.as_ref().map(|u| document_base(page, u));
        let host = Fonts { images: &self.images, base };
        let w = self.view.w.max(200);
        let l = page.layout(w, self.view.h.max(200), &host);
        self.laid_out_width = w;
        self.layout = Some(l);
        self.clamp_scroll();
    }

    fn max_scroll(&self) -> i32 {
        self.layout.as_ref().map_or(0, |l| (l.height - self.view.h).max(0))
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }

    fn scroll_to_fragment(&mut self, frag: &str) {
        let name = aurora_web::url::percent_decode(frag, false);
        if let Some(y) = self.layout.as_ref().and_then(|l| l.anchor(&name)) {
            self.scroll = y;
            self.clamp_scroll();
        } else if frag.is_empty() || frag == "top" {
            self.scroll = 0;
        }
    }

    fn back(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
            let t = self.history[self.pos].clone();
            self.go(&t, false);
        }
    }

    fn forward(&mut self) {
        if self.pos + 1 < self.history.len() {
            self.pos += 1;
            let t = self.history[self.pos].clone();
            self.go(&t, false);
        }
    }

    fn reload(&mut self) {
        if let Some(t) = self.history.get(self.pos).cloned() {
            self.go(&t, false);
        }
    }

    fn doc(&self) -> Option<&Document> {
        self.page.as_ref().map(|p| &p.doc)
    }

    /// The absolute URL of an <a> element.
    fn href(&self, node: NodeId) -> Option<String> {
        let page = self.page.as_ref()?;
        let href = page.element(node)?.attr("href")?;
        let base = document_base(page, self.url.as_ref()?);
        let href = href.trim();
        if href.starts_with("javascript:") {
            return None;
        }
        base.join(href).map(|u| u.to_string())
    }

    fn follow(&mut self, node: NodeId) {
        if let Some(target) = self.href(node) {
            if target.starts_with("mailto:") || target.starts_with("tel:") {
                return;
            }
            self.go(&target, true);
        }
    }

    // ------------------------------------------------------------ forms

    fn form_of(&self, node: NodeId) -> Option<NodeId> {
        let doc = self.doc()?;
        let mut p = doc.parent(node);
        while let Some(n) = p {
            if doc.tag(n) == "form" {
                return Some(n);
            }
            p = doc.parent(n);
        }
        None
    }

    fn field_value(&self, node: NodeId) -> String {
        if let Some(e) = self.fields.get(&node) {
            return String::from(e.text());
        }
        let doc = self.doc().unwrap();
        let el = doc.element(node).unwrap();
        match el.tag.as_str() {
            "textarea" => doc.text_content(node),
            "select" => {
                let opts: Vec<NodeId> = doc.descendants(node).into_iter().filter(|o| doc.tag(*o) == "option").collect();
                let sel = opts.iter().find(|o| doc.element(**o).unwrap().attr("selected").is_some()).or(opts.first());
                sel.map(|o| {
                    let e = doc.element(*o).unwrap();
                    e.attr("value").map(String::from).unwrap_or_else(|| String::from(doc.text_content(*o).trim()))
                })
                .unwrap_or_default()
            }
            _ => String::from(el.attr("value").unwrap_or("")),
        }
    }

    /// Submits the form containing `from` (GET; POST forms are sent as GET too).
    fn submit(&mut self, from: NodeId) {
        let Some(form) = self.form_of(from) else { return };
        let Some(doc) = self.doc() else { return };
        let fe = doc.element(form).unwrap();
        let action = fe.attr("action").unwrap_or("").trim();
        let base = document_base(self.page.as_ref().unwrap(), self.url.as_ref().unwrap());
        let Some(mut target) = (if action.is_empty() { Some(base.clone()) } else { base.join(action) }) else { return };
        let mut pairs: Vec<(String, String)> = Vec::new();
        for n in doc.descendants(form) {
            let Some(e) = doc.element(n) else { continue };
            let Some(name) = e.attr("name") else { continue };
            if e.attr("disabled").is_some() {
                continue;
            }
            let kind = e.attr("type").unwrap_or("text").to_ascii_lowercase();
            match e.tag.as_str() {
                "input" => match kind.as_str() {
                    "submit" | "image" | "button" | "reset" | "file" => {
                        if n == from && kind != "reset" {
                            pairs.push((String::from(name), String::from(e.attr("value").unwrap_or(""))));
                        }
                    }
                    "checkbox" | "radio" => {
                        if e.attr("checked").is_some() {
                            pairs.push((String::from(name), String::from(e.attr("value").unwrap_or("on"))));
                        }
                    }
                    _ => pairs.push((String::from(name), self.field_value(n))),
                },
                "button" if n == from => pairs.push((String::from(name), String::from(e.attr("value").unwrap_or("")))),
                "select" | "textarea" => pairs.push((String::from(name), self.field_value(n))),
                _ => {}
            }
        }
        let query: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v))).collect();
        let path = target.path.split('?').next().unwrap_or("/").to_string();
        target.path = format!("{}?{}", path, query.join("&"));
        target.fragment = None;
        self.go(&target.to_string(), true);
    }

    // ------------------------------------------------------------ geometry

    fn back_rect(&self) -> Rect {
        Rect::new(10, 10, 28, 28)
    }
    fn forward_rect(&self) -> Rect {
        Rect::new(42, 10, 28, 28)
    }
    fn reload_rect(&self) -> Rect {
        Rect::new(74, 10, 28, 28)
    }
    fn address_rect(&self, area: Rect) -> Rect {
        Rect::new(112, 8, area.w - 124, 32)
    }

    /// Page coordinates of a window point in the content area.
    fn page_point(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        (self.view.contains(x, y)).then(|| (x - self.view.x, y - self.view.y + self.scroll))
    }

    // ------------------------------------------------------------ painting

    fn paint_page(&self, cv: &mut Canvas) {
        let v = self.view;
        let Some(l) = &self.layout else {
            cv.fill_rect(v, 0xFFFF_FFFF);
            return;
        };
        let bg = if l.background >> 24 == 0xFF { l.background } else { 0xFFFF_FFFF };
        cv.fill_rect(v, 0xFFFF_FFFF);
        cv.fill_rect(v, bg);
        let (ox, oy) = (v.x, v.y - self.scroll);
        let base = self.page.as_ref().zip(self.url.as_ref()).map(|(p, u)| document_base(p, u));
        let visible = |r: Rect| r.y + r.h >= v.y && r.y < v.bottom();
        cv.with_clip(v, |cv| {
            for item in &l.items {
                match item {
                    Item::Rect { rect, color, radius } => {
                        if *color >> 24 == 0 {
                            continue;
                        }
                        let r = Rect::new(ox + rect.x, oy + rect.y, rect.w, rect.h);
                        if !visible(r) {
                            continue;
                        }
                        if *radius > 0 {
                            cv.fill_round_rect(r, (*radius).min(r.h / 2).min(r.w / 2), *color);
                        } else {
                            cv.fill_rect(r, *color);
                        }
                    }
                    Item::Text { x, y, text, font, color, underline, strike } => {
                        let by = oy + y;
                        let size = font.size as i32;
                        if by + size < v.y || by - size * 2 > v.bottom() {
                            continue;
                        }
                        let f = font_for(*font);
                        let bx = ox + x;
                        let w = if font.italic { cv.text_slanted(bx, by, text, f, *color) } else { cv.text(bx, by, text, f, *color) };
                        let t = (size / 14).max(1);
                        if *underline {
                            cv.fill_rect(Rect::new(bx, by + (size / 8).max(1) + 1, w, t), *color);
                        }
                        if *strike {
                            cv.fill_rect(Rect::new(bx, by - f.ascent as i32 * 3 / 10, w, t), *color);
                        }
                    }
                    Item::Image { rect, src } => {
                        let r = Rect::new(ox + rect.x, oy + rect.y, rect.w, rect.h);
                        if !visible(r) {
                            continue;
                        }
                        let key = resolve(base.as_ref(), src);
                        match key.as_ref().and_then(|k| self.images.get(k)) {
                            Some(Some(img)) => cv.draw_image(&img.pixels, img.width, img.height, r, 255),
                            // Couldn't load (or an unsupported format): leave the space empty.
                            Some(None) => {}
                            None => cv.fill_rect(r, 0x18808088),
                        }
                    }
                    Item::Bullet { rect, color, kind } => {
                        let r = Rect::new(ox + rect.x, oy + rect.y, rect.w, rect.h);
                        if !visible(r) {
                            continue;
                        }
                        match kind {
                            ListStyle::Square => cv.fill_rect(r, *color),
                            ListStyle::Circle => cv.stroke_round_rect(r, r.w / 2, *color),
                            _ => cv.fill_circle(r.x + r.w / 2, r.y + r.h / 2, r.w / 2, *color),
                        }
                    }
                }
            }
            // Typed text in form fields.
            for (node, edit) in &self.fields {
                let Some(f) = l.fields.iter().find(|f| f.node == *node) else { continue };
                let r = Rect::new(ox + f.rect.x + 2, oy + f.rect.y + 2, f.rect.w - 4, f.rect.h - 4);
                if !visible(r) {
                    continue;
                }
                cv.fill_rect(r, 0xFFFF_FFFF);
                let font = theme::ui(((f.rect.h - 8).clamp(10, 18)) as u16);
                let base_y = r.y + (r.h + font.ascent as i32 + font.descent as i32) / 2;
                cv.with_clip(r, |cv| {
                    let caret_x = font.width(&edit.text()[..edit.caret()]);
                    let shift = (caret_x - (r.w - 8)).max(0);
                    let tx = r.x + 3 - shift;
                    if let Some((a, b)) = edit.selection() {
                        let (xa, xb) = (font.width(&edit.text()[..a]), font.width(&edit.text()[..b]));
                        cv.fill_rect(Rect::new(tx + xa, r.y + 2, xb - xa, r.h - 4), theme::accent() & 0x00FF_FFFF | 0x5000_0000);
                    }
                    cv.text(tx, base_y, edit.text(), font, 0xFF1D_1D24);
                    if self.focus == Some(*node) {
                        cv.fill_rect(Rect::new(tx + caret_x, r.y + 3, 1, r.h - 6), theme::accent());
                    }
                });
            }
        });
        // A slim scroll indicator.
        if let Some(l) = &self.layout {
            if l.height > v.h {
                let h = (v.h * v.h / l.height).max(24);
                let y = v.y + (v.h - h) * self.scroll / self.max_scroll().max(1);
                cv.fill_round_rect(Rect::new(v.right() - 7, y + 2, 5, h - 4), 2, 0x60000000);
            }
        }
    }

    fn paint_toolbar(&self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let bar = Rect::new(area.x, area.y, area.w, TOOLBAR_H);
        cv.fill_rect(bar, t.titlebar);
        cv.fill_rect(Rect::new(bar.x, bar.bottom() - 1, bar.w, 1), t.separator);
        let enabled = |on: bool| if on { t.text } else { t.text_secondary & 0x00FF_FFFF | 0x6000_0000 };
        // Back / forward chevrons.
        let (bx, by) = (self.back_rect().x + 14, self.back_rect().y + 14);
        cv.polyline(&[(bx + 3, by - 7), (bx - 4, by), (bx + 3, by + 7)], 2, enabled(self.pos > 0));
        let (fx, fy) = (self.forward_rect().x + 14, self.forward_rect().y + 14);
        cv.polyline(&[(fx - 3, fy - 7), (fx + 4, fy), (fx - 3, fy + 7)], 2, enabled(self.pos + 1 < self.history.len()));
        // Reload (a circular arrow) or stop (×).
        let (rx, ry) = (self.reload_rect().x + 14, self.reload_rect().y + 14);
        if self.loading() {
            cv.line(rx - 6, ry - 6, rx + 6, ry + 6, 2, t.text);
            cv.line(rx - 6, ry + 6, rx + 6, ry - 6, 2, t.text);
        } else {
            let pts: Vec<(i32, i32)> = (0..=20).map(|i| {
                let a = 128 + i * 800 / 20;
                (rx + 7 * sin(a + 256) / 16384, ry + 7 * sin(a) / 16384)
            }).collect();
            cv.polyline(&pts, 2, t.text);
            let (ex, ey) = *pts.last().unwrap();
            cv.polyline(&[(ex - 5, ey - 3), (ex, ey), (ex + 1, ey - 6)], 2, t.text);
        }
        // The address field.
        let r = self.address_rect(area);
        cv.fill_round_rect(r, 9, t.control_bg);
        cv.stroke_round_rect(r, 9, if self.editing { theme::accent() } else { t.control_border });
        let font = theme::ui(14);
        let text_x = r.x + 30;
        let base_y = r.y + (r.h + font.ascent as i32 + font.descent as i32) / 2;
        let secure = self.url.as_ref().is_some_and(|u| u.scheme == "https");
        // Lock (https) or globe.
        let (ix, iy) = (r.x + 15, r.y + r.h / 2);
        if secure && !self.editing {
            cv.stroke_round_rect(Rect::new(ix - 4, iy - 7, 8, 8), 4, t.text_secondary);
            cv.fill_round_rect(Rect::new(ix - 5, iy - 2, 10, 8), 2, t.text_secondary);
        } else {
            cv.stroke_round_rect(Rect::new(ix - 6, iy - 6, 12, 12), 6, t.text_secondary);
            cv.fill_rect(Rect::new(ix - 6, iy, 12, 1), t.text_secondary);
        }
        cv.with_clip(Rect::new(text_x, r.y, r.w - 40, r.h), |cv| {
            let text = self.address.text();
            if self.editing {
                let caret_x = font.width(&text[..self.address.caret()]);
                let shift = (caret_x - (r.w - 50)).max(0);
                let tx = text_x - shift;
                if let Some((a, b)) = self.address.selection() {
                    let (xa, xb) = (font.width(&text[..a]), font.width(&text[..b]));
                    cv.fill_rect(Rect::new(tx + xa, r.y + 6, xb - xa, r.h - 12), theme::accent() & 0x00FF_FFFF | 0x5000_0000);
                }
                cv.text(tx, base_y, text, font, t.text);
                if env.focused {
                    cv.fill_rect(Rect::new(tx + caret_x, r.y + 7, 2, r.h - 14), theme::accent());
                }
            } else if text == HOME || text.is_empty() {
                cv.text(text_x, base_y, "Search or enter an address", font, t.text_secondary);
            } else {
                // Host in full colour, the rest dimmed.
                let (pre, rest) = text.split_once("://").unwrap_or(("", text));
                let (host, path) = rest.find('/').map_or((rest, ""), |p| rest.split_at(p));
                let mut x = text_x;
                if !pre.is_empty() && pre != "https" {
                    x += cv.text(x, base_y, &format!("{pre}://"), font, t.text_secondary);
                }
                x += cv.text(x, base_y, host, font, t.text);
                cv.text(x, base_y, path, font, t.text_secondary);
            }
        });
        // Progress.
        if self.loading() {
            let elapsed = (env.now_ms - self.loading_since) as i32;
            let frac = 1000 - 1000 * 1000 / (1000 + elapsed);
            let w = r.w * frac / 1000;
            cv.fill_round_rect(Rect::new(r.x + 6, r.bottom() - 3, w.max(8) - 12, 2), 1, theme::accent());
        }
    }

    fn paint_status(&self, cv: &mut Canvas) {
        let text = match (&self.hover, &self.job) {
            (Some(h), _) => h.clone(),
            (None, Some((job, _))) => job.progress.lock().clone(),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let t = theme::current();
        let font = theme::ui(12);
        let w = (font.width(&text) + 20).min(self.view.w - 20);
        let r = Rect::new(self.view.x + 8, self.view.bottom() - 30, w, 24);
        cv.fill_round_rect(r, 7, t.panel_tint | 0xFF00_0000);
        cv.stroke_round_rect(r, 7, t.separator);
        cv.text_clipped(r.x + 10, r.y + 16, &text, font, t.text, r.w - 20);
    }

    fn page_key(&mut self, ev: &KeyEvent) -> bool {
        let page_step = self.view.h - 60;
        let before = self.scroll;
        match ev.code {
            KeyCode::Down => self.scroll += 48,
            KeyCode::Up => self.scroll -= 48,
            KeyCode::PageDown => self.scroll += page_step,
            KeyCode::PageUp => self.scroll -= page_step,
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = i32::MAX / 2,
            KeyCode::Char if ev.ch == Some(' ') => self.scroll += if ev.mods.shift { -page_step } else { page_step },
            KeyCode::Backspace => {
                self.back();
                return true;
            }
            _ => return false,
        }
        self.clamp_scroll();
        self.scroll != before
    }
}

impl App for Surf {
    fn title(&self) -> String {
        match self.page.as_ref().map(|p| p.title.as_str()) {
            Some(t) if !t.is_empty() => String::from(t),
            _ if self.loading() => String::from("Loading…"),
            _ => String::from("Surf"),
        }
    }

    fn size(&self) -> (i32, i32) {
        (1100, 760)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.view = Rect::new(area.x, area.y + TOOLBAR_H, area.w, area.h - TOOLBAR_H);
        if self.page.is_some() && self.laid_out_width != self.view.w {
            self.relayout();
        }
        self.paint_page(cv);
        self.paint_toolbar(cv, area, env);
        self.paint_status(cv);
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        let (ctrl, alt) = (ev.mods.ctrl || ev.mods.super_key, ev.mods.alt);
        // Browser shortcuts.
        if ctrl && ev.code == KeyCode::Char {
            match ev.ch.map(|c| c.to_ascii_lowercase()) {
                Some('l') => {
                    self.editing = true;
                    self.focus = None;
                    self.address.select_all();
                    return true;
                }
                Some('r') => {
                    self.reload();
                    return true;
                }
                Some('[') => {
                    self.back();
                    return true;
                }
                Some(']') => {
                    self.forward();
                    return true;
                }
                _ => {}
            }
        }
        if ev.code == KeyCode::F5 {
            self.reload();
            return true;
        }
        if alt && ev.code == KeyCode::Left {
            self.back();
            return true;
        }
        if alt && ev.code == KeyCode::Right {
            self.forward();
            return true;
        }
        if self.editing {
            match ev.code {
                KeyCode::Enter => {
                    let t = String::from(self.address.text());
                    self.go(&t, true);
                    return true;
                }
                KeyCode::Escape => {
                    self.editing = false;
                    let cur = self.history.get(self.pos).cloned().unwrap_or_default();
                    self.address.set_text(&cur);
                    return true;
                }
                _ => return self.address.handle_key(ev, env.now_ms).handled,
            }
        }
        if let Some(node) = self.focus {
            match ev.code {
                KeyCode::Enter => {
                    self.submit(node);
                    return true;
                }
                KeyCode::Escape => {
                    self.focus = None;
                    return true;
                }
                KeyCode::Tab => {}
                _ => {
                    if !self.fields.contains_key(&node) {
                        let v = self.field_value(node);
                        self.fields.insert(node, TextEdit::new(&v, false));
                    }
                    let e = self.fields.get_mut(&node).unwrap();
                    return e.handle_key(ev, env.now_ms).handled;
                }
            }
        }
        if ev.code == KeyCode::Escape && self.loading() {
            self.stop();
            return true;
        }
        self.page_key(ev)
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        self.press = Some((x, y));
        if y < TOOLBAR_H {
            self.focus = None;
            if self.back_rect().contains(x, y) {
                self.back();
            } else if self.forward_rect().contains(x, y) {
                self.forward();
            } else if self.reload_rect().contains(x, y) {
                if self.loading() {
                    self.stop();
                } else {
                    self.reload();
                }
            } else if self.address_rect(area).contains(x, y) {
                if !self.editing {
                    self.editing = true;
                    if self.address.text() == HOME {
                        self.address.set_text("");
                    }
                    self.address.select_all();
                }
            }
            return true;
        }
        let was_editing = core::mem::take(&mut self.editing);
        if was_editing {
            let cur = self.history.get(self.pos).cloned().unwrap_or_default();
            self.address.set_text(&cur);
        }
        let Some((px, py)) = self.page_point(x, y) else { return was_editing };
        let Some(l) = &self.layout else { return was_editing };
        if let Some(node) = l.field_at(px, py) {
            let doc = self.doc().unwrap();
            let e = doc.element(node).unwrap();
            let kind = e.attr("type").unwrap_or("text").to_ascii_lowercase();
            match (e.tag.as_str(), kind.as_str()) {
                ("input", "submit" | "image") | ("button", _) => {
                    if e.tag == "button" && kind == "button" {
                        return was_editing;
                    }
                    self.submit(node);
                }
                ("input", "checkbox" | "radio") => {
                    let checked = e.attr("checked").is_some();
                    let radio = kind == "radio";
                    let name = String::from(e.attr("name").unwrap_or(""));
                    let page = self.page.as_mut().unwrap();
                    if radio {
                        // Uncheck the group.
                        for n in page.doc.find_all("input") {
                            if let aurora_surf::dom::NodeData::Element(el) = &mut page.doc.nodes[n].data {
                                if el.attr("name") == Some(name.as_str()) && el.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio")) {
                                    el.attrs.retain(|(k, _)| k != "checked");
                                }
                            }
                        }
                    }
                    if let aurora_surf::dom::NodeData::Element(el) = &mut page.doc.nodes[node].data {
                        if checked && !radio {
                            el.attrs.retain(|(k, _)| k != "checked");
                        } else if !checked {
                            el.attrs.push((String::from("checked"), String::new()));
                        }
                    }
                    self.relayout();
                }
                ("input", "hidden") => {}
                _ => {
                    self.focus = Some(node);
                    if !self.fields.contains_key(&node) {
                        let v = self.field_value(node);
                        self.fields.insert(node, TextEdit::new(&v, false));
                    }
                    if let Some(e) = self.fields.get_mut(&node) {
                        let end = e.text().len();
                        e.set_caret(end, false);
                    }
                }
            }
            return true;
        }
        self.focus = None;
        if let Some(link) = l.link_at(px, py) {
            self.follow(link);
            return true;
        }
        true
    }

    fn hover(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        let link = self.page_point(x, y).and_then(|(px, py)| self.layout.as_ref()?.link_at(px, py));
        let text = link.and_then(|l| self.href(l));
        if text != self.hover {
            self.hover = text;
            return true;
        }
        false
    }

    fn scroll(&mut self, delta: i32, _area: Rect) -> bool {
        let before = self.scroll;
        self.scroll -= delta * 48;
        self.clamp_scroll();
        self.scroll != before
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        let mut dirty = false;
        // A finished page load.
        let done = self.job.as_ref().and_then(|(job, _)| job.result.lock().take());
        if let Some(result) = done {
            let (_, h) = self.job.take().unwrap();
            h.join();
            let target = self.history.get(self.pos).cloned().unwrap_or_default();
            match result {
                Ok(loaded) => self.install(loaded),
                Err(msg) => self.show_error(&target, &msg),
            }
            dirty = true;
        }
        // Finished images.
        let finished: Vec<(u64, String, Option<Image>)> = core::mem::take(&mut self.queue.lock().done);
        if !finished.is_empty() {
            let g = self.generation.load(Ordering::Relaxed);
            let mut any = false;
            for (gen, url, img) in finished {
                if gen == g {
                    any |= img.is_some();
                    self.images.insert(url, img);
                }
            }
            if any && self.relayout_at.is_none() {
                self.relayout_at = Some(env.now_ms + 200);
            }
        }
        if self.relayout_at.is_some_and(|t| env.now_ms >= t) {
            self.relayout_at = None;
            self.relayout();
            dirty = true;
        }
        self.old_jobs.retain(|h| !h.is_finished());
        dirty | self.loading()
    }

    fn tick_interval(&self) -> u64 {
        if self.loading() || self.relayout_at.is_some() || !self.workers.iter().all(|w| w.is_finished()) {
            50
        } else {
            500
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// application/x-www-form-urlencoded.
fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

const HOME_PAGE: &str = r#"<!doctype html><html><head><title>Start Page</title><style>
body { margin: 0; font-family: sans-serif; background: #f3f4f8; color: #1d1d24 }
.hero { text-align: center; padding: 72px 16px 20px }
.logo { font-size: 46px; font-weight: 600; color: #0a6cff; margin: 0; letter-spacing: -1px }
.tag { color: #6e6e7a; margin: 4px 0 28px; font-size: 15px }
form { display: flex; justify-content: center; gap: 8px }
input[type=search] { width: 440px; font-size: 16px; padding: 9px 16px; border: 1px solid #c8c8d2; border-radius: 22px; background: #fff }
button { font-size: 15px; padding: 9px 20px; border-radius: 22px; border: 0; background: #0a6cff; color: #fff }
.grid { display: flex; flex-wrap: wrap; justify-content: center; gap: 14px; max-width: 780px; margin: 36px auto 0; padding: 0 16px }
.card { display: block; width: 200px; padding: 14px 16px; background: #fff; border-radius: 12px; border: 1px solid #e2e2ea; text-decoration: none; color: inherit }
.card b { display: block; font-size: 15px; margin-bottom: 3px; color: #1d1d24 }
.card span { color: #6e6e7a; font-size: 13px }
.foot { text-align: center; color: #9a9aa6; font-size: 12px; margin: 40px 0 }
</style></head><body>
<div class="hero"><p class="logo">Surf</p><p class="tag">The web, on WaveOS Aurora</p>
<form action="https://html.duckduckgo.com/html/" method="get"><input type="search" name="q" placeholder="Search the web"><button type="submit">Search</button></form></div>
<div class="grid">
<a class="card" href="https://en.wikipedia.org/wiki/Main_Page"><b>Wikipedia</b><span>The free encyclopedia</span></a>
<a class="card" href="https://news.ycombinator.com/"><b>Hacker News</b><span>Links and discussion</span></a>
<a class="card" href="https://text.npr.org/"><b>NPR</b><span>News in plain text</span></a>
<a class="card" href="https://lite.cnn.com/"><b>CNN Lite</b><span>Headlines, lightly</span></a>
<a class="card" href="https://www.rust-lang.org/"><b>Rust</b><span>The language Aurora is written in</span></a>
<a class="card" href="https://github.com/Sw3bbl3/waveos-aurora-rs"><b>WaveOS Aurora</b><span>This system's source code</span></a>
</div>
<p class="foot">Surf 0.6 — HTML, CSS, layout, TLS and TCP/IP written for WaveOS Aurora</p>
</body></html>"#;

fn main(args: aurora::Args) -> i32 {
    let start = match args.get(1) {
        Some(a) if a.starts_with('/') => format!("file://{a}"),
        Some(a) => a.clone(),
        None => String::from(HOME),
    };
    ripple::run(Surf::new(&start))
}
