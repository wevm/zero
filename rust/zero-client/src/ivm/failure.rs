//! Errors operators find while a pipeline is fetched or pushed.
//!
//! JS throws from inside a fetch, and the error comes out of whatever
//! hydrates the view (`materialize`, `run`). Fetches here are lazy streams
//! that can't fail, so an operator reports the error with [`report`] and
//! yields nothing, and [`capture`], around the code that hydrates or pushes,
//! returns it once that code is done.

use std::cell::RefCell;
use std::fmt;

/// An error building or running a pipeline, with JS's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineError(pub String);

impl PipelineError {
    pub fn new(message: impl Into<String>) -> Self {
        PipelineError(message.into())
    }
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PipelineError {}

thread_local! {
    /// The running captures, innermost last, each with the first error
    /// reported to it.
    static CAPTURES: RefCell<Vec<Option<PipelineError>>> = const { RefCell::new(Vec::new()) };
}

struct Frame;

impl Drop for Frame {
    fn drop(&mut self) {
        CAPTURES.with(|c| c.borrow_mut().pop());
    }
}

/// Runs `body`, and fails with the first error an operator reported while
/// it ran.
pub fn capture<T>(body: impl FnOnce() -> T) -> Result<T, PipelineError> {
    CAPTURES.with(|c| c.borrow_mut().push(None));
    let frame = Frame;
    let result = body();
    let error = CAPTURES.with(|c| c.borrow_mut().last_mut().and_then(Option::take));
    drop(frame);
    match error {
        Some(e) => Err(e),
        None => Ok(result),
    }
}

/// Whether an operator reported an error to the running [`capture`]. Where
/// JS's throw would unwind, a source stops pushing.
pub fn has_failed() -> bool {
    CAPTURES.with(|c| matches!(c.borrow().last(), Some(Some(_))))
}

/// Reports `message` to the running [`capture`]; the first one wins.
/// Outside one (a pipeline driven directly) the error goes to stderr, and the
/// operator yields nothing.
pub fn report(message: impl Into<String>) {
    let message = message.into();
    CAPTURES.with(|c| match c.borrow_mut().last_mut() {
        Some(slot) => {
            if slot.is_none() {
                *slot = Some(PipelineError(message));
            }
        }
        None => eprintln!("zero-client: pipeline error: {message}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_innermost_capture_gets_the_first_report() {
        let outer = capture(|| {
            let inner = capture(|| {
                report("a");
                report("b");
                assert!(has_failed());
            });
            assert_eq!(inner, Err(PipelineError::new("a")));
            assert!(!has_failed());
        });
        assert_eq!(outer, Ok(()));
    }
}
