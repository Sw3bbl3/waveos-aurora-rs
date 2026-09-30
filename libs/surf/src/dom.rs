//! The document tree: an arena of nodes.

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
}

impl Default for Document {
    fn default() -> Self {
        Document { nodes: alloc::vec![Node { parent: None, children: Vec::new(), data: NodeData::Document }] }
    }
}

impl Document {
    pub const ROOT: NodeId = 0;

    pub fn add(&mut self, parent: NodeId, data: NodeData) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node { parent: Some(parent), children: Vec::new(), data });
        self.nodes[parent].children.push(id);
        id
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
