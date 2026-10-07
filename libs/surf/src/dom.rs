//! The document tree: an arena of nodes.
//!
//! Scripts can change the tree. Removed nodes stay in the arena, detached
//! (no parent), so `NodeId`s never change meaning; style and layout only walk
//! what is reachable from the root.

use alloc::string::String;
use alloc::vec::Vec;

pub type NodeId = usize;

#[derive(Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

#[derive(Debug)]
pub enum NodeData {
    Document,
    Element(Element),
    Text(String),
}

#[derive(Debug, Clone)]
pub struct Element {
    /// Lower-case tag name.
    pub tag: String,
    /// Attributes with lower-case names, in source order.
    pub attrs: Vec<(String, String)>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class").is_some_and(|c| c.split_ascii_whitespace().any(|x| x == class))
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_ascii_whitespace()
    }
}

pub struct Document {
    pub nodes: Vec<Node>,
    /// Bumped by every change (the browser re-styles when it moves).
    pub version: u64,
    /// The current values of form controls (what was typed, or set by a
    /// script), which may differ from their `value` attributes.
    pub values: alloc::collections::BTreeMap<NodeId, String>,
}

impl Default for Document {
    fn default() -> Self {
        Document {
            nodes: alloc::vec![Node { parent: None, children: Vec::new(), data: NodeData::Document }],
            version: 0,
            values: alloc::collections::BTreeMap::new(),
        }
    }
}

/// The tag of a document fragment (an element that never renders and whose
/// children move when it is inserted).
pub const FRAGMENT: &str = "#document-fragment";

impl Document {
    pub const ROOT: NodeId = 0;

    pub fn add(&mut self, parent: NodeId, data: NodeData) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node { parent: Some(parent), children: Vec::new(), data });
        self.nodes[parent].children.push(id);
        id
    }

    /// A new node with no parent.
    pub fn create(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node { parent: None, children: Vec::new(), data });
        self.version += 1;
        self.nodes.len() - 1
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        match &mut self.nodes[id].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Whether `a` is `b` or one of its ancestors.
    pub fn is_inclusive_ancestor(&self, a: NodeId, b: NodeId) -> bool {
        let mut cur = Some(b);
        while let Some(n) = cur {
            if n == a {
                return true;
            }
            cur = self.nodes[n].parent;
        }
        false
    }

    /// Takes a node out of its parent.
    pub fn detach(&mut self, n: NodeId) {
        if let Some(p) = self.nodes[n].parent.take() {
            self.nodes[p].children.retain(|c| *c != n);
            self.version += 1;
        }
    }

    /// Inserts `child` into `parent` before `before` (or at the end). A
    /// fragment's children are inserted instead. Returns false (and changes
    /// nothing) if that would make a cycle.
    pub fn insert(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> bool {
        if self.is_inclusive_ancestor(child, parent) {
            return false;
        }
        if self.tag(child) == FRAGMENT {
            for c in self.nodes[child].children.clone() {
                self.insert(parent, c, before);
            }
            return true;
        }
        self.detach(child);
        let at = before
            .and_then(|b| self.nodes[parent].children.iter().position(|c| *c == b))
            .unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(at, child);
        self.nodes[child].parent = Some(parent);
        self.version += 1;
        true
    }

    pub fn set_attr(&mut self, n: NodeId, name: &str, value: &str) {
        if let Some(e) = self.element_mut(n) {
            match e.attrs.iter_mut().find(|(k, _)| k == name) {
                Some(slot) => slot.1 = String::from(value),
                None => e.attrs.push((String::from(name), String::from(value))),
            }
            self.version += 1;
        }
    }

    pub fn remove_attr(&mut self, n: NodeId, name: &str) {
        if let Some(e) = self.element_mut(n) {
            e.attrs.retain(|(k, _)| k != name);
            self.version += 1;
        }
    }

    /// Replaces a node's children with one text node (or none for "").
    pub fn set_text_content(&mut self, n: NodeId, text: &str) {
        if let NodeData::Text(t) = &mut self.nodes[n].data {
            *t = String::from(text);
            self.version += 1;
            return;
        }
        for c in core::mem::take(&mut self.nodes[n].children) {
            self.nodes[c].parent = None;
        }
        if !text.is_empty() {
            let t = self.create(NodeData::Text(String::from(text)));
            self.insert(n, t, None);
        }
        self.version += 1;
    }

    /// A copy of a node (and its subtree, if `deep`), detached.
    pub fn clone_node(&mut self, n: NodeId, deep: bool) -> NodeId {
        let data = match &self.nodes[n].data {
            NodeData::Document => NodeData::Element(Element { tag: String::from(FRAGMENT), attrs: Vec::new() }),
            NodeData::Element(e) => NodeData::Element(e.clone()),
            NodeData::Text(t) => NodeData::Text(t.clone()),
        };
        let copy = self.create(data);
        if deep {
            for c in self.nodes[n].children.clone() {
                let cc = self.clone_node(c, true);
                self.insert(copy, cc, None);
            }
        }
        copy
    }

    /// Copies `from` (a node of another document) and its subtree in here,
    /// detached; returns the copy.
    pub fn import(&mut self, other: &Document, from: NodeId) -> NodeId {
        let data = match &other.nodes[from].data {
            NodeData::Document => NodeData::Element(Element { tag: String::from(FRAGMENT), attrs: Vec::new() }),
            NodeData::Element(e) => NodeData::Element(e.clone()),
            NodeData::Text(t) => NodeData::Text(t.clone()),
        };
        let copy = self.create(data);
        for &c in &other.nodes[from].children {
            let cc = self.import(other, c);
            self.insert(copy, cc, None);
        }
        copy
    }

    /// Whether a node is in the tree (reachable from the root).
    pub fn is_connected(&self, n: NodeId) -> bool {
        self.is_inclusive_ancestor(Self::ROOT, n)
    }

    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match &self.nodes[id].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn tag(&self, id: NodeId) -> &str {
        self.element(id).map_or("", |e| e.tag.as_str())
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id].parent
    }

    /// The element siblings before `id`, nearest first.
    pub fn previous_elements(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let siblings: &[NodeId] = match self.nodes[id].parent {
            Some(p) => &self.nodes[p].children,
            None => &[],
        };
        let pos = siblings.iter().position(|c| *c == id).unwrap_or(0);
        siblings[..pos].iter().rev().copied().filter(|c| self.element(*c).is_some())
    }

    pub fn next_elements(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let siblings: &[NodeId] = match self.nodes[id].parent {
            Some(p) => &self.nodes[p].children,
            None => &[],
        };
        let pos = siblings.iter().position(|c| *c == id).map_or(siblings.len(), |p| p + 1);
        siblings[pos..].iter().copied().filter(|c| self.element(*c).is_some())
    }

    /// All nodes below `id` in document order (not including `id`).
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.nodes[id].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }

    /// The first element with `tag`, in document order.
    pub fn find(&self, tag: &str) -> Option<NodeId> {
        self.descendants(Self::ROOT).into_iter().find(|n| self.tag(*n) == tag)
    }

    pub fn find_all(&self, tag: &str) -> Vec<NodeId> {
        self.descendants(Self::ROOT).into_iter().filter(|n| self.tag(*n) == tag).collect()
    }

    /// The concatenated text below `id`.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut s = String::new();
        if let NodeData::Text(t) = &self.nodes[id].data {
            s.push_str(t);
        }
        for n in self.descendants(id) {
            if let NodeData::Text(t) = &self.nodes[n].data {
                s.push_str(t);
            }
        }
        s
    }
}
