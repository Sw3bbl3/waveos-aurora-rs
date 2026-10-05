//! Layout with a fake monospaced font: every character is half the font size wide.

use nebula_engine::layout::Item;
use nebula_engine::{FontSpec, Host, Metrics, Page, Rect};

struct Fake;

impl Host for Fake {
    fn measure(&self, font: FontSpec, text: &str) -> i32 {
        text.chars().count() as i32 * font.size as i32 / 2
    }
    fn metrics(&self, font: FontSpec) -> Metrics {
        Metrics { ascent: font.size as i32 * 8 / 10, descent: font.size as i32 * 2 / 10 }
    }
    fn image_size(&self, src: &str) -> Option<(u32, u32)> {
        (src == "big.png").then_some((1000, 500))
    }
}

fn texts(html: &str, width: i32) -> Vec<(i32, i32, String)> {
    let page = Page::parse(html);
    let l = page.layout(width, 600, &Fake);
    l.items
        .iter()
        .filter_map(|i| match i {
            Item::Text { x, y, text, .. } => Some((*x, *y, text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn wraps_lines_and_collapses_margins() {
    // body margin 8; 16px text: 8px per char. Width 200 → 184 content → 23 chars per line.
    let t = texts("<p>alpha beta gamma delta epsilon zeta</p><p>next</p>", 200);
    assert_eq!(t[0].2, "alpha beta gamma delta");
    assert_eq!(t[1].2, "epsilon zeta");
    assert_eq!(t[0].0, 8);
    // Line height 20 (1.25 × 16): baselines 20 apart.
    assert_eq!(t[1].1 - t[0].1, 20);
    // The paragraphs' 16px margins collapse to one 16px gap.
    assert_eq!(t[2].1 - t[1].1, 20 + 16);
}

#[test]
fn inline_styles_links_and_alignment() {
    let page = Page::parse("<div style='text-align:center;width:200px'>go <a href='/x'>there <b>now</b></a></div>");
    let l = page.layout(400, 600, &Fake);
    let t: Vec<_> = l
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text { x, text, font, underline, .. } => Some((*x, text.clone(), font.bold, *underline)),
            _ => None,
        })
        .collect();
    // "go there now" = 12 chars = 96px, centred in 200 → starts at 8 + 52.
    assert_eq!(t[0], (60, String::from("go"), false, false));
    assert_eq!(t[1], (84, String::from("there"), false, true));
    assert_eq!(t[2], (132, String::from("now"), true, true));
    let link = page.doc.find("a").unwrap();
    assert_eq!(l.link_at(90, 20), Some(link));
    assert_eq!(l.link_at(62, 20), None);
}

#[test]
fn lists_tables_and_images() {
    let page = Page::parse(
        "<ol><li>one<li>two</ol><table><tr><td>a</td><td>bbbb</td></tr><tr><td colspan=2>w</td></tr></table><img src=big.png>",
    );
    let l = page.layout(400, 600, &Fake);
    let t: Vec<String> = l
        .items
        .iter()
        .filter_map(|i| if let Item::Text { text, .. } = i { Some(text.clone()) } else { None })
        .collect();
    let mut first4 = t[..4].to_vec();
    first4.sort();
    assert_eq!(first4, ["1.", "2.", "one", "two"]);
    let cell = |s: &str| {
        l.items.iter().find_map(|i| match i {
            Item::Text { x, text, .. } if text == s => Some(*x),
            _ => None,
        })
    };
    // Columns: 'a' (8px + 2 padding) and 'bbbb' (32px + 2); spacing 2.
    let (a, b) = (cell("a").unwrap(), cell("bbbb").unwrap());
    assert_eq!(b - a, 8 + 2 + 2);
    // The image is scaled to the 384px line, keeping its 2:1 shape.
    let img = l.items.iter().find_map(|i| if let Item::Image { rect, .. } = i { Some(*rect) } else { None }).unwrap();
    assert_eq!((img.w, img.h), (384, 192));
}

#[test]
fn css_hides_and_flexes() {
    let page = Page::parse(
        "<style>.nav{display:flex;justify-content:space-between} .sr{position:absolute;width:1px;height:1px;overflow:hidden} @media (max-width: 300px) { .wide { display: none } }</style>
         <div class=nav><a href=/a>A</a><a href=/b>B</a></div><span class=sr>skip</span><p class=wide>wide only</p>",
    );
    let narrow = page.layout(300, 600, &Fake);
    let t: Vec<(i32, String)> = narrow
        .items
        .iter()
        .filter_map(|i| if let Item::Text { x, text, .. } = i { Some((*x, text.clone())) } else { None })
        .collect();
    assert_eq!(t, [(8, String::from("A")), (284, String::from("B"))]);
    let wide = page.layout(600, 600, &Fake);
    assert!(wide.items.iter().any(|i| matches!(i, Item::Text { text, .. } if text == "wide only")));
    assert_eq!(wide.height > narrow.height, true);
    let _ = Rect::default();
}

#[test]
fn preformatted_text_and_anchors() {
    let page = Page::parse("<pre>a  b\n\tc</pre><h2 id=sec>Section</h2>");
    let l = page.layout(400, 600, &Fake);
    let t: Vec<String> = l
        .items
        .iter()
        .filter_map(|i| if let Item::Text { text, .. } = i { Some(text.clone()) } else { None })
        .collect();
    assert_eq!(&t[..2], ["a  b", "        c"]);
    assert!(l.anchor("sec").unwrap() > 0);
}

#[test]
fn breaks_only_at_opportunities() {
    // 8px per char; content width 100 - 16 = 84 → 10 chars. "(" is glued to the link.
    let t = texts("<p>aaaa bbb (<a href=x>cc</a>) dd</p>", 100);
    assert_eq!(t.iter().map(|t| t.2.as_str()).collect::<Vec<_>>(), ["aaaa bbb", "(", "cc", ") dd"]);
    assert_eq!((t[1].0, t[2].0, t[3].0), (8, 16, 32)); // "(cc)" moved to the second line together
    assert!(t[1].1 > t[0].1);
    // pre-wrap keeps runs of spaces.
    let t = texts("<p style='white-space:pre-wrap'>a   b</p>", 400);
    assert_eq!(t.iter().map(|t| (t.0, t.2.as_str())).collect::<Vec<_>>(), [(8, "a   b")]);
}

#[test]
fn generated_content() {
    let css = "<style>li { display: inline } li::after { content: \" · \" } li:last-child::after { content: none }
               .q::before { content: open-quote \"\\2192 \" attr(data-x) } .q::after { content: close-quote }</style>";
    let t = texts(&format!("{css}<ul><li>a</li><li>b</li><li>c</li></ul><p class=q data-x=hi>x</p>"), 400);
    let all: Vec<&str> = t.iter().map(|t| t.2.as_str()).collect();
    // The escape's trailing space ends the escape (CSS rules), so "→hi" is joined.
    assert_eq!(all, ["a", "·", "b", "·", "c", "\u{201C}→hi", "x", "\u{201D}"]);
}

fn positions(html: &str, width: i32) -> Vec<(String, i32, i32)> {
    texts(html, width).into_iter().map(|(x, y, t)| (t, x, y)).collect()
}

#[test]
fn grids() {
    // 100px + 1fr + 1fr in 400 - 16 = 384 wide, gap 10 → fr columns (384 - 100 - 20) / 2 = 132.
    let p = positions(
        "<div style='display:grid;grid-template-columns:100px repeat(2, 1fr);gap:10px'><p style=margin:0>a<p style=margin:0>b<p style=margin:0>c<p style=margin:0>d</div>",
        400,
    );
    let x = |t: &str| p.iter().find(|q| q.0 == t).unwrap().1;
    let y = |t: &str| p.iter().find(|q| q.0 == t).unwrap().2;
    assert_eq!((x("a"), x("b"), x("c"), x("d")), (8, 118, 260, 8));
    assert_eq!(y("a"), y("c"));
    assert_eq!(y("d") - y("a"), 20 + 10); // one row plus the row gap
                                          // Named areas: the sidebar spans two rows, the header both columns.
    let p = positions(
        "<style>.g{display:grid;grid-template-columns:50px 1fr;grid-template-areas:'head head' 'side main' 'side foot'}
         .g>*{margin:0}</style><div class=g><p style=grid-area:main>m<p style=grid-area:head>h<p style=grid-area:side>s<p style=grid-area:foot>f</div>",
        400,
    );
    let at = |t: &str| p.iter().find(|q| q.0 == t).map(|q| (q.1, q.2)).unwrap();
    assert_eq!(at("h").0, 8);
    assert_eq!(at("s").0, 8);
    assert_eq!(at("m").0, 58);
    assert_eq!(at("m").1, at("s").1);
    assert!(at("f").1 > at("m").1 && at("f").0 == 58);
    // Spans and auto-fill.
    let p = positions(
        "<div style='display:grid;grid-template-columns:repeat(auto-fill, minmax(100px, 1fr))'><p style='margin:0;grid-column:1/-1'>wide<p style=margin:0>x<p style=margin:0>y<p style=margin:0>z</div>",
        400,
    );
    let at = |t: &str| p.iter().find(|q| q.0 == t).map(|q| (q.1, q.2)).unwrap();
    // 384 / 100 → 3 columns of 128.
    assert_eq!((at("x").0, at("y").0, at("z").0), (8, 136, 264));
    assert!(at("x").1 > at("wide").1);
}
