//! Page scripts against the DOM: run on the host, check the tree and console.

use nebula_engine::nebula_script::Host;
use nebula_engine::{Page, ScriptSource, Scripts};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

struct Capture(Rc<RefCell<Vec<String>>>, Rc<RefCell<f64>>);

impl Host for Capture {
    fn now_ms(&mut self) -> f64 {
        *self.1.borrow()
    }

    fn console(&mut self, _level: &str, line: &str) {
        self.0.borrow_mut().push(line.to_string());
    }
}

fn page(html: &str) -> (Page, Scripts, Rc<RefCell<Vec<String>>>, Rc<RefCell<f64>>) {
    let mut p = Page::parse(html);
    let log = Rc::new(RefCell::new(Vec::new()));
    let clock = Rc::new(RefCell::new(1000.0));
    let mut js = Scripts::new(
        Box::new(Capture(log.clone(), clock.clone())),
        "https://example.com/dir/page.html?q=1#top",
        (1024, 768),
        BTreeMap::new(),
    );
    for s in p.scripts() {
        if let ScriptSource::Inline(code) = s {
            p.run_script(&mut js, &code, "inline");
        }
    }
    p.finish_loading(&mut js);
    (p, js, log, clock)
}

/// The body's visible text: text nodes outside scripts, space-separated.
fn body_text(p: &Page) -> String {
    use nebula_engine::dom::NodeData;
    let b = p.doc.find("body").unwrap();
    let mut words = Vec::new();
    for n in p.doc.descendants(b) {
        if let NodeData::Text(t) = &p.doc.nodes[n].data {
            let in_script = p.doc.parent(n).is_some_and(|x| p.doc.tag(x) == "script");
            if !in_script {
                words.extend(t.split_whitespace().map(String::from));
            }
        }
    }
    words.join(" ")
}

#[test]
fn scripts_change_the_document() {
    let (p, _js, log, _) = page(
        r##"<!doctype html><title>Old</title><body><p id="a">hello</p><ul id="list"></ul>
        <script>
          document.getElementById("a").textContent = "changed";
          const ul = document.querySelector("#list");
          for (const t of ["one", "two"]) { const li = document.createElement("li"); li.textContent = t; li.className = "item"; ul.appendChild(li); }
          document.title = "New title";
          console.log(document.querySelectorAll("li.item").length, ul.children[1].textContent, document.body.tagName);
          ul.insertAdjacentHTML("beforeend", "<li><b>three</b></li>");
          console.log(ul.innerHTML);
          console.log(location.hostname, location.pathname, location.search, location.hash);
        </script></body>"##,
    );
    assert_eq!(body_text(&p), "changed one two three");
    assert_eq!(p.title, "New title");
    assert_eq!(
        *log.borrow(),
        vec![
            "2 two BODY".to_string(),
            "<li class=\"item\">one</li><li class=\"item\">two</li><li><b>three</b></li>".to_string(),
            "example.com /dir/page.html ?q=1 #top".to_string(),
        ]
    );
}

#[test]
fn events_listeners_and_default_actions() {
    let (mut p, mut js, log, _) = page(
        r##"<body><a id="go" href="/x">go</a><button id="b" onclick="count++">add</button><span id="n">0</span>
        <script>
          var count = 0;
          document.getElementById("go").addEventListener("click", e => { e.preventDefault(); console.log("link", e.target.id, e.currentTarget.id); });
          document.body.addEventListener("click", e => console.log("bubbled to body from", e.target.id));
          document.getElementById("b").addEventListener("click", () => { document.getElementById("n").textContent = String(count); });
          document.addEventListener("DOMContentLoaded", () => console.log("ready", document.readyState));
          window.addEventListener("load", () => console.log("loaded", document.readyState));
        </script></body>"##,
    );
    let a = p.doc.find("a").unwrap();
    assert!(!p.dispatch_click(&mut js, a, 0, 0), "preventDefault cancels the link");
    let b = p.doc.find("button").unwrap();
    assert!(p.dispatch_click(&mut js, b, 0, 0));
    p.dispatch_click(&mut js, b, 0, 0);
    assert_eq!(body_text(&p), "go add 2");
    let l = log.borrow();
    assert_eq!(l[0], "ready interactive");
    assert_eq!(l[1], "loaded complete");
    assert_eq!(l[2], "link go go");
    assert_eq!(l[3], "bubbled to body from go");
}

#[test]
fn timers_and_animation_frames() {
    let (mut p, mut js, log, clock) = page(
        r##"<body><script>
          let ticks = 0;
          const id = setInterval(() => { ticks++; if (ticks === 3) { clearInterval(id); console.log("interval done"); } }, 100);
          setTimeout((a, b) => console.log("timeout", a + b), 50, 2, 3);
          requestAnimationFrame(t => console.log("frame", typeof t));
          Promise.resolve().then(() => console.log("microtask"));
        </script></body>"##,
    );
    assert_eq!(*log.borrow(), vec!["microtask".to_string()]);
    for step in 1..=5 {
        *clock.borrow_mut() = 1000.0 + step as f64 * 100.0;
        let now = *clock.borrow();
        p.run_timers(&mut js, now);
    }
    assert_eq!(*log.borrow(), vec!["microtask", "timeout 5", "frame number", "interval done"]);
    assert_eq!(js.next_wakeup(2000.0), None);
}

#[test]
fn forms_storage_and_navigation() {
    let (mut p, mut js, log, _) = page(
        r##"<body><form id="f"><input id="name" name="name" value="initial"><input type="checkbox" id="c"></form>
        <script>
          const input = document.getElementById("name");
          console.log(input.value);
          input.value = "typed";
          document.getElementById("c").checked = true;
          localStorage.setItem("visits", "1");
          console.log(localStorage.getItem("visits"), localStorage.length, sessionStorage.getItem("x"));
          document.getElementById("f").addEventListener("submit", e => { e.preventDefault(); location.href = "/done"; });
          input.classList.add("big", "bold"); input.classList.toggle("big");
          input.style.backgroundColor = "red";
          console.log(input.className, input.getAttribute("style"));
        </script></body>"##,
    );
    let input = p.doc.find("input").unwrap();
    assert_eq!(nebula_engine::script::control_value(&p.doc, input), "typed");
    let form = p.doc.find("form").unwrap();
    assert!(!p.dispatch(&mut js, form, "submit", true, true));
    assert_eq!(js.take_navigation().as_deref(), Some("/done"));
    assert_eq!(js.take_storage().unwrap().get("visits").map(String::as_str), Some("1"));
    let l = log.borrow();
    assert_eq!(l[0], "initial");
    assert_eq!(l[1], "1 1 null");
    assert_eq!(l[2], "bold background-color: red;");
}

#[test]
fn runaway_page_scripts_are_stopped_and_errors_reported() {
    let (p, _js, log, _) = page(
        r##"<body><p>x</p><script>while (true) {}</script><script>missing();</script><script>console.log("still running")</script></body>"##,
    );
    let l = log.borrow();
    assert!(l[0].contains("took too long"), "{l:?}");
    assert!(l[1].contains("ReferenceError: missing is not defined"), "{l:?}");
    assert_eq!(l[2], "still running");
    assert_eq!(body_text(&p), "x");
}

#[test]
fn the_bundled_demo_page_works() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/nebula/pulsar-demo.html");
    let html = std::fs::read_to_string(path).unwrap();
    let (mut p, mut js, log, clock) = page(&html);
    assert!(log.borrow().contains(&"Pulsar demo ready".to_string()), "{:?}", log.borrow());
    let by_id = |p: &Page, id: &str| {
        p.doc.descendants(0).into_iter().find(|n| p.doc.element(*n).is_some_and(|e| e.id() == Some(id))).unwrap()
    };
    // Counter.
    for _ in 0..3 {
        let plus = by_id(&p, "plus");
        p.dispatch_click(&mut js, plus, 0, 0);
    }
    let count = by_id(&p, "count");
    assert_eq!(p.doc.text_content(count), "3");
    assert_eq!(js.take_storage().unwrap().get("demo.count").map(String::as_str), Some("3"));
    // To-do list: type, submit, finish.
    let input = by_id(&p, "todo");
    p.doc.values.insert(input, "Write a JavaScript engine".into());
    let form = by_id(&p, "todo-form");
    assert!(!p.dispatch(&mut js, form, "submit", true, true), "the page handles submit itself");
    let list = by_id(&p, "todos");
    assert_eq!(p.doc.text_content(list), "Write a JavaScript engine");
    let li = p.doc.nodes[list].children[0];
    p.dispatch_click(&mut js, li, 0, 0);
    let li = p.doc.nodes[list].children[0];
    assert!(p.doc.element(li).unwrap().has_class("done"));
    // Clock: a timer redraws it.
    let clock_el = by_id(&p, "clock");
    assert_ne!(p.doc.text_content(clock_el), "…");
    *clock.borrow_mut() += 1500.0;
    let now = *clock.borrow();
    assert!(p.run_timers(&mut js, now));
    assert_eq!(p.title, "Pulsar Demo");
}

#[test]
fn inserted_scripts_run_once_and_external_ones_are_queued() {
    let (mut p, mut js, log, _) = page(
        r##"<body><div id="host"></div><script>
          const s = document.createElement("script");
          s.textContent = "console.log('inline ran', document.currentScript === undefined)";
          document.getElementById("host").appendChild(s);
          document.getElementById("host").appendChild(s); // moving it doesn't run it again
          const ext = document.createElement("script");
          ext.src = "/lib.js";
          ext.onload = () => console.log("loaded, lib says", window.libValue);
          document.head.appendChild(ext);
          const notJs = document.createElement("script");
          notJs.type = "application/json"; notJs.textContent = "{}";
          document.body.appendChild(notJs);
        </script></body>"##,
    );
    let fetches = js.take_fetches();
    assert_eq!(fetches.len(), 1, "{:?}", log.borrow());
    assert_eq!(fetches[0].1, "/lib.js");
    p.run_fetched(&mut js, fetches[0].0, "/lib.js", Some("window.libValue = 42;"));
    assert_eq!(*log.borrow(), vec!["inline ran true", "loaded, lib says 42"]);
}

#[test]
fn jquery_works() {
    let jquery = include_str!("fixtures/jquery-3.7.1.min.js");
    let (mut p, mut js, log, _clock) = page(
        r##"<!doctype html><html><head><title>jq</title></head>
        <body><ul id="list"><li class="a">one</li><li>two</li></ul><button id="b">go</button><div id="out"></div></body></html>"##,
    );
    p.run_script(&mut js, jquery, "jquery.js");
    p.run_script(
        &mut js,
        r##"
        $(function () {
          console.log("jquery", $.fn.jquery, $("li").length, $("#list li.a").text());
          $("#list").append("<li>three</li>").find("li").addClass("item");
          $("#b").on("click", function () { $("#out").text("clicked " + $(".item").length).css("color", "red"); });
          $("#b").trigger("click");
          console.log($("#out").text(), $("#out").attr("style"), $("li").map(function () { return $(this).text(); }).get().join());
          console.log(JSON.stringify($.extend({}, { a: 1 }, { b: 2 })), $("<p>hi</p>").html());
          $("li:first").remove();
          console.log($("li").length, $("li").eq(0).text(), $("ul").children().last().text());
          $("#out").hide(); console.log($("#out").css("display"), $("#out").is(":hidden"));
          $("#out").toggleClass("on").data("k", 5); console.log($("#out").hasClass("on"), $("#out").data("k"));
        });"##,
        "page",
    );
    // The page has loaded already: jQuery runs ready handlers from timers
    // (which schedule more timers), as browser ticks would.
    while js.next_wakeup(1e13).is_some() {
        p.run_timers(&mut js, 1e13);
    }
    assert_eq!(
        *log.borrow(),
        vec![
            "jquery 3.7.1 2 one",
            "clicked 3 color: red; one,two,three",
            "{\"a\":1,\"b\":2} hi",
            "2 two three",
            "none true",
            "true 5",
        ]
    );
    // A real click from the browser reaches jQuery's handler too.
    let b = p.doc.descendants(0).into_iter().find(|n| p.doc.element(*n).is_some_and(|e| e.id() == Some("b"))).unwrap();
    p.dispatch_click(&mut js, b, 0, 0);
}

#[test]
fn prelude_apis_behave_like_browsers() {
    let (mut p, mut js, log, _) = page(
        r##"<body><img id="lazy" data-src="a.png"><script>
          const u = new URL("../b/c.html?x=1&y=two#frag", "https://example.com/dir/sub/page.html");
          console.log(u.href, u.host, u.pathname, u.searchParams.get("y"));
          u.searchParams.set("x", "9 9"); console.log(u.search, String(new URLSearchParams({ a: 1, b: "&" })));
          new IntersectionObserver(entries => entries.forEach(e => { e.target.src = e.target.dataset.src; console.log("seen", e.isIntersecting); }))
            .observe(document.getElementById("lazy"));
          console.log(typeof MutationObserver, PerformanceObserver.supportedEntryTypes.length, URL.canParse("nope"));
        </script></body>"##,
    );
    while js.next_wakeup(1e13).is_some() {
        p.run_timers(&mut js, 1e13);
    }
    let img = p.doc.find("img").unwrap();
    assert_eq!(p.doc.element(img).unwrap().attr("src"), Some("a.png"));
    assert_eq!(
        *log.borrow(),
        vec![
            "https://example.com/dir/b/c.html?x=1&y=two#frag example.com /dir/b/c.html two",
            "?x=9+9&y=two a=1&b=%26",
            "function 0 false",
            "seen true",
        ]
    );
}

#[test]
fn fetch_and_xhr_go_through_the_browser() {
    let (mut p, mut js, log, _) = page(
        r##"<body><script>
          fetch("/api/items", { method: "POST", body: JSON.stringify({ q: 1 }), headers: { "X-Test": "yes" } })
            .then(r => { console.log("fetch", r.ok, r.status, r.headers.get("content-type")); return r.json(); })
            .then(data => console.log("json", data.items.join("+")));
          const xhr = new XMLHttpRequest();
          xhr.open("GET", "/hello.txt");
          xhr.onload = () => console.log("xhr", xhr.status, xhr.responseText, xhr.getResponseHeader("X-Server"));
          xhr.send();
          fetch("https://down.example/").catch(e => console.log("failed", e.name));
        </script></body>"##,
    );
    let reqs = js.take_requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(
        (reqs[0].method.as_str(), reqs[0].url.as_str(), reqs[0].body.as_str()),
        ("POST", "/api/items", "{\"q\":1}")
    );
    assert!(reqs[0].headers.iter().any(|(k, v)| k == "x-test" && v == "yes"));
    use nebula_engine::script::HttpResponse;
    let ok = |body: &str, header: (&str, &str)| HttpResponse {
        status: 200,
        status_text: "OK".into(),
        url: "https://example.com/x".into(),
        headers: vec![(header.0.into(), header.1.into())],
        body: body.into(),
    };
    p.complete_request(&mut js, reqs[0].id, Ok(ok("{\"items\":[\"a\",\"b\"]}", ("Content-Type", "application/json"))));
    p.complete_request(&mut js, reqs[1].id, Ok(ok("hi there", ("X-Server", "nebula"))));
    p.complete_request(&mut js, reqs[2].id, Err("no route".into()));
    assert_eq!(
        *log.borrow(),
        vec!["fetch true 200 application/json", "json a+b", "xhr 200 hi there nebula", "failed TypeError"]
    );
}

#[test]
fn text_codecs_and_crypto() {
    let (_p, _js, log, _) = page(
        r##"<body><script>
          const bytes = new TextEncoder().encode("héllo 😀");
          console.log(bytes.length, [...bytes.slice(0, 3)].join(), new TextDecoder().decode(bytes));
          console.log(new TextDecoder().decode(new Uint8Array([0xff, 0x41])));
          const r = crypto.getRandomValues(new Uint32Array(4));
          console.log(r.length, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID()));
        </script></body>"##,
    );
    assert_eq!(*log.borrow(), vec!["11 104,195,169 héllo 😀", "\u{FFFD}A", "4 true"]);
}
