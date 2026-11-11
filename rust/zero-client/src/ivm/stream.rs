//! Lazy streams (`packages/zql/src/ivm/stream.ts`).
//!
//! JS streams are generators, whose bodies start running on the first
//! `next()`. [`lazy`] matches that: its closure runs when the first element
//! is requested, not when the stream is created. Pipelines rely on this,
//! because a fetch reads operator and source state when it starts.
//!
//! The JS `'yield'` tokens, which only hand control back to a scheduler, have
//! no counterpart: the Rust engine hydrates synchronously.

use super::Node;

/// A stream of values, pulled once.
pub type Stream<T> = Box<dyn Iterator<Item = T>>;

/// A stream of nodes: what a fetch returns.
pub type NodeStream = Stream<Node>;

/// A stream whose elements `make` produces, called on the first `next()`.
pub fn lazy<T, I, F>(make: F) -> Stream<T>
where
    T: 'static,
    I: IntoIterator<Item = T>,
    I::IntoIter: 'static,
    F: FnOnce() -> I + 'static,
{
    Box::new(Lazy {
        make: Some(make),
        iter: None,
    })
}

/// A stream of `elements`.
pub fn from_vec<T: 'static>(elements: Vec<T>) -> Stream<T> {
    Box::new(elements.into_iter())
}

/// A stream with no elements.
pub fn empty<T: 'static>() -> Stream<T> {
    Box::new(std::iter::empty())
}

struct Lazy<F, I> {
    make: Option<F>,
    iter: Option<I>,
}

impl<T, I, F> Iterator for Lazy<F, I::IntoIter>
where
    I: IntoIterator<Item = T>,
    F: FnOnce() -> I,
{
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if let Some(make) = self.make.take() {
            self.iter = Some(make().into_iter());
        }
        self.iter.as_mut()?.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn lazy_runs_on_the_first_next() {
        let ran = Rc::new(Cell::new(false));
        let r = ran.clone();
        let mut s = lazy(move || {
            r.set(true);
            vec![1, 2]
        });
        assert!(!ran.get());
        assert_eq!(s.next(), Some(1));
        assert!(ran.get());
        assert_eq!(s.collect::<Vec<_>>(), vec![2]);
    }
}
