//! A persistent B+ tree of rows, for `MemorySource`'s indexes (`BTreeSet`
//! in zero-swift, `btree-set.ts` in JS).
//!
//! Nodes sit behind `Rc` and are copied on write, so cloning a tree is O(1)
//! (`MemorySource::fork`), a write copies only the nodes on its path, and an
//! iterator keeps reading the tree as it was when it started.

use std::cmp::Ordering;
use std::rc::Rc;

#[cfg(not(test))]
const MAX_NODE: usize = 64;
/// Small nodes in tests, so that a few rows build a deep tree.
#[cfg(test)]
const MAX_NODE: usize = 8;

#[derive(Clone)]
struct Node<T> {
    /// A leaf's elements, or the highest element of each child.
    keys: Vec<T>,
    /// Empty for a leaf.
    children: Vec<Rc<Node<T>>>,
}

impl<T> Node<T> {
    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}

/// The first index whose key is not before the bound.
fn lower_bound<T>(keys: &[T], probe: &dyn Fn(&T) -> Ordering) -> usize {
    keys.partition_point(|k| probe(k) == Ordering::Less)
}

/// The first index whose key is after the bound.
fn upper_bound<T>(keys: &[T], probe: &dyn Fn(&T) -> Ordering) -> usize {
    keys.partition_point(|k| probe(k) != Ordering::Greater)
}

/// A sorted set of `T`, ordered by the comparator each call is given (the
/// same one for the tree's life).
#[derive(Clone)]
pub(crate) struct Tree<T> {
    root: Rc<Node<T>>,
    len: usize,
}

impl<T: Clone> Tree<T> {
    pub(crate) fn new() -> Self {
        Tree {
            root: Rc::new(Node {
                keys: Vec::new(),
                children: Vec::new(),
            }),
            len: 0,
        }
    }

    /// A tree of `sorted`, which is in order with no equal elements, in
    /// O(n).
    pub(crate) fn from_sorted(sorted: Vec<T>) -> Self {
        let len = sorted.len();
        let mut level: Vec<Rc<Node<T>>> = Vec::with_capacity(len / MAX_NODE + 1);
        let mut rows = sorted.into_iter().peekable();
        while rows.peek().is_some() {
            let keys: Vec<T> = rows.by_ref().take(MAX_NODE).collect();
            level.push(Rc::new(Node {
                keys,
                children: Vec::new(),
            }));
        }
        if level.is_empty() {
            return Tree::new();
        }
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len() / MAX_NODE + 1);
            let mut nodes = level.into_iter().peekable();
            while nodes.peek().is_some() {
                let children: Vec<Rc<Node<T>>> = nodes.by_ref().take(MAX_NODE).collect();
                let keys = children.iter().map(|c| last(c).clone()).collect();
                next.push(Rc::new(Node { keys, children }));
            }
            level = next;
        }
        Tree {
            root: level.pop().expect("a root"),
            len,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The element equal to `x`.
    pub(crate) fn get(&self, x: &T, cmp: &dyn Fn(&T, &T) -> Ordering) -> Option<&T> {
        let probe = |k: &T| cmp(k, x);
        let mut node = &*self.root;
        loop {
            let i = lower_bound(&node.keys, &probe);
            if node.is_leaf() {
                return node.keys.get(i).filter(|k| cmp(k, x) == Ordering::Equal);
            }
            node = node.children.get(i)?;
        }
    }

    /// Adds `x`, replacing an equal element. Returns whether it was new.
    pub(crate) fn add(&mut self, x: T, cmp: &dyn Fn(&T, &T) -> Ordering) -> bool {
        let (inserted, split) = insert(Rc::make_mut(&mut self.root), x, cmp);
        if let Some(right) = split {
            let left = self.root.clone();
            self.root = Rc::new(Node {
                keys: vec![last(&left).clone(), last(&right).clone()],
                children: vec![left, Rc::new(right)],
            });
        }
        if inserted {
            self.len += 1;
        }
        inserted
    }

    /// Removes the element equal to `x`. Returns whether there was one.
    pub(crate) fn delete(&mut self, x: &T, cmp: &dyn Fn(&T, &T) -> Ordering) -> bool {
        if self.get(x, cmp).is_none() {
            return false;
        }
        remove(Rc::make_mut(&mut self.root), x, cmp);
        while !self.root.is_leaf() && self.root.children.len() == 1 {
            self.root = self.root.children[0].clone();
        }
        if !self.root.is_leaf() && self.root.children.is_empty() {
            *self = Tree::new();
            return true;
        }
        self.len -= 1;
        true
    }

    /// The elements in order, from the first one not before `probe`'s
    /// bound, or, reversed, from the last one not after it. `probe(e)` is
    /// how `e` compares to the bound. Without a probe, from the first
    /// (last) element.
    pub(crate) fn iter(&self, probe: Option<&dyn Fn(&T) -> Ordering>, reverse: bool) -> Iter<T> {
        let mut path = Vec::new();
        let mut node = self.root.clone();
        while !node.is_leaf() {
            let n = node.keys.len();
            let i = match (probe, reverse) {
                (None, false) => 0,
                (None, true) => n - 1,
                (Some(p), false) => lower_bound(&node.keys, p),
                // The first child that ends after the bound holds the
                // last element not after it, or the child before does.
                (Some(p), true) => upper_bound(&node.keys, p).min(n - 1),
            };
            if i == n {
                return Iter {
                    path: Vec::new(),
                    leaf: None,
                    index: 0,
                    reverse,
                };
            }
            let child = node.children[i].clone();
            path.push((node, i));
            node = child;
        }
        let index = match (probe, reverse) {
            (None, false) => 0,
            (None, true) => node.keys.len() as isize - 1,
            (Some(p), false) => lower_bound(&node.keys, p) as isize,
            (Some(p), true) => upper_bound(&node.keys, p) as isize - 1,
        };
        Iter {
            path,
            leaf: Some(node),
            index,
            reverse,
        }
    }
}

fn last<T>(node: &Node<T>) -> &T {
    node.keys.last().expect("a node has keys")
}

/// Inserts into `node`. Returns whether `x` was new and the right half of
/// `node` if it split.
fn insert<T: Clone>(
    node: &mut Node<T>,
    x: T,
    cmp: &dyn Fn(&T, &T) -> Ordering,
) -> (bool, Option<Node<T>>) {
    let i = lower_bound(&node.keys, &|k| cmp(k, &x));
    if node.is_leaf() {
        if i < node.keys.len() && cmp(&node.keys[i], &x) == Ordering::Equal {
            node.keys[i] = x;
            return (false, None);
        }
        node.keys.insert(i, x);
        return (true, split_if_needed(node));
    }
    // Past every child's highest element: it goes into the last child.
    let i = i.min(node.keys.len() - 1);
    let child = Rc::make_mut(&mut node.children[i]);
    let (inserted, split) = insert(child, x, cmp);
    node.keys[i] = last(child).clone();
    if let Some(right) = split {
        node.keys.insert(i + 1, last(&right).clone());
        node.children.insert(i + 1, Rc::new(right));
    }
    (inserted, split_if_needed(node))
}

fn split_if_needed<T>(node: &mut Node<T>) -> Option<Node<T>> {
    if node.keys.len() <= MAX_NODE {
        return None;
    }
    let half = node.keys.len() / 2;
    let keys = node.keys.split_off(half);
    let children = if node.is_leaf() {
        Vec::new()
    } else {
        node.children.split_off(half)
    };
    Some(Node { keys, children })
}

/// Removes `x`, which the subtree holds, from `node`.
fn remove<T: Clone>(node: &mut Node<T>, x: &T, cmp: &dyn Fn(&T, &T) -> Ordering) {
    let i = lower_bound(&node.keys, &|k| cmp(k, x));
    if node.is_leaf() {
        node.keys.remove(i);
        return;
    }
    let child = Rc::make_mut(&mut node.children[i]);
    remove(child, x, cmp);
    if child.keys.is_empty() {
        node.children.remove(i);
        node.keys.remove(i);
        return;
    }
    node.keys[i] = last(child).clone();
    if child.keys.len() >= MAX_NODE / 4 {
        return;
    }
    // Merge with a neighbor when the two fit in one node.
    let j = if i + 1 < node.children.len() {
        i + 1
    } else if i > 0 {
        i - 1
    } else {
        return;
    };
    if node.children[i].keys.len() + node.children[j].keys.len() > MAX_NODE {
        return;
    }
    let (l, r) = if j > i { (i, j) } else { (j, i) };
    let right = node.children.remove(r);
    node.keys.remove(r);
    let right = Rc::try_unwrap(right).unwrap_or_else(|shared| (*shared).clone());
    let left = Rc::make_mut(&mut node.children[l]);
    left.keys.extend(right.keys);
    left.children.extend(right.children);
    node.keys[l] = last(left).clone();
}

/// A tree's elements in order or reversed, as the tree was when the
/// iterator was made.
pub(crate) struct Iter<T> {
    /// Each internal node above the leaf, with the child the walk is in.
    path: Vec<(Rc<Node<T>>, usize)>,
    leaf: Option<Rc<Node<T>>>,
    index: isize,
    reverse: bool,
}

impl<T: Clone> Iterator for Iter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        loop {
            let leaf = self.leaf.as_ref()?;
            if self.index >= 0 && (self.index as usize) < leaf.keys.len() {
                let x = leaf.keys[self.index as usize].clone();
                self.index += if self.reverse { -1 } else { 1 };
                return Some(x);
            }
            self.leaf = None;
            while let Some((node, ci)) = self.path.pop() {
                let next = if self.reverse {
                    ci.checked_sub(1)
                } else {
                    Some(ci + 1).filter(|&c| c < node.children.len())
                };
                let Some(c) = next else { continue };
                let mut n = node.children[c].clone();
                self.path.push((node, c));
                while !n.is_leaf() {
                    let c = if self.reverse {
                        n.children.len() - 1
                    } else {
                        0
                    };
                    let child = n.children[c].clone();
                    self.path.push((n, c));
                    n = child;
                }
                self.index = if self.reverse {
                    n.keys.len() as isize - 1
                } else {
                    0
                };
                self.leaf = Some(n);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(a: &i64, b: &i64) -> Ordering {
        a.cmp(b)
    }

    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    /// Random adds and deletes against a sorted `Vec`, with forks kept
    /// from along the way: every tree reads as its model, in both
    /// directions and from any bound, and writes to one leave the others
    /// as they were.
    #[test]
    fn matches_a_sorted_vec_and_forks_stay_apart() {
        for seed in 1..60u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let span = 1 + rng.below(400) as i64;
            let mut tree = Tree::new();
            let mut model: Vec<i64> = Vec::new();
            let mut forks: Vec<(Tree<i64>, Vec<i64>)> = Vec::new();
            for step in 0..1500 {
                let x = rng.below(span as u64) as i64;
                if rng.below(3) == 0 {
                    let found = model.binary_search(&x);
                    assert_eq!(
                        tree.delete(&x, &cmp),
                        found.is_ok(),
                        "seed {seed} step {step}"
                    );
                    if let Ok(i) = found {
                        model.remove(i);
                    }
                } else {
                    let found = model.binary_search(&x);
                    assert_eq!(tree.add(x, &cmp), found.is_err(), "seed {seed} step {step}");
                    if let Err(i) = found {
                        model.insert(i, x);
                    }
                }
                assert_eq!(tree.len, model.len());
                if step % 97 == 0 {
                    forks.push((tree.clone(), model.clone()));
                }
                if step % 50 == 0 {
                    check(&tree, &model, span, seed);
                }
            }
            check(&tree, &model, span, seed);
            for (fork, model) in &forks {
                check(fork, model, span, seed);
            }
            let rebuilt = Tree::from_sorted(model.clone());
            check(&rebuilt, &model, span, seed);
        }
    }

    fn check(tree: &Tree<i64>, model: &[i64], span: i64, seed: u64) {
        assert_eq!(
            tree.iter(None, false).collect::<Vec<_>>(),
            model,
            "seed {seed}"
        );
        let reversed: Vec<i64> = model.iter().rev().copied().collect();
        assert_eq!(
            tree.iter(None, true).collect::<Vec<_>>(),
            reversed,
            "seed {seed}"
        );
        for b in -1..=span {
            let probe = |k: &i64| k.cmp(&b);
            let from: Vec<i64> = model.iter().copied().filter(|&k| k >= b).collect();
            assert_eq!(
                tree.iter(Some(&probe), false).collect::<Vec<_>>(),
                from,
                "seed {seed} from {b}"
            );
            let to: Vec<i64> = model.iter().rev().copied().filter(|&k| k <= b).collect();
            assert_eq!(
                tree.iter(Some(&probe), true).collect::<Vec<_>>(),
                to,
                "seed {seed} to {b}"
            );
            assert_eq!(
                tree.get(&b, &cmp).copied(),
                model.binary_search(&b).ok().map(|_| b)
            );
        }
    }

    /// A probe that ties with many elements, as a bound on a prefix of the
    /// sort does: reversed, the scan starts at the last tie.
    #[test]
    fn a_reverse_scan_starts_after_every_tie() {
        let tree = Tree::from_sorted((0..200i64).collect());
        let probe = |k: &i64| (k / 50).cmp(&1);
        let to: Vec<i64> = (0..100).rev().collect();
        assert_eq!(tree.iter(Some(&probe), true).collect::<Vec<_>>(), to);
        let from: Vec<i64> = (50..200).collect();
        assert_eq!(tree.iter(Some(&probe), false).collect::<Vec<_>>(), from);
    }
}
