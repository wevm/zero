//! JSON with JS semantics: `JSON.parse`, `JSON.stringify`, JS property order,
//! and JS number formatting and parsing.

pub mod number;
mod parse;
#[cfg(feature = "serde")]
mod serde_value;
mod stringify;
mod value;

pub use parse::{MAX_DEPTH, ParseError, TOO_DEEP, parse};
#[cfg(feature = "serde")]
pub use serde_value::{SerdeError, from_value, to_value};
pub use stringify::{stringify, write, write_string};
pub use value::{Object, Value};
