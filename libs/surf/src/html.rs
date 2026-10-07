//! HTML parsing: a tokenizer and a forgiving tree builder.
//!
//! This follows the spirit of the HTML5 algorithm, not its letter: raw-text
//! elements, void elements, character references and the common implied end
//! tags (p, li, dt/dd, table parts, option) are handled, which is what real
//! pages lean on. Misnested formatting tags are closed rather than adopted.

use crate::dom::{Document, Element, NodeData, NodeId};
use alloc::string::String;
use alloc::vec::Vec;

const VOID: &[&str] =
    &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title", "xmp", "iframe", "noembed", "plaintext"];
/// Start tags that close an open <p>.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
    "dd",
    "dt",
];

pub fn parse(html: &str) -> Document {
    let mut b =
        Builder { doc: Document::default(), stack: alloc::vec![Document::ROOT], html: None, head: None, body: None };
    let mut t = Tokenizer { s: html, i: 0 };
    while let Some(tok) = t.next() {
        match tok {
            Token::Text(s) => b.text(&s),
            Token::Start { name, attrs, self_closing } => {
                let raw = RAW_TEXT.contains(&name.as_str()) && !self_closing;
                let is_void = VOID.contains(&name.as_str()) || self_closing;
                let el = b.start(name.clone(), attrs, is_void);
                if raw {
                    let content = t.raw_text(&name);
                    let content = if name == "textarea" || name == "title" { decode(&content) } else { content };
                    if let Some(el) = el {
                        if !content.is_empty() {
                            b.doc.add(el, NodeData::Text(content));
                        }
                        b.end(&name);
                    }
                }
            }
            Token::End(name) => b.end(&name),
        }
    }
    // Every document has <html>, <head> and <body>, as in browsers.
    let html = match b.doc.find("html") {
        Some(h) => h,
        None => {
            let h = b.doc.create(NodeData::Element(Element { tag: String::from("html"), attrs: Vec::new() }));
            for c in core::mem::take(&mut b.doc.nodes[Document::ROOT].children) {
                b.doc.nodes[c].parent = None;
                b.doc.insert(h, c, None);
            }
            b.doc.insert(Document::ROOT, h, None);
            h
        }
    };
    if b.doc.find("head").is_none() {
        let head = b.doc.create(NodeData::Element(Element { tag: String::from("head"), attrs: Vec::new() }));
        // Head content before the body moves into the head.
        let leading: Vec<NodeId> = b.doc.nodes[html]
            .children
            .iter()
            .copied()
            .take_while(|c| b.doc.tag(*c) != "body")
            .filter(|c| HEAD_CONTENT.contains(&b.doc.tag(*c)))
            .collect();
        let first = b.doc.nodes[html].children.first().copied();
        b.doc.insert(html, head, first);
        for c in leading {
            b.doc.insert(head, c, None);
        }
    }
    if b.doc.find("body").is_none() {
        let body = b.doc.create(NodeData::Element(Element { tag: String::from("body"), attrs: Vec::new() }));
        b.doc.insert(html, body, None);
    }
    b.doc.version = 0;
    b.doc
}

enum Token {
    Text(String),
    Start { name: String, attrs: Vec<(String, String)>, self_closing: bool },
    End(String),
}

struct Tokenizer<'a> {
    s: &'a str,
    i: usize,
}

impl Tokenizer<'_> {
    fn rest(&self) -> &str {
        &self.s[self.i..]
    }

    fn peek(&self, k: usize) -> u8 {
        *self.s.as_bytes().get(self.i + k).unwrap_or(&0)
    }

    /// Skips past `end` (or to the end of input).
    fn skip_past(&mut self, end: &str) {
        match self.rest().find(end) {
            Some(p) => self.i += p + end.len(),
            None => self.i = self.s.len(),
        }
    }

    fn next(&mut self) -> Option<Token> {
        loop {
            if self.i >= self.s.len() {
                return None;
            }
            if self.peek(0) != b'<' {
                // The rest doesn't start with '<', so any '<' found is past the first char.
                let end = self.rest().find('<').map_or(self.s.len(), |p| self.i + p);
                let text = &self.s[self.i..end];
                self.i = end;
                return Some(Token::Text(decode(text)));
            }
            let c = self.peek(1);
            if c == b'!' {
                if self.rest().starts_with("<!--") {
                    self.i += 4;
                    self.skip_past("-->");
                } else if self.rest().starts_with("<![CDATA[") {
                    self.i += 9;
                    let end = self.rest().find("]]>").map_or(self.s.len(), |p| self.i + p);
                    let text = String::from(&self.s[self.i..end]);
                    self.i = (end + 3).min(self.s.len());
                    return Some(Token::Text(text));
                } else {
                    self.skip_past(">");
                }
                continue;
            }
            if c == b'?' {
                self.skip_past(">");
                continue;
            }
            if c == b'/' && self.peek(2).is_ascii_alphabetic() {
                self.i += 2;
                let name = self.name();
                self.skip_past(">");
                return Some(Token::End(name));
            }
            if c == b'/' {
                // "</>" or "</ ..." : a bogus comment.
                self.skip_past(">");
                continue;
            }
            if c.is_ascii_alphabetic() {
                self.i += 1;
                return Some(self.start_tag());
            }
            // A lone '<' is text.
            self.i += 1;
            return Some(Token::Text(String::from("<")));
        }
    }

    fn name(&mut self) -> String {
        let start = self.i;
        while self.i < self.s.len() && !matches!(self.peek(0), b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>') {
            self.i += 1;
        }
        self.s[start..self.i].to_ascii_lowercase()
    }

    fn skip_ws(&mut self) {
        while self.peek(0).is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn start_tag(&mut self) -> Token {
        let name = self.name();
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            self.skip_ws();
            match self.peek(0) {
                0 => break,
                b'>' => {
                    self.i += 1;
                    break;
                }
                b'/' => {
                    self.i += 1;
                    if self.peek(0) == b'>' {
                        self_closing = true;
                    }
                    continue;
                }
                _ => {}
            }
            let start = self.i;
            while self.i < self.s.len()
                && !matches!(self.peek(0), b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>' | b'=')
            {
                self.i += 1;
            }
            if self.i == start {
                // A stray '=' or similar.
                self.i += 1;
                continue;
            }
            let key = self.s[start..self.i].to_ascii_lowercase();
            self.skip_ws();
            let mut value = String::new();
            if self.peek(0) == b'=' {
                self.i += 1;
                self.skip_ws();
                let q = self.peek(0);
                if q == b'"' || q == b'\'' {
                    self.i += 1;
                    let end = self.rest().find(q as char).map_or(self.s.len(), |p| self.i + p);
                    value = decode(&self.s[self.i..end]);
                    self.i = (end + 1).min(self.s.len());
                } else {
                    let vs = self.i;
                    while self.i < self.s.len() && !matches!(self.peek(0), b' ' | b'\t' | b'\n' | b'\r' | b'>') {
                        self.i += 1;
                    }
                    value = decode(&self.s[vs..self.i]);
                }
            }
            if !attrs.iter().any(|(k, _)| *k == key) {
                attrs.push((key, value));
            }
        }
        Token::Start { name, attrs, self_closing }
    }

    /// The contents of a raw-text element, up to its end tag.
    fn raw_text(&mut self, name: &str) -> String {
        let bytes = self.s.as_bytes();
        let mut j = self.i;
        while j + 2 + name.len() <= bytes.len() {
            if bytes[j] == b'<'
                && bytes[j + 1] == b'/'
                && bytes[j + 2..j + 2 + name.len()].eq_ignore_ascii_case(name.as_bytes())
                && !bytes.get(j + 2 + name.len()).is_some_and(|c| c.is_ascii_alphanumeric())
            {
                let text = String::from(&self.s[self.i..j]);
                self.i = j;
                return text;
            }
            j += 1;
        }
        let text = String::from(&self.s[self.i..]);
        self.i = self.s.len();
        text
    }
}

struct Builder {
    doc: Document,
    stack: Vec<NodeId>,
    html: Option<NodeId>,
    head: Option<NodeId>,
    body: Option<NodeId>,
}

/// Elements that may sit in <head> (before <body> is implied).
const HEAD_CONTENT: &[&str] = &["meta", "title", "link", "style", "script", "base", "template", "noscript"];

impl Builder {
    fn current(&self) -> NodeId {
        *self.stack.last().unwrap()
    }

    fn open(&self, tag: &str) -> Option<usize> {
        self.stack.iter().rposition(|n| self.doc.tag(*n) == tag)
    }

    /// Closes an open `tags` element, unless a `boundary` element is nearer.
    fn close_within(&mut self, tags: &[&str], boundary: &[&str]) {
        for pos in (1..self.stack.len()).rev() {
            let t = self.doc.tag(self.stack[pos]);
            if tags.contains(&t) {
                self.stack.truncate(pos);
                return;
            }
            if boundary.contains(&t) {
                return;
            }
        }
    }

    fn element(&mut self, parent: NodeId, tag: &str, attrs: Vec<(String, String)>) -> NodeId {
        self.doc.add(parent, NodeData::Element(Element { tag: String::from(tag), attrs }))
    }

    fn ensure_html(&mut self) -> NodeId {
        if let Some(h) = self.html {
            return h;
        }
        let h = self.element(Document::ROOT, "html", Vec::new());
        self.html = Some(h);
        self.stack = alloc::vec![Document::ROOT, h];
        h
    }

    fn ensure_body(&mut self) {
        if self.body.is_some() {
            return;
        }
        let html = self.ensure_html();
        // Leave <head> (and anything open in it).
        if let Some(pos) = self.stack.iter().position(|n| *n == html) {
            self.stack.truncate(pos + 1);
        }
        let b = self.element(html, "body", Vec::new());
        self.body = Some(b);
        self.stack.push(b);
    }

    /// Merges attributes into an existing html/body element.
    fn merge(&mut self, id: NodeId, attrs: Vec<(String, String)>) {
        if let NodeData::Element(e) = &mut self.doc.nodes[id].data {
            for (k, v) in attrs {
                if e.attr(&k).is_none() {
                    e.attrs.push((k, v));
                }
            }
        }
    }

    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.body.is_none() {
            if s.chars().all(|c| c.is_ascii_whitespace()) {
                return;
            }
            // Text inside <title> etc. is handled as raw text; anything else starts the body.
            if !self.stack.last().is_some_and(|n| HEAD_CONTENT.contains(&self.doc.tag(*n))) {
                self.ensure_body();
            }
        }
        let cur = self.current();
        // Text directly in table structure is only whitespace worth dropping.
        if matches!(self.doc.tag(cur), "table" | "tbody" | "thead" | "tfoot" | "tr" | "html" | "head")
            && s.trim().is_empty()
        {
            return;
        }
        if let Some(&last) = self.doc.nodes[cur].children.last() {
            if let NodeData::Text(t) = &mut self.doc.nodes[last].data {
                t.push_str(s);
                return;
            }
        }
        self.doc.add(cur, NodeData::Text(String::from(s)));
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, is_void: bool) -> Option<NodeId> {
        match name.as_str() {
            "html" => {
                match self.html {
                    Some(h) => self.merge(h, attrs),
                    None => {
                        let h = self.element(Document::ROOT, "html", attrs);
                        self.html = Some(h);
                        self.stack = alloc::vec![Document::ROOT, h];
                    }
                }
                return None;
            }
            "head" => {
                if self.head.is_some() || self.body.is_some() {
                    return None;
                }
                let html = self.ensure_html();
                let h = self.element(html, "head", attrs);
                self.head = Some(h);
                self.stack.push(h);
                return None;
            }
            "body" => {
                match self.body {
                    Some(b) => self.merge(b, attrs),
                    None => {
                        self.ensure_body();
                        let b = self.body.unwrap();
                        self.merge(b, attrs);
                    }
                }
                return None;
            }
            n if self.body.is_none() && HEAD_CONTENT.contains(&n) => {
                self.ensure_html();
            }
            _ => self.ensure_body(),
        }
        match name.as_str() {
            "li" => self.close_within(&["li"], &["ul", "ol", "menu", "table"]),
            "dt" | "dd" => self.close_within(&["dt", "dd"], &["dl", "table"]),
            "tr" => self.close_within(&["tr"], &["table", "tbody", "thead", "tfoot"]),
            "td" | "th" => self.close_within(&["td", "th"], &["tr", "table"]),
            "thead" | "tbody" | "tfoot" => self.close_within(&["thead", "tbody", "tfoot"], &["table"]),
            "option" => self.close_within(&["option"], &["select", "datalist"]),
            "optgroup" => self.close_within(&["option", "optgroup"], &["select"]),
            "a" => self.close_within(&["a"], &["td", "th", "table", "button"]),
            _ => {}
        }
        if CLOSES_P.contains(&name.as_str()) {
            self.close_within(&["p"], &["button", "table", "td", "th", "li", "div", "blockquote"]);
        }
        if matches!(name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            let cur = self.doc.tag(self.current());
            if matches!(cur, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
                self.stack.pop();
            }
        }
        // Rows and cells outside their containers still form a table.
        if name == "td" || name == "th" {
            let t = self.doc.tag(self.current());
            if matches!(t, "table" | "tbody" | "thead" | "tfoot") {
                let tr =
                    self.doc.add(self.current(), NodeData::Element(Element { tag: "tr".into(), attrs: Vec::new() }));
                self.stack.push(tr);
            }
        }
        let parent = self.current();
        let id = self.doc.add(parent, NodeData::Element(Element { tag: name, attrs }));
        if !is_void {
            self.stack.push(id);
        }
        Some(id)
    }

    fn end(&mut self, name: &str) {
        match name {
            "html" | "body" => return,
            "head" => {
                if let Some(h) = self.head {
                    if let Some(pos) = self.stack.iter().position(|n| *n == h) {
                        self.stack.truncate(pos);
                    }
                }
                return;
            }
            "br" => {
                self.start(String::from("br"), Vec::new(), true);
                return;
            }
            "p" if self.open("p").is_none() => {
                // "</p>" without a <p>: an empty paragraph.
                self.start(String::from("p"), Vec::new(), true);
                return;
            }
            _ => {}
        }
        if let Some(pos) = self.open(name) {
            if pos > 0 {
                self.stack.truncate(pos);
            }
        }
    }
}

/// Decodes character references (&amp; &#169; &#x1F600; …).
pub fn decode(s: &str) -> String {
    if !s.contains('&') {
        return String::from(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let (c, used) = reference(rest);
        match c {
            Some(c) => {
                out.push_str(&c.as_str());
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

enum Decoded {
    One(char),
    Str(&'static str),
}

impl Decoded {
    fn as_str(&self) -> String {
        match self {
            Decoded::One(c) => {
                let mut s = String::new();
                s.push(*c);
                s
            }
            Decoded::Str(s) => String::from(*s),
        }
    }
}

/// One reference at the start of `s` ("&…"): the text and bytes consumed.
fn reference(s: &str) -> (Option<Decoded>, usize) {
    let b = s.as_bytes();
    if b.get(1) == Some(&b'#') {
        let hex = matches!(b.get(2), Some(b'x' | b'X'));
        let start = if hex { 3 } else { 2 };
        let mut end = start;
        while end < b.len() && (if hex { b[end].is_ascii_hexdigit() } else { b[end].is_ascii_digit() }) {
            end += 1;
        }
        if end == start || end - start > 8 {
            return (None, 0);
        }
        let n = u32::from_str_radix(&s[start..end], if hex { 16 } else { 10 }).unwrap_or(0xFFFD);
        let used = if b.get(end) == Some(&b';') { end + 1 } else { end };
        let c = match n {
            0x80..=0x9F => WINDOWS_1252[(n - 0x80) as usize],
            0 => '\u{FFFD}',
            n => char::from_u32(n).unwrap_or('\u{FFFD}'),
        };
        return (Some(Decoded::One(c)), used);
    }
    let mut end = 1;
    while end < b.len() && end < 32 && b[end].is_ascii_alphanumeric() {
        end += 1;
    }
    let name = &s[1..end];
    let semicolon = b.get(end) == Some(&b';');
    if let Some((_, v)) = ENTITIES.iter().find(|(k, _)| *k == name) {
        return (Some(Decoded::Str(v)), end + semicolon as usize);
    }
    // Legacy: "&copy" without ';' followed by more letters ("&copy2024").
    for (k, v) in ENTITIES.iter().filter(|(k, _)| k.len() >= 2) {
        if name.starts_with(k) && LEGACY_NO_SEMICOLON.contains(k) {
            return (Some(Decoded::Str(v)), 1 + k.len());
        }
    }
    (None, 0)
}

const LEGACY_NO_SEMICOLON: &[&str] = &["amp", "lt", "gt", "quot", "nbsp", "copy", "reg"];

/// Windows-1252 for bytes (or references) 0x80–0x9F.
pub const WINDOWS_1252: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’',
    '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

const ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", "\u{A0}"),
    ("iexcl", "¡"),
    ("cent", "¢"),
    ("pound", "£"),
    ("curren", "¤"),
    ("yen", "¥"),
    ("brvbar", "¦"),
    ("sect", "§"),
    ("uml", "¨"),
    ("copy", "©"),
    ("ordf", "ª"),
    ("laquo", "«"),
    ("not", "¬"),
    ("shy", "\u{AD}"),
    ("reg", "®"),
    ("macr", "¯"),
    ("deg", "°"),
    ("plusmn", "±"),
    ("sup2", "²"),
    ("sup3", "³"),
    ("acute", "´"),
    ("micro", "µ"),
    ("para", "¶"),
    ("middot", "·"),
    ("cedil", "¸"),
    ("sup1", "¹"),
    ("ordm", "º"),
    ("raquo", "»"),
    ("frac14", "¼"),
    ("frac12", "½"),
    ("frac34", "¾"),
    ("iquest", "¿"),
    ("Agrave", "À"),
    ("Aacute", "Á"),
    ("Acirc", "Â"),
    ("Atilde", "Ã"),
    ("Auml", "Ä"),
    ("Aring", "Å"),
    ("AElig", "Æ"),
    ("Ccedil", "Ç"),
    ("Egrave", "È"),
    ("Eacute", "É"),
    ("Ecirc", "Ê"),
    ("Euml", "Ë"),
    ("Igrave", "Ì"),
    ("Iacute", "Í"),
    ("Icirc", "Î"),
    ("Iuml", "Ï"),
    ("ETH", "Ð"),
    ("Ntilde", "Ñ"),
    ("Ograve", "Ò"),
    ("Oacute", "Ó"),
    ("Ocirc", "Ô"),
    ("Otilde", "Õ"),
    ("Ouml", "Ö"),
    ("times", "×"),
    ("Oslash", "Ø"),
    ("Ugrave", "Ù"),
    ("Uacute", "Ú"),
    ("Ucirc", "Û"),
    ("Uuml", "Ü"),
    ("Yacute", "Ý"),
    ("THORN", "Þ"),
    ("szlig", "ß"),
    ("agrave", "à"),
    ("aacute", "á"),
    ("acirc", "â"),
    ("atilde", "ã"),
    ("auml", "ä"),
    ("aring", "å"),
    ("aelig", "æ"),
    ("ccedil", "ç"),
    ("egrave", "è"),
    ("eacute", "é"),
    ("ecirc", "ê"),
    ("euml", "ë"),
    ("igrave", "ì"),
    ("iacute", "í"),
    ("icirc", "î"),
    ("iuml", "ï"),
    ("eth", "ð"),
    ("ntilde", "ñ"),
    ("ograve", "ò"),
    ("oacute", "ó"),
    ("ocirc", "ô"),
    ("otilde", "õ"),
    ("ouml", "ö"),
    ("divide", "÷"),
    ("oslash", "ø"),
    ("ugrave", "ù"),
    ("uacute", "ú"),
    ("ucirc", "û"),
    ("uuml", "ü"),
    ("yacute", "ý"),
    ("thorn", "þ"),
    ("yuml", "ÿ"),
    ("OElig", "Œ"),
    ("oelig", "œ"),
    ("Scaron", "Š"),
    ("scaron", "š"),
    ("Yuml", "Ÿ"),
    ("fnof", "ƒ"),
    ("circ", "ˆ"),
    ("tilde", "˜"),
    ("ensp", "\u{2002}"),
    ("emsp", "\u{2003}"),
    ("thinsp", "\u{2009}"),
    ("zwnj", ""),
    ("zwj", ""),
    ("lrm", ""),
    ("rlm", ""),
    ("ndash", "–"),
    ("mdash", "—"),
    ("lsquo", "‘"),
    ("rsquo", "’"),
    ("sbquo", "‚"),
    ("ldquo", "“"),
    ("rdquo", "”"),
    ("bdquo", "„"),
    ("dagger", "†"),
    ("Dagger", "‡"),
    ("bull", "•"),
    ("hellip", "…"),
    ("permil", "‰"),
    ("prime", "′"),
    ("Prime", "″"),
    ("lsaquo", "‹"),
    ("rsaquo", "›"),
    ("oline", "‾"),
    ("euro", "€"),
    ("trade", "™"),
    ("larr", "←"),
    ("uarr", "↑"),
    ("rarr", "→"),
    ("darr", "↓"),
    ("harr", "↔"),
    ("crarr", "↵"),
    ("lArr", "⇐"),
    ("rArr", "⇒"),
    ("hArr", "⇔"),
    ("forall", "∀"),
    ("part", "∂"),
    ("exist", "∃"),
    ("empty", "∅"),
    ("nabla", "∇"),
    ("isin", "∈"),
    ("notin", "∉"),
    ("sum", "∑"),
    ("minus", "−"),
    ("lowast", "∗"),
    ("radic", "√"),
    ("infin", "∞"),
    ("and", "∧"),
    ("or", "∨"),
    ("cap", "∩"),
    ("cup", "∪"),
    ("int", "∫"),
    ("asymp", "≈"),
    ("ne", "≠"),
    ("equiv", "≡"),
    ("le", "≤"),
    ("ge", "≥"),
    ("sub", "⊂"),
    ("sup", "⊃"),
    ("sdot", "⋅"),
    ("loz", "◊"),
    ("spades", "♠"),
    ("clubs", "♣"),
    ("hearts", "♥"),
    ("diams", "♦"),
    ("Alpha", "Α"),
    ("Beta", "Β"),
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Omega", "Ω"),
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("pi", "π"),
    ("sigma", "σ"),
    ("tau", "τ"),
    ("phi", "φ"),
    ("omega", "ω"),
    ("check", "✓"),
    ("star", "☆"),
    ("starf", "★"),
    ("dollar", "$"),
    ("num", "#"),
    ("percnt", "%"),
    ("lpar", "("),
    ("rpar", ")"),
    ("comma", ","),
    ("period", "."),
    ("colon", ":"),
    ("semi", ";"),
    ("excl", "!"),
    ("quest", "?"),
    ("equals", "="),
    ("plus", "+"),
    ("sol", "/"),
    ("bsol", "\\"),
    ("lsqb", "["),
    ("rsqb", "]"),
    ("lcub", "{"),
    ("rcub", "}"),
    ("vert", "|"),
    ("verbar", "|"),
    ("ast", "*"),
    ("commat", "@"),
    ("grave", "`"),
    ("Hat", "^"),
    ("lowbar", "_"),
    ("NewLine", "\n"),
    ("Tab", "\t"),
];

/// Parses an HTML fragment (as `innerHTML` does): a document whose root
/// holds the parsed `<body>` contents.
pub fn parse_fragment(html: &str) -> Document {
    let full = parse(&alloc::format!("<!doctype html><html><head></head><body>{html}</body></html>"));
    let mut out = Document::default();
    if let Some(body) = full.find("body") {
        for &c in &full.nodes[body].children {
            let copy = out.import(&full, c);
            out.insert(Document::ROOT, copy, None);
        }
    }
    out
}

fn escape_into(out: &mut String, s: &str, attr: bool) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

/// The HTML of a node's children (`innerHTML`).
pub fn serialize_children(doc: &Document, n: NodeId) -> String {
    let mut out = String::new();
    for &c in &doc.nodes[n].children {
        serialize_into(doc, c, &mut out);
    }
    out
}

/// The HTML of a node itself (`outerHTML`).
pub fn serialize(doc: &Document, n: NodeId) -> String {
    let mut out = String::new();
    serialize_into(doc, n, &mut out);
    out
}

fn serialize_into(doc: &Document, n: NodeId, out: &mut String) {
    match &doc.nodes[n].data {
        NodeData::Document => {
            for &c in &doc.nodes[n].children {
                serialize_into(doc, c, out);
            }
        }
        NodeData::Text(t) => {
            let raw = doc.parent(n).is_some_and(|p| matches!(doc.tag(p), "script" | "style" | "xmp" | "plaintext"));
            if raw {
                out.push_str(t);
            } else {
                escape_into(out, t, false);
            }
        }
        NodeData::Element(e) => {
            if e.tag == crate::dom::FRAGMENT {
                for &c in &doc.nodes[n].children {
                    serialize_into(doc, c, out);
                }
                return;
            }
            out.push('<');
            out.push_str(&e.tag);
            for (k, v) in &e.attrs {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                escape_into(out, v, true);
                out.push('"');
            }
            out.push('>');
            if VOID.contains(&e.tag.as_str()) {
                return;
            }
            for &c in &doc.nodes[n].children {
                serialize_into(doc, c, out);
            }
            out.push_str("</");
            out.push_str(&e.tag);
            out.push('>');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.nodes[id].data {
            NodeData::Element(e) => {
                out.push('<');
                out.push_str(&e.tag);
                out.push('>');
                for c in &doc.nodes[id].children {
                    outline(doc, *c, out);
                }
                out.push_str("</");
                out.push_str(&e.tag);
                out.push('>');
            }
            NodeData::Text(t) => out.push_str(t),
            NodeData::Document => {
                for c in &doc.nodes[id].children {
                    outline(doc, *c, out);
                }
            }
        }
    }

    /// The body's contents as tags.
    fn tree(html: &str) -> String {
        let doc = parse(html);
        let mut s = String::new();
        for c in &doc.nodes[doc.find("body").unwrap()].children {
            outline(&doc, *c, &mut s);
        }
        s
    }

    #[test]
    fn implied_end_tags() {
        assert_eq!(tree("<p>one<p>two"), "<p>one</p><p>two</p>");
        assert_eq!(tree("<ul><li>a<li>b</ul>"), "<ul><li>a</li><li>b</li></ul>");
        assert_eq!(tree("<p>x<div>y</div>"), "<p>x</p><div>y</div>");
        assert_eq!(
            tree("<table><tr><td>1<td>2<tr><td>3</table>"),
            "<table><tr><td>1</td><td>2</td></tr><tr><td>3</td></tr></table>"
        );
        assert_eq!(tree("<b>bold <i>both</b> after"), "<b>bold <i>both</i></b> after");
    }

    #[test]
    fn raw_text_attributes_and_references() {
        let doc = parse("<script>if (a < b) { x = '</div>' }</script><a href=\"/x?a=1&amp;b=2\" title='t'>&copy; 2024 &#8212; &#x1F600; &nope; AT&T</a>");
        let script = doc.find("script").unwrap();
        assert_eq!(doc.text_content(script), "if (a < b) { x = '</div>' }");
        let a = doc.find("a").unwrap();
        assert_eq!(doc.element(a).unwrap().attr("href"), Some("/x?a=1&b=2"));
        assert_eq!(doc.element(a).unwrap().attr("title"), Some("t"));
        assert_eq!(doc.text_content(a), "© 2024 — 😀 &nope; AT&T");
    }

    #[test]
    fn document_structure() {
        let doc = parse("<!DOCTYPE html><html lang=en><head><title>Hi &amp; bye</title><!-- c --></head><body class=x><br/><img src=a.png></body></html>trailing");
        assert_eq!(doc.text_content(doc.find("title").unwrap()), "Hi & bye");
        let body = doc.find("body").unwrap();
        assert!(doc.element(body).unwrap().has_class("x"));
        assert_eq!(doc.nodes[body].children.len(), 3); // br, img, "trailing"
                                                       // Without the tags, html/head/body are implied.
        let doc = parse("<title>t</title><p>x");
        let html = doc.find("html").unwrap();
        let kids: Vec<&str> = doc.nodes[html].children.iter().map(|c| doc.tag(*c)).collect();
        assert_eq!(kids, ["head", "body"]);
        let head = doc.find("head").unwrap();
        assert_eq!(doc.tag(doc.nodes[head].children[0]), "title");
    }
}
