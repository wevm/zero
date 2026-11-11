//! JSON with JS semantics: `JSON.parse`, `JSON.stringify`, JS property order,
//! and JS number formatting and parsing.

pub mod number;
mod parse;
mod stringify;
mod value;

pub use parse::{MAX_DEPTH, ParseError, TOO_DEEP, parse};
pub use stringify::{stringify, write, write_string};
pub use value::{Object, Value};
