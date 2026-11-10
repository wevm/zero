mod json;
mod primitives;
mod values;

use crate::Suite;

pub fn all() -> Vec<Suite> {
    [json::suites(), primitives::suites(), values::suites()].concat()
}
