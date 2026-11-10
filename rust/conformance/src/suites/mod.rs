mod ast;
pub(crate) mod json;
mod primitives;
mod protocol;
mod values;

use crate::Suite;

pub fn all() -> Vec<Suite> {
    [
        ast::suites(),
        json::suites(),
        primitives::suites(),
        protocol::suites(),
        values::suites(),
    ]
    .concat()
}
