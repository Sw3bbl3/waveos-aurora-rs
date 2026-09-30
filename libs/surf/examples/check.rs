//! Parses and lays out HTML files with a fake font, reporting timings:
//! `cargo run --release -p aurora-surf --example check -- page.html [css…]`
use aurora_surf::{FontSpec, Host, Metrics, Page};
use std::time::Instant;

struct Fake;
impl Host for Fake {
    fn measure(&self, font: FontSpec, text: &str) -> i32 {
        text.chars().count() as i32 * font.size as i32 / 2
    }
    fn metrics(&self, font: FontSpec) -> Metrics {
        Metrics { ascent: font.size as i32 * 8 / 10, descent: font.size as i32 * 2 / 10 }
    }
    fn image_size(&self, _: &str) -> Option<(u32, u32)> {
        None
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let html = std::fs::read(&args[0]).unwrap();
    let t = Instant::now();
    let text = aurora_surf::decode(&html, "");
    let mut page = Page::parse(&text);
    let parsed = t.elapsed();
    for (i, (slot, _)) in page.pending_stylesheets().into_iter().enumerate() {
        let css: Vec<&String> = args[1..].iter().take_while(|a| !a.starts_with("--")).collect();
        if let Some(path) = css.get(i) {
            page.set_stylesheet(slot, &String::from_utf8_lossy(&std::fs::read(path).unwrap()));
        }
    }
    let t2 = Instant::now();
    let l = page.layout(1000, 700, &Fake);
    if let Some(pos) = args.iter().position(|a| a == "--grep") {
        let word = &args[pos + 1];
        let texts: Vec<_> = l.items.iter().filter_map(|i| if let aurora_surf::Item::Text { x, y, text, .. } = i { Some((*x, *y, text.clone())) } else { None }).collect();
        for (k, t) in texts.iter().enumerate() {
            if t.2.contains(word.as_str()) {
                for u in &texts[k.saturating_sub(3)..(k + 2).min(texts.len())] {
                    println!("  {:>5} {:>6} {:?}", u.0, u.1, u.2);
                }
                println!("--");
            }
        }
    }
    println!(
        "{}: {} nodes, title {:?}; parse {:?}, layout {:?}; {} items, {} links, height {}",
        args[0],
        page.doc.nodes.len(),
        page.title,
        parsed,
        t2.elapsed(),
        l.items.len(),
        l.links.len(),
        l.height
    );
}
