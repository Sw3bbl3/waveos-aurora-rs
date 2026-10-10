//! The page's stylesheets as its `<style>` and `<link>` elements are right
//! now, so scripts that add, edit or remove them change the cascade.
//!
//! [`Sources`] is what parsing and loading produced (shared, read-only, so
//! scripts can run the cascade too); [`Dynamic`] holds sheets for `<style>`
//! elements scripts created or changed.

use crate::css::{self, Stylesheet};
use crate::dom::{Document, NodeId};
use crate::style::{self, Cascade, Computed};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// The user-agent sheet and the sheets the page came with, by element.
#[derive(Default)]
pub struct Sources {
    pub ua: Stylesheet,
    /// Per `<style>` or `<link>`: the style text at parse time (None for a
    /// link) and its sheet (None: a link that didn't load).
    pub parsed: BTreeMap<NodeId, (Option<String>, Option<Stylesheet>)>,
}

/// Sheets for `<style>` elements whose text isn't what was parsed.
#[derive(Default)]
pub struct Dynamic {
    inline: BTreeMap<NodeId, (String, Stylesheet)>,
}

fn is_stylesheet_link(e: &crate::dom::Element) -> bool {
    let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
    let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
    rels.contains(&"stylesheet") && !rels.contains(&"alternate") && e.attr("disabled").is_none()
}

/// The author sheets in document order, then the cascade for a viewport.
/// `all` also styles the inside of `display: none` subtrees (for
/// getComputedStyle; layout doesn't need them).
pub fn compute(doc: &Document, sources: &Sources, dynamic: &mut Dynamic, size: (i32, i32), all: bool) -> Computed {
    let (width, height) = size;
    // (element, whether its sheet is in `dynamic`)
    let mut picks: Vec<(NodeId, bool)> = Vec::new();
    for n in doc.descendants(Document::ROOT) {
        let Some(e) = doc.element(n) else { continue };
        let media_ok = || e.attr("media").is_none_or(|m| css::media_matches(&m.to_ascii_lowercase(), width, height));
        match e.tag.as_str() {
            "style" => {
                if !media_ok() {
                    continue;
                }
                let text = doc.text_content(n);
                match sources.parsed.get(&n) {
                    Some((Some(parsed), Some(_))) if *parsed == text => picks.push((n, false)),
                    _ => {
                        if dynamic.inline.get(&n).is_none_or(|(t, _)| *t != text) {
                            let sheet = css::parse(&text);
                            dynamic.inline.insert(n, (text, sheet));
                        }
                        picks.push((n, true));
                    }
                }
            }
            "link" if is_stylesheet_link(e) && media_ok() => {
                if sources.parsed.get(&n).is_some_and(|(_, s)| s.is_some()) {
                    picks.push((n, false));
                }
            }
            _ => {}
        }
    }
    // Forget sheets of elements that left the page.
    dynamic.inline.retain(|n, _| picks.contains(&(*n, true)));
    let author: Vec<&Stylesheet> = picks
        .iter()
        .map(
            |&(n, dynamic_sheet)| {
                if dynamic_sheet {
                    &dynamic.inline[&n].1
                } else {
                    sources.parsed[&n].1.as_ref().unwrap()
                }
            },
        )
        .collect();
    let cascade = Cascade::new(&sources.ua, &author, width, height);
    style::compute_with(doc, &cascade, size, all)
}
