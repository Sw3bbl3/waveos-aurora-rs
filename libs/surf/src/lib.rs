//! The Surf web engine: HTML parsing, CSS, style and layout to a display
//! list. No system calls and no drawing: the app measures text, loads
//! resources and paints. Runs (and is tested) on the host.

#![no_std]

extern crate alloc;

pub mod css;
pub mod dom;
pub mod html;
pub mod layout;
pub mod style;
pub mod values;

use alloc::string::String;
use alloc::vec::Vec;
use css::Stylesheet;
use dom::{Document, NodeId};
pub use layout::{FontSpec, Host, Item, Layout, Metrics, Rect};

/// A stylesheet in document order: inline, or linked (loaded later).
pub enum Sheet {
    Inline(Stylesheet),
    Linked { href: String, sheet: Option<Stylesheet> },
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

impl Page {
    pub fn parse(source: &str) -> Page {
        let doc = html::parse(source);
        let title = doc.find("title").map(|t| collapse_ws(&doc.text_content(t))).unwrap_or_default();
        let base = doc.find("base").and_then(|b| doc.element(b)?.attr("href").map(String::from));
        let mut sheets = Vec::new();
        for n in doc.descendants(Document::ROOT) {
            let Some(e) = doc.element(n) else { continue };
            let media_ok = e.attr("media").is_none_or(|m| css::media_matches(&m.to_ascii_lowercase(), 1024, 768) || m.contains("width"));
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
                let url = rest.trim().trim_start_matches(|c: char| c.is_ascii_alphabetic() && c != '=').trim_start_matches('=');
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
            let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
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
