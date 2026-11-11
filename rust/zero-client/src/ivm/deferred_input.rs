//! `DeferredInput` (`packages/zql/src/ivm/deferred-input.ts`).

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Builds a [`DeferredInput`]'s pipeline.
pub type BuildPipeline = Box<dyn Fn() -> Result<Rc<dyn Input>, PipelineError>>;

/// A placeholder input for a view whose pipeline is not built yet.
///
/// Until [`attach`](Self::attach) it behaves like an empty pipeline: `fetch`
/// yields nothing and nothing is pushed. `attach` builds the real pipeline
/// and hydrates the output with its rows as add changes, as if they had just
/// arrived.
pub struct DeferredInput {
    schema: Rc<SourceSchema>,
    build: BuildPipeline,
    output: RefCell<Weak<dyn Output>>,
    input: RefCell<Option<Rc<dyn Input>>>,
    destroyed: Cell<bool>,
}

impl DeferredInput {
    /// `schema` is what the built pipeline reports; `build` builds it.
    pub fn new(schema: Rc<SourceSchema>, build: BuildPipeline) -> Rc<Self> {
        Rc::new(DeferredInput {
            schema,
            build,
            output: RefCell::new(no_output()),
            input: RefCell::new(None),
            destroyed: Cell::new(false),
        })
    }

    pub fn attached(&self) -> bool {
        self.input.borrow().is_some()
    }

    pub fn destroyed(&self) -> bool {
        self.destroyed.get()
    }

    /// Builds the pipeline and pushes its rows to the output. Leaves this
    /// input unattached when building or hydrating fails.
    pub fn attach(&self) -> Result<(), PipelineError> {
        assert!(!self.attached(), "DeferredInput: pipeline already attached");
        assert!(
            !self.destroyed.get(),
            "DeferredInput: cannot attach after destroy"
        );
        let input = (self.build)()?;
        let weak = self.output.borrow().clone();
        let Some(output) = weak.upgrade() else {
            *self.input.borrow_mut() = Some(input);
            return Ok(());
        };
        input.set_output(weak);
        let hydrated = capture(|| {
            for node in input.fetch(&FetchRequest::default()) {
                output.push(Change::Add(node), input.as_ref());
            }
        });
        if let Err(e) = hydrated {
            input.destroy();
            return Err(e);
        }
        output.reconcile(input.as_ref());
        *self.input.borrow_mut() = Some(input);
        Ok(())
    }
}

impl InputBase for DeferredInput {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        self.destroyed.set(true);
        let input = self.input.borrow().clone();
        if let Some(input) = input {
            input.destroy();
        }
    }
}

impl Input for DeferredInput {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output.clone();
        let input = self.input.borrow().clone();
        if let Some(input) = input {
            input.set_output(output);
        }
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let input = self.input.borrow().clone();
        match input {
            Some(input) => input.fetch(req),
            None => empty(),
        }
    }
}
