//! The archive's flat entry list as a folder tree. Folders that are only implied by paths
//! ("a/b/c.txt" without "a/" entries) are made up; folder sizes are the sums of what's inside.

use crate::backend::Entry;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub packed: Option<u64>,
    pub modified: String,
    pub encrypted: bool,
    pub parent: usize,
    pub children: Vec<usize>,
    /// files inside, counting subfolders
    pub files: u64,
}

#[derive(Debug, Default)]
pub struct Tree {
    /// node 0 is the archive's root
    pub nodes: Vec<Node>,
    index: HashMap<String, usize>,
}

impl Tree {
    pub fn build(entries: &[Entry]) -> Tree {
        let mut t = Tree { nodes: vec![Node { is_dir: true, packed: Some(0), ..Default::default() }], index: HashMap::new() };
        t.index.insert(String::new(), 0);
        let mut explicit = vec![];
        for e in entries {
            let i = t.ensure(&e.path, e.is_dir);
            explicit.resize(t.nodes.len(), false);
            explicit[i] = true;
            let n = &mut t.nodes[i];
            n.encrypted = e.encrypted;
            n.modified.clone_from(&e.modified);
            if !e.is_dir {
                n.is_dir = false;
                n.size = e.size;
                n.packed = e.packed;
            }
        }
        explicit.resize(t.nodes.len(), false);
        // parents always come before their children, so one backwards pass sums everything
        for i in (1..t.nodes.len()).rev() {
            let (p, n) = (t.nodes[i].parent, &t.nodes[i]);
            let (size, packed, files) = (n.size, n.packed, if n.is_dir { n.files } else { 1 });
            let mtime = n.modified.clone();
            let parent = &mut t.nodes[p];
            if parent.is_dir {
                parent.size += size;
                parent.files += files;
                parent.packed = match (parent.packed, packed) {
                    (Some(a), Some(b)) => Some(a + b),
                    _ => None,
                };
                // implied folders get the newest date of what's inside
                if !explicit[p] && p != 0 && parent.modified < mtime {
                    parent.modified = mtime;
                }
            }
        }
        t
    }

    fn ensure(&mut self, path: &str, is_dir: bool) -> usize {
        if let Some(&i) = self.index.get(path) {
            return i;
        }
        let (parent_path, name) = match path.rfind('/') {
            Some(k) => (&path[..k], &path[k + 1..]),
            None => ("", path),
        };
        let parent = self.ensure(parent_path, true);
        let i = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            parent,
            packed: if is_dir { Some(0) } else { None },
            ..Default::default()
        });
        self.nodes[parent].children.push(i);
        self.nodes[parent].is_dir = true;
        self.index.insert(path.to_string(), i);
        i
    }

    pub fn find(&self, path: &str) -> Option<usize> {
        self.index.get(path).copied()
    }

    /// Root first: [0, a, a/b]
    pub fn ancestors(&self, mut i: usize) -> Vec<usize> {
        let mut v = vec![i];
        while i != 0 {
            i = self.nodes[i].parent;
            v.push(i);
        }
        v.reverse();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(path: &str, size: u64) -> Entry {
        Entry { path: path.into(), size, packed: Some(size / 2), is_dir: size == 0, ..Default::default() }
    }
    #[test]
    fn implied_folders_and_sums() {
        let t = Tree::build(&[e("a/b/c.txt", 10), e("a/d.txt", 4), e("top.txt", 1), e("empty", 0)]);
        let a = t.find("a").unwrap();
        assert!(t.nodes[a].is_dir);
        assert_eq!((t.nodes[a].size, t.nodes[a].files, t.nodes[a].packed), (14, 2, Some(7)));
        assert_eq!(t.nodes[0].size, 15);
        assert_eq!(t.nodes[0].children.len(), 3);
        assert_eq!(t.ancestors(t.find("a/b").unwrap()), vec![0, a, t.find("a/b").unwrap()]);
        assert!(t.nodes[t.find("empty").unwrap()].is_dir);
    }
}
