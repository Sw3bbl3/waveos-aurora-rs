//! Any page — markup, styles, sizes — lays out without a panic.

use nebula_engine::{FontSpec, Host, Metrics, Page};

/// Every character is half the font size wide.
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

fn layout_both(html: &str) {
    let page = Page::parse(html);
    let _ = page.layout(800, 600, &Fake);
    let _ = page.layout(1, 1, &Fake);
}

#[test]
fn enormous_lengths_saturate() {
    // Lengths past what layout's integers hold used to overflow its sums.
    layout_both(
        "<div style='margin:1e40px;padding:1e30px;width:1e40px;border:1e20px solid'>\
         <p style='margin-left:-1e40px;text-indent:1e40px;line-height:1e40px'>text</p>\
         <div style='display:flex;gap:1e40px'><span style='flex-basis:1e40px'>a</span><span>b</span></div>\
         <div style='display:grid;grid-template-columns:1e40px 1e40px;gap:1e40px'><i>1</i><i>2</i></div>\
         <table style='border-spacing:1e40px'><tr><td style='width:1e40px'>c</td><td>d</td></tr></table>\
         <div style='position:absolute;left:1e40px;top:-1e40px;height:1e40px'>e</div></div>",
    );
}

const WORDS: &[&str] = &[
    "<table>",
    "<tr>",
    "<td colspan=99999>",
    "</td>",
    "<div style='display:grid;grid-template-columns:repeat(999,1fr)'>",
    "<div style='display:flex;flex-wrap:wrap'>",
    "</div>",
    "style='width:-1px'",
    "style='margin:-99999px'",
    "calc(100% - 1e9px)",
    "1e40px",
    "<li>",
    "<ol start=2147483647>",
    "<!--",
    "-->",
    "<style>",
    "</style>",
    "{",
    "}",
    ";",
    ":",
    "var(--x)",
    "--x:var(--x);",
    "::before{content:'x'}",
    "@media (max-width:0px){",
    "position:absolute;",
    "float:right;",
    "font-size:0",
    "font-size:1e9px",
    "line-height:-5",
    "rowspan=65535",
    "&#xFFFFFFFF;",
    "&amp",
    "<img src=big.png>",
    "<br>",
    "grid-row:1/-1;",
    "grid-column:span 0;",
    "order:-2147483648;",
    "border-radius:50%;",
    "<p>",
    "<pre>",
    "\t",
    "<a href=x>",
    "%",
    "!important",
    "<input type=range min=10 max=0>",
    "<select><option>",
];

#[test]
fn mutated_pages_never_panic() {
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/nebula");
    let mut seeds: Vec<Vec<u8>> = ["browser-apis.html", "canvas-demo.html", "css-demo.html", "pulsar-demo.html"]
        .iter()
        .map(|f| std::fs::read(assets.join(f)).unwrap())
        .collect();
    seeds.push(
        b"<table><tr><td colspan=3 rowspan=0>a<td>b</table><ul><li>x<ol start=-5><li>y</ol></ul>\
          <div style='display:grid;grid-template-columns:repeat(3,1fr);gap:4px'><p>1<p>2</div>\
          <div style='display:flex;flex-wrap:wrap'><span style='flex:1 1 0'>a</span></div><pre>  x\ty</pre>\
          <img src=big.png width=0 height=-1><select><option>a</select><textarea>t</textarea>\
          <div style='float:left;width:50%'>f</div>\
          <p style='white-space:nowrap;text-overflow:ellipsis;overflow:hidden;width:3px'>long text here</p>"
            .to_vec(),
    );
    aurora_mutate::check_with(
        "html",
        &seeds,
        60,
        |m, seed| {
            let mut page = m.mutate(seed);
            m.splice_words(&mut page, WORDS);
            page
        },
        |page| layout_both(&String::from_utf8_lossy(page)),
    );
}

#[test]
fn mutated_stylesheets_never_panic() {
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/nebula");
    let seeds: Vec<Vec<u8>> =
        ["css-demo.css", "css-demo-theme.css"].iter().map(|f| std::fs::read(assets.join(f)).unwrap()).collect();
    let words = [
        "{",
        "}",
        ";",
        ":",
        "(",
        ")",
        "[",
        "]",
        "calc(",
        "var(",
        "--a:",
        "@media",
        "@import",
        "!important",
        "::",
        ":nth-child(-n+3)",
        ":not(",
        ":is(",
        ":has(",
        "\\",
        "\"",
        "'",
        "/*",
        "*/",
        "url(",
        "#",
        ".",
        ">",
        "+",
        "~",
        "*",
        "e9",
        "-",
        "%",
        "rgb(",
        "hsl(",
        "linear-gradient(",
    ];
    aurora_mutate::check_with(
        "css",
        &seeds,
        3000,
        |m, seed| {
            let mut sheet = m.mutate(seed);
            m.splice_words(&mut sheet, &words);
            sheet
        },
        |sheet| {
            let css = String::from_utf8_lossy(sheet);
            let _ = nebula_engine::css::parse(&css);
            let _ = nebula_engine::css::parse_declarations(&css);
            let _ = nebula_engine::css::parse_selector(&css);
        },
    );
}
