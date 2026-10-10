use nebula_engine::{css, Page};
use nebula_web::Url;

fn color(page: &Page, w: i32, h: i32) -> u32 {
    let node = page.doc.find("p").unwrap();
    page.styles(w, h).styles[node].as_ref().unwrap().color
}

#[test]
fn sheet_media_is_evaluated_at_the_real_viewport() {
    let page = Page::parse("<style>p{color:red}</style><style media='(max-width:600px)'>p{color:blue}</style><style media='(min-height:900px)'>p{color:green}</style><style media=print>p{color:black}</style><p>Test</p>");
    assert_eq!(color(&page, 1000, 700), 0xffff0000);
    assert_eq!(color(&page, 400, 700), 0xff0000ff);
    assert_eq!(color(&page, 400, 1000), 0xff008000);
}

#[test]
fn imports_resolve_redirects_preserve_order_and_media() {
    let mut page = Page::parse(
        "<link rel=stylesheet href='/main.css' media='(min-height:400px)'><style>p{color:purple}</style><p>Test</p>",
    );
    let mut requests = Vec::new();
    page.load_stylesheets(&Url::parse("https://example.com/page").unwrap(), 16, |u| {
        requests.push(u.to_string());
        match u.path.as_str() {
            "/main.css" => Some((
                Url::parse("https://example.com/css/main.css").unwrap(),
                "@import 'base.css'; @import url('small.css') (max-width:600px); p{color:green}".into(),
            )),
            "/css/base.css" => Some((u.clone(), "p{color:red!important}".into())),
            "/css/small.css" => Some((u.clone(), "p{color:blue!important}".into())),
            _ => None,
        }
    });
    assert_eq!(
        requests,
        ["https://example.com/main.css", "https://example.com/css/base.css", "https://example.com/css/small.css"]
    );
    assert_eq!(color(&page, 1000, 700), 0xffff0000);
    assert_eq!(color(&page, 400, 700), 0xff0000ff);
    assert_eq!(color(&page, 400, 300), 0xff800080);
}

#[test]
fn imports_are_bounded_and_cycles_do_not_recurse() {
    let mut page = Page::parse("<style>@import 'a.css';p{color:green}</style><p>Test</p>");
    let mut count = 0;
    page.load_stylesheets(&Url::parse("https://example.com/").unwrap(), 16, |u| {
        count += 1;
        Some((u.clone(), "@import 'a.css';p{color:red}".into()))
    });
    assert_eq!(count, 1);
    assert_eq!(color(&page, 1000, 700), 0xff008000);
    let mut page = Page::parse("<style>@import 'a.css';</style><p>Test</p>");
    let mut count = 0;
    page.load_stylesheets(&Url::parse("https://example.com/").unwrap(), 3, |u| {
        count += 1;
        Some((u.clone(), format!("@import '{}.css';", count)))
    });
    assert_eq!(count, 3);
}

#[test]
fn comments_and_important_inside_strings_are_not_syntax() {
    let sheet = css::parse(r#"p{content:'/* keep */ !important';color:red ! IMPORTANT}"#);
    assert_eq!(sheet.rules[0].decls[0].value, "'/* keep */ !important'");
    assert!(!sheet.rules[0].decls[0].important);
    assert!(sheet.rules[0].decls[1].important);
    let decls = css::parse_declarations(r#"content:"a\";b";color:/* comment */blue"#);
    assert_eq!(decls.len(), 2);
    assert_eq!(decls[1].value, "blue");
}

#[test]
fn variable_and_literal_declarations_keep_cascade_order() {
    let page = Page::parse("<style>:root{--ink:blue}p{color:red;color:var(--ink);color:green ! important}</style><p style='color:purple'>Test</p>");
    assert_eq!(color(&page, 1000, 700), 0xff008000);
}
