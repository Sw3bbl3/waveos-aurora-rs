//! Page scripts against the DOM: run on the host, check the tree and console.

use nebula_engine::nebula_script::Host;
use nebula_engine::{Page, ScriptSource};
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

fn page(html: &str) -> (Page, Rc<RefCell<Vec<String>>>, Rc<RefCell<f64>>) {
    let mut p = Page::parse(html);
    let log = Rc::new(RefCell::new(Vec::new()));
    let clock = Rc::new(RefCell::new(1000.0));
    p.enable_scripts(
        Box::new(Capture(log.clone(), clock.clone())),
        "https://example.com/dir/page.html?q=1#top",
        (1024, 768),
        BTreeMap::new(),
    );
    for s in p.scripts() {
        if let ScriptSource::Inline(code) = s {
            p.run_script(&code, "inline");
        }
    }
    p.finish_loading();
    (p, log, clock)
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
    let (p, log, _) = page(
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
    let (mut p, log, _) = page(
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
    assert!(!p.dispatch_click(a, 0, 0), "preventDefault cancels the link");
    let b = p.doc.find("button").unwrap();
    assert!(p.dispatch_click(b, 0, 0));
    p.dispatch_click(b, 0, 0);
    assert_eq!(body_text(&p), "go add 2");
    let l = log.borrow();
    assert_eq!(l[0], "ready interactive");
    assert_eq!(l[1], "loaded complete");
    assert_eq!(l[2], "link go go");
    assert_eq!(l[3], "bubbled to body from go");
}

#[test]
fn timers_and_animation_frames() {
    let (mut p, log, clock) = page(
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
        p.run_timers(now);
    }
    assert_eq!(*log.borrow(), vec!["microtask", "timeout 5", "frame number", "interval done"]);
    assert_eq!(p.next_wakeup(2000.0), None);
}

#[test]
fn forms_storage_and_navigation() {
    let (mut p, log, _) = page(
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
    assert!(!p.dispatch(form, "submit", true, true));
    assert_eq!(p.take_navigation().as_deref(), Some("/done"));
    assert_eq!(p.take_storage().unwrap().get("visits").map(String::as_str), Some("1"));
    let l = log.borrow();
    assert_eq!(l[0], "initial");
    assert_eq!(l[1], "1 1 null");
    assert_eq!(l[2], "bold background-color: red;");
}

#[test]
fn runaway_page_scripts_are_stopped_and_errors_reported() {
    let (p, log, _) = page(
        r##"<body><p>x</p><script>while (true) {}</script><script>missing();</script><script>console.log("still running")</script></body>"##,
    );
    let l = log.borrow();
    assert!(l[0].contains("took too long"), "{l:?}");
    assert!(l[1].contains("ReferenceError: missing is not defined"), "{l:?}");
    assert_eq!(l[2], "still running");
    assert_eq!(body_text(&p), "x");
}
