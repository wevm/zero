mod json;
mod primitives;

use crate::Suite;

pub fn all() -> Vec<Suite> {
    [json::suites(), primitives::suites()].concat()
}
