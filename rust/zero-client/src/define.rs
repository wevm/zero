//! Typed named queries and mutators, behind the `serde` feature:
//! [`define_query`] and [`define_mutator`] (`defineQuery` in
//! `packages/zql/src/query/query-registry.ts` and `defineMutator` in
//! `packages/zql/src/mutate/mutator.ts`, shaped like the Swift client's).
//!
//! Each is defined once, by its name in the TS registry the server runs and
//! a serde argument type, and called with a value of that type: the client
//! sends the name and the arguments as JSON, as JS does, and reads them back
//! as the type before a mutator's body runs. The context comes from the
//! [`Zero`](crate::client::Zero) the request goes to.
//!
//! ```
//! # use zero_client::define::{MutatorDefinition, QueryDefinition, define_mutator, define_query};
//! # use zero_client::client::{Error, Transaction};
//! # use zero_client::typed::Query;
//! # struct Label;
//! # fn labels_query(project: &str) -> Query<Label> { unimplemented!() }
//! #[derive(serde::Serialize, serde::Deserialize)]
//! #[serde(rename_all = "camelCase")]
//! struct ProjectArgs { project_name: String }
//!
//! /// The logged-in user, which `Zero::new` was given.
//! type Context = Option<String>;
//!
//! const LABELS: QueryDefinition<ProjectArgs, Context, Label> =
//!     define_query("labels", |args, _ctx| labels_query(&args.project_name));
//! const RENAME: MutatorDefinition<ProjectArgs, Context> =
//!     define_mutator("project.rename", |tx, args, ctx| Ok(()));
//!
//! // zero.materialize(&LABELS.with(ProjectArgs { project_name: "Zero".into() }), &ttl)?;
//! // zero.mutate_request(RENAME.with(ProjectArgs { project_name: "Zero".into() }));
//! // Zero::new(schema, context, vec![RENAME.mutator()], ...);
//! ```
//!
//! A definition that takes no arguments takes `()` (or another type with
//! no fields), which sends none, as JS's `undefined` does.

use crate::client::{ApplicationError, Error, MutateRequest, Mutator, Transaction};
use crate::ivm::ViewValue;
use crate::json::{self, Value};
use crate::query::{AnyQuery, ZeroQuery};
use crate::typed::{Query, QueryValue, ZeroValueError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::rc::Rc;

/// `args` as sent: `None` for a type without fields that writes `null`,
/// such as `()`, which stands for no arguments.
fn encode_args<A: Serialize>(args: &A) -> Option<Value> {
    let value = json::to_value(args).expect("arguments must encode as JSON");
    (size_of::<A>() != 0 || !value.is_null()).then_some(value)
}

/// `args` (none, or one JSON value) read as `A`.
fn decode_args<A: DeserializeOwned>(args: Option<&Value>) -> Result<A, String> {
    json::from_value(args.unwrap_or(&Value::Null)).map_err(|e| e.to_string())
}

/// A named query (`defineQuery`): `A` is its arguments, `C` the client's
/// context, and `Query<R, T>` what it builds. Make one with
/// [`define_query`].
///
/// The name must match the TS registry entry the server runs: the client
/// builds the query here for optimistic results, and the server builds its
/// own from the name and arguments.
pub struct QueryDefinition<A, C, R, T = Vec<R>> {
    /// Its name in the TS query registry.
    pub name: &'static str,
    body: fn(&A, &C) -> Query<R, T>,
}

impl<A, C, R, T> Clone for QueryDefinition<A, C, R, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A, C, R, T> Copy for QueryDefinition<A, C, R, T> {}

/// A named query: `name` in the TS registry, and the query `body` builds
/// from the arguments and the client's context.
pub const fn define_query<A, C, R, T>(
    name: &'static str,
    body: fn(&A, &C) -> Query<R, T>,
) -> QueryDefinition<A, C, R, T> {
    QueryDefinition { name, body }
}

impl<A, C, R, T> QueryDefinition<A, C, R, T>
where
    A: Serialize + 'static,
    C: 'static,
    R: 'static,
    T: 'static,
{
    /// The query with `args`, ready to materialize, run, or preload.
    pub fn with(&self, args: A) -> QueryRequest<C, R, T> {
        let encoded = encode_args(&args).into_iter().collect();
        let body = self.body;
        QueryRequest {
            name: self.name,
            args: encoded,
            build: Rc::new(move |context| body(&args, context)),
        }
    }
}

/// Builds a request's query from the client's context.
type BuildQuery<C, R, T> = Rc<dyn Fn(&C) -> Query<R, T>>;

/// A [`QueryDefinition`] applied to its arguments: what
/// [`Zero::materialize`](crate::client::Zero::materialize), `run`, and
/// `preload` take, which build it with the client's context.
pub struct QueryRequest<C, R, T = Vec<R>> {
    name: &'static str,
    args: Vec<Value>,
    build: BuildQuery<C, R, T>,
}

impl<C, R, T> Clone for QueryRequest<C, R, T> {
    fn clone(&self) -> Self {
        QueryRequest {
            name: self.name,
            args: self.args.clone(),
            build: self.build.clone(),
        }
    }
}

impl<C, R, T> QueryRequest<C, R, T> {
    /// The query's name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The arguments as sent: none, or one JSON value.
    pub fn args(&self) -> &[Value] {
        &self.args
    }

    /// The query as `context` sees it, named so the server knows it.
    pub fn query(&self, context: &C) -> Query<R, T> {
        let any = (self.build)(context).into_any();
        Query::from_any(any.name_and_args(self.name, self.args.clone()))
    }
}

impl<C, R, T: QueryValue> QueryRequest<C, R, T> {
    /// Reads a view of this query as its result, as `Query::read` does.
    pub fn read(&self, view: &ViewValue) -> Result<T, ZeroValueError> {
        T::from_view(view)
    }
}

impl<C, R, T> ZeroQuery<C> for QueryRequest<C, R, T> {
    fn any_query(&self, context: &C) -> Cow<'_, AnyQuery> {
        Cow::Owned(self.query(context).into_any())
    }
}

/// A named query whose arguments come as JSON, as the server gets them,
/// whatever its types: for a list of an app's queries, such as a test that
/// checks them against the TS definitions.
pub trait NamedQuery<C> {
    /// Its name in the TS query registry.
    fn name(&self) -> &'static str;

    /// The query for `args` (none, or one JSON value) read as the
    /// definition's argument type, as `context` sees it, with the arguments
    /// as this side sends them.
    fn query_from_json(
        &self,
        args: &[Value],
        context: &C,
    ) -> Result<(AnyQuery, Vec<Value>), String>;
}

impl<A, C, R, T> NamedQuery<C> for QueryDefinition<A, C, R, T>
where
    A: Serialize + DeserializeOwned + 'static,
    C: 'static,
    R: 'static,
    T: 'static,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn query_from_json(
        &self,
        args: &[Value],
        context: &C,
    ) -> Result<(AnyQuery, Vec<Value>), String> {
        let decoded: A = match args {
            [] => decode_args(None)?,
            [a] => decode_args(Some(a))?,
            _ => return Err(format!("expected at most one argument, got {}", args.len())),
        };
        let request = self.with(decoded);
        Ok((request.query(context).into_any(), request.args))
    }
}

type MutatorBody<A, C, O> = fn(&Transaction, A, &C) -> Result<O, Error>;

/// A named mutator (`defineMutator`): `A` is its arguments, `C` the
/// client's context, and `O` what its optimistic run returns to the
/// caller's `client` outcome (`()` for nothing). Make one with
/// [`define_mutator`].
///
/// The name must match the TS registry entry the API server runs. The body
/// runs once optimistically and again on every rebase while the mutation is
/// pending, so it must only depend on `tx`, the arguments, and the context.
pub struct MutatorDefinition<A, C, O = ()> {
    /// Its name in the TS mutator registry.
    pub name: &'static str,
    body: MutatorBody<A, C, O>,
}

impl<A, C, O> Clone for MutatorDefinition<A, C, O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A, C, O> Copy for MutatorDefinition<A, C, O> {}

/// A named mutator: `name` in the TS registry, and its `body`.
pub const fn define_mutator<A, C, O>(
    name: &'static str,
    body: MutatorBody<A, C, O>,
) -> MutatorDefinition<A, C, O> {
    MutatorDefinition { name, body }
}

impl<A, C, O> MutatorDefinition<A, C, O>
where
    A: Serialize + DeserializeOwned + 'static,
    C: 'static,
    O: Serialize + 'static,
{
    /// The mutation of this mutator with `args`, for
    /// [`Zero::mutate_request`](crate::client::Zero::mutate_request).
    pub fn with(&self, args: A) -> MutateRequest {
        MutateRequest::new(self.name, encode_args(&args))
    }

    /// The mutator to register with `Zero::new`, so the client runs it and
    /// replays it by name. Arguments that don't read as `A` fail it with
    /// an [`ApplicationError`].
    pub fn mutator(&self) -> Mutator<C> {
        let body = self.body;
        Mutator::new(self.name, move |tx, args, context: &C| {
            let args: A = decode_args(args).map_err(ApplicationError::new)?;
            Ok(encode_args(&body(tx, args, context)?))
        })
    }

    /// `args` read as `A` and encoded again: what this side would send for
    /// them.
    pub fn reencode(&self, args: Option<&Value>) -> Result<Option<Value>, String> {
        Ok(encode_args(&decode_args::<A>(args)?))
    }
}

/// A named mutator whatever its types: for a list of an app's mutators,
/// to register them all and to check them against the TS definitions.
pub trait NamedMutator<C> {
    /// Its name in the TS mutator registry.
    fn name(&self) -> &'static str;
    /// [`MutatorDefinition::mutator`].
    fn mutator(&self) -> Mutator<C>;
    /// [`MutatorDefinition::reencode`].
    fn reencode(&self, args: Option<&Value>) -> Result<Option<Value>, String>;
}

impl<A, C, O> NamedMutator<C> for MutatorDefinition<A, C, O>
where
    A: Serialize + DeserializeOwned + 'static,
    C: 'static,
    O: Serialize + 'static,
{
    fn name(&self) -> &'static str {
        self.name
    }
    fn mutator(&self) -> Mutator<C> {
        MutatorDefinition::mutator(self)
    }
    fn reencode(&self, args: Option<&Value>) -> Result<Option<Value>, String> {
        MutatorDefinition::reencode(self, args)
    }
}
