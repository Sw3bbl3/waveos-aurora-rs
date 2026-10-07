//! The Nebula web engine: HTML parsing, CSS, style and layout to a display
//! list. No system calls and no drawing: the app measures text, loads
//! resources and paints. Runs (and is tested) on the host.

#![no_std]

extern crate alloc;

pub mod css;
pub mod dom;
pub mod html;
pub mod layout;
pub mod script;
pub mod style;
pub mod values;

pub use nebula_script;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use css::Stylesheet;
use dom::{Document, NodeId};
pub use layout::{FontSpec, Host, Item, Layout, Metrics, Rect};
use nebula_script::Realm;

/// A stylesheet in document order: inline, or linked (loaded later).
pub enum Sheet {
    Inline(Stylesheet),
    Linked { href: String, sheet: Option<Stylesheet> },
}

/// A script of the page: inline source, or a URL to load.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptSource {
    Inline(String),
    External(String),
}

pub struct Page {
    pub doc: Document,
    pub title: String,
    pub sheets: Vec<Sheet>,
    /// From <base href>, if any.
    pub base: Option<String>,
    /// No doctype: quirks mode.
    pub quirks: bool,
    ua: Stylesheet,
}

/// A page's JavaScript: its realm and DOM state. Kept by the browser beside
/// the [`Page`] (on the thread that runs scripts; a Page itself can move
/// between threads).
pub struct Scripts {
    pub realm: Realm,
}

impl Scripts {
    pub fn new(
        host: Box<dyn nebula_script::Host>,
        url: &str,
        viewport: (i32, i32),
        storage: BTreeMap<String, String>,
    ) -> Scripts {
        let mut realm = Realm::new(host);
        realm.step_limit = 200_000_000;
        script::install(&mut realm, url, viewport, storage);
        Scripts { realm }
    }

    /// When scripts next want to run: the earliest timer, or soon when
    /// animation frames are waiting.
    pub fn next_wakeup(&mut self, now: f64) -> Option<f64> {
        let (timer, frames) = script::pending(&mut self.realm);
        if frames {
            return Some(now + 16.0);
        }
        timer
    }

    /// A navigation a script asked for (`location.href = …`).
    pub fn take_navigation(&mut self) -> Option<String> {
        script::dom_mut(&mut self.realm).navigate.take()
    }

    /// localStorage, if a script changed it.
    pub fn take_storage(&mut self) -> Option<BTreeMap<String, String>> {
        let dom = script::dom_mut(&mut self.realm);
        if core::mem::take(&mut dom.storage_dirty) {
            Some(dom.storage.clone())
        } else {
            None
        }
    }

    /// Tells scripts where everything is: element boxes from a layout (in
    /// CSS pixels), the page size and the scroll position.
    pub fn set_layout(&mut self, layout: &Layout, scroll: (i32, i32)) {
        let dom = script::dom_mut(&mut self.realm);
        dom.geometry.clear();
        for f in layout.boxes.iter().chain(layout.fields.iter()) {
            let r = f.rect;
            dom.geometry
                .entry(f.node)
                .and_modify(|g| {
                    // The union of an element's boxes.
                    let (x0, y0) = (g.0.min(r.x), g.1.min(r.y));
                    let (x1, y1) = ((g.0 + g.2).max(r.x + r.w), (g.1 + g.3).max(r.y + r.h));
                    *g = (x0, y0, x1 - x0, y1 - y0);
                })
                .or_insert((r.x, r.y, r.w, r.h));
        }
        dom.page_size = (layout.width, layout.height);
        dom.scroll = scroll;
    }

    /// Network requests scripts made (`fetch`, `XMLHttpRequest`).
    pub fn take_requests(&mut self) -> Vec<script::HttpRequest> {
        core::mem::take(&mut script::dom_mut(&mut self.realm).requests)
    }

    /// A scroll position a script asked for.
    pub fn take_scroll_request(&mut self) -> Option<(i32, i32)> {
        script::dom_mut(&mut self.realm).scroll_request.take()
    }

    /// External scripts that scripts inserted, to fetch: (element, src).
    pub fn take_fetches(&mut self) -> Vec<(NodeId, String)> {
        core::mem::take(&mut script::dom_mut(&mut self.realm).fetches)
    }

    /// The element scripts last focused, if any.
    pub fn focus(&mut self) -> Option<NodeId> {
        script::dom_mut(&mut self.realm).focus
    }

    pub fn set_viewport(&mut self, w: i32, h: i32) {
        script::dom_mut(&mut self.realm).viewport = (w, h);
    }
}

impl Page {
    pub fn parse(source: &str) -> Page {
        let doc = html::parse(source);
        let title = doc.find("title").map(|t| collapse_ws(&doc.text_content(t))).unwrap_or_default();
        let base = doc.find("base").and_then(|b| doc.element(b)?.attr("href").map(String::from));
        let mut sheets = Vec::new();
        for n in doc.descendants(Document::ROOT) {
            let Some(e) = doc.element(n) else { continue };
            let media_ok = e
                .attr("media")
                .is_none_or(|m| css::media_matches(&m.to_ascii_lowercase(), 1024, 768) || m.contains("width"));
            match e.tag.as_str() {
                "style" if media_ok => sheets.push(Sheet::Inline(css::parse(&doc.text_content(n)))),
                "link" if media_ok => {
                    let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
                    let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
                    if rels.contains(&"stylesheet") && !rels.contains(&"alternate") {
                        if let Some(href) = e.attr("href") {
                            sheets.push(Sheet::Linked { href: String::from(href.trim()), sheet: None });
                        }
                    }
                }
                _ => {}
            }
        }
        let quirks = !has_doctype(source);
        let mut ua = css::parse(style::UA_CSS);
        if quirks {
            ua.rules.extend(css::parse(style::QUIRKS_CSS).rules);
        }
        Page { doc, title, sheets, base, quirks, ua }
    }

    // ------------------------------------------------------------ scripts

    /// The classic scripts to run, in document order. Modules are skipped
    /// (so `nomodule` fallbacks run), as are data blocks like JSON.
    pub fn scripts(&self) -> Vec<ScriptSource> {
        let mut out = Vec::new();
        for n in self.doc.find_all("script") {
            let e = self.doc.element(n).unwrap();
            let ty = e.attr("type").unwrap_or("").trim().to_ascii_lowercase();
            let js = ty.is_empty()
                || matches!(
                    ty.as_str(),
                    "text/javascript"
                        | "application/javascript"
                        | "text/ecmascript"
                        | "application/ecmascript"
                        | "text/jscript"
                        | "text/livescript"
                        | "application/x-javascript"
                );
            if !js {
                continue;
            }
            match e.attr("src") {
                Some(src) if !src.trim().is_empty() => out.push(ScriptSource::External(String::from(src.trim()))),
                _ => out.push(ScriptSource::Inline(self.doc.text_content(n))),
            }
        }
        out
    }

    /// Runs `f` with the document inside the realm.
    pub fn with_js<R>(&mut self, js: &mut Scripts, f: impl FnOnce(&mut Realm) -> R) -> R {
        let rt = &mut js.realm;
        script::dom_mut(rt).doc = core::mem::take(&mut self.doc);
        // Each turn of the event loop gets a fresh budget.
        rt.steps = 0;
        rt.interrupted = false;
        let r = f(rt);
        self.doc = core::mem::take(&mut script::dom_mut(rt).doc);
        if let Some(t) = self.doc.find("title") {
            self.title = collapse_ws(&self.doc.text_content(t));
        }
        r
    }

    /// Runs a script that a script inserted, once the browser fetched it
    /// (None: it couldn't be loaded).
    pub fn run_fetched(&mut self, js: &mut Scripts, node: NodeId, name: &str, source: Option<&str>) {
        self.with_js(js, |rt| script::run_fetched(rt, node, name, source));
    }

    /// Answers a script's network request (Err: it failed, with why).
    pub fn complete_request(&mut self, js: &mut Scripts, id: u32, result: Result<script::HttpResponse, String>) {
        self.with_js(js, |rt| script::complete_request(rt, id, result));
    }

    /// Runs one script; errors go to the console.
    pub fn run_script(&mut self, js: &mut Scripts, source: &str, name: &str) {
        // Scripts the parser put in the page never run again if moved.
        let parsed = self.doc.find_all("script");
        script::dom_mut(&mut js.realm).started_scripts.extend(parsed);
        self.with_js(js, |rt| {
            let r = rt.eval(source, name);
            script::report(rt, r);
            rt.run_jobs();
        });
    }

    /// After the scripts: DOMContentLoaded, then load.
    pub fn finish_loading(&mut self, js: &mut Scripts) {
        self.with_js(js, |rt| {
            script::dom_mut(rt).ready_state = "interactive";
            script::fire(rt, Document::ROOT, "readystatechange", false, false);
            script::fire(rt, Document::ROOT, "DOMContentLoaded", true, false);
            script::dom_mut(rt).ready_state = "complete";
            script::fire(rt, Document::ROOT, "readystatechange", false, false);
            script::fire(rt, script::WINDOW_TARGET, "load", false, false);
            rt.report_unhandled();
        });
    }

    /// A click on `node` (mousedown, mouseup, click). Returns whether the
    /// browser should carry out the default action (follow, submit, …).
    pub fn dispatch_click(&mut self, js: &mut Scripts, node: NodeId, x: i32, y: i32) -> bool {
        self.with_js(js, |rt| {
            script::fire_mouse(rt, node, "mousedown", x, y);
            script::fire_mouse(rt, node, "mouseup", x, y);
            script::fire_mouse(rt, node, "click", x, y)
        })
    }

    /// A plain event (input, change, submit, focus…). Returns whether the
    /// default action should happen.
    pub fn dispatch(&mut self, js: &mut Scripts, node: NodeId, kind: &str, bubbles: bool, cancelable: bool) -> bool {
        self.with_js(js, |rt| script::fire(rt, node, kind, bubbles, cancelable))
    }

    /// keydown/keyup at `node`.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_key(
        &mut self,
        js: &mut Scripts,
        node: NodeId,
        kind: &str,
        key: &str,
        code: &str,
        ctrl: bool,
        shift: bool,
        alt: bool,
    ) -> bool {
        self.with_js(js, |rt| script::fire_key(rt, node, kind, key, code, ctrl, shift, alt))
    }

    /// Runs due timers and animation frames. Returns whether any ran.
    pub fn run_timers(&mut self, js: &mut Scripts, now: f64) -> bool {
        self.with_js(js, |rt| {
            let a = script::fire_timers(rt, now);
            let b = script::fire_frames(rt, now);
            rt.report_unhandled();
            a || b
        })
    }

    /// Linked stylesheets still to load: (slot, href).
    pub fn pending_stylesheets(&self) -> Vec<(usize, String)> {
        self.sheets
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                Sheet::Linked { href, sheet: None } => Some((i, href.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn set_stylesheet(&mut self, slot: usize, css_text: &str) {
        if let Some(Sheet::Linked { sheet, .. }) = self.sheets.get_mut(slot) {
            *sheet = Some(css::parse(css_text));
        }
    }

    /// Image sources in document order, without duplicates.
    pub fn images(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in self.doc.find_all("img") {
            if let Some(src) = layout::image_source(self.doc.element(n).unwrap()) {
                if !out.contains(&src) {
                    out.push(src);
                }
            }
        }
        out
    }

    /// Lays the page out `width` × `height` CSS pixels wide (the viewport).
    pub fn layout(&self, width: i32, height: i32, host: &dyn Host) -> Layout {
        let styles = self.styles(width, height);
        self.layout_with(&styles, width, host)
    }

    /// Lays out with styles computed earlier by [`Page::styles`] (for the same
    /// viewport): relayouts after images arrive needn't redo the cascade.
    pub fn layout_with(&self, styles: &style::Computed, width: i32, host: &dyn Host) -> Layout {
        layout::layout(&self.doc, &styles.styles, &styles.pseudo, host, width)
    }

    /// Runs the cascade for a viewport.
    pub fn styles(&self, width: i32, height: i32) -> style::Computed {
        let author: Vec<&Stylesheet> = self
            .sheets
            .iter()
            .filter_map(|s| match s {
                Sheet::Inline(s) => Some(s),
                Sheet::Linked { sheet, .. } => sheet.as_ref(),
            })
            .collect();
        let cascade = style::Cascade::new(&self.ua, &author, width, height);
        style::compute(&self.doc, &cascade, (width, height))
    }

    pub fn element(&self, n: NodeId) -> Option<&dom::Element> {
        self.doc.element(n)
    }

    /// `<meta http-equiv=refresh content="0; url=…">`: (seconds, target).
    pub fn refresh(&self) -> Option<(u32, String)> {
        for m in self.doc.find_all("meta") {
            let e = self.doc.element(m)?;
            if e.attr("http-equiv").is_some_and(|h| h.eq_ignore_ascii_case("refresh")) {
                let content = e.attr("content")?;
                let (secs, rest) = content.split_once([';', ',']).unwrap_or((content, ""));
                let url = rest
                    .trim()
                    .trim_start_matches(|c: char| c.is_ascii_alphabetic() && c != '=')
                    .trim_start_matches('=');
                let url = url.trim().trim_matches(|c| c == '\'' || c == '"');
                return Some((secs.trim().parse().unwrap_or(0), String::from(url)));
            }
        }
        None
    }
}

/// Whether the source starts (after comments and whitespace) with `<!doctype html`.
fn has_doctype(s: &str) -> bool {
    let mut rest = s.trim_start_matches('\u{FEFF}').trim_start();
    while let Some(r) = rest.strip_prefix("<!--") {
        rest = r.split_once("-->").map_or("", |(_, after)| after).trim_start();
    }
    rest.get(..14).is_some_and(|d| d.eq_ignore_ascii_case("<!doctype html"))
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Decodes a document's bytes: UTF-8 unless the header or a <meta> says Latin-1/Windows-1252.
pub fn decode(bytes: &[u8], content_type: &str) -> String {
    let lower_ct = content_type.to_ascii_lowercase();
    let mut charset = lower_ct.split("charset=").nth(1).map(|c| String::from(c.trim().trim_matches('"')));
    if charset.is_none() {
        let head = &bytes[..bytes.len().min(2048)];
        let head = String::from_utf8_lossy(head).to_ascii_lowercase();
        if let Some(p) = head.find("charset=") {
            let rest = &head[p + 8..];
            let rest = rest.trim_start_matches(['"', '\'']);
            let name: String =
                rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
            charset = Some(name);
        }
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match charset.as_deref() {
        Some("iso-8859-1" | "latin1" | "windows-1252" | "cp1252" | "us-ascii" | "iso-8859-15") => bytes
            .iter()
            .map(|&b| match b {
                0x80..=0x9F => html::WINDOWS_1252[(b - 0x80) as usize],
                b => b as char,
            })
            .collect(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}
