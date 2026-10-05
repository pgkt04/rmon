//! the surface document: a tree of nodes, and the ops that turn the tree the
//! terminal holds into a new one.
//! spec: https://docs.stencil.so/tern/protocol/documents.html

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub k: &'static str,
    pub p: Map<String, Value>,
    pub c: Vec<Node>,
}

impl Node {
    pub fn new(id: impl Into<String>, k: &'static str) -> Node {
        Node {
            id: id.into(),
            k,
            p: Map::new(),
            c: Vec::new(),
        }
    }

    pub fn prop(mut self, key: &str, v: impl Into<Value>) -> Node {
        self.p.insert(key.to_string(), v.into());
        self
    }

    pub fn child(mut self, n: Node) -> Node {
        self.c.push(n);
        self
    }

    pub fn children(mut self, ns: impl IntoIterator<Item = Node>) -> Node {
        self.c.extend(ns);
        self
    }

    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), Value::String(self.id.clone()));
        o.insert("k".into(), Value::String(self.k.into()));
        if !self.p.is_empty() {
            o.insert("p".into(), Value::Object(self.p.clone()));
        }
        if !self.c.is_empty() {
            o.insert(
                "c".into(),
                Value::Array(self.c.iter().map(Node::to_json).collect()),
            );
        }
        Value::Object(o)
    }
}

/// ops that turn `old` into `new`; both are the same surface root. deletes go
/// first so an id that changed kind or parent can be added fresh after them
pub fn diff(old: &Node, new: &Node) -> Vec<Value> {
    let mut dels = Vec::new();
    let mut ops = Vec::new();
    diff_node(old, new, &mut dels, &mut ops);
    dels.extend(ops);
    dels
}

fn diff_node(old: &Node, new: &Node, dels: &mut Vec<Value>, ops: &mut Vec<Value>) {
    let mut changed = Map::new();
    for (k, v) in &new.p {
        if old.p.get(k) != Some(v) {
            changed.insert(k.clone(), v.clone());
        }
    }
    for k in old.p.keys() {
        if !new.p.contains_key(k) {
            changed.insert(k.clone(), Value::Null);
        }
    }
    if !changed.is_empty() {
        ops.push(json!(["set", new.id, changed]));
    }

    // a child survives when its id stays under this parent with the same kind
    let new_kind: HashMap<&str, &'static str> =
        new.c.iter().map(|n| (n.id.as_str(), n.k)).collect();
    let mut kept: HashMap<&str, (usize, &Node)> = HashMap::new();
    for o in &old.c {
        if new_kind.get(o.id.as_str()) == Some(&o.k) {
            kept.insert(o.id.as_str(), (kept.len(), o));
        } else {
            dels.push(json!(["del", o.id]));
        }
    }

    // survivors in the longest run that is already in order stay put; every
    // other child is moved or added, walking back from the end so each one
    // lands before a sibling already in its final place
    let pos: Vec<Option<usize>> = new
        .c
        .iter()
        .map(|n| kept.get(n.id.as_str()).map(|(i, _)| *i))
        .collect();
    let stay: HashSet<usize> = longest_increasing(&pos).into_iter().collect();
    let mut before = Value::Null;
    for (i, n) in new.c.iter().enumerate().rev() {
        if pos[i].is_none() {
            ops.push(json!(["add", n.id, new.id, before, n.to_json()]));
        } else if !stay.contains(&i) {
            ops.push(json!(["move", n.id, new.id, before]));
        }
        before = Value::String(n.id.clone());
    }
    for n in &new.c {
        if let Some((_, o)) = kept.get(n.id.as_str()) {
            diff_node(o, n, dels, ops);
        }
    }
}

/// indexes into `seq` (skipping None) of one longest strictly increasing run
fn longest_increasing(seq: &[Option<usize>]) -> Vec<usize> {
    // tails[l] = index of the smallest tail of an increasing run of length l+1
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; seq.len()];
    for (i, v) in seq.iter().enumerate() {
        let Some(v) = *v else { continue };
        let l = tails.partition_point(|&t| seq[t].is_some_and(|tv| tv < v));
        if l > 0 {
            prev[i] = Some(tails[l - 1]);
        }
        if l == tails.len() {
            tails.push(i);
        } else {
            tails[l] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.push(i);
        cur = prev[i];
    }
    out
}

#[cfg(test)]
pub mod testing {
    //! the terminal's side of the ops, after the spec, for checking diffs

    use super::*;

    fn find<'a>(n: &'a mut Node, id: &str) -> Option<&'a mut Node> {
        if n.id == id {
            return Some(n);
        }
        n.c.iter_mut().find_map(|c| find(c, id))
    }

    fn take(n: &mut Node, id: &str) -> Option<Node> {
        if let Some(i) = n.c.iter().position(|c| c.id == id) {
            return Some(n.c.remove(i));
        }
        n.c.iter_mut().find_map(|c| take(c, id))
    }

    fn contains(n: &Node, id: &str) -> bool {
        n.id == id || n.c.iter().any(|c| contains(c, id))
    }

    /// every kind the spec lists; the applier keeps kinds as `&'static str` like `Node`
    const KINDS: &[&str] = &[
        "col",
        "row",
        "card",
        "section",
        "rule",
        "spacer",
        "text",
        "md",
        "code",
        "diff",
        "ansi",
        "rows",
        "math",
        "kv",
        "table",
        "tree",
        "badge",
        "kbd",
        "icon",
        "image",
        "list",
        "item",
        "tabs",
        "picker",
        "spinner",
        "shimmer",
        "elapsed",
        "rate",
        "progress",
        "meter",
        "chart",
        "effort",
        "editor",
        "input",
        "status",
        "seg",
        "toast",
        "overlay",
        "tool",
        "agent",
        "checklist",
        "block",
        "prefs",
        "el",
    ];

    fn from_json(v: &Value) -> Node {
        let k = v["k"].as_str().unwrap();
        Node {
            id: v["id"].as_str().unwrap().to_string(),
            k: KINDS
                .iter()
                .find(|known| **known == k)
                .expect("a spec kind"),
            p: v["p"].as_object().cloned().unwrap_or_default(),
            c: v["c"]
                .as_array()
                .map(|a| a.iter().map(from_json).collect())
                .unwrap_or_default(),
        }
    }

    fn insert(root: &mut Node, parent: &str, before: &Value, n: Node) -> Result<(), String> {
        let p = find(root, parent).ok_or(format!("unknown parent {parent}"))?;
        let at = match before.as_str() {
            None => p.c.len(),
            Some(b) => {
                p.c.iter()
                    .position(|c| c.id == b)
                    .ok_or(format!("{b} is not a child of {parent}"))?
            }
        };
        p.c.insert(at, n);
        Ok(())
    }

    /// apply ops like Tern does; any op Tern would reject is an error
    pub fn apply(root: &mut Node, ops: &[Value]) -> Result<(), String> {
        for op in ops {
            let a = op.as_array().unwrap();
            let id = a[1].as_str().unwrap();
            match a[0].as_str().unwrap() {
                "add" => {
                    let n = from_json(&a[4]);
                    if n.id != id || contains(root, id) {
                        return Err(format!("bad add {id}"));
                    }
                    insert(root, a[2].as_str().unwrap(), &a[3], n)?;
                }
                "set" => {
                    let n = find(root, id).ok_or(format!("set on unknown {id}"))?;
                    for (k, v) in a[2].as_object().unwrap() {
                        if v.is_null() {
                            n.p.remove(k);
                        } else {
                            n.p.insert(k.clone(), v.clone());
                        }
                    }
                }
                "move" => {
                    if a[3].as_str() == Some(id) {
                        return Err(format!("{id} moved before itself"));
                    }
                    let n = take(root, id).ok_or(format!("move of unknown {id}"))?;
                    insert(root, a[2].as_str().unwrap(), &a[3], n)?;
                }
                "del" => {
                    take(root, id).ok_or(format!("del of unknown {id}"))?;
                }
                "focus" => {}
                other => return Err(format!("unknown op {other}")),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::testing::apply;
    use super::*;

    fn leaf(id: &str, text: &str) -> Node {
        Node::new(id, "text").prop("text", text)
    }

    fn check(old: &Node, new: &Node) -> Vec<Value> {
        let ops = diff(old, new);
        let mut got = old.clone();
        apply(&mut got, &ops).unwrap_or_else(|e| panic!("{e}\nops: {ops:#?}"));
        assert_eq!(&got, new, "ops: {ops:#?}");
        ops
    }

    #[test]
    fn same_tree_sends_nothing() {
        let t = Node::new("r", "col")
            .child(leaf("a", "1"))
            .child(leaf("b", "2"));
        assert!(diff(&t, &t).is_empty());
    }

    #[test]
    fn prop_change_and_removal_is_one_set() {
        let old = Node::new("r", "col").child(leaf("a", "1").prop("tone", "error"));
        let new = Node::new("r", "col").child(leaf("a", "2"));
        let ops = check(&old, &new);
        assert_eq!(ops, vec![json!(["set", "a", {"text": "2", "tone": null}])]);
    }

    #[test]
    fn one_row_jumping_to_the_top_is_one_move() {
        let ids = ["a", "b", "c", "d", "e"];
        let old = Node::new("r", "list").children(ids.iter().map(|i| leaf(i, i)));
        let new =
            Node::new("r", "list").children(["e", "a", "b", "c", "d"].iter().map(|i| leaf(i, i)));
        let ops = check(&old, &new);
        assert_eq!(ops, vec![json!(["move", "e", "r", "a"])]);
    }

    #[test]
    fn kind_change_replaces_the_node() {
        let old = Node::new("r", "col").child(leaf("a", "x"));
        let new = Node::new("r", "col").child(Node::new("a", "meter").prop("value", 0.5));
        let ops = check(&old, &new);
        assert_eq!(ops[0], json!(["del", "a"]));
    }

    #[test]
    fn node_changing_parent_is_rebuilt() {
        let old = Node::new("r", "col")
            .child(Node::new("x", "col").child(leaf("a", "1")))
            .child(Node::new("y", "col"));
        let new = Node::new("r", "col")
            .child(Node::new("x", "col"))
            .child(Node::new("y", "col").child(leaf("a", "1")));
        check(&old, &new);
    }

    /// random trees over a small id pool, so ids collide across parents,
    /// change kind, reorder, appear and vanish; every diff must replay exactly
    #[test]
    fn random_trees_replay_exactly() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        fn forest(
            rnd: &mut dyn FnMut(u64) -> u64,
            used: &mut HashSet<String>,
            depth: u32,
        ) -> Vec<Node> {
            let mut out = Vec::new();
            for _ in 0..rnd(6) {
                let id = format!("n{}", rnd(30));
                if !used.insert(id.clone()) {
                    continue;
                }
                let k = ["col", "row", "text"][rnd(3) as usize];
                let mut n = Node::new(id, k);
                if rnd(2) == 0 {
                    n = n.prop("v", rnd(3));
                }
                if rnd(3) == 0 {
                    n = n.prop("w", "x");
                }
                if depth < 3 && k != "text" {
                    n.c = forest(rnd, used, depth + 1);
                }
                out.push(n);
            }
            out
        }
        let tree = |rnd: &mut dyn FnMut(u64) -> u64| {
            let mut used = HashSet::new();
            Node::new("root", "col").children(forest(rnd, &mut used, 0))
        };
        let mut prev = tree(&mut rnd);
        for _ in 0..2000 {
            let next = tree(&mut rnd);
            check(&prev, &next);
            prev = next;
        }
    }
}
