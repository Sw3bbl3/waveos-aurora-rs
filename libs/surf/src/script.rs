//! JavaScript for pages: the DOM, events, timers and `window`, on Pulsar.
//!
//! The page's `Document` lives in [`crate::Page`]; while a script runs it is
//! moved into the realm's embedder slot (a [`Dom`]), where the native
//! functions below find it. Element wrappers, listeners and timers stay in
//! the `Dom` between runs, and are traced by the collector.

use crate::dom::{Document, Element, NodeData, NodeId, FRAGMENT};
use crate::{css, html};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use nebula_script::object::{Kind, NativeFn, Obj, CONFIGURABLE};
use nebula_script::{Call, Embedder, JsResult, ObjRef, PropKey, Realm, Sym, Value, DEFAULT, HIDDEN};

/// Host objects are tagged with what they wrap.
const NODE: u32 = 1;
/// Listeners on `window` use this target.
const WINDOW: NodeId = usize::MAX;

struct Listener {
    target: NodeId,
    kind: String,
    f: Value,
    capture: bool,
    once: bool,
    /// Set through an `onclick`-style property or attribute.
    property: bool,
}

/// A network request a script made (`fetch`, `XMLHttpRequest`).
#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub id: u32,
    pub method: String,
    /// As the script wrote it (the browser resolves it against the page).
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// What came back for an [`HttpRequest`].
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

struct Timer {
    id: u32,
    due: f64,
    every: Option<f64>,
    f: Value,
    args: Vec<Value>,
}

#[derive(Default)]
struct Protos {
    node: Option<ObjRef>,
    element: Option<ObjRef>,
    html_element: Option<ObjRef>,
    text: Option<ObjRef>,
    document: Option<ObjRef>,
    fragment: Option<ObjRef>,
    event: Option<ObjRef>,
    style: Option<ObjRef>,
    class_list: Option<ObjRef>,
    storage: Option<ObjRef>,
}

/// The browser state scripts see, kept inside the realm.
pub struct Dom {
    pub doc: Document,
    wrappers: BTreeMap<NodeId, ObjRef>,
    listeners: Vec<Listener>,
    /// Compiled `on…` attributes: (node, type) → (source, function).
    attr_handlers: BTreeMap<(NodeId, String), (String, Value)>,
    timers: Vec<Timer>,
    frames: Vec<(u32, Value)>,
    next_id: u32,
    protos: Protos,
    /// Private symbols for event flags.
    stop_sym: Option<Sym>,
    stop_now_sym: Option<Sym>,
    pub url: String,
    pub viewport: (i32, i32),
    pub ready_state: &'static str,
    /// `location.href = …` and friends.
    pub navigate: Option<String>,
    pub storage: BTreeMap<String, String>,
    pub storage_dirty: bool,
    session: BTreeMap<String, String>,
    pub cookie: BTreeMap<String, String>,
    pub focus: Option<NodeId>,
    pub alerts: Vec<String>,
    started: f64,
    /// Script elements inserted by scripts, to start once the change is done.
    inserted_scripts: Vec<NodeId>,
    /// Script elements that have run (or are loading): they never run twice.
    pub started_scripts: alloc::collections::BTreeSet<NodeId>,
    /// External scripts to fetch: (element, src as written).
    pub fetches: Vec<(NodeId, String)>,
    /// Element boxes from the last layout, in page coordinates: x, y, w, h.
    pub geometry: BTreeMap<NodeId, (i32, i32, i32, i32)>,
    /// The page's size and how far it is scrolled (set by the browser).
    pub page_size: (i32, i32),
    pub scroll: (i32, i32),
    /// A scroll position scripts asked for (window.scrollTo).
    pub scroll_request: Option<(i32, i32)>,
    /// Network requests to make, and the promise functions awaiting each.
    pub requests: Vec<HttpRequest>,
    pending_requests: BTreeMap<u32, (Value, Value)>,
}

impl Embedder for Dom {
    fn trace(&self, out: &mut Vec<ObjRef>) {
        out.extend(self.wrappers.values().copied());
        let mut v = |x: &Value| {
            if let Value::Object(o) = x {
                out.push(*o);
            }
        };
        for l in &self.listeners {
            v(&l.f);
        }
        for (_, f) in self.attr_handlers.values() {
            v(f);
        }
        for t in &self.timers {
            v(&t.f);
            t.args.iter().for_each(&mut v);
        }
        for (_, f) in &self.frames {
            v(f);
        }
        for (a, b) in self.pending_requests.values() {
            v(a);
            v(b);
        }
        let p = &self.protos;
        for o in [
            p.node,
            p.element,
            p.html_element,
            p.text,
            p.document,
            p.fragment,
            p.event,
            p.style,
            p.class_list,
            p.storage,
        ]
        .into_iter()
        .flatten()
        {
            out.push(o);
        }
    }

    fn as_any(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

/// Runs `f` with the DOM state taken out of the realm (so `f` may use the
/// realm freely, but must not run scripts).
fn with_dom<R>(rt: &mut Realm, f: impl FnOnce(&mut Realm, &mut Dom) -> R) -> R {
    let mut b = rt.embedder.take().expect("page scripts need a DOM");
    let r = {
        let dom = b.as_any().downcast_mut::<Dom>().expect("page scripts need a DOM");
        f(rt, dom)
    };
    rt.embedder = Some(b);
    r
}

pub fn dom_mut(rt: &mut Realm) -> &mut Dom {
    rt.embedder.as_mut().expect("page scripts need a DOM").as_any().downcast_mut::<Dom>().unwrap()
}

// ---------------------------------------------------------------- wrappers

fn wrap(rt: &mut Realm, dom: &mut Dom, n: NodeId) -> Value {
    if let Some(o) = dom.wrappers.get(&n) {
        return Value::Object(*o);
    }
    let p = &dom.protos;
    let proto = match &dom.doc.nodes[n].data {
        NodeData::Document => p.document,
        NodeData::Text(_) => p.text,
        NodeData::Element(e) if e.tag == FRAGMENT => p.fragment,
        NodeData::Element(_) => p.html_element,
    };
    let o = rt.alloc(Obj::new(proto, Kind::Host(NODE, n as u64)));
    dom.wrappers.insert(n, o);
    Value::Object(o)
}

fn wrap_opt(rt: &mut Realm, dom: &mut Dom, n: Option<NodeId>) -> Value {
    match n {
        Some(n) => wrap(rt, dom, n),
        None => Value::Null,
    }
}

fn node_of(rt: &Realm, v: &Value) -> Option<NodeId> {
    match v {
        Value::Object(o) => match rt.heap.get(*o).kind {
            Kind::Host(NODE, n) => Some(n as NodeId),
            _ => None,
        },
        _ => None,
    }
}

fn this_node(rt: &mut Realm, c: &Call) -> Result<NodeId, Value> {
    if c.this == Value::Object(rt.global) {
        return Ok(WINDOW);
    }
    node_of(rt, &c.this).ok_or_else(|| rt.type_error("Illegal invocation"))
}

fn arg_node(rt: &mut Realm, c: &Call, i: usize) -> Result<NodeId, Value> {
    node_of(rt, &c.arg(i)).ok_or_else(|| rt.type_error("parameter is not of type 'Node'"))
}

fn arg_str(rt: &mut Realm, c: &Call, i: usize) -> Result<String, Value> {
    rt.to_rust_string(&c.arg(i))
}

fn list(rt: &mut Realm, dom: &mut Dom, nodes: Vec<NodeId>) -> Value {
    let items: Vec<Value> = nodes.into_iter().map(|n| wrap(rt, dom, n)).collect();
    rt.array_from(items)
}

fn elements(doc: &Document, nodes: &[NodeId]) -> Vec<NodeId> {
    nodes.iter().copied().filter(|c| doc.element(*c).is_some()).collect()
}

fn hierarchy_error(rt: &mut Realm) -> Value {
    rt.error(nebula_script::ErrorKind::Error, "HierarchyRequestError: The new child element contains the parent.")
}

/// Text or a node from an `append(...)`-style argument.
fn to_node(rt: &mut Realm, dom: &mut Dom, v: &Value) -> Result<NodeId, Value> {
    if let Some(n) = node_of(rt, v) {
        return Ok(n);
    }
    let s = rt.to_rust_string(v)?;
    Ok(dom.doc.create(NodeData::Text(s)))
}

// ---------------------------------------------------------------- selectors

/// Parses a selector list as `querySelector` does: None for a syntax error
/// (including pseudo-classes CSS doesn't define, like jQuery's `:first`).
fn parse_selectors(selector: &str) -> Option<Vec<css::Selector>> {
    if !css::standard_pseudos(selector) {
        return None;
    }
    css::split_top(selector, b',').iter().map(|s| css::parse_selector(s.trim())).collect()
}

fn select(doc: &Document, root: NodeId, selector: &str, first: bool) -> Option<Vec<NodeId>> {
    let sels = parse_selectors(selector)?;
    let mut out = Vec::new();
    for n in doc.descendants(root) {
        if doc.element(n).is_some() && sels.iter().any(|s| css::matches(doc, n, s)) {
            out.push(n);
            if first {
                break;
            }
        }
    }
    Some(out)
}

fn syntax_error(rt: &mut Realm, sel: &str) -> Value {
    rt.error(nebula_script::ErrorKind::SyntaxError, &format!("'{sel}' is not a valid selector"))
}

fn query_selector(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let sel = arg_str(rt, c, 0)?;
    with_dom(rt, |rt, dom| {
        let root = if n == WINDOW { Document::ROOT } else { n };
        match select(&dom.doc, root, &sel, true) {
            Some(v) => Ok(wrap_opt(rt, dom, v.first().copied())),
            None => Err(syntax_error(rt, &sel)),
        }
    })
}

fn query_selector_all(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let sel = arg_str(rt, c, 0)?;
    with_dom(rt, |rt, dom| match select(&dom.doc, n, &sel, false) {
        Some(v) => Ok(list(rt, dom, v)),
        None => Err(syntax_error(rt, &sel)),
    })
}

fn matches_fn(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let sel = arg_str(rt, c, 0)?;
    with_dom(rt, |rt, dom| {
        let sels: Option<Vec<css::Selector>> = parse_selectors(&sel);
        match sels {
            Some(s) => Ok(Value::Bool(s.iter().any(|x| css::matches(&dom.doc, n, x)))),
            None => Err(syntax_error(rt, &sel)),
        }
    })
}

fn closest(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let sel = arg_str(rt, c, 0)?;
    with_dom(rt, |rt, dom| {
        let Some(sels) = parse_selectors(&sel) else {
            return Err(syntax_error(rt, &sel));
        };
        let mut cur = Some(n);
        while let Some(x) = cur {
            if dom.doc.element(x).is_some() && sels.iter().any(|s| css::matches(&dom.doc, x, s)) {
                return Ok(wrap(rt, dom, x));
            }
            cur = dom.doc.parent(x);
        }
        Ok(Value::Null)
    })
}

fn by_tag(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let tag = arg_str(rt, c, 0)?.to_ascii_lowercase();
    with_dom(rt, |rt, dom| {
        let found: Vec<NodeId> = dom
            .doc
            .descendants(n)
            .into_iter()
            .filter(|x| dom.doc.element(*x).is_some_and(|e| tag == "*" || e.tag == tag))
            .collect();
        Ok(list(rt, dom, found))
    })
}

fn by_class(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let names = arg_str(rt, c, 0)?;
    let wanted: Vec<&str> = names.split_ascii_whitespace().collect();
    with_dom(rt, |rt, dom| {
        let found: Vec<NodeId> = dom
            .doc
            .descendants(n)
            .into_iter()
            .filter(|x| {
                dom.doc.element(*x).is_some_and(|e| !wanted.is_empty() && wanted.iter().all(|w| e.has_class(w)))
            })
            .collect();
        Ok(list(rt, dom, found))
    })
}

fn get_element_by_id(rt: &mut Realm, c: &Call) -> JsResult {
    let id = arg_str(rt, c, 0)?;
    with_dom(rt, |rt, dom| {
        let found = dom
            .doc
            .descendants(Document::ROOT)
            .into_iter()
            .find(|x| dom.doc.element(*x).is_some_and(|e| e.id() == Some(id.as_str())));
        Ok(wrap_opt(rt, dom, found))
    })
}

// ---------------------------------------------------------------- tree

fn node_type(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| {
        Ok(Value::Number(match &dom.doc.nodes[n].data {
            NodeData::Document => 9.0,
            NodeData::Text(_) => 3.0,
            NodeData::Element(e) if e.tag == FRAGMENT => 11.0,
            NodeData::Element(_) => 1.0,
        }))
    })
}

fn node_name(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| {
        Ok(Value::str(&match &dom.doc.nodes[n].data {
            NodeData::Document => String::from("#document"),
            NodeData::Text(_) => String::from("#text"),
            NodeData::Element(e) => e.tag.to_ascii_uppercase(),
        }))
    })
}

fn text_of(doc: &Document, n: NodeId) -> String {
    match &doc.nodes[n].data {
        NodeData::Text(t) => t.clone(),
        _ => doc.text_content(n),
    }
}

fn text_content(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| {
        if matches!(dom.doc.nodes[n].data, NodeData::Document) {
            return Ok(Value::Null);
        }
        Ok(Value::str(&text_of(&dom.doc, n)))
    })
}

fn set_text_content(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let s = if c.arg(0).is_nullish() { String::new() } else { arg_str(rt, c, 0)? };
    with_dom(rt, |_, dom| {
        dom.doc.set_text_content(n, &s);
        Ok(Value::Undefined)
    })
}

macro_rules! relative {
    ($name:ident, $f:expr) => {
        fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            let n = this_node(rt, c)?;
            with_dom(rt, |rt, dom| {
                let f: fn(&Document, NodeId) -> Option<NodeId> = $f;
                let r = f(&dom.doc, n);
                Ok(wrap_opt(rt, dom, r))
            })
        }
    };
}

fn siblings(doc: &Document, n: NodeId) -> (&[NodeId], usize) {
    match doc.parent(n) {
        Some(p) => {
            let kids = &doc.nodes[p].children;
            (kids, kids.iter().position(|c| *c == n).unwrap_or(0))
        }
        None => (&[], 0),
    }
}

relative!(parent_node, |d, n| d.parent(n));
relative!(parent_element, |d, n| d.parent(n).filter(|p| d.element(*p).is_some()));
relative!(first_child, |d, n| d.nodes[n].children.first().copied());
relative!(last_child, |d, n| d.nodes[n].children.last().copied());
relative!(next_sibling, |d, n| {
    let (s, i) = siblings(d, n);
    s.get(i + 1).copied()
});
relative!(previous_sibling, |d, n| {
    let (s, i) = siblings(d, n);
    if i == 0 || s.is_empty() {
        None
    } else {
        s.get(i - 1).copied()
    }
});
relative!(first_element_child, |d, n| d.nodes[n].children.iter().copied().find(|c| d.element(*c).is_some()));
relative!(last_element_child, |d, n| d.nodes[n].children.iter().rev().copied().find(|c| d.element(*c).is_some()));
relative!(next_element_sibling, |d, n| d.next_elements(n).next());
relative!(previous_element_sibling, |d, n| d.previous_elements(n).next());

fn child_nodes(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |rt, dom| {
        let kids = dom.doc.nodes[n].children.clone();
        Ok(list(rt, dom, kids))
    })
}

fn children(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |rt, dom| {
        let kids = elements(&dom.doc, &dom.doc.nodes[n].children);
        Ok(list(rt, dom, kids))
    })
}

fn child_element_count(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::Number(elements(&dom.doc, &dom.doc.nodes[n].children).len() as f64)))
}

fn has_child_nodes(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::Bool(!dom.doc.nodes[n].children.is_empty())))
}

fn is_connected(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::Bool(dom.doc.is_connected(n))))
}

fn owner_document(rt: &mut Realm, c: &Call) -> JsResult {
    this_node(rt, c)?;
    with_dom(rt, |rt, dom| Ok(wrap(rt, dom, Document::ROOT)))
}

fn insert_checked(
    rt: &mut Realm,
    dom: &mut Dom,
    parent: NodeId,
    child: NodeId,
    before: Option<NodeId>,
) -> Result<(), Value> {
    // A fragment's children are what gets inserted.
    let roots: Vec<NodeId> =
        if dom.doc.tag(child) == FRAGMENT { dom.doc.nodes[child].children.clone() } else { vec![child] };
    if !dom.doc.insert(parent, child, before) {
        return Err(hierarchy_error(rt));
    }
    if dom.doc.is_connected(parent) {
        for r in roots {
            let mut all = vec![r];
            all.extend(dom.doc.descendants(r));
            dom.inserted_scripts.extend(all.into_iter().filter(|n| dom.doc.tag(*n) == "script"));
        }
    }
    Ok(())
}

/// Whether a script element holds classic JavaScript.
pub fn is_classic_script(doc: &Document, n: NodeId) -> bool {
    let ty = doc.element(n).and_then(|e| e.attr("type")).unwrap_or("").trim().to_ascii_lowercase();
    ty.is_empty()
        || matches!(
            ty.as_str(),
            "text/javascript"
                | "application/javascript"
                | "text/ecmascript"
                | "application/ecmascript"
                | "application/x-javascript"
        )
}

/// Starts scripts that were just inserted: inline ones run now, external
/// ones are queued for the browser to fetch.
fn run_inserted_scripts(rt: &mut Realm) {
    let list = with_dom(rt, |_, dom| core::mem::take(&mut dom.inserted_scripts));
    for n in list {
        let job = with_dom(rt, |_, dom| {
            if !dom.doc.is_connected(n) || !is_classic_script(&dom.doc, n) || !dom.started_scripts.insert(n) {
                return None;
            }
            match dom.doc.element(n).and_then(|e| e.attr("src")).map(str::trim).filter(|s| !s.is_empty()) {
                Some(src) => {
                    dom.fetches.push((n, String::from(src)));
                    None
                }
                None => Some(dom.doc.text_content(n)),
            }
        });
        if let Some(code) = job {
            let r = rt.eval(&code, "inserted script");
            report(rt, r);
        }
    }
}

fn append_child(rt: &mut Realm, c: &Call) -> JsResult {
    let p = this_node(rt, c)?;
    let child = arg_node(rt, c, 0)?;
    with_dom(rt, |rt, dom| insert_checked(rt, dom, p, child, None))?;
    run_inserted_scripts(rt);
    Ok(c.arg(0))
}

fn insert_before(rt: &mut Realm, c: &Call) -> JsResult {
    let p = this_node(rt, c)?;
    let child = arg_node(rt, c, 0)?;
    let before = if c.arg(1).is_nullish() { None } else { Some(arg_node(rt, c, 1)?) };
    with_dom(rt, |rt, dom| insert_checked(rt, dom, p, child, before))?;
    run_inserted_scripts(rt);
    Ok(c.arg(0))
}

fn remove_child(rt: &mut Realm, c: &Call) -> JsResult {
    let p = this_node(rt, c)?;
    let child = arg_node(rt, c, 0)?;
    with_dom(rt, |rt, dom| {
        if dom.doc.parent(child) != Some(p) {
            return Err(rt.error(
                nebula_script::ErrorKind::Error,
                "NotFoundError: The node to be removed is not a child of this node.",
            ));
        }
        dom.doc.detach(child);
        Ok(())
    })?;
    Ok(c.arg(0))
}

fn replace_child(rt: &mut Realm, c: &Call) -> JsResult {
    let p = this_node(rt, c)?;
    let new = arg_node(rt, c, 0)?;
    let old = arg_node(rt, c, 1)?;
    with_dom(rt, |rt, dom| {
        if dom.doc.parent(old) != Some(p) {
            return Err(rt.error(
                nebula_script::ErrorKind::Error,
                "NotFoundError: The node to be replaced is not a child of this node.",
            ));
        }
        insert_checked(rt, dom, p, new, Some(old))?;
        dom.doc.detach(old);
        Ok(())
    })?;
    run_inserted_scripts(rt);
    Ok(c.arg(1))
}

fn remove(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| dom.doc.detach(n));
    Ok(Value::Undefined)
}

/// append / prepend / before / after / replaceWith.
fn insert_many(rt: &mut Realm, c: &Call, mode: u8) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |rt, dom| {
        let mut nodes = Vec::new();
        for a in &c.args {
            nodes.push(to_node(rt, dom, a)?);
        }
        let (parent, before) = match mode {
            0 => (Some(n), None),
            1 => (Some(n), dom.doc.nodes[n].children.first().copied()),
            2 => (dom.doc.parent(n), Some(n)),
            _ => {
                let (s, i) = siblings(&dom.doc, n);
                (dom.doc.parent(n), s.get(i + 1).copied())
            }
        };
        let Some(parent) = parent else { return Ok(Value::Undefined) };
        for x in nodes {
            insert_checked(rt, dom, parent, x, before)?;
        }
        if mode == 4 {
            dom.doc.detach(n);
        }
        Ok(Value::Undefined)
    })
}

fn append(rt: &mut Realm, c: &Call) -> JsResult {
    let r = insert_many(rt, c, 0);
    run_inserted_scripts(rt);
    r
}

fn prepend(rt: &mut Realm, c: &Call) -> JsResult {
    let r = insert_many(rt, c, 1);
    run_inserted_scripts(rt);
    r
}

fn before(rt: &mut Realm, c: &Call) -> JsResult {
    let r = insert_many(rt, c, 2);
    run_inserted_scripts(rt);
    r
}

fn after(rt: &mut Realm, c: &Call) -> JsResult {
    let r = insert_many(rt, c, 3);
    run_inserted_scripts(rt);
    r
}

fn replace_with(rt: &mut Realm, c: &Call) -> JsResult {
    // after(...) then remove.
    let n = this_node(rt, c)?;
    insert_many(rt, c, 3)?;
    with_dom(rt, |_, dom| dom.doc.detach(n));
    Ok(Value::Undefined)
}

fn clone_node(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let deep = c.arg(0).truthy();
    with_dom(rt, |rt, dom| {
        let copy = dom.doc.clone_node(n, deep);
        Ok(wrap(rt, dom, copy))
    })
}

fn contains(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let Some(other) = node_of(rt, &c.arg(0)) else { return Ok(Value::Bool(false)) };
    with_dom(rt, |_, dom| Ok(Value::Bool(dom.doc.is_inclusive_ancestor(n, other))))
}

// ---------------------------------------------------------------- elements

fn tag_name(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::str(&dom.doc.tag(n).to_ascii_uppercase())))
}

fn get_attribute(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    with_dom(rt, |_, dom| Ok(dom.doc.element(n).and_then(|e| e.attr(&name)).map(Value::str).unwrap_or(Value::Null)))
}

fn set_attribute(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    let value = arg_str(rt, c, 1)?;
    with_dom(rt, |_, dom| dom.doc.set_attr(n, &name, &value));
    Ok(Value::Undefined)
}

fn remove_attribute(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    with_dom(rt, |_, dom| dom.doc.remove_attr(n, &name));
    Ok(Value::Undefined)
}

fn has_attribute(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    with_dom(rt, |_, dom| Ok(Value::Bool(dom.doc.element(n).is_some_and(|e| e.attr(&name).is_some()))))
}

fn toggle_attribute(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    let force = if c.arg(1).is_undefined() { None } else { Some(c.arg(1).truthy()) };
    with_dom(rt, |_, dom| {
        let has = dom.doc.element(n).is_some_and(|e| e.attr(&name).is_some());
        let want = force.unwrap_or(!has);
        if want && !has {
            dom.doc.set_attr(n, &name, "");
        } else if !want && has {
            dom.doc.remove_attr(n, &name);
        }
        Ok(Value::Bool(want))
    })
}

fn attribute_names(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let names: Vec<Value> = with_dom(rt, |_, dom| {
        dom.doc.element(n).map(|e| e.attrs.iter().map(|(k, _)| Value::str(k)).collect()).unwrap_or_default()
    });
    Ok(rt.array_from(names))
}

/// Reflected string attributes: `el.id`, `el.href`, … (name in slot 0).
fn reflect_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = rt.native_slots(c.callee)[0].clone();
    let name = rt.to_rust_string(&name)?;
    with_dom(rt, |_, dom| Ok(Value::str(dom.doc.element(n).and_then(|e| e.attr(&name)).unwrap_or(""))))
}

fn reflect_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = rt.native_slots(c.callee)[0].clone();
    let name = rt.to_rust_string(&name)?;
    let v = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| dom.doc.set_attr(n, &name, &v));
    Ok(Value::Undefined)
}

/// Reflected boolean attributes: `hidden`, `disabled`, …
fn reflect_bool_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = rt.native_slots(c.callee)[0].clone();
    let name = rt.to_rust_string(&name)?;
    with_dom(rt, |_, dom| Ok(Value::Bool(dom.doc.element(n).is_some_and(|e| e.attr(&name).is_some()))))
}

fn reflect_bool_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let name = rt.native_slots(c.callee)[0].clone();
    let name = rt.to_rust_string(&name)?;
    let on = c.arg(0).truthy();
    with_dom(rt, |_, dom| {
        if on {
            dom.doc.set_attr(n, &name, "");
        } else {
            dom.doc.remove_attr(n, &name);
        }
    });
    Ok(Value::Undefined)
}

fn inner_html(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::str(&html::serialize_children(&dom.doc, n))))
}

fn outer_html(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::str(&html::serialize(&dom.doc, n))))
}

/// Parses HTML into detached nodes of `doc`.
fn parse_into(doc: &mut Document, src: &str) -> Vec<NodeId> {
    let frag = html::parse_fragment(src);
    frag.nodes[Document::ROOT].children.clone().into_iter().map(|c| doc.import(&frag, c)).collect()
}

fn set_inner_html(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let src = if c.arg(0).is_nullish() { String::new() } else { arg_str(rt, c, 0)? };
    with_dom(rt, |_, dom| {
        dom.doc.set_text_content(n, "");
        for x in parse_into(&mut dom.doc, &src) {
            dom.doc.insert(n, x, None);
        }
    });
    Ok(Value::Undefined)
}

fn set_outer_html(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let src = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| {
        let Some(p) = dom.doc.parent(n) else { return };
        for x in parse_into(&mut dom.doc, &src) {
            dom.doc.insert(p, x, Some(n));
        }
        dom.doc.detach(n);
    });
    Ok(Value::Undefined)
}

fn insert_adjacent(rt: &mut Realm, c: &Call, what: u8) -> JsResult {
    let n = this_node(rt, c)?;
    let pos = arg_str(rt, c, 0)?.to_ascii_lowercase();
    let arg = c.arg(1);
    with_dom(rt, |rt, dom| {
        let nodes = match what {
            0 => {
                let s = rt.to_rust_string(&arg)?;
                parse_into(&mut dom.doc, &s)
            }
            1 => vec![node_of(rt, &arg).ok_or_else(|| rt.type_error("parameter 2 is not of type 'Element'"))?],
            _ => {
                let s = rt.to_rust_string(&arg)?;
                vec![dom.doc.create(NodeData::Text(s))]
            }
        };
        let (parent, before) = match pos.as_str() {
            "beforebegin" => (dom.doc.parent(n), Some(n)),
            "afterbegin" => (Some(n), dom.doc.nodes[n].children.first().copied()),
            "beforeend" => (Some(n), None),
            "afterend" => {
                let (s, i) = siblings(&dom.doc, n);
                (dom.doc.parent(n), s.get(i + 1).copied())
            }
            _ => {
                return Err(rt.error(
                    nebula_script::ErrorKind::SyntaxError,
                    "The value provided is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'.",
                ))
            }
        };
        if let Some(p) = parent {
            for x in nodes {
                insert_checked(rt, dom, p, x, before)?;
            }
        }
        Ok(Value::Undefined)
    })
}

fn insert_adjacent_html(rt: &mut Realm, c: &Call) -> JsResult {
    insert_adjacent(rt, c, 0)
}

fn insert_adjacent_element(rt: &mut Realm, c: &Call) -> JsResult {
    insert_adjacent(rt, c, 1)?;
    run_inserted_scripts(rt);
    Ok(c.arg(1))
}

fn insert_adjacent_text(rt: &mut Realm, c: &Call) -> JsResult {
    insert_adjacent(rt, c, 2)
}

/// The current value of a form control.
pub fn control_value(doc: &Document, n: NodeId) -> String {
    if let Some(v) = doc.values.get(&n) {
        return v.clone();
    }
    let Some(e) = doc.element(n) else { return String::new() };
    match e.tag.as_str() {
        "textarea" => doc.text_content(n),
        "select" => {
            let opts: Vec<NodeId> = doc.descendants(n).into_iter().filter(|o| doc.tag(*o) == "option").collect();
            let sel = opts.iter().find(|o| doc.element(**o).unwrap().attr("selected").is_some()).or(opts.first());
            sel.map(|o| option_value(doc, *o)).unwrap_or_default()
        }
        "option" => option_value(doc, n),
        _ => String::from(e.attr("value").unwrap_or(if e.attr("type") == Some("checkbox") { "on" } else { "" })),
    }
}

fn option_value(doc: &Document, o: NodeId) -> String {
    let e = doc.element(o).unwrap();
    e.attr("value").map(String::from).unwrap_or_else(|| String::from(doc.text_content(o).trim()))
}

fn value_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::str(&control_value(&dom.doc, n))))
}

fn value_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let v = if c.arg(0).is_nullish() { String::new() } else { arg_str(rt, c, 0)? };
    with_dom(rt, |_, dom| {
        if dom.doc.tag(n) == "select" {
            // Select the option with that value.
            for o in dom.doc.descendants(n) {
                if dom.doc.tag(o) == "option" {
                    if option_value(&dom.doc, o) == v {
                        dom.doc.set_attr(o, "selected", "");
                    } else {
                        dom.doc.remove_attr(o, "selected");
                    }
                }
            }
        } else {
            dom.doc.values.insert(n, v);
            dom.doc.version += 1;
        }
    });
    Ok(Value::Undefined)
}

fn checked_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| Ok(Value::Bool(dom.doc.element(n).is_some_and(|e| e.attr("checked").is_some()))))
}

fn checked_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let on = c.arg(0).truthy();
    with_dom(rt, |_, dom| set_checked(&mut dom.doc, n, on));
    Ok(Value::Undefined)
}

/// Checks a box or radio button (unchecking the rest of a radio group).
pub fn set_checked(doc: &mut Document, n: NodeId, on: bool) {
    let Some(e) = doc.element(n) else { return };
    let radio = e.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio"));
    let name = String::from(e.attr("name").unwrap_or(""));
    if on && radio && !name.is_empty() {
        for other in doc.find_all("input") {
            let same = doc.element(other).is_some_and(|o| {
                o.attr("name") == Some(name.as_str()) && o.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("radio"))
            });
            if same && other != n {
                doc.remove_attr(other, "checked");
            }
        }
    }
    if on {
        doc.set_attr(n, "checked", "");
    } else {
        doc.remove_attr(n, "checked");
    }
}

fn selected_index_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| {
        let opts: Vec<NodeId> = dom.doc.descendants(n).into_iter().filter(|o| dom.doc.tag(*o) == "option").collect();
        let i = opts.iter().position(|o| dom.doc.element(*o).unwrap().attr("selected").is_some()).map(|i| i as f64);
        Ok(Value::Number(i.unwrap_or(if opts.is_empty() { -1.0 } else { 0.0 })))
    })
}

fn selected_index_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let i = rt.to_number(&c.arg(0))?;
    with_dom(rt, |_, dom| {
        let opts: Vec<NodeId> = dom.doc.descendants(n).into_iter().filter(|o| dom.doc.tag(*o) == "option").collect();
        for (k, o) in opts.into_iter().enumerate() {
            if k as f64 == i {
                dom.doc.set_attr(o, "selected", "");
            } else {
                dom.doc.remove_attr(o, "selected");
            }
        }
    });
    Ok(Value::Undefined)
}

fn options(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |rt, dom| {
        let opts: Vec<NodeId> = dom.doc.descendants(n).into_iter().filter(|o| dom.doc.tag(*o) == "option").collect();
        Ok(list(rt, dom, opts))
    })
}

fn form_of(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |rt, dom| {
        let mut cur = dom.doc.parent(n);
        while let Some(p) = cur {
            if dom.doc.tag(p) == "form" {
                return Ok(wrap(rt, dom, p));
            }
            cur = dom.doc.parent(p);
        }
        Ok(Value::Null)
    })
}

fn dataset(rt: &mut Realm, c: &Call) -> JsResult {
    // A snapshot of the data-* attributes (writes don't reflect back).
    let n = this_node(rt, c)?;
    let pairs: Vec<(String, String)> = with_dom(rt, |_, dom| {
        dom.doc
            .element(n)
            .map(|e| {
                e.attrs
                    .iter()
                    .filter_map(|(k, v)| {
                        let rest = k.strip_prefix("data-")?;
                        let mut name = String::new();
                        let mut up = false;
                        for ch in rest.chars() {
                            if ch == '-' {
                                up = true;
                            } else if up {
                                name.extend(ch.to_uppercase());
                                up = false;
                            } else {
                                name.push(ch);
                            }
                        }
                        Some((name, v.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    let o = rt.new_object();
    for (k, v) in pairs {
        rt.define(o, k.as_str(), Value::str(&v), DEFAULT);
    }
    Ok(Value::Object(o))
}

/// An element's box: from the last layout; the viewport for <html>.
fn box_of(dom: &Dom, n: NodeId) -> Option<(i32, i32, i32, i32)> {
    match dom.doc.tag(n) {
        "html" => Some((0, 0, dom.viewport.0, dom.page_size.1.max(dom.viewport.1))),
        _ => dom.geometry.get(&n).copied(),
    }
}

fn dom_rect(rt: &mut Realm, x: i32, y: i32, w: i32, h: i32) -> Value {
    let o = rt.new_object();
    for (k, v) in
        [("x", x), ("y", y), ("left", x), ("top", y), ("width", w), ("height", h), ("right", x + w), ("bottom", y + h)]
    {
        rt.define(o, k, Value::Number(v as f64), DEFAULT);
    }
    Value::Object(o)
}

fn rect(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let (b, scroll) = with_dom(rt, |_, dom| (box_of(dom, n), dom.scroll));
    let (x, y, w, h) = b.unwrap_or((0, 0, 0, 0));
    Ok(dom_rect(rt, x - scroll.0, y - scroll.1, w, h))
}

fn client_rects(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let (b, scroll) = with_dom(rt, |_, dom| (box_of(dom, n), dom.scroll));
    let items = match b {
        Some((x, y, w, h)) => vec![dom_rect(rt, x - scroll.0, y - scroll.1, w, h)],
        None => Vec::new(),
    };
    Ok(rt.array_from(items))
}

/// offsetWidth, clientHeight, …: which measure is in slot 0.
fn measure(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let what = rt.native_slots(c.callee)[0].clone();
    let what = rt.to_rust_string(&what)?;
    let v = with_dom(rt, |_, dom| {
        let is_root = matches!(dom.doc.tag(n), "html" | "body");
        let (x, y, w, h) = box_of(dom, n).unwrap_or((0, 0, 0, 0));
        match what.as_str() {
            "offsetWidth" | "scrollWidth" => w,
            "offsetHeight" => h,
            "clientWidth" if is_root => dom.viewport.0,
            "clientHeight" if is_root => dom.viewport.1,
            "clientWidth" => w,
            "clientHeight" => h,
            "scrollHeight" if is_root => dom.page_size.1,
            "scrollHeight" => h,
            "offsetTop" => y,
            "offsetLeft" => x,
            "scrollTop" if is_root => dom.scroll.1,
            "scrollLeft" if is_root => dom.scroll.0,
            _ => 0,
        }
    });
    Ok(Value::Number(v as f64))
}

/// window.scrollX (slot 0 false) and scrollY (true).
fn scroll_xy(rt: &mut Realm, c: &Call) -> JsResult {
    let y = rt.native_slots(c.callee)[0].truthy();
    let s = with_dom(rt, |_, dom| dom.scroll);
    Ok(Value::Number(if y { s.1 } else { s.0 } as f64))
}

/// window.scrollTo(x, y) / scrollTo({ top, left }); scrollBy adds (slot 0 true).
fn scroll_to(rt: &mut Realm, c: &Call) -> JsResult {
    let by = rt.native_slots(c.callee).first().is_some_and(|v| v.truthy());
    let (mut x, mut y) = (None, None);
    match c.arg(0) {
        Value::Object(o) => {
            let l = rt.get_str(o, "left")?;
            let t = rt.get_str(o, "top")?;
            if !l.is_undefined() {
                x = Some(rt.to_number(&l)?);
            }
            if !t.is_undefined() {
                y = Some(rt.to_number(&t)?);
            }
        }
        v => {
            x = Some(rt.to_number(&v)?);
            y = Some(rt.to_number(&c.arg(1))?);
        }
    }
    with_dom(rt, |_, dom| {
        let base = dom.scroll_request.unwrap_or(dom.scroll);
        let fx = |v: f64, b: i32| if v.is_finite() { v as i32 } else { b };
        let nx = x.map(|v| if by { base.0 + fx(v, 0) } else { fx(v, base.0) }).unwrap_or(base.0);
        let ny = y.map(|v| if by { base.1 + fx(v, 0) } else { fx(v, base.1) }).unwrap_or(base.1);
        dom.scroll_request = Some((nx.max(0), ny.max(0)));
    });
    Ok(Value::Undefined)
}

fn scroll_into_view(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| {
        if let Some((_, y, _, _)) = box_of(dom, n) {
            dom.scroll_request = Some((0, y.max(0)));
        }
    });
    Ok(Value::Undefined)
}

fn zero(_rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::Number(0.0))
}

fn noop(_rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::Undefined)
}

fn focus(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| dom.focus = Some(n));
    Ok(Value::Undefined)
}

fn blur(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    with_dom(rt, |_, dom| {
        if dom.focus == Some(n) {
            dom.focus = None;
        }
    });
    Ok(Value::Undefined)
}

fn click(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let ev = make_event(rt, "click", true, true)?;
    dispatch(rt, n, ev)?;
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------- classList and style

fn class_list(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let proto = with_dom(rt, |_, dom| dom.protos.class_list);
    Ok(Value::Object(rt.alloc(Obj::new(proto, Kind::Host(NODE + 1, n as u64)))))
}

fn list_node(rt: &mut Realm, c: &Call, tag: u32) -> Result<NodeId, Value> {
    match &c.this {
        Value::Object(o) => match rt.heap.get(*o).kind {
            Kind::Host(t, n) if t == tag => Ok(n as NodeId),
            _ => Err(rt.type_error("Illegal invocation")),
        },
        _ => Err(rt.type_error("Illegal invocation")),
    }
}

fn classes(doc: &Document, n: NodeId) -> Vec<String> {
    doc.element(n).map(|e| e.classes().map(String::from).collect()).unwrap_or_default()
}

fn set_classes(doc: &mut Document, n: NodeId, list: &[String]) {
    doc.set_attr(n, "class", &list.join(" "));
}

fn cl_add(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    let mut add = Vec::new();
    for i in 0..c.args.len() {
        add.push(arg_str(rt, c, i)?);
    }
    with_dom(rt, |_, dom| {
        let mut list = classes(&dom.doc, n);
        for a in add {
            if !list.contains(&a) {
                list.push(a);
            }
        }
        set_classes(&mut dom.doc, n, &list);
    });
    Ok(Value::Undefined)
}

fn cl_remove(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    let mut rm = Vec::new();
    for i in 0..c.args.len() {
        rm.push(arg_str(rt, c, i)?);
    }
    with_dom(rt, |_, dom| {
        let mut list = classes(&dom.doc, n);
        list.retain(|x| !rm.contains(x));
        set_classes(&mut dom.doc, n, &list);
    });
    Ok(Value::Undefined)
}

fn cl_toggle(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    let name = arg_str(rt, c, 0)?;
    let force = if c.arg(1).is_undefined() { None } else { Some(c.arg(1).truthy()) };
    with_dom(rt, |_, dom| {
        let mut list = classes(&dom.doc, n);
        let has = list.contains(&name);
        let want = force.unwrap_or(!has);
        if want && !has {
            list.push(name);
        } else if !want && has {
            list.retain(|x| *x != name);
        }
        set_classes(&mut dom.doc, n, &list);
        Ok(Value::Bool(want))
    })
}

fn cl_contains(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    let name = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| Ok(Value::Bool(classes(&dom.doc, n).contains(&name))))
}

fn cl_replace(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    let old = arg_str(rt, c, 0)?;
    let new = arg_str(rt, c, 1)?;
    with_dom(rt, |_, dom| {
        let mut list = classes(&dom.doc, n);
        let Some(i) = list.iter().position(|x| *x == old) else { return Ok(Value::Bool(false)) };
        list[i] = new;
        set_classes(&mut dom.doc, n, &list);
        Ok(Value::Bool(true))
    })
}

fn cl_length(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    with_dom(rt, |_, dom| Ok(Value::Number(classes(&dom.doc, n).len() as f64)))
}

fn cl_item(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    let i = rt.to_number(&c.arg(0))? as usize;
    with_dom(rt, |_, dom| Ok(classes(&dom.doc, n).get(i).map(|s| Value::str(s)).unwrap_or(Value::Null)))
}

fn cl_value(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 1)?;
    with_dom(rt, |_, dom| Ok(Value::str(&classes(&dom.doc, n).join(" "))))
}

fn style_obj(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let proto = with_dom(rt, |_, dom| dom.protos.style);
    Ok(Value::Object(rt.alloc(Obj::new(proto, Kind::Host(NODE + 2, n as u64)))))
}

fn camel_to_kebab(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_uppercase() {
            out.push('-');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    if out.starts_with("webkit-") || out.starts_with("moz-") {
        out.insert(0, '-');
    }
    out
}

/// The inline style declarations of an element, in order.
fn inline_decls(doc: &Document, n: NodeId) -> Vec<(String, String)> {
    let Some(s) = doc.element(n).and_then(|e| e.attr("style")) else { return Vec::new() };
    s.split(';')
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            Some((String::from(k.trim()).to_ascii_lowercase(), String::from(v.trim())))
        })
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

fn write_decls(doc: &mut Document, n: NodeId, decls: &[(String, String)]) {
    let s: Vec<String> = decls.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    if s.is_empty() {
        doc.remove_attr(n, "style");
    } else {
        doc.set_attr(n, "style", &(s.join("; ") + ";"));
    }
}

fn style_prop_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 2)?;
    let name = rt.native_slots(c.callee)[0].clone();
    let name = rt.to_rust_string(&name)?;
    with_dom(rt, |_, dom| {
        Ok(Value::str(
            inline_decls(&dom.doc, n).iter().rev().find(|(k, _)| *k == name).map(|(_, v)| v.as_str()).unwrap_or(""),
        ))
    })
}

fn set_style_prop(doc: &mut Document, n: NodeId, name: &str, value: &str) {
    let mut decls = inline_decls(doc, n);
    decls.retain(|(k, _)| k != name);
    if !value.is_empty() {
        decls.push((String::from(name), String::from(value)));
    }
    write_decls(doc, n, &decls);
}

fn style_prop_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 2)?;
    let name = rt.native_slots(c.callee)[0].clone();
    let name = rt.to_rust_string(&name)?;
    let mut v = if c.arg(0).is_nullish() { String::new() } else { arg_str(rt, c, 0)? };
    // Numbers for lengths get no unit in browsers either, except 0.
    if let Value::Number(x) = c.arg(0) {
        if x == 0.0 {
            v = String::from("0");
        }
    }
    with_dom(rt, |_, dom| set_style_prop(&mut dom.doc, n, &name, &v));
    Ok(Value::Undefined)
}

fn style_set_property(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 2)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    let v = if c.arg(1).is_nullish() { String::new() } else { arg_str(rt, c, 1)? };
    with_dom(rt, |_, dom| set_style_prop(&mut dom.doc, n, &name, &v));
    Ok(Value::Undefined)
}

fn style_get_property(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 2)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    with_dom(rt, |_, dom| {
        Ok(Value::str(
            inline_decls(&dom.doc, n).iter().rev().find(|(k, _)| *k == name).map(|(_, v)| v.as_str()).unwrap_or(""),
        ))
    })
}

fn style_remove_property(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 2)?;
    let name = arg_str(rt, c, 0)?.to_ascii_lowercase();
    with_dom(rt, |_, dom| set_style_prop(&mut dom.doc, n, &name, ""));
    Ok(Value::Undefined)
}

fn style_css_text(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 2)?;
    with_dom(rt, |_, dom| Ok(Value::str(dom.doc.element(n).and_then(|e| e.attr("style")).unwrap_or(""))))
}

fn style_set_css_text(rt: &mut Realm, c: &Call) -> JsResult {
    let n = list_node(rt, c, NODE + 2)?;
    let v = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| dom.doc.set_attr(n, "style", &v));
    Ok(Value::Undefined)
}

/// The CSS properties scripts commonly set through `el.style.x`.
const STYLE_PROPS: &[&str] = &[
    "alignContent",
    "alignItems",
    "alignSelf",
    "animation",
    "background",
    "backgroundColor",
    "backgroundImage",
    "backgroundPosition",
    "backgroundRepeat",
    "backgroundSize",
    "border",
    "borderBottom",
    "borderColor",
    "borderLeft",
    "borderRadius",
    "borderRight",
    "borderStyle",
    "borderTop",
    "borderWidth",
    "bottom",
    "boxShadow",
    "boxSizing",
    "clear",
    "color",
    "columnGap",
    "content",
    "cursor",
    "display",
    "flex",
    "flexBasis",
    "flexDirection",
    "flexGrow",
    "flexShrink",
    "flexWrap",
    "float",
    "font",
    "fontFamily",
    "fontSize",
    "fontStyle",
    "fontWeight",
    "gap",
    "gridTemplateColumns",
    "gridTemplateRows",
    "height",
    "justifyContent",
    "left",
    "letterSpacing",
    "lineHeight",
    "listStyle",
    "margin",
    "marginBottom",
    "marginLeft",
    "marginRight",
    "marginTop",
    "maxHeight",
    "maxWidth",
    "minHeight",
    "minWidth",
    "opacity",
    "order",
    "outline",
    "overflow",
    "overflowX",
    "overflowY",
    "padding",
    "paddingBottom",
    "paddingLeft",
    "paddingRight",
    "paddingTop",
    "pointerEvents",
    "position",
    "right",
    "rowGap",
    "textAlign",
    "textDecoration",
    "textOverflow",
    "textTransform",
    "top",
    "transform",
    "transition",
    "userSelect",
    "verticalAlign",
    "visibility",
    "whiteSpace",
    "width",
    "wordBreak",
    "wordWrap",
    "zIndex",
];

// ---------------------------------------------------------------- events

/// A new event object.
pub fn make_event(rt: &mut Realm, kind: &str, bubbles: bool, cancelable: bool) -> JsResult {
    let proto = with_dom(rt, |_, dom| dom.protos.event);
    let o = rt.alloc(Obj::new(proto, Kind::Ordinary));
    rt.define(o, "type", Value::str(kind), DEFAULT);
    rt.define(o, "bubbles", Value::Bool(bubbles), DEFAULT);
    rt.define(o, "cancelable", Value::Bool(cancelable), DEFAULT);
    rt.define(o, "defaultPrevented", Value::Bool(false), DEFAULT);
    rt.define(o, "target", Value::Null, DEFAULT);
    rt.define(o, "currentTarget", Value::Null, DEFAULT);
    rt.define(o, "eventPhase", Value::Number(0.0), DEFAULT);
    rt.define(o, "isTrusted", Value::Bool(true), DEFAULT);
    let started = with_dom(rt, |_, dom| dom.started);
    let now = rt.host.now_ms() - started;
    rt.define(o, "timeStamp", Value::Number(now), DEFAULT);
    Ok(Value::Object(o))
}

fn event_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(rt.type_error("Failed to construct 'Event': Please use the 'new' operator"));
    }
    let kind = arg_str(rt, c, 0)?;
    let (mut bubbles, mut cancelable) = (false, false);
    let mut detail = Value::Null;
    if let Value::Object(opts) = c.arg(1) {
        bubbles = rt.get_str(opts, "bubbles")?.truthy();
        cancelable = rt.get_str(opts, "cancelable")?.truthy();
        detail = rt.get_str(opts, "detail")?;
        if detail.is_undefined() {
            detail = Value::Null;
        }
    }
    let ev = make_event(rt, &kind, bubbles, cancelable)?;
    let Value::Object(o) = ev else { unreachable!() };
    rt.define(o, "isTrusted", Value::Bool(false), DEFAULT);
    rt.define(o, "detail", detail, DEFAULT);
    if let Value::Object(nt) = &c.new_target {
        let p = rt.get_str(*nt, "prototype")?;
        if let Value::Object(p) = p {
            rt.heap.get_mut(o).proto = Some(p);
        }
    }
    Ok(ev)
}

fn prevent_default(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.this {
        if rt.get_str(o, "cancelable")?.truthy() {
            rt.define(o, "defaultPrevented", Value::Bool(true), DEFAULT);
        }
    }
    Ok(Value::Undefined)
}

fn stop_propagation(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.this {
        let s = with_dom(rt, |_, dom| dom.stop_sym.unwrap());
        rt.define(o, s, Value::Bool(true), 0);
    }
    Ok(Value::Undefined)
}

fn stop_immediate(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.this {
        let (s, s2) = with_dom(rt, |_, dom| (dom.stop_sym.unwrap(), dom.stop_now_sym.unwrap()));
        rt.define(o, s, Value::Bool(true), 0);
        rt.define(o, s2, Value::Bool(true), 0);
    }
    Ok(Value::Undefined)
}

fn add_event_listener(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let kind = arg_str(rt, c, 0)?;
    let f = c.arg(1);
    if f.is_nullish() {
        return Ok(Value::Undefined);
    }
    let (capture, once) = match c.arg(2) {
        Value::Object(o) => (rt.get_str(o, "capture")?.truthy(), rt.get_str(o, "once")?.truthy()),
        v => (v.truthy(), false),
    };
    with_dom(rt, |_, dom| {
        let dup = dom
            .listeners
            .iter()
            .any(|l| l.target == n && l.kind == kind && l.f == f && l.capture == capture && !l.property);
        if !dup {
            dom.listeners.push(Listener { target: n, kind, f, capture, once, property: false });
        }
    });
    Ok(Value::Undefined)
}

fn remove_event_listener(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let kind = arg_str(rt, c, 0)?;
    let f = c.arg(1);
    let capture = match c.arg(2) {
        Value::Object(o) => rt.get_str(o, "capture")?.truthy(),
        v => v.truthy(),
    };
    with_dom(rt, |_, dom| {
        dom.listeners.retain(|l| !(l.target == n && l.kind == kind && l.f == f && l.capture == capture && !l.property))
    });
    Ok(Value::Undefined)
}

fn dispatch_event(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let ev = c.arg(0);
    if !matches!(ev, Value::Object(_)) {
        return Err(rt.type_error("parameter 1 is not of type 'Event'"));
    }
    let r = dispatch(rt, n, ev)?;
    Ok(Value::Bool(r))
}

/// `el.onclick` and friends (event type in slot 0).
fn handler_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let kind = rt.native_slots(c.callee)[0].clone();
    let kind = rt.to_rust_string(&kind)?;
    with_dom(rt, |_, dom| {
        Ok(dom
            .listeners
            .iter()
            .find(|l| l.target == n && l.kind == kind && l.property)
            .map(|l| l.f.clone())
            .unwrap_or(Value::Null))
    })
}

fn handler_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let kind = rt.native_slots(c.callee)[0].clone();
    let kind = rt.to_rust_string(&kind)?;
    let f = c.arg(0);
    let callable = rt.is_callable(&f);
    with_dom(rt, |_, dom| {
        dom.listeners.retain(|l| !(l.target == n && l.kind == kind && l.property));
        if callable {
            dom.listeners.push(Listener { target: n, kind, f, capture: false, once: false, property: true });
        }
    });
    Ok(Value::Undefined)
}

const HANDLERS: &[&str] = &[
    "click",
    "dblclick",
    "input",
    "change",
    "submit",
    "reset",
    "keydown",
    "keyup",
    "keypress",
    "load",
    "error",
    "focus",
    "blur",
    "mousedown",
    "mouseup",
    "mouseover",
    "mouseout",
    "mousemove",
    "mouseenter",
    "mouseleave",
    "scroll",
    "resize",
    "contextmenu",
    "wheel",
    "pointerdown",
    "pointerup",
    "pointermove",
    "touchstart",
    "touchend",
    "animationend",
    "transitionend",
    "hashchange",
    "popstate",
    "beforeunload",
    "unload",
    "DOMContentLoaded",
];

/// The listeners for one node during dispatch: (function, once, index, is_attr).
fn listeners_for(rt: &mut Realm, dom: &mut Dom, n: NodeId, kind: &str, capture: bool) -> Vec<(Value, bool, usize)> {
    let mut out: Vec<(Value, bool, usize)> = dom
        .listeners
        .iter()
        .enumerate()
        .filter(|(_, l)| l.target == n && l.kind == kind && (l.capture == capture || l.property && !capture))
        .map(|(i, l)| (l.f.clone(), l.once, i))
        .collect();
    // An `on…` attribute acts as a property handler.
    if !capture && n != WINDOW && !out.iter().any(|(_, _, i)| dom.listeners[*i].property) {
        if let Some(src) = dom.doc.element(n).and_then(|e| e.attr(&format!("on{kind}"))).map(String::from) {
            let key = (n, String::from(kind));
            let f = match dom.attr_handlers.get(&key) {
                Some((s, f)) if *s == src => Some(f.clone()),
                _ => {
                    let code = format!("(function (event) {{\n{src}\n}})");
                    match rt.eval(&code, "event handler") {
                        Ok(f) => {
                            dom.attr_handlers.insert(key, (src, f.clone()));
                            Some(f)
                        }
                        Err(e) => {
                            let msg = rt.describe_error(&e);
                            rt.host.console("error", &format!("Uncaught {msg}"));
                            None
                        }
                    }
                }
            };
            // Attributes were parsed before any script ran: they go first.
            if let Some(f) = f {
                out.insert(0, (f, false, usize::MAX));
            }
        }
    }
    out
}

/// Dispatches `ev` at `target`: capture, target and bubble phases. Returns
/// false if a listener called preventDefault().
pub fn dispatch(rt: &mut Realm, target: NodeId, ev: Value) -> Result<bool, Value> {
    let Value::Object(eo) = ev else { return Ok(true) };
    let kind = rt.get_str(eo, "type")?;
    let kind = rt.to_rust_string(&kind)?;
    let bubbles = rt.get_str(eo, "bubbles")?.truthy();
    let (stop, stop_now) = with_dom(rt, |_, dom| (dom.stop_sym.unwrap(), dom.stop_now_sym.unwrap()));
    // The propagation path: target, ancestors, then window.
    let path: Vec<NodeId> = with_dom(rt, |_, dom| {
        let mut p = vec![target];
        if target != WINDOW {
            let mut cur = dom.doc.parent(target);
            while let Some(x) = cur {
                p.push(x);
                cur = dom.doc.parent(x);
            }
            if dom.doc.is_connected(target) {
                p.push(WINDOW);
            }
        }
        p
    });
    let target_v =
        with_dom(rt, |rt, dom| if target == WINDOW { Value::Object(rt.global) } else { wrap(rt, dom, target) });
    rt.define(eo, "target", target_v, DEFAULT);
    let mut phases: Vec<(NodeId, bool, f64)> = Vec::new();
    for &n in path.iter().skip(1).rev() {
        phases.push((n, true, 1.0));
    }
    phases.push((target, true, 2.0));
    phases.push((target, false, 2.0));
    if bubbles {
        for &n in path.iter().skip(1) {
            phases.push((n, false, 3.0));
        }
    }
    for (n, capture, phase) in phases {
        if rt.get(eo, &PropKey::Sym(stop), ev.clone())?.truthy() {
            break;
        }
        let ls = with_dom(rt, |rt, dom| listeners_for(rt, dom, n, &kind, capture));
        if ls.is_empty() {
            continue;
        }
        let current = with_dom(rt, |rt, dom| if n == WINDOW { Value::Object(rt.global) } else { wrap(rt, dom, n) });
        rt.define(eo, "currentTarget", current.clone(), DEFAULT);
        rt.define(eo, "eventPhase", Value::Number(phase), DEFAULT);
        for (f, once, index) in ls {
            if once {
                with_dom(rt, |_, dom| {
                    if index < dom.listeners.len() {
                        let l = &dom.listeners[index];
                        let (t, k, func, c) = (l.target, l.kind.clone(), l.f.clone(), l.capture);
                        dom.listeners.retain(|l| !(l.target == t && l.kind == k && l.f == func && l.capture == c));
                    }
                });
            }
            let r = if rt.is_callable(&f) {
                let r = rt.call(&f, current.clone(), &[ev.clone()]);
                // `return false` from an on… handler cancels.
                if let Ok(Value::Bool(false)) = r {
                    if index == usize::MAX
                        || with_dom(rt, |_, dom| dom.listeners.get(index).is_some_and(|l| l.property))
                    {
                        rt.define(eo, "defaultPrevented", Value::Bool(true), DEFAULT);
                    }
                }
                r
            } else if let Value::Object(fo) = &f {
                match rt.get_str(*fo, "handleEvent") {
                    Ok(h) if rt.is_callable(&h) => rt.call(&h, f.clone(), &[ev.clone()]),
                    Ok(_) => Ok(Value::Undefined),
                    Err(e) => Err(e),
                }
            } else {
                Ok(Value::Undefined)
            };
            if let Err(e) = r {
                if rt.interrupted {
                    return Err(e);
                }
                let msg = rt.describe_error(&e);
                rt.host.console("error", &format!("Uncaught {msg}"));
            }
            rt.run_jobs();
            if rt.get(eo, &PropKey::Sym(stop_now), ev.clone())?.truthy() {
                break;
            }
        }
    }
    rt.define(eo, "currentTarget", Value::Null, DEFAULT);
    rt.define(eo, "eventPhase", Value::Number(0.0), DEFAULT);
    Ok(!rt.get_str(eo, "defaultPrevented")?.truthy())
}

// ---------------------------------------------------------------- document

fn create_element(rt: &mut Realm, c: &Call) -> JsResult {
    let tag = arg_str(rt, c, 0)?.to_ascii_lowercase();
    if tag.is_empty() || !tag.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
        return Err(rt.error(
            nebula_script::ErrorKind::Error,
            &format!("InvalidCharacterError: The tag name provided ('{tag}') is not a valid name."),
        ));
    }
    with_dom(rt, |rt, dom| {
        let n = dom.doc.create(NodeData::Element(Element { tag, attrs: Vec::new() }));
        Ok(wrap(rt, dom, n))
    })
}

fn create_element_ns(rt: &mut Realm, c: &Call) -> JsResult {
    let tag = arg_str(rt, c, 1)?.to_ascii_lowercase();
    let tag = tag.rsplit(':').next().unwrap_or("").to_string();
    with_dom(rt, |rt, dom| {
        let n = dom.doc.create(NodeData::Element(Element { tag, attrs: Vec::new() }));
        Ok(wrap(rt, dom, n))
    })
}

fn create_text_node(rt: &mut Realm, c: &Call) -> JsResult {
    let s = arg_str(rt, c, 0)?;
    with_dom(rt, |rt, dom| {
        let n = dom.doc.create(NodeData::Text(s));
        Ok(wrap(rt, dom, n))
    })
}

fn create_comment(rt: &mut Realm, _c: &Call) -> JsResult {
    // Comments aren't kept; an empty text node stands in.
    with_dom(rt, |rt, dom| {
        let n = dom.doc.create(NodeData::Text(String::new()));
        Ok(wrap(rt, dom, n))
    })
}

fn create_fragment(rt: &mut Realm, _c: &Call) -> JsResult {
    with_dom(rt, |rt, dom| {
        let n = dom.doc.create(NodeData::Element(Element { tag: String::from(FRAGMENT), attrs: Vec::new() }));
        Ok(wrap(rt, dom, n))
    })
}

fn create_event(rt: &mut Realm, _c: &Call) -> JsResult {
    let ev = make_event(rt, "", false, false)?;
    let Value::Object(o) = ev else { unreachable!() };
    let init = rt.native("initEvent", 3, init_event, false);
    rt.define(o, "initEvent", Value::Object(init), HIDDEN);
    Ok(ev)
}

fn init_event(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.this {
        rt.define(o, "type", c.arg(0), DEFAULT);
        rt.define(o, "bubbles", Value::Bool(c.arg(1).truthy()), DEFAULT);
        rt.define(o, "cancelable", Value::Bool(c.arg(2).truthy()), DEFAULT);
    }
    Ok(Value::Undefined)
}

/// The tag of a document made by `document.implementation.createHTMLDocument`
/// (a detached subtree acting as its own document).
const DOCUMENT: &str = "#document";

macro_rules! doc_find {
    ($name:ident, $tag:expr) => {
        fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            let this = node_of(rt, &c.this).unwrap_or(Document::ROOT);
            with_dom(rt, |rt, dom| {
                let n = dom.doc.descendants(this).into_iter().find(|x| dom.doc.tag(*x) == $tag);
                Ok(wrap_opt(rt, dom, n))
            })
        }
    };
}

fn create_html_document(rt: &mut Realm, c: &Call) -> JsResult {
    let title = if c.arg(0).is_undefined() { None } else { Some(arg_str(rt, c, 0)?) };
    with_dom(rt, |rt, dom| {
        let el = |dom: &mut Dom, tag: &str| {
            dom.doc.create(NodeData::Element(Element { tag: String::from(tag), attrs: Vec::new() }))
        };
        let d = el(dom, DOCUMENT);
        let html = el(dom, "html");
        let head = el(dom, "head");
        let body = el(dom, "body");
        dom.doc.insert(d, html, None);
        dom.doc.insert(html, head, None);
        dom.doc.insert(html, body, None);
        if let Some(t) = title {
            let te = el(dom, "title");
            dom.doc.insert(head, te, None);
            dom.doc.set_text_content(te, &t);
        }
        // It behaves as a document: wrap it with the Document prototype.
        let o = rt.alloc(Obj::new(dom.protos.document, Kind::Host(NODE, d as u64)));
        dom.wrappers.insert(d, o);
        Ok(Value::Object(o))
    })
}

fn has_feature(_rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::Bool(true))
}
doc_find!(doc_body, "body");
doc_find!(doc_head, "head");
doc_find!(doc_element, "html");

fn doc_all(rt: &mut Realm, c: &Call) -> JsResult {
    let tag = rt.native_slots(c.callee)[0].clone();
    let tag = rt.to_rust_string(&tag)?;
    with_dom(rt, |rt, dom| {
        let mut found = dom.doc.find_all(&tag);
        if tag == "a" {
            found.retain(|n| dom.doc.element(*n).is_some_and(|e| e.attr("href").is_some()));
        }
        Ok(list(rt, dom, found))
    })
}

fn title_get(rt: &mut Realm, _c: &Call) -> JsResult {
    with_dom(rt, |_, dom| {
        let t = dom.doc.find("title").map(|t| dom.doc.text_content(t)).unwrap_or_default();
        Ok(Value::str(t.split_whitespace().collect::<Vec<_>>().join(" ").as_str()))
    })
}

fn title_set(rt: &mut Realm, c: &Call) -> JsResult {
    let s = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| {
        let t = match dom.doc.find("title") {
            Some(t) => t,
            None => {
                let head = dom.doc.find("head").unwrap_or(Document::ROOT);
                let t = dom.doc.create(NodeData::Element(Element { tag: String::from("title"), attrs: Vec::new() }));
                dom.doc.insert(head, t, None);
                t
            }
        };
        dom.doc.set_text_content(t, &s);
    });
    Ok(Value::Undefined)
}

fn ready_state(rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::str(with_dom(rt, |_, dom| dom.ready_state)))
}

fn cookie_get(rt: &mut Realm, _c: &Call) -> JsResult {
    let s = with_dom(rt, |_, dom| dom.cookie.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "));
    Ok(Value::str(&s))
}

fn cookie_set(rt: &mut Realm, c: &Call) -> JsResult {
    let s = arg_str(rt, c, 0)?;
    let first = s.split(';').next().unwrap_or("");
    let expired =
        s.to_ascii_lowercase().contains("max-age=0") || s.to_ascii_lowercase().contains("expires=thu, 01 jan 1970");
    if let Some((k, v)) = first.split_once('=') {
        let (k, v) = (String::from(k.trim()), String::from(v.trim()));
        with_dom(rt, |_, dom| {
            if expired {
                dom.cookie.remove(&k);
            } else {
                dom.cookie.insert(k, v);
            }
        });
    }
    Ok(Value::Undefined)
}

fn active_element(rt: &mut Realm, _c: &Call) -> JsResult {
    with_dom(rt, |rt, dom| {
        let n = dom.focus.or_else(|| dom.doc.find("body"));
        Ok(wrap_opt(rt, dom, n))
    })
}

fn document_write(rt: &mut Realm, c: &Call) -> JsResult {
    let mut s = String::new();
    for i in 0..c.args.len() {
        s.push_str(&arg_str(rt, c, i)?);
    }
    with_dom(rt, |rt, dom| {
        let body = dom.doc.find("body").unwrap_or(Document::ROOT);
        for x in parse_into(&mut dom.doc, &s) {
            let _ = insert_checked(rt, dom, body, x, None);
        }
    });
    run_inserted_scripts(rt);
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------- window

fn add_timer(rt: &mut Realm, c: &Call, repeat: bool) -> JsResult {
    let f = c.arg(0);
    let delay = rt.to_number(&c.arg(1)).unwrap_or(0.0);
    let delay = if delay.is_finite() { delay.max(0.0) } else { 0.0 };
    let args: Vec<Value> = c.args.iter().skip(2).cloned().collect();
    let now = rt.host.now_ms();
    let id = with_dom(rt, |_, dom| {
        dom.next_id += 1;
        let id = dom.next_id;
        dom.timers.push(Timer { id, due: now + delay, every: repeat.then_some(delay.max(4.0)), f, args });
        id
    });
    Ok(Value::Number(id as f64))
}

fn set_timeout(rt: &mut Realm, c: &Call) -> JsResult {
    add_timer(rt, c, false)
}

fn set_interval(rt: &mut Realm, c: &Call) -> JsResult {
    add_timer(rt, c, true)
}

fn clear_timer(rt: &mut Realm, c: &Call) -> JsResult {
    let id = rt.to_number(&c.arg(0)).unwrap_or(0.0) as u32;
    with_dom(rt, |_, dom| dom.timers.retain(|t| t.id != id));
    Ok(Value::Undefined)
}

fn request_animation_frame(rt: &mut Realm, c: &Call) -> JsResult {
    let f = c.arg(0);
    let id = with_dom(rt, |_, dom| {
        dom.next_id += 1;
        dom.frames.push((dom.next_id, f));
        dom.next_id
    });
    Ok(Value::Number(id as f64))
}

fn cancel_animation_frame(rt: &mut Realm, c: &Call) -> JsResult {
    let id = rt.to_number(&c.arg(0)).unwrap_or(0.0) as u32;
    with_dom(rt, |_, dom| dom.frames.retain(|(i, _)| *i != id));
    Ok(Value::Undefined)
}

fn alert(rt: &mut Realm, c: &Call) -> JsResult {
    let s = if c.args.is_empty() { String::new() } else { arg_str(rt, c, 0)? };
    rt.host.console("info", &format!("[alert] {s}"));
    with_dom(rt, |_, dom| dom.alerts.push(s));
    Ok(Value::Undefined)
}

fn confirm(rt: &mut Realm, c: &Call) -> JsResult {
    alert(rt, c)?;
    Ok(Value::Bool(false))
}

fn prompt(rt: &mut Realm, c: &Call) -> JsResult {
    alert(rt, c)?;
    Ok(Value::Null)
}

fn inner_width(rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::Number(with_dom(rt, |_, dom| dom.viewport.0) as f64))
}

fn inner_height(rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::Number(with_dom(rt, |_, dom| dom.viewport.1) as f64))
}

fn performance_now(rt: &mut Realm, _c: &Call) -> JsResult {
    let started = with_dom(rt, |_, dom| dom.started);
    Ok(Value::Number(rt.host.now_ms() - started))
}

fn match_media(rt: &mut Realm, c: &Call) -> JsResult {
    let q = arg_str(rt, c, 0)?;
    let (w, h) = with_dom(rt, |_, dom| dom.viewport);
    let matches = css::media_matches(&q.to_ascii_lowercase(), w, h);
    let o = rt.new_object();
    rt.define(o, "matches", Value::Bool(matches), DEFAULT);
    rt.define(o, "media", Value::str(&q), DEFAULT);
    for m in ["addListener", "removeListener", "addEventListener", "removeEventListener"] {
        let f = rt.native(m, 1, noop, false);
        rt.define(o, m, Value::Object(f), HIDDEN);
    }
    Ok(Value::Object(o))
}

fn get_computed_style(rt: &mut Realm, c: &Call) -> JsResult {
    // The inline style stands in for the computed one.
    let n = arg_node(rt, c, 0)?;
    let proto = with_dom(rt, |_, dom| dom.protos.style);
    Ok(Value::Object(rt.alloc(Obj::new(proto, Kind::Host(NODE + 2, n as u64)))))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn btoa(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    if s.units().iter().any(|&u| u > 255) {
        return Err(rt.error(
            nebula_script::ErrorKind::Error,
            "InvalidCharacterError: The string to be encoded contains characters outside of the Latin1 range.",
        ));
    }
    let bytes: Vec<u8> = s.units().iter().map(|&u| u as u8).collect();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    Ok(Value::str(&out))
}

fn atob(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_rust_string(&c.arg(0))?;
    let clean: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=').collect();
    let mut bits = 0u32;
    let mut n = 0;
    let mut out: Vec<u16> = Vec::new();
    for b in clean {
        let Some(v) = B64.iter().position(|x| *x == b) else {
            return Err(rt.error(
                nebula_script::ErrorKind::Error,
                "InvalidCharacterError: The string to be decoded is not correctly encoded.",
            ));
        };
        bits = bits << 6 | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push(((bits >> n) & 0xFF) as u16);
        }
    }
    Ok(Value::String(nebula_script::JsStr::from_units(out)))
}

fn storage_area(rt: &mut Realm, c: &Call) -> bool {
    // Slot 0: true for localStorage.
    rt.native_slots(c.callee).first().is_some_and(|v| v.truthy())
}

fn storage_this(rt: &mut Realm, c: &Call) -> bool {
    matches!(&c.this, Value::Object(o) if matches!(rt.heap.get(*o).kind, Kind::Host(t, 1) if t == NODE + 3))
}

fn storage_get(rt: &mut Realm, c: &Call) -> JsResult {
    let local = storage_this(rt, c) || storage_area(rt, c);
    let k = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| {
        let m = if local { &dom.storage } else { &dom.session };
        Ok(m.get(&k).map(|v| Value::str(v)).unwrap_or(Value::Null))
    })
}

fn storage_set(rt: &mut Realm, c: &Call) -> JsResult {
    let local = storage_this(rt, c);
    let k = arg_str(rt, c, 0)?;
    let v = arg_str(rt, c, 1)?;
    with_dom(rt, |_, dom| {
        if local {
            dom.storage.insert(k, v);
            dom.storage_dirty = true;
        } else {
            dom.session.insert(k, v);
        }
    });
    Ok(Value::Undefined)
}

fn storage_remove(rt: &mut Realm, c: &Call) -> JsResult {
    let local = storage_this(rt, c);
    let k = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| {
        if local {
            dom.storage.remove(&k);
            dom.storage_dirty = true;
        } else {
            dom.session.remove(&k);
        }
    });
    Ok(Value::Undefined)
}

fn storage_clear(rt: &mut Realm, c: &Call) -> JsResult {
    let local = storage_this(rt, c);
    with_dom(rt, |_, dom| {
        if local {
            dom.storage.clear();
            dom.storage_dirty = true;
        } else {
            dom.session.clear();
        }
    });
    Ok(Value::Undefined)
}

fn storage_key(rt: &mut Realm, c: &Call) -> JsResult {
    let local = storage_this(rt, c);
    let i = rt.to_number(&c.arg(0))? as usize;
    with_dom(rt, |_, dom| {
        let m = if local { &dom.storage } else { &dom.session };
        Ok(m.keys().nth(i).map(|k| Value::str(k)).unwrap_or(Value::Null))
    })
}

fn storage_length(rt: &mut Realm, c: &Call) -> JsResult {
    let local = storage_this(rt, c);
    with_dom(rt, |_, dom| Ok(Value::Number(if local { dom.storage.len() } else { dom.session.len() } as f64)))
}

// location

fn location_part(rt: &mut Realm, c: &Call) -> JsResult {
    let part = rt.native_slots(c.callee)[0].clone();
    let part = rt.to_rust_string(&part)?;
    let url = with_dom(rt, |_, dom| dom.url.clone());
    Ok(Value::str(&url_part(&url, &part)))
}

/// A part of a URL, as `location` reports it.
fn url_part(url: &str, part: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let (hostport, path_etc) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (path_q, hash) = match path_etc.find('#') {
        Some(i) => (&path_etc[..i], &path_etc[i..]),
        None => (path_etc, ""),
    };
    let (path, search) = match path_q.find('?') {
        Some(i) => (&path_q[..i], &path_q[i..]),
        None => (path_q, ""),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, p),
        _ => (hostport, ""),
    };
    match part {
        "href" => String::from(url),
        "protocol" => format!("{scheme}:"),
        "host" => String::from(hostport),
        "hostname" => String::from(host),
        "port" => String::from(port),
        "pathname" => String::from(path),
        "search" => String::from(if search == "?" { "" } else { search }),
        "hash" => String::from(if hash == "#" { "" } else { hash }),
        "origin" => {
            if scheme.is_empty() {
                String::from("null")
            } else {
                format!("{scheme}://{hostport}")
            }
        }
        _ => String::new(),
    }
}

fn navigate_to(rt: &mut Realm, c: &Call) -> JsResult {
    let target = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| {
        let resolved = if target.starts_with('#') {
            let base = dom.url.split('#').next().unwrap_or("").to_string();
            format!("{base}{target}")
        } else {
            target
        };
        dom.navigate = Some(resolved);
    });
    Ok(Value::Undefined)
}

fn reload(rt: &mut Realm, _c: &Call) -> JsResult {
    with_dom(rt, |_, dom| dom.navigate = Some(dom.url.clone()));
    Ok(Value::Undefined)
}

fn history_push(rt: &mut Realm, c: &Call) -> JsResult {
    // Updates the address without loading (the URL argument, if any).
    if c.args.len() > 2 && !c.arg(2).is_nullish() {
        let u = arg_str(rt, c, 2)?;
        with_dom(rt, |_, dom| {
            if u.contains("://") {
                dom.url = u;
            } else if let Some(stripped) = u.strip_prefix('?') {
                let base = dom.url.split(['?', '#']).next().unwrap_or("").to_string();
                dom.url = format!("{base}?{stripped}");
            } else if u.starts_with('#') {
                let base = dom.url.split('#').next().unwrap_or("").to_string();
                dom.url = format!("{base}{u}");
            } else if u.starts_with('/') {
                let origin = url_part(&dom.url, "origin");
                dom.url = format!("{origin}{u}");
            }
        });
    }
    Ok(Value::Undefined)
}

fn history_back(rt: &mut Realm, _c: &Call) -> JsResult {
    with_dom(rt, |_, dom| dom.navigate = Some(String::from("about:back")));
    Ok(Value::Undefined)
}

fn illegal_constructor(rt: &mut Realm, _c: &Call) -> JsResult {
    Err(rt.type_error("Illegal constructor"))
}

// ---------------------------------------------------------------- setup

fn accessor(rt: &mut Realm, o: ObjRef, name: &str, get: NativeFn, set: Option<NativeFn>) {
    let g = rt.native(&format!("get {name}"), 0, get, false);
    let s = match set {
        Some(f) => Value::Object(rt.native(&format!("set {name}"), 1, f, false)),
        None => Value::Undefined,
    };
    rt.define_accessor(o, name, Value::Object(g), s, CONFIGURABLE | nebula_script::object::ENUMERABLE);
}

fn accessor_with(rt: &mut Realm, o: ObjRef, name: &str, slot: Value, get: NativeFn, set: Option<NativeFn>) {
    let g = rt.native_with(&format!("get {name}"), 0, get, false, vec![slot.clone()]);
    let s = match set {
        Some(f) => Value::Object(rt.native_with(&format!("set {name}"), 1, f, false, vec![slot])),
        None => Value::Undefined,
    };
    rt.define_accessor(o, name, Value::Object(g), s, CONFIGURABLE | nebula_script::object::ENUMERABLE);
}

fn methods(rt: &mut Realm, o: ObjRef, list: &[(&str, u32, NativeFn)]) {
    for (name, len, f) in list {
        rt.method(o, name, *len, *f);
    }
}

/// An interface object (for `instanceof` and `X.prototype`): constructor + prototype.
fn interface(rt: &mut Realm, name: &str, proto: ObjRef, parent: Option<ObjRef>) {
    let c = rt.native(name, 0, illegal_constructor, true);
    rt.define(c, "prototype", Value::Object(proto), 0);
    rt.define(proto, "constructor", Value::Object(c), HIDDEN);
    if let Some(p) = parent {
        rt.heap.get_mut(proto).proto = Some(p);
    }
    nebula_script::builtins::to_string_tag(rt, proto, name);
    rt.set_global(name, Value::Object(c));
}

/// Gives a realm a DOM for `doc` (which is then moved in and out around
/// each script run by the page).
pub fn install(rt: &mut Realm, url: &str, viewport: (i32, i32), storage: BTreeMap<String, String>) {
    let started = rt.host.now_ms();
    let stop_sym = rt.new_private(nebula_script::JsStr::from("stop"));
    let stop_now_sym = rt.new_private(nebula_script::JsStr::from("stopNow"));
    rt.embedder = Some(Box::new(Dom {
        doc: Document::default(),
        wrappers: BTreeMap::new(),
        listeners: Vec::new(),
        attr_handlers: BTreeMap::new(),
        timers: Vec::new(),
        frames: Vec::new(),
        next_id: 0,
        protos: Protos::default(),
        stop_sym: Some(stop_sym),
        stop_now_sym: Some(stop_now_sym),
        url: String::from(url),
        viewport,
        ready_state: "loading",
        navigate: None,
        storage,
        storage_dirty: false,
        session: BTreeMap::new(),
        cookie: BTreeMap::new(),
        focus: None,
        alerts: Vec::new(),
        started,
        inserted_scripts: Vec::new(),
        started_scripts: alloc::collections::BTreeSet::new(),
        fetches: Vec::new(),
        geometry: BTreeMap::new(),
        page_size: viewport,
        scroll: (0, 0),
        scroll_request: None,
        requests: Vec::new(),
        pending_requests: BTreeMap::new(),
    }));

    let op = rt.intr.object_proto;
    let event_target = rt.new_object_with(Some(op));
    interface(rt, "EventTarget", event_target, None);
    methods(
        rt,
        event_target,
        &[
            ("addEventListener", 2, add_event_listener),
            ("removeEventListener", 2, remove_event_listener),
            ("dispatchEvent", 1, dispatch_event),
        ],
    );

    // Node.
    let node = rt.new_object_with(Some(event_target));
    interface(rt, "Node", node, None);
    for (name, v) in
        [("ELEMENT_NODE", 1.0), ("TEXT_NODE", 3.0), ("DOCUMENT_NODE", 9.0), ("DOCUMENT_FRAGMENT_NODE", 11.0)]
    {
        rt.define(node, name, Value::Number(v), 0);
    }
    accessor(rt, node, "nodeType", node_type, None);
    accessor(rt, node, "nodeName", node_name, None);
    accessor(rt, node, "textContent", text_content, Some(set_text_content));
    accessor(rt, node, "parentNode", parent_node, None);
    accessor(rt, node, "parentElement", parent_element, None);
    accessor(rt, node, "childNodes", child_nodes, None);
    accessor(rt, node, "firstChild", first_child, None);
    accessor(rt, node, "lastChild", last_child, None);
    accessor(rt, node, "nextSibling", next_sibling, None);
    accessor(rt, node, "previousSibling", previous_sibling, None);
    accessor(rt, node, "isConnected", is_connected, None);
    accessor(rt, node, "ownerDocument", owner_document, None);
    methods(
        rt,
        node,
        &[
            ("appendChild", 1, append_child),
            ("insertBefore", 2, insert_before),
            ("removeChild", 1, remove_child),
            ("replaceChild", 2, replace_child),
            ("cloneNode", 0, clone_node),
            ("contains", 1, contains),
            ("hasChildNodes", 0, has_child_nodes),
        ],
    );

    // ParentNode / ChildNode members shared by elements, documents and fragments.
    let parent_members = |rt: &mut Realm, o: ObjRef| {
        accessor(rt, o, "children", children, None);
        accessor(rt, o, "firstElementChild", first_element_child, None);
        accessor(rt, o, "lastElementChild", last_element_child, None);
        accessor(rt, o, "childElementCount", child_element_count, None);
        methods(
            rt,
            o,
            &[
                ("querySelector", 1, query_selector),
                ("querySelectorAll", 1, query_selector_all),
                ("getElementsByTagName", 1, by_tag),
                ("getElementsByClassName", 1, by_class),
                ("append", 0, append),
                ("prepend", 0, prepend),
            ],
        );
    };

    // Element and HTMLElement.
    let element = rt.new_object_with(Some(node));
    interface(rt, "Element", element, None);
    parent_members(rt, element);
    accessor(rt, element, "tagName", tag_name, None);
    accessor(rt, element, "localName", node_name, None);
    accessor(rt, element, "innerHTML", inner_html, Some(set_inner_html));
    accessor(rt, element, "outerHTML", outer_html, Some(set_outer_html));
    accessor(rt, element, "nextElementSibling", next_element_sibling, None);
    accessor(rt, element, "previousElementSibling", previous_element_sibling, None);
    accessor(rt, element, "classList", class_list, None);
    accessor(rt, element, "style", style_obj, None);
    accessor(rt, element, "dataset", dataset, None);
    for (name, attr) in [("id", "id"), ("className", "class"), ("slot", "slot")] {
        accessor_with(rt, element, name, Value::str(attr), reflect_get, Some(reflect_set));
    }
    methods(
        rt,
        element,
        &[
            ("getAttribute", 1, get_attribute),
            ("setAttribute", 2, set_attribute),
            ("removeAttribute", 1, remove_attribute),
            ("hasAttribute", 1, has_attribute),
            ("toggleAttribute", 1, toggle_attribute),
            ("getAttributeNames", 0, attribute_names),
            ("matches", 1, matches_fn),
            ("closest", 1, closest),
            ("remove", 0, remove),
            ("before", 0, before),
            ("after", 0, after),
            ("replaceWith", 0, replace_with),
            ("insertAdjacentHTML", 2, insert_adjacent_html),
            ("insertAdjacentElement", 2, insert_adjacent_element),
            ("insertAdjacentText", 2, insert_adjacent_text),
            ("getBoundingClientRect", 0, rect),
            ("getClientRects", 0, client_rects),
            ("scrollIntoView", 0, scroll_into_view),
            ("scrollTo", 0, noop),
            ("setPointerCapture", 1, noop),
            ("releasePointerCapture", 1, noop),
        ],
    );
    let html_element = rt.new_object_with(Some(element));
    interface(rt, "HTMLElement", html_element, None);
    accessor(rt, html_element, "innerText", text_content, Some(set_text_content));
    accessor(rt, html_element, "value", value_get, Some(value_set));
    accessor(rt, html_element, "checked", checked_get, Some(checked_set));
    accessor(rt, html_element, "selectedIndex", selected_index_get, Some(selected_index_set));
    accessor(rt, html_element, "options", options, None);
    accessor(rt, html_element, "form", form_of, None);
    for attr in [
        "href",
        "src",
        "alt",
        "title",
        "type",
        "name",
        "placeholder",
        "action",
        "method",
        "target",
        "rel",
        "lang",
        "dir",
        "htmlFor",
        "role",
    ] {
        let real = if attr == "htmlFor" { "for" } else { attr };
        accessor_with(rt, html_element, attr, Value::str(real), reflect_get, Some(reflect_set));
    }
    for attr in ["hidden", "disabled", "readOnly", "required", "multiple", "autofocus", "selected", "open"] {
        accessor_with(
            rt,
            html_element,
            attr,
            Value::str(&attr.to_ascii_lowercase()),
            reflect_bool_get,
            Some(reflect_bool_set),
        );
    }
    for name in [
        "offsetWidth",
        "offsetHeight",
        "offsetTop",
        "offsetLeft",
        "clientWidth",
        "clientHeight",
        "scrollTop",
        "scrollLeft",
        "scrollWidth",
        "scrollHeight",
    ] {
        accessor_with(rt, html_element, name, Value::str(name), measure, None);
    }
    methods(rt, html_element, &[("click", 0, click), ("focus", 0, focus), ("blur", 0, blur)]);
    for h in HANDLERS {
        accessor_with(
            rt,
            html_element,
            &format!("on{}", h.to_ascii_lowercase()),
            Value::str(h),
            handler_get,
            Some(handler_set),
        );
    }

    // Text, fragments, the document.
    let text = rt.new_object_with(Some(node));
    interface(rt, "Text", text, None);
    accessor(rt, text, "data", text_content, Some(set_text_content));
    accessor(rt, text, "nodeValue", text_content, Some(set_text_content));
    methods(
        rt,
        text,
        &[("remove", 0, remove), ("before", 0, before), ("after", 0, after), ("replaceWith", 0, replace_with)],
    );
    let fragment = rt.new_object_with(Some(node));
    interface(rt, "DocumentFragment", fragment, None);
    parent_members(rt, fragment);
    methods(rt, fragment, &[("getElementById", 1, get_element_by_id)]);
    let document = rt.new_object_with(Some(node));
    interface(rt, "Document", document, None);
    parent_members(rt, document);
    methods(
        rt,
        document,
        &[
            ("getElementById", 1, get_element_by_id),
            ("createElement", 1, create_element),
            ("createElementNS", 2, create_element_ns),
            ("createTextNode", 1, create_text_node),
            ("createComment", 1, create_comment),
            ("createDocumentFragment", 0, create_fragment),
            ("createEvent", 1, create_event),
            ("write", 1, document_write),
            ("writeln", 1, document_write),
            ("hasFocus", 0, noop),
        ],
    );
    accessor(rt, document, "body", doc_body, None);
    accessor(rt, document, "head", doc_head, None);
    accessor(rt, document, "documentElement", doc_element, None);
    accessor(rt, document, "title", title_get, Some(title_set));
    accessor(rt, document, "readyState", ready_state, None);
    accessor(rt, document, "cookie", cookie_get, Some(cookie_set));
    accessor(rt, document, "activeElement", active_element, None);
    for (name, tag) in [("forms", "form"), ("images", "img"), ("links", "a"), ("scripts", "script")] {
        accessor_with(rt, document, name, Value::str(tag), doc_all, None);
    }
    let implementation = rt.new_object();
    rt.method(implementation, "createHTMLDocument", 1, create_html_document);
    rt.method(implementation, "hasFeature", 0, has_feature);
    rt.define(document, "implementation", Value::Object(implementation), HIDDEN);
    rt.define(document, "visibilityState", Value::str("visible"), HIDDEN);
    rt.define(document, "hidden", Value::Bool(false), HIDDEN);
    for h in HANDLERS {
        accessor_with(
            rt,
            document,
            &format!("on{}", h.to_ascii_lowercase()),
            Value::str(h),
            handler_get,
            Some(handler_set),
        );
    }

    // Events.
    let event = rt.new_object_with(Some(op));
    let ev_ctor = rt.native("Event", 1, event_ctor, true);
    rt.define(ev_ctor, "prototype", Value::Object(event), 0);
    rt.define(event, "constructor", Value::Object(ev_ctor), HIDDEN);
    rt.set_global("Event", Value::Object(ev_ctor));
    methods(
        rt,
        event,
        &[
            ("preventDefault", 0, prevent_default),
            ("stopPropagation", 0, stop_propagation),
            ("stopImmediatePropagation", 0, stop_immediate),
        ],
    );
    for (name, parent) in [
        ("CustomEvent", ev_ctor),
        ("KeyboardEvent", ev_ctor),
        ("MouseEvent", ev_ctor),
        ("InputEvent", ev_ctor),
        ("FocusEvent", ev_ctor),
        ("UIEvent", ev_ctor),
    ] {
        let proto = rt.new_object_with(Some(event));
        let c = rt.native(name, 1, event_ctor, true);
        rt.heap.get_mut(c).proto = Some(parent);
        rt.define(c, "prototype", Value::Object(proto), 0);
        rt.define(proto, "constructor", Value::Object(c), HIDDEN);
        rt.set_global(name, Value::Object(c));
    }

    // classList and style objects.
    let cl = rt.new_object_with(Some(op));
    methods(
        rt,
        cl,
        &[
            ("add", 1, cl_add),
            ("remove", 1, cl_remove),
            ("toggle", 1, cl_toggle),
            ("contains", 1, cl_contains),
            ("replace", 2, cl_replace),
            ("item", 1, cl_item),
            ("toString", 0, cl_value),
        ],
    );
    accessor(rt, cl, "length", cl_length, None);
    accessor(rt, cl, "value", cl_value, None);
    let style = rt.new_object_with(Some(op));
    methods(
        rt,
        style,
        &[
            ("setProperty", 2, style_set_property),
            ("getPropertyValue", 1, style_get_property),
            ("removeProperty", 1, style_remove_property),
        ],
    );
    accessor(rt, style, "cssText", style_css_text, Some(style_set_css_text));
    for p in STYLE_PROPS {
        accessor_with(rt, style, p, Value::str(&camel_to_kebab(p)), style_prop_get, Some(style_prop_set));
    }

    // Storage.
    let storage_proto = rt.new_object_with(Some(op));
    methods(
        rt,
        storage_proto,
        &[
            ("getItem", 1, storage_get),
            ("setItem", 2, storage_set),
            ("removeItem", 1, storage_remove),
            ("clear", 0, storage_clear),
            ("key", 1, storage_key),
        ],
    );
    accessor(rt, storage_proto, "length", storage_length, None);

    with_dom(rt, |_, dom| {
        dom.protos = Protos {
            node: Some(node),
            element: Some(element),
            html_element: Some(html_element),
            text: Some(text),
            document: Some(document),
            fragment: Some(fragment),
            event: Some(event),
            style: Some(style),
            class_list: Some(cl),
            storage: Some(storage_proto),
        };
    });

    // window.
    let g = rt.global;
    rt.heap.get_mut(g).proto = Some(event_target);
    for name in ["window", "self", "frames", "parent", "top"] {
        rt.define(g, name, Value::Object(g), HIDDEN);
    }
    let doc_v = with_dom(rt, |rt, dom| wrap(rt, dom, Document::ROOT));
    rt.define(g, "document", doc_v.clone(), nebula_script::object::ENUMERABLE);
    methods(
        rt,
        g,
        &[
            ("setTimeout", 2, set_timeout),
            ("setInterval", 2, set_interval),
            ("clearTimeout", 1, clear_timer),
            ("clearInterval", 1, clear_timer),
            ("requestAnimationFrame", 1, request_animation_frame),
            ("cancelAnimationFrame", 1, cancel_animation_frame),
            ("alert", 1, alert),
            ("confirm", 1, confirm),
            ("prompt", 1, prompt),
            ("matchMedia", 1, match_media),
            ("getComputedStyle", 1, get_computed_style),
            ("scroll", 2, scroll_to),
            ("focus", 0, noop),
            ("blur", 0, noop),
            ("btoa", 1, btoa),
            ("atob", 1, atob),
        ],
    );
    accessor(rt, g, "innerWidth", inner_width, None);
    accessor(rt, g, "innerHeight", inner_height, None);
    accessor(rt, g, "outerWidth", inner_width, None);
    accessor(rt, g, "outerHeight", inner_height, None);
    for (name, y) in [("scrollX", false), ("scrollY", true), ("pageXOffset", false), ("pageYOffset", true)] {
        accessor_with(rt, g, name, Value::Bool(y), scroll_xy, None);
    }
    for name in ["screenX", "screenY"] {
        accessor(rt, g, name, zero, None);
    }
    let to = rt.native_with("scrollTo", 2, scroll_to, false, vec![Value::Bool(false)]);
    rt.define(g, "scrollTo", Value::Object(to), HIDDEN);
    let by = rt.native_with("scrollBy", 2, scroll_to, false, vec![Value::Bool(true)]);
    rt.define(g, "scrollBy", Value::Object(by), HIDDEN);
    rt.define(g, "devicePixelRatio", Value::Number(1.0), DEFAULT);
    for h in HANDLERS {
        accessor_with(rt, g, &format!("on{}", h.to_ascii_lowercase()), Value::str(h), handler_get, Some(handler_set));
    }

    let local = rt.alloc(Obj::new(Some(storage_proto), Kind::Host(NODE + 3, 1)));
    rt.define(g, "localStorage", Value::Object(local), HIDDEN);
    let session = rt.alloc(Obj::new(Some(storage_proto), Kind::Host(NODE + 3, 0)));
    rt.define(g, "sessionStorage", Value::Object(session), HIDDEN);

    let perf = rt.new_object();
    rt.method(perf, "now", 0, performance_now);
    rt.define(perf, "timeOrigin", Value::Number(started), DEFAULT);
    rt.set_global("performance", Value::Object(perf));

    let nav = rt.new_object();
    for (k, v) in [
        ("userAgent", "Mozilla/5.0 (X11; WaveOS Aurora x86_64) Nebula/0.8 Pulsar/0.8"),
        ("appName", "Netscape"),
        ("appVersion", "5.0 (WaveOS Aurora)"),
        ("platform", "WaveOS"),
        ("language", "en-CA"),
        ("vendor", "WaveOS"),
        ("product", "Gecko"),
    ] {
        rt.define(nav, k, Value::str(v), DEFAULT);
    }
    let langs = rt.array_from(vec![Value::str("en-CA"), Value::str("en"), Value::str("fr-CA")]);
    rt.define(nav, "languages", langs, DEFAULT);
    rt.define(nav, "onLine", Value::Bool(true), DEFAULT);
    rt.define(nav, "cookieEnabled", Value::Bool(true), DEFAULT);
    rt.define(nav, "hardwareConcurrency", Value::Number(4.0), DEFAULT);
    rt.define(nav, "maxTouchPoints", Value::Number(0.0), DEFAULT);
    rt.set_global("navigator", Value::Object(nav));

    let screen = rt.new_object();
    rt.define(screen, "width", Value::Number(viewport.0 as f64), DEFAULT);
    rt.define(screen, "height", Value::Number(viewport.1 as f64), DEFAULT);
    rt.define(screen, "colorDepth", Value::Number(24.0), DEFAULT);
    rt.set_global("screen", Value::Object(screen));

    let loc = rt.new_object();
    for part in ["href", "protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin"] {
        let setter: Option<NativeFn> = if part == "href" { Some(navigate_to) } else { None };
        accessor_with(rt, loc, part, Value::str(part), location_part, setter);
    }
    methods(rt, loc, &[("assign", 1, navigate_to), ("replace", 1, navigate_to), ("reload", 0, reload)]);
    let ts = rt.native_with("toString", 0, location_part, false, vec![Value::str("href")]);
    rt.define(loc, "toString", Value::Object(ts), HIDDEN);
    rt.set_global("location", Value::Object(loc));
    if let Value::Object(d) = doc_v {
        rt.define(d, "location", Value::Object(loc), HIDDEN);
        rt.define(d, "defaultView", Value::Object(g), HIDDEN);
        accessor_with(rt, d, "URL", Value::str("href"), location_part, None);
        accessor_with(rt, d, "domain", Value::str("hostname"), location_part, None);
        rt.define(d, "referrer", Value::str(""), HIDDEN);
        rt.define(d, "characterSet", Value::str("UTF-8"), HIDDEN);
        rt.define(d, "compatMode", Value::str("CSS1Compat"), HIDDEN);
    }

    let history = rt.new_object();
    methods(
        rt,
        history,
        &[
            ("pushState", 3, history_push),
            ("replaceState", 3, history_push),
            ("back", 0, history_back),
            ("forward", 0, noop),
            ("go", 0, noop),
        ],
    );
    rt.define(history, "length", Value::Number(1.0), DEFAULT);
    rt.define(history, "state", Value::Null, DEFAULT);
    rt.set_global("history", Value::Object(history));

    let rnd = rt.native("__nebula_random", 1, native_random, false);
    rt.define(g, "__nebula_random", Value::Object(rnd), 0);
    let req = rt.native("__nebula_request", 4, native_request, false);
    rt.define(g, "__nebula_request", Value::Object(req), 0);

    // Web APIs written in JavaScript.
    if let Err(e) = rt.eval(PRELUDE, "prelude.js") {
        let msg = rt.describe_error(&e);
        rt.host.console("error", &format!("prelude: {msg}"));
    }
}

const PRELUDE: &str = include_str!("js/prelude.js");

// ---------------------------------------------------------------- the event loop

/// Runs due timers (only those due when called, so a 0 ms timer that
/// re-arms itself can't spin forever). Returns whether any ran.
pub fn fire_timers(rt: &mut Realm, now: f64) -> bool {
    let due: Vec<u32> = with_dom(rt, |_, dom| {
        let mut d: Vec<(f64, u32)> = dom.timers.iter().filter(|t| t.due <= now).map(|t| (t.due, t.id)).collect();
        d.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        d.into_iter().map(|(_, id)| id).collect()
    });
    let mut ran = false;
    for id in due {
        let t = with_dom(rt, |_, dom| {
            let i = dom.timers.iter().position(|t| t.id == id)?;
            let t = &mut dom.timers[i];
            let job = (t.f.clone(), t.args.clone());
            match t.every {
                Some(every) => t.due = now + every,
                None => {
                    dom.timers.remove(i);
                }
            }
            Some(job)
        });
        let Some((f, args)) = t else { continue };
        ran = true;
        let r = if rt.is_callable(&f) {
            rt.call(&f, Value::Undefined, &args)
        } else {
            match rt.to_rust_string(&f) {
                Ok(code) => rt.eval(&code, "timer"),
                Err(e) => Err(e),
            }
        };
        report(rt, r);
        rt.run_jobs();
        if rt.interrupted {
            break;
        }
    }
    ran
}

/// Runs requestAnimationFrame callbacks (one frame's worth).
pub fn fire_frames(rt: &mut Realm, now: f64) -> bool {
    let frames = with_dom(rt, |_, dom| core::mem::take(&mut dom.frames));
    let started = with_dom(rt, |_, dom| dom.started);
    let ran = !frames.is_empty();
    for (_, f) in frames {
        let r = rt.call(&f, Value::Undefined, &[Value::Number(now - started)]);
        report(rt, r);
        rt.run_jobs();
    }
    ran
}

/// When the next timer is due, and whether animation frames are waiting.
pub fn pending(rt: &mut Realm) -> (Option<f64>, bool) {
    with_dom(rt, |_, dom| {
        let next = dom.timers.iter().map(|t| t.due).fold(None, |m: Option<f64>, d| Some(m.map_or(d, |m| m.min(d))));
        (next, !dom.frames.is_empty())
    })
}

pub fn report(rt: &mut Realm, r: JsResult) {
    if let Err(e) = r {
        let msg = rt.describe_error(&e);
        rt.host.console("error", &format!("Uncaught {msg}"));
    }
}

/// Fires an event with no extra fields (load, DOMContentLoaded, input…).
pub fn fire(rt: &mut Realm, target: NodeId, kind: &str, bubbles: bool, cancelable: bool) -> bool {
    let ev = match make_event(rt, kind, bubbles, cancelable) {
        Ok(e) => e,
        Err(_) => return true,
    };
    match dispatch(rt, target, ev) {
        Ok(r) => r,
        Err(e) => {
            report(rt, Err(e));
            true
        }
    }
}

pub const WINDOW_TARGET: NodeId = WINDOW;

/// A keyboard event at the focused element (or the body).
pub fn fire_key(
    rt: &mut Realm,
    target: NodeId,
    kind: &str,
    key: &str,
    code: &str,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> bool {
    let Ok(ev) = make_event(rt, kind, true, true) else { return true };
    let Value::Object(o) = ev else { return true };
    rt.define(o, "key", Value::str(key), DEFAULT);
    rt.define(o, "code", Value::str(code), DEFAULT);
    rt.define(o, "ctrlKey", Value::Bool(ctrl), DEFAULT);
    rt.define(o, "shiftKey", Value::Bool(shift), DEFAULT);
    rt.define(o, "altKey", Value::Bool(alt), DEFAULT);
    rt.define(o, "metaKey", Value::Bool(false), DEFAULT);
    rt.define(o, "repeat", Value::Bool(false), DEFAULT);
    let kc = match key {
        "Enter" => 13.0,
        "Escape" => 27.0,
        "Backspace" => 8.0,
        "Tab" => 9.0,
        " " => 32.0,
        "ArrowLeft" => 37.0,
        "ArrowUp" => 38.0,
        "ArrowRight" => 39.0,
        "ArrowDown" => 40.0,
        k if k.chars().count() == 1 => k.chars().next().unwrap().to_ascii_uppercase() as u32 as f64,
        _ => 0.0,
    };
    rt.define(o, "keyCode", Value::Number(kc), DEFAULT);
    rt.define(o, "which", Value::Number(kc), DEFAULT);
    match dispatch(rt, target, ev) {
        Ok(r) => r,
        Err(e) => {
            report(rt, Err(e));
            true
        }
    }
}

/// A mouse event (click etc.) at `target`, with page coordinates.
pub fn fire_mouse(rt: &mut Realm, target: NodeId, kind: &str, x: i32, y: i32) -> bool {
    let Ok(ev) = make_event(rt, kind, true, true) else { return true };
    let Value::Object(o) = ev else { return true };
    for (k, v) in [
        ("clientX", x),
        ("clientY", y),
        ("pageX", x),
        ("pageY", y),
        ("offsetX", 0),
        ("offsetY", 0),
        ("screenX", x),
        ("screenY", y),
    ] {
        rt.define(o, k, Value::Number(v as f64), DEFAULT);
    }
    rt.define(o, "button", Value::Number(0.0), DEFAULT);
    rt.define(o, "buttons", Value::Number(1.0), DEFAULT);
    rt.define(o, "detail", Value::Number(1.0), DEFAULT);
    for k in ["ctrlKey", "shiftKey", "altKey", "metaKey"] {
        rt.define(o, k, Value::Bool(false), DEFAULT);
    }
    match dispatch(rt, target, ev) {
        Ok(r) => r,
        Err(e) => {
            report(rt, Err(e));
            true
        }
    }
}

/// Runs an external script the browser fetched for an inserted element,
/// then fires `load` (or `error` when `source` is None) at it.
pub fn run_fetched(rt: &mut Realm, node: NodeId, name: &str, source: Option<&str>) {
    match source {
        Some(code) => {
            let r = rt.eval(code, name);
            report(rt, r);
            rt.run_jobs();
            fire(rt, node, "load", false, false);
        }
        None => {
            fire(rt, node, "error", false, false);
        }
    }
}

/// `__nebula_request(method, url, [[name, value]…], body)`: a promise of
/// `{ status, statusText, url, headers, body }` (the prelude's `fetch` and
/// `XMLHttpRequest` build on it).
fn native_request(rt: &mut Realm, c: &Call) -> JsResult {
    let method = arg_str(rt, c, 0)?.to_ascii_uppercase();
    let url = arg_str(rt, c, 1)?;
    let mut headers = Vec::new();
    if let Value::Object(_) = c.arg(2) {
        for pair in rt.list_from_array_like(&c.arg(2))? {
            let kv = rt.list_from_array_like(&pair)?;
            if kv.len() == 2 {
                headers.push((rt.to_rust_string(&kv[0])?, rt.to_rust_string(&kv[1])?));
            }
        }
    }
    let body = if c.arg(3).is_nullish() { String::new() } else { arg_str(rt, c, 3)? };
    // A promise, settled by `complete_request`.
    let ctor = Value::Object(rt.intr.promise_ctor);
    let wr = rt.native("", 0, noop, false);
    let executor = rt.native_with("", 2, capture_resolvers, false, vec![Value::Object(wr)]);
    let p = rt.construct(&ctor, &[Value::Object(executor)], None)?;
    let res = rt.get_str(wr, "resolve")?;
    let rej = rt.get_str(wr, "reject")?;
    with_dom(rt, |_, dom| {
        dom.next_id += 1;
        let id = dom.next_id;
        dom.pending_requests.insert(id, (res, rej));
        dom.requests.push(HttpRequest { id, method, url, headers, body });
    });
    Ok(p)
}

/// A promise executor that stores resolve/reject on the object in slot 0.
fn capture_resolvers(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = rt.native_slots(c.callee)[0] else { unreachable!() };
    rt.define(o, "resolve", c.arg(0), DEFAULT);
    rt.define(o, "reject", c.arg(1), DEFAULT);
    Ok(Value::Undefined)
}

/// Settles a script's request with the browser's answer (Err: it failed).
pub fn complete_request(rt: &mut Realm, id: u32, result: Result<HttpResponse, String>) {
    let Some((res, rej)) = with_dom(rt, |_, dom| dom.pending_requests.remove(&id)) else { return };
    let r = match result {
        Ok(resp) => {
            let o = rt.new_object();
            rt.define(o, "status", Value::Number(resp.status as f64), DEFAULT);
            rt.define(o, "statusText", Value::str(&resp.status_text), DEFAULT);
            rt.define(o, "url", Value::str(&resp.url), DEFAULT);
            let pairs: Vec<Value> = resp
                .headers
                .iter()
                .map(|(k, v)| rt.array_from(vec![Value::str(&k.to_ascii_lowercase()), Value::str(v)]))
                .collect();
            let h = rt.array_from(pairs);
            rt.define(o, "headers", h, DEFAULT);
            rt.define(o, "body", Value::str(&resp.body), DEFAULT);
            rt.call(&res, Value::Undefined, &[Value::Object(o)])
        }
        Err(msg) => {
            let e = rt.type_error(&format!("Failed to fetch: {msg}"));
            rt.call(&rej, Value::Undefined, &[e])
        }
    };
    report(rt, r);
    rt.run_jobs();
}

/// `__nebula_random(typedArray)`: fills an integer typed array with random
/// bytes from the host (crypto.getRandomValues).
fn native_random(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = c.arg(0) else { return Err(rt.type_error("getRandomValues: not a typed array")) };
    let Some((kind, buffer, offset, len)) = rt.typed_array(o) else {
        return Err(rt.type_error("getRandomValues: not a typed array"));
    };
    if matches!(kind, nebula_script::object::TAKind::F32 | nebula_script::object::TAKind::F64) {
        return Err(
            rt.error(nebula_script::ErrorKind::Error, "TypeMismatchError: The data provided is not an integer array")
        );
    }
    let n = len * kind.size();
    if n > 65536 {
        return Err(
            rt.error(nebula_script::ErrorKind::Error, "QuotaExceededError: getRandomValues takes at most 65536 bytes")
        );
    }
    let mut bytes = vec![0u8; n];
    rt.random_bytes(&mut bytes);
    if let Kind::ArrayBuffer(b) = &mut rt.heap.get_mut(buffer).kind {
        b.bytes[offset..offset + n].copy_from_slice(&bytes);
    }
    Ok(c.arg(0))
}
