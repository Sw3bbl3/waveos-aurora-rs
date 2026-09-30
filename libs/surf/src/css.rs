//! CSS: stylesheets, selectors and matching.
//!
//! Supported: type/class/id/universal/attribute selectors, the descendant,
//! child and sibling combinators, a few structural pseudo-classes, @media
//! (width, orientation, colour scheme), @supports (assumed true) and !important.
//! Anything else makes a selector never match, so unknown rules stay inert.

use crate::dom::{Document, NodeId};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// URLs from @import, in order.
    pub imports: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Declaration>,
    /// The @media conditions this rule sits in (all must hold).
    pub media: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// Lower-case property name (custom properties keep their case).
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub pseudos: Vec<Pseudo>,
    /// Contains something we don't support: never matches.
    pub never: bool,
    /// Targets ::before (1) or ::after (2) of the element.
    pub pseudo_element: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrSel {
    pub name: String,
    /// (operator, value): '=' '~' '^' '$' '*' '|'
    pub op: Option<(char, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pseudo {
    Link,
    FirstChild,
    LastChild,
    OnlyChild,
    Root,
    /// :nth-child(a n + b)
    NthChild(i32, i32),
    Not(Vec<Compound>),
    /// :is() / :where(): any of these.
    Is(Vec<Compound>),
}

/// A complex selector, stored right to left: `parts[0]` is the subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    pub parts: Vec<(Compound, Option<Combinator>)>,
    pub specificity: (u16, u16, u16),
    /// Hashes of ids, classes and tags some ancestor must have (for a quick
    /// rejection against an element's ancestor filter).
    pub ancestor_hashes: Vec<u32>,
}

/// A hash for the ancestor filter: kind 0 = id, 1 = class, 2 = tag.
pub fn key_hash(kind: u8, s: &str) -> u32 {
    let mut h: u32 = 0x811C_9DC5 ^ kind as u32;
    for b in s.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

/// 256 bits of "some ancestor has this key".
#[derive(Clone, Copy, Default)]
pub struct Bloom(pub [u64; 4]);

impl Bloom {
    pub fn insert(&mut self, h: u32) {
        for bit in [h & 255, (h >> 8) & 255] {
            self.0[(bit >> 6) as usize] |= 1 << (bit & 63);
        }
    }
    pub fn may_contain(&self, h: u32) -> bool {
        [h & 255, (h >> 8) & 255].iter().all(|bit| self.0[(bit >> 6) as usize] & (1 << (bit & 63)) != 0)
    }
}

// ------------------------------------------------------------------ parsing

/// Removes /* comments */.
fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("/*") {
        out.push_str(&rest[..p]);
        match rest[p + 2..].find("*/") {
            Some(e) => rest = &rest[p + 2 + e + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The index of the '}' matching the '{' before `from`, honouring strings.
fn block_end(s: &[u8], from: usize) -> usize {
    let mut depth = 1;
    let mut i = from;
    let mut quote = 0u8;
    while i < s.len() {
        let c = s[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    s.len()
}

pub fn parse(css: &str) -> Stylesheet {
    let css = strip_comments(css);
    let mut sheet = Stylesheet::default();
    parse_into(&css, &Vec::new(), &mut sheet);
    sheet
}

fn parse_into(css: &str, media: &Vec<String>, sheet: &mut Stylesheet) {
    let b = css.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // The prelude runs to '{' (or ';' for statements like @import).
        let start = i;
        let mut quote = 0u8;
        while i < b.len() {
            let c = b[i];
            if quote != 0 {
                if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if c == b'{' || c == b';' || c == b'}' {
                break;
            }
            i += 1;
        }
        let prelude = css[start..i].trim();
        if i >= b.len() {
            break;
        }
        if b[i] == b';' || b[i] == b'}' {
            if let Some(rest) = prelude.strip_prefix("@import") {
                if let Some(url) = import_url(rest) {
                    sheet.imports.push(url);
                }
            }
            i += 1;
            continue;
        }
        let end = block_end(b, i + 1);
        let body = &css[i + 1..end];
        i = end + 1;
        if let Some(at) = prelude.strip_prefix('@') {
            let (name, cond) =
                at.split_once(|c: char| c.is_whitespace() || c == '(').map_or((at, ""), |(n, _)| (n, &at[n.len()..]));
            match name.to_ascii_lowercase().as_str() {
                "media" => {
                    let mut m = media.clone();
                    m.push(cond.trim().to_ascii_lowercase());
                    parse_into(body, &m, sheet);
                }
                "supports" | "layer" | "document" | "container" => parse_into(body, media, sheet),
                _ => {} // @font-face, @keyframes, @page, …
            }
            continue;
        }
        let selectors: Vec<Selector> = split_top(prelude, b',').iter().filter_map(|s| parse_selector(s)).collect();
        if selectors.is_empty() {
            continue;
        }
        let decls = parse_declarations(body);
        if !decls.is_empty() {
            sheet.rules.push(Rule { selectors, decls, media: media.clone() });
        }
    }
}

fn import_url(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.strip_prefix("url(").map_or(s, |r| r.split(')').next().unwrap_or(""));
    let s = s.trim().trim_matches(|c| c == '"' || c == '\'');
    let url = s.split(|c: char| c == '"' || c == '\'' || c.is_whitespace()).next()?;
    (!url.is_empty()).then(|| String::from(url))
}

/// Splits on `sep` outside parentheses, brackets and strings.
pub fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, 0u8, 0);
    for (i, &c) in b.iter().enumerate() {
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
            continue;
        }
        match c {
            b'"' | b'\'' => quote = c,
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

pub fn parse_declarations(body: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for d in split_top(body, b';') {
        let Some((name, value)) = d.split_once(':') else { continue };
        let name = name.trim();
        if name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == '{' || c == '}') {
            continue;
        }
        let mut value = value.trim();
        let mut important = false;
        if let Some(p) = value.to_ascii_lowercase().rfind("!important") {
            important = true;
            value = value[..p].trim();
        }
        let name = if name.starts_with("--") { String::from(name) } else { name.to_ascii_lowercase() };
        out.push(Declaration { name, value: String::from(value), important });
    }
    out
}

pub fn parse_selector(s: &str) -> Option<Selector> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut parts_ltr: Vec<(Compound, Option<Combinator>)> = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    let mut pending: Option<Combinator> = None;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            if pending.is_none() && !parts_ltr.is_empty() {
                pending = Some(Combinator::Descendant);
            }
            continue;
        }
        if matches!(c, b'>' | b'+' | b'~') {
            pending = Some(match c {
                b'>' => Combinator::Child,
                b'+' => Combinator::Adjacent,
                _ => Combinator::Sibling,
            });
            i += 1;
            continue;
        }
        let (compound, used) = parse_compound(&s[i..])?;
        if used == 0 {
            return None;
        }
        i += used;
        // The combinator links this compound to the previous one.
        if let Some(last) = parts_ltr.last_mut() {
            last.1 = pending.take().or(Some(Combinator::Descendant));
        }
        parts_ltr.push((compound, None));
    }
    if parts_ltr.is_empty() {
        return None;
    }
    // Store right to left: each part's combinator says how it relates to the next part (leftwards).
    let n = parts_ltr.len();
    let mut parts = Vec::with_capacity(n);
    for k in (0..n).rev() {
        let comb = if k > 0 { parts_ltr[k - 1].1.clone() } else { None };
        parts.push((parts_ltr[k].0.clone(), comb));
    }
    let mut spec = (0u16, 0u16, 0u16);
    for (c, _) in &parts {
        add_specificity(c, &mut spec);
    }
    // Compounds reached through child/descendant combinators must be ancestors.
    let mut ancestor_hashes = Vec::new();
    for k in 1..parts.len() {
        if !matches!(parts[k - 1].1, Some(Combinator::Child | Combinator::Descendant)) {
            break;
        }
        let c = &parts[k].0;
        if let Some(id) = &c.id {
            ancestor_hashes.push(key_hash(0, id));
        }
        for cl in &c.classes {
            ancestor_hashes.push(key_hash(1, cl));
        }
        if let Some(t) = &c.tag {
            ancestor_hashes.push(key_hash(2, t));
        }
    }
    Some(Selector { parts, specificity: spec, ancestor_hashes })
}

fn add_specificity(c: &Compound, spec: &mut (u16, u16, u16)) {
    spec.0 += c.id.is_some() as u16;
    spec.1 += (c.classes.len() + c.attrs.len()) as u16;
    for p in &c.pseudos {
        match p {
            Pseudo::Not(list) | Pseudo::Is(list) => {
                // The most specific argument counts (for :where it should be zero; close enough).
                let mut best = (0, 0, 0);
                for c in list {
                    let mut s = (0, 0, 0);
                    add_specificity(c, &mut s);
                    best = best.max(s);
                }
                spec.0 += best.0;
                spec.1 += best.1;
                spec.2 += best.2;
            }
            _ => spec.1 += 1,
        }
    }
    spec.2 += c.tag.is_some() as u16;
}

fn ident_len(s: &[u8]) -> usize {
    let mut i = 0;
    while i < s.len() && (s[i].is_ascii_alphanumeric() || s[i] == b'-' || s[i] == b'_' || s[i] >= 0x80 || s[i] == b'\\')
    {
        if s[i] == b'\\' {
            i += 1;
        }
        i += 1;
        // Stay on a character boundary.
        while i < s.len() && s[i] & 0xC0 == 0x80 {
            i += 1;
        }
    }
    i.min(s.len())
}

fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return String::from(s);
    }
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// One compound selector at the start of `s`: (compound, bytes used).
fn parse_compound(s: &str) -> Option<(Compound, usize)> {
    let b = s.as_bytes();
    let mut c = Compound::default();
    let mut i = 0;
    if i < b.len() && b[i] == b'*' {
        i += 1;
    } else {
        let n = ident_len(&b[i..]);
        if n > 0 {
            c.tag = Some(s[i..i + n].to_ascii_lowercase());
            i += n;
        }
    }
    while i < b.len() {
        match b[i] {
            b'#' => {
                let n = ident_len(&b[i + 1..]);
                c.id = Some(unescape(&s[i + 1..i + 1 + n]));
                i += 1 + n;
            }
            b'.' => {
                let n = ident_len(&b[i + 1..]);
                c.classes.push(unescape(&s[i + 1..i + 1 + n]));
                i += 1 + n;
            }
            b'[' => {
                let end = s[i..].find(']')? + i;
                c.attrs.push(parse_attr(&s[i + 1..end]));
                i = end + 1;
            }
            b':' => {
                let element = b.get(i + 1) == Some(&b':');
                let start = i + 1 + element as usize;
                let n = ident_len(&b[start..]);
                let name = s[start..start + n].to_ascii_lowercase();
                i = start + n;
                let mut arg = None;
                if b.get(i) == Some(&b'(') {
                    let mut depth = 0;
                    let mut j = i;
                    while j < b.len() {
                        match b[j] {
                            b'(' => depth += 1,
                            b')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    arg = Some(&s[i + 1..j.min(s.len())]);
                    i = (j + 1).min(s.len());
                }
                match name.as_str() {
                    "before" => {
                        c.pseudo_element = 1;
                        continue;
                    }
                    "after" => {
                        c.pseudo_element = 2;
                        continue;
                    }
                    _ if element => {
                        c.never = true; // ::marker, ::placeholder … aren't generated here.
                        continue;
                    }
                    _ => {}
                }
                match (name.as_str(), arg) {
                    ("link" | "any-link", _) => c.pseudos.push(Pseudo::Link),
                    ("first-child", _) => c.pseudos.push(Pseudo::FirstChild),
                    ("last-child", _) => c.pseudos.push(Pseudo::LastChild),
                    ("only-child", _) => c.pseudos.push(Pseudo::OnlyChild),
                    ("root", _) => c.pseudos.push(Pseudo::Root),
                    ("nth-child", Some(a)) => match nth(a) {
                        Some((a, b)) => c.pseudos.push(Pseudo::NthChild(a, b)),
                        None => c.never = true,
                    },
                    ("not" | "is" | "where" | "matches", Some(a)) => {
                        let mut list = Vec::new();
                        for part in split_top(a, b',') {
                            let part = part.trim();
                            match parse_compound(part) {
                                Some((comp, used)) if used == part.len() => list.push(comp),
                                _ => {
                                    // Complex arguments: :not() can't be decided, :is() never matches.
                                    c.never = true;
                                }
                            }
                        }
                        c.pseudos.push(if name == "not" { Pseudo::Not(list) } else { Pseudo::Is(list) });
                    }
                    // :visited, :hover, :focus … are never in effect here.
                    _ => c.never = true,
                }
            }
            _ => break,
        }
    }
    Some((c, i))
}

fn parse_attr(s: &str) -> AttrSel {
    let s = s.trim();
    let ops = ['~', '^', '$', '*', '|'];
    if let Some(eq) = s.find('=') {
        let (mut name, value) = (&s[..eq], &s[eq + 1..]);
        let mut op = '=';
        if let Some(last) = name.chars().last() {
            if ops.contains(&last) {
                op = last;
                name = &name[..name.len() - 1];
            }
        }
        let value = value.trim();
        // Drop a trailing case flag (" i").
        let value = value.strip_suffix(" i").or(value.strip_suffix(" s")).unwrap_or(value).trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        AttrSel { name: name.trim().to_ascii_lowercase(), op: Some((op, String::from(value))) }
    } else {
        AttrSel { name: s.to_ascii_lowercase(), op: None }
    }
}

/// "odd", "even", "3", "2n+1", "-n+3".
fn nth(s: &str) -> Option<(i32, i32)> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some((a, b)) = s.split_once('n') {
        let a = match a {
            "" | "+" => 1,
            "-" => -1,
            a => a.parse().ok()?,
        };
        let b = if b.is_empty() { 0 } else { b.parse().ok()? };
        Some((a, b))
    } else {
        Some((0, s.parse().ok()?))
    }
}

// ----------------------------------------------------------------- matching

pub fn matches(doc: &Document, node: NodeId, sel: &Selector) -> bool {
    match_from(doc, node, &sel.parts, 0)
}

fn match_from(doc: &Document, node: NodeId, parts: &[(Compound, Option<Combinator>)], k: usize) -> bool {
    let (compound, comb) = &parts[k];
    if !matches_compound(doc, node, compound) {
        return false;
    }
    let Some(comb) = comb else { return true };
    match comb {
        Combinator::Child => {
            doc.parent(node).is_some_and(|p| doc.element(p).is_some() && match_from(doc, p, parts, k + 1))
        }
        Combinator::Descendant => {
            let mut p = doc.parent(node);
            while let Some(n) = p {
                if doc.element(n).is_none() {
                    return false;
                }
                if match_from(doc, n, parts, k + 1) {
                    return true;
                }
                p = doc.parent(n);
            }
            false
        }
        Combinator::Adjacent => doc.previous_elements(node).next().is_some_and(|s| match_from(doc, s, parts, k + 1)),
        Combinator::Sibling => doc.previous_elements(node).any(|s| match_from(doc, s, parts, k + 1)),
    }
}

pub fn matches_compound(doc: &Document, node: NodeId, c: &Compound) -> bool {
    if c.never {
        return false;
    }
    let Some(e) = doc.element(node) else { return false };
    if c.tag.as_ref().is_some_and(|t| *t != e.tag) {
        return false;
    }
    if c.id.as_ref().is_some_and(|id| e.id() != Some(id.as_str())) {
        return false;
    }
    if !c.classes.iter().all(|cl| e.has_class(cl)) {
        return false;
    }
    for a in &c.attrs {
        let Some(v) = e.attr(&a.name) else { return false };
        let ok = match &a.op {
            None => true,
            Some(('=', x)) => v == x,
            Some(('~', x)) => v.split_ascii_whitespace().any(|w| w == x),
            Some(('^', x)) => !x.is_empty() && v.starts_with(x.as_str()),
            Some(('$', x)) => !x.is_empty() && v.ends_with(x.as_str()),
            Some(('*', x)) => !x.is_empty() && v.contains(x.as_str()),
            Some(('|', x)) => v == x || v.strip_prefix(x.as_str()).is_some_and(|r| r.starts_with('-')),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    for p in &c.pseudos {
        let ok = match p {
            Pseudo::Link => matches!(e.tag.as_str(), "a" | "area") && e.attr("href").is_some(),
            Pseudo::FirstChild => doc.previous_elements(node).next().is_none(),
            Pseudo::LastChild => doc.next_elements(node).next().is_none(),
            Pseudo::OnlyChild => {
                doc.previous_elements(node).next().is_none() && doc.next_elements(node).next().is_none()
            }
            Pseudo::Root => e.tag == "html",
            Pseudo::NthChild(a, b) => {
                let pos = doc.previous_elements(node).count() as i32 + 1;
                if *a == 0 {
                    pos == *b
                } else {
                    (pos - b) % a == 0 && (pos - b) / a >= 0
                }
            }
            Pseudo::Not(list) => !list.iter().any(|c| matches_compound(doc, node, c)),
            Pseudo::Is(list) => list.iter().any(|c| matches_compound(doc, node, c)),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Evaluates a media query list for a viewport `width` × `height` (CSS px).
pub fn media_matches(query: &str, width: i32, height: i32) -> bool {
    split_top(query, b',').iter().any(|q| single_media(q.trim(), width, height))
}

fn single_media(q: &str, width: i32, height: i32) -> bool {
    let (negate, q) = match q.strip_prefix("not ") {
        Some(r) => (true, r.trim()),
        None => (false, q.strip_prefix("only ").unwrap_or(q).trim()),
    };
    let mut ok = true;
    for part in q.split(" and ") {
        let part = part.trim();
        let cond = if let Some(inner) = part.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
            feature(inner.trim(), width, height)
        } else {
            matches!(part, "" | "all" | "screen")
        };
        ok &= cond;
    }
    ok != negate
}

fn feature(f: &str, width: i32, height: i32) -> bool {
    if f.contains(['<', '>']) {
        return range(f, width, height);
    }
    let (name, value) = f.split_once(':').map_or((f, ""), |(n, v)| (n.trim(), v.trim()));
    match name {
        "min-width" => width >= media_length(value),
        "max-width" => width <= media_length(value),
        "min-height" => height >= media_length(value),
        "max-height" => height <= media_length(value),
        "orientation" => (value == "landscape") == (width >= height),
        "prefers-color-scheme" => value == "light",
        "prefers-reduced-motion" => value == "no-preference",
        "hover" | "any-hover" => value == "hover" || value.is_empty(),
        "pointer" | "any-pointer" => value == "fine" || value.is_empty(),
        "color" => true,
        "min-resolution" | "-webkit-min-device-pixel-ratio" | "min-device-pixel-ratio" => false,
        _ => false,
    }
}

/// Range syntax: "width >= 600px", "400px <= width <= 700px".
fn range(f: &str, width: i32, height: i32) -> bool {
    let b = f.as_bytes();
    let (mut operands, mut ops) = (Vec::new(), Vec::new());
    let (mut start, mut i) = (0, 0);
    while i < b.len() {
        if b[i] == b'<' || b[i] == b'>' {
            operands.push(f[start..i].trim());
            let len = if b.get(i + 1) == Some(&b'=') { 2 } else { 1 };
            ops.push(&f[i..i + len]);
            i += len;
            start = i;
        } else {
            i += 1;
        }
    }
    operands.push(f[start..].trim());
    let value = |s: &str| match s {
        "width" => width,
        "height" => height,
        s => media_length(s),
    };
    (0..ops.len()).all(|k| {
        let (a, c) = (value(operands[k]), value(operands[k + 1]));
        match ops[k] {
            ">=" => a >= c,
            "<=" => a <= c,
            ">" => a > c,
            _ => a < c,
        }
    })
}

fn media_length(v: &str) -> i32 {
    let v = v.trim();
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f32 = num.parse().unwrap_or(0.0);
    let unit = &v[num.len()..];
    match unit {
        "em" | "rem" => (n * 16.0) as i32,
        _ => n as i32,
    }
}

impl core::fmt::Display for Declaration {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}: {}", self.name, self.value)
    }
}

/// For callers that want a key to bucket rules by (id, class or tag).
pub fn bucket_key(sel: &Selector) -> (u8, String) {
    let c = &sel.parts[0].0;
    if let Some(id) = &c.id {
        (0, id.clone())
    } else if let Some(cl) = c.classes.first() {
        (1, cl.clone())
    } else if let Some(t) = &c.tag {
        (2, t.to_string())
    } else {
        (3, String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    fn select(html_src: &str, sel: &str) -> Vec<String> {
        let doc = html::parse(html_src);
        let s = parse_selector(sel).unwrap();
        doc.descendants(Document::ROOT)
            .into_iter()
            .filter(|n| matches(&doc, *n, &s))
            .map(|n| doc.element(n).unwrap().id().unwrap_or("?").to_string())
            .collect()
    }

    #[test]
    fn selectors() {
        let h = r#"<div id=a class="x y"><p id=b class=x>t</p><span id=c></span><p id=d data-k="foo-bar"></p></div><p id=e></p>"#;
        assert_eq!(select(h, "p"), ["b", "d", "e"]);
        assert_eq!(select(h, ".x"), ["a", "b"]);
        assert_eq!(select(h, "div.x.y > p"), ["b", "d"]);
        assert_eq!(select(h, "div p.x"), ["b"]);
        assert_eq!(select(h, "p + span"), ["c"]);
        assert_eq!(select(h, "p ~ p"), ["d"]);
        assert_eq!(select(h, "[data-k|=foo]"), ["d"]);
        assert_eq!(select(h, "[data-k^='foo']"), ["d"]);
        assert_eq!(select(h, "div > :first-child"), ["b"]);
        assert_eq!(select(h, "div > :nth-child(2n+1)"), ["b", "d"]);
        assert_eq!(select(h, "p:not(.x)"), ["d", "e"]);
        assert!(select(h, "p:hover").is_empty());
        assert!(select(h, "p::marker").is_empty());
        assert_eq!(parse_selector("li:last-child::after").unwrap().parts[0].0.pseudo_element, 2);
        assert_eq!(parse_selector("q:before").unwrap().parts[0].0.pseudo_element, 1);
        assert_eq!(parse_selector("#a .b c").unwrap().specificity, (1, 1, 1));
    }

    #[test]
    fn sheets_and_media() {
        let s = parse(
            "/* c */ @import url('x.css'); a{color:red!important;--Brand: #fff} @media (max-width: 600px) { p, q { margin: 0 } } @font-face { src: url(x) } bad { } ",
        );
        assert_eq!(s.imports, ["x.css"]);
        assert_eq!(s.rules.len(), 2);
        assert_eq!(s.rules[0].decls[0], Declaration { name: "color".into(), value: "red".into(), important: true });
        assert_eq!(s.rules[0].decls[1].name, "--Brand");
        assert_eq!(s.rules[1].media, ["(max-width: 600px)"]);
        assert_eq!(s.rules[1].selectors.len(), 2);
        assert!(media_matches("(max-width: 600px)", 500, 800));
        assert!(!media_matches("(max-width: 600px)", 700, 800));
        assert!(media_matches("screen and (min-width: 40em)", 700, 800));
        assert!(!media_matches("print", 700, 800));
        assert!(media_matches("print, (width >= 500px)", 700, 800));
        assert!(media_matches("(400px <= width <= 800px)", 700, 800));
        assert!(!media_matches("not all and (min-width: 100px)", 700, 800));
    }
}
