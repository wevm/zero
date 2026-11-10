//! Incremental view maintenance (`packages/zql/src/ivm`): the operators a
//! query's pipeline is built from, the memory source they read, and the view
//! that materializes their output.
//!
//! A pipeline is a graph of reference-counted operators on one thread. Each
//! operator holds its inputs strongly and its outputs weakly, so the view at
//! the end of a pipeline owns it; a push that reaches a released output is
//! dropped. Operators take `&self` and keep their state in cells, borrowed
//! only between calls to other operators: a push can fetch back through the
//! operator that pushed it.
//!
//! Fetches are lazy streams, like JS's generators: a fetch reads operator and
//! source state when its first row is pulled, not when it is created.

mod change;
mod constraint;
mod data;
mod failure;
mod operator;
mod predicate;
mod stream;

pub use change::*;
pub use constraint::*;
pub use data::*;
pub use failure::*;
pub use operator::*;
pub use predicate::*;
pub use stream::*;

use crate::json::Object;
use std::rc::Rc;

/// A row: column name to value. A missing column reads as absent, as an
/// `undefined` property does in JS. Rows are shared, not copied, as they
/// flow through a pipeline.
pub type Row = Rc<Object>;
