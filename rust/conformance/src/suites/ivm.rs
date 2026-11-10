//! Incremental view maintenance: `ivm/ivm.test.ts`,
//! `ivm/ivm-features.test.ts`, `ivm/bughunt.test.ts` (its IVM cases), and
//! `ivm/sweep-ivm.test.ts`.
//!
//! Each case is a program of query builder calls (`ivm/harness.ts`). Rust
//! replays it with its own builder and must build JS's AST and format, or
//! reject it with JS's message. Then it builds the pipeline for JS's AST
//! over memory sources loaded with the case's dataset, hydrates a view,
//! applies the dataset's pushes one by one, and compares the changes the
//! view received and the view after each with JS's.

use crate::case::{Case, ensure, same};
use crate::{Outcome, Source, Suite};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use zero_client::ast::{
    AggregateFunction, Ast, Condition, Direction, JsonPathSegment, SimpleOperator, System,
};
use zero_client::builder::{BuilderDelegate, build_pipeline, group_view_input};
use zero_client::decode::JsonCodable;
use zero_client::format::Format;
use zero_client::ivm::{
    ArrayView, Change, FetchRequest, Input, InputBase, MemorySource, NodeStream, Output,
    PipelineError, Row, Source as IvmSource, SourceChange, SourceSchema, capture, has_failed,
};
use zero_client::json::{Object, Value};
use zero_client::query::{AnyQuery, ExistsOptions, ExpressionBuilder};
use zero_client::schema::Schema;

pub fn suites() -> Vec<Suite> {
    let suite = |file, source| Suite {
        file,
        source,
        check: ivm,
    };
    vec![
        suite("ivm", Source::Checked),
        suite("ivm-features", Source::Checked),
        suite("bughunt-ivm", Source::Checked),
        suite("ivm-fuzz", Source::Generated),
        suite("ivm-features-fuzz", Source::Generated),
        suite("sweep-ivm-curated", Source::Generated),
        suite("sweep-ivm-fuzz", Source::Generated),
        Suite {
            file: "harden-ivm-fuzz",
            source: Source::Generated,
            check: harden,
        },
        Suite {
            file: "harden-ivm-special",
            source: Source::Generated,
            check: harden,
        },
    ]
}

/// The exception for a program with a JSON path index Rust's types can't
/// hold (a fraction or a negative number), which JS rejects.
const UNWRITABLE_PATH: &str = "unwritable path";

fn ivm(c: &Value, file: &Value) -> Result<Outcome, String> {
    let shared = file.field("shared")?;
    let schema = Arc::new(Schema::from_json(shared.field("schema")?).map_err(|e| e.to_string())?);
    let program = c.field("program")?;
    if let Some(expected) = c.get("builderError").and_then(Value::as_str) {
        // Rust's types rule out a fractional or negative index; JS throws
        // for it.
        if has_unwritable_path(program) {
            ensure(expected.starts_with("Invalid JSON path segment"), || {
                format!("unwritable path, but JS's builder error is {expected}")
            })?;
            return Ok(Outcome::Exception(UNWRITABLE_PATH));
        }
        let built = build(&schema, program)?;
        same(
            format!("builder error of {program}"),
            built.invalid(),
            Some(expected),
        )?;
        return Ok(Outcome::Pass);
    }

    let json_ast = c.field("ast")?;
    let json_format = c.field("format")?;
    let built = build(&schema, program)?;
    if let Some(m) = built.invalid() {
        return Err(format!("{program}: Rust made the query invalid: {m}"));
    }
    same(
        format!("AST of {program}"),
        &built.ast().to_json(),
        json_ast,
    )?;
    same(
        format!("format of {program}"),
        &built.format().to_json(),
        json_format,
    )?;

    let ast = Ast::from_json(json_ast).map_err(|e| e.to_string())?;
    let format = Format::from_json(json_format).map_err(|e| e.to_string())?;
    let dataset = shared
        .arr("datasets")?
        .get(c.num("dataset")? as usize)
        .ok_or("no such dataset")?;
    let sources = load_sources(&schema, dataset, |v| v.clone())?;
    let delegate = Delegate(sources.clone());
    let result = c.field("result")?;
    let expected_error = result.get("error").and_then(Value::as_str);

    let built = (|| -> Result<(Rc<Recorder>, Rc<ArrayView>), PipelineError> {
        let input = build_pipeline(&ast, &delegate, "q")?;
        let recorder = Recorder::new(input);
        let view_input: Rc<dyn Input> = if ast.group_by.is_some() {
            group_view_input(recorder.clone(), &ast, &delegate)?
        } else {
            recorder.clone()
        };
        let view = capture(|| ArrayView::new(view_input, format, None, true))?;
        Ok((recorder, view))
    })();
    let (recorder, view) = match built {
        Ok(v) => v,
        Err(e) => {
            same("error", Some(e.0.as_str()), expected_error)?;
            return Ok(Outcome::Pass);
        }
    };
    let view_json = || view.data().to_json().unwrap_or(Value::Null);
    if expected_error.is_none() {
        same("initial view", &view_json(), result.field("initial")?)?;
    }
    let steps = result.get("steps").and_then(Value::as_array);
    for (i, push) in dataset.arr("pushes")?.iter().enumerate() {
        recorder.changes.borrow_mut().clear();
        let (table, change) = source_change(push)?;
        let source = sources.get(table).ok_or("push to an unknown table")?;
        if let Err(e) = capture(|| source.push(change)) {
            same(
                format!("error at push {i} {push}"),
                Some(e.0.as_str()),
                expected_error,
            )?;
            return Ok(Outcome::Pass);
        }
        view.flush();
        let Some(step) = steps.and_then(|s| s.get(i)) else {
            return Err(format!("JS failed with {expected_error:?}, Rust did not"));
        };
        let changes = Value::Array(recorder.changes.borrow().clone());
        same(
            format!("changes at push {i} {push}"),
            &changes,
            step.field("changes")?,
        )?;
        same(
            format!("view at push {i} {push}"),
            &view_json(),
            step.field("data")?,
        )?;
    }
    if let Some(e) = expected_error {
        return Err(format!("JS failed with {e:?}, Rust did not"));
    }
    Ok(Outcome::Pass)
}

/// A memory source for each table of `schema`, loaded with the dataset's
/// rows as `decode` reads them.
fn load_sources(
    schema: &Schema,
    dataset: &Value,
    decode: impl Fn(&Value) -> Value,
) -> Result<HashMap<String, Rc<MemorySource>>, String> {
    let mut sources = HashMap::new();
    let data = dataset.get("data");
    for (name, table) in schema.tables.iter() {
        let source = MemorySource::new(table.clone());
        let rows = data
            .and_then(|d| d.get(name))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        source.push_adds(
            rows.iter()
                .map(|r| row(&decode(r)))
                .collect::<Result<_, _>>()?,
        );
        sources.insert(name.clone(), source);
    }
    Ok(sources)
}

/// `harden-ivm.test.ts`: deep `related`/`whereExists` nesting, junctions,
/// `flip`/`scalar`, OR of EXISTS, batches of pushes flushed once, and
/// whether the view notified its listeners after each batch. Numbers JSON
/// can't carry are tagged as `{"$number": "NaN"}` and so on.
fn harden(c: &Value, file: &Value) -> Result<Outcome, String> {
    let shared = file.field("shared")?;
    let schema = Arc::new(Schema::from_json(shared.field("schema")?).map_err(|e| e.to_string())?);
    let result = c.field("result")?;
    let expected_error = result.get("error").and_then(Value::as_str);
    // JS threw from this batch's push: no steps were recorded.
    let failing_batch = result
        .get("batch")
        .and_then(Value::as_f64)
        .map(|b| b as usize);
    let dataset = shared
        .arr("datasets")?
        .get(c.num("dataset")? as usize)
        .ok_or("no such dataset")?;
    let sources = load_sources(&schema, dataset, decode_numbers)?;
    let program = decode_numbers(c.field("program")?);
    let built = build(&schema, &program)?;
    if let Some(m) = built.invalid() {
        return Err(format!("{program}: Rust made the query invalid: {m}"));
    }
    // With -0 read as 0: JS's `start` keeps 0 where a builder that copies
    // the row keeps -0, which JSON (and so the hash and the server) can't
    // tell apart.
    same(
        format!("AST of {program}"),
        encode_numbers(&positive_zero(&built.ast().to_json())),
        encode_numbers(&positive_zero(&decode_numbers(c.field("ast")?))),
    )?;
    let format = Format::from_json(c.field("format")?).map_err(|e| e.to_string())?;
    same(
        format!("format of {program}"),
        &built.format().to_json(),
        c.field("format")?,
    )?;
    let delegate = Delegate(sources.clone());
    let notified = Rc::new(std::cell::Cell::new(0));
    let built = (|| -> Result<(Rc<Recorder>, Rc<ArrayView>), PipelineError> {
        let input = build_pipeline(built.ast(), &delegate, "q")?;
        let recorder = Recorder::new(input);
        let view = capture(|| ArrayView::new(recorder.clone(), format, None, true))?;
        Ok((recorder, view))
    })();
    let (recorder, view) = match built {
        Ok(v) => v,
        Err(e) => {
            same("error", Some(e.0.as_str()), expected_error)?;
            return Ok(Outcome::Pass);
        }
    };
    let n = notified.clone();
    let _remove = view.add_listener(move |_, _| n.set(n.get() + 1));
    if expected_error.is_some() && failing_batch.is_none() {
        return Err(format!("JS failed with {expected_error:?}, Rust did not"));
    }
    let view_json = || encode_numbers(&view.data().to_json().unwrap_or(Value::Null));
    if failing_batch.is_none() {
        same("initial view", &view_json(), result.field("initial")?)?;
    }
    let steps = result.get("steps").and_then(Value::as_array);
    for (i, batch) in dataset.arr("batches")?.iter().enumerate() {
        recorder.changes.borrow_mut().clear();
        notified.set(0);
        let pushes = batch.as_array().ok_or("batch is not a list")?;
        // As `QueryContext.processChanges` pushes, and `IVMBranch.apply`,
        // which stops at a failed push.
        let pushed = capture(|| -> Result<(), String> {
            for push in pushes {
                if has_failed() {
                    break;
                }
                let push = decode_numbers(push);
                let (table, change) = source_change(&push)?;
                sources
                    .get(table)
                    .ok_or("push to an unknown table")?
                    .push(change);
            }
            Ok(())
        });
        match pushed {
            Err(e) if failing_batch == Some(i) => {
                same(
                    format!("error at batch {i}"),
                    Some(e.0.as_str()),
                    expected_error,
                )?;
                return Ok(Outcome::Pass);
            }
            Err(e) => return Err(format!("batch {i}: Rust failed with {e}; JS: {result}")),
            Ok(r) => r?,
        }
        if failing_batch == Some(i) {
            return Err(format!(
                "batch {i}: JS failed with {expected_error:?}, Rust did not"
            ));
        }
        view.flush();
        // Steps were recorded only when no push threw.
        if failing_batch.is_some() {
            continue;
        }
        let step = steps.and_then(|s| s.get(i)).ok_or("no step")?;
        let changes = encode_numbers(&Value::Array(recorder.changes.borrow().clone()));
        same(
            format!("changes at batch {i} {batch}"),
            &changes,
            step.field("changes")?,
        )?;
        same(
            format!("view at batch {i} {batch}"),
            &view_json(),
            step.field("data")?,
        )?;
        same(
            format!("notified at batch {i} {batch}"),
            Value::Bool(notified.get() > 0),
            step.field("notified")?.clone(),
        )?;
    }
    if let Some(b) = failing_batch {
        return Err(format!("JS failed at batch {b}, Rust did not"));
    }
    Ok(Outcome::Pass)
}

fn map_values(v: &Value, f: &dyn Fn(&Value) -> Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(f).collect()),
        Value::Object(o) => {
            let mut out = Object::new();
            for (k, x) in o.iter() {
                out.insert(k.clone(), f(x));
            }
            Value::Object(out)
        }
        v => v.clone(),
    }
}

/// Reads the tagged numbers of the harden vectors.
fn decode_numbers(v: &Value) -> Value {
    if let Value::Object(o) = v
        && o.len() == 1
        && let Some(s) = o.get("$number").and_then(Value::as_str)
    {
        return Value::Number(match s {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            _ => -0.0,
        });
    }
    map_values(v, &decode_numbers)
}

/// Tags the numbers JSON can't carry, as the harden vectors do.
fn encode_numbers(v: &Value) -> Value {
    let tag = |s: &str| object(vec![("$number", Value::String(s.to_owned()))]);
    match v {
        Value::Number(n) if n.is_nan() => tag("NaN"),
        Value::Number(n) if *n == f64::INFINITY => tag("Infinity"),
        Value::Number(n) if *n == f64::NEG_INFINITY => tag("-Infinity"),
        Value::Number(n) if *n == 0.0 && n.is_sign_negative() => tag("-0"),
        v => map_values(v, &encode_numbers),
    }
}

fn positive_zero(v: &Value) -> Value {
    match v {
        Value::Number(n) if *n == 0.0 => Value::Number(0.0),
        v => map_values(v, &positive_zero),
    }
}

fn row(v: &Value) -> Result<Row, String> {
    v.as_object()
        .map(|o| Rc::new(o.clone()))
        .ok_or_else(|| format!("row {v} is not an object"))
}

fn source_change(push: &Value) -> Result<(&str, SourceChange), String> {
    let r = row(push.field("row")?)?;
    let change = match push.str("type")? {
        "add" => SourceChange::Add(r),
        "remove" => SourceChange::Remove(r),
        "edit" => SourceChange::Edit {
            row: r,
            old_row: row(push.field("oldRow")?)?,
        },
        other => return Err(format!("unknown push type {other}")),
    };
    Ok((push.str("table")?, change))
}

struct Delegate(HashMap<String, Rc<MemorySource>>);

impl BuilderDelegate for Delegate {
    fn get_source(&self, table_name: &str) -> Option<Rc<dyn IvmSource>> {
        self.0
            .get(table_name)
            .map(|s| s.clone() as Rc<dyn IvmSource>)
    }
}

/// Records what the pipeline pushes to the view.
struct Recorder {
    input: Rc<dyn Input>,
    output: RefCell<Weak<dyn Output>>,
    changes: RefCell<Vec<Value>>,
}

impl Recorder {
    fn new(input: Rc<dyn Input>) -> Rc<Self> {
        let r = Rc::new(Recorder {
            input: input.clone(),
            output: RefCell::new(zero_client::ivm::no_output()),
            changes: RefCell::default(),
        });
        input.set_output(Rc::downgrade(&r) as Weak<dyn Output>);
        r
    }
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    let mut o = Object::new();
    for (k, v) in entries {
        o.insert(k, v);
    }
    Value::Object(o)
}

fn record(change: &Change) -> Value {
    let row = |r: &Row| Value::Object((**r).clone());
    let kind = |s: &str| Value::String(s.to_owned());
    match change {
        Change::Add(n) => object(vec![("type", kind("add")), ("row", row(&n.row))]),
        Change::Remove(n) => object(vec![("type", kind("remove")), ("row", row(&n.row))]),
        Change::Edit { node, old_node } => object(vec![
            ("type", kind("edit")),
            ("row", row(&node.row)),
            ("oldRow", row(&old_node.row)),
        ]),
        Change::Child(n, c) => object(vec![
            ("type", kind("child")),
            ("row", row(&n.row)),
            (
                "child",
                object(vec![
                    ("relationshipName", kind(&c.relationship_name)),
                    ("change", record(&c.change)),
                ]),
            ),
        ]),
    }
}

impl InputBase for Recorder {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for Recorder {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }
    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        self.input.fetch(req)
    }
}

impl Output for Recorder {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        self.changes.borrow_mut().push(record(&change));
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.push(change, self);
        }
    }
    fn reconcile(&self, _pusher: &dyn InputBase) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.reconcile(self);
        }
    }
}

/// Replays a harness `Program` with the Rust query builder.
fn build(schema: &Arc<Schema>, program: &Value) -> Result<AnyQuery, String> {
    let q = AnyQuery::new(schema.clone(), program.str("table")?, System::Client);
    apply_ops(q, program.arr("ops")?)
}

fn apply_ops(mut q: AnyQuery, ops: &[Value]) -> Result<AnyQuery, String> {
    for op in ops {
        q = apply(q, op)?;
    }
    Ok(q)
}

/// The sub-program of an op, as a builder callback. A malformed one is
/// recorded and leaves the subquery as it is.
fn sub<'a>(
    ops: Option<&'a Value>,
    error: &'a RefCell<Option<String>>,
) -> impl FnOnce(AnyQuery) -> AnyQuery + 'a {
    move |s: AnyQuery| match ops.and_then(Value::as_array) {
        None => s,
        Some(ops) => match apply_ops(s.clone(), ops) {
            Ok(q) => q,
            Err(e) => {
                error.borrow_mut().get_or_insert(e);
                s
            }
        },
    }
}

fn operator(v: &Value) -> Result<SimpleOperator, String> {
    let s = v
        .as_str()
        .ok_or_else(|| format!("operator {v} is not a string"))?;
    SimpleOperator::parse(s).ok_or_else(|| format!("unknown operator {s}"))
}

fn exists_options(v: &Value) -> ExistsOptions {
    ExistsOptions {
        flip: v.get("flip").and_then(Value::as_bool),
        scalar: v.get("scalar").and_then(Value::as_bool),
    }
}

/// A JSON path of a program, or `None` when it has a segment Rust can't
/// write, such as a fractional index.
fn path(v: &Value) -> Option<Vec<JsonPathSegment>> {
    v.as_array()?
        .iter()
        .map(|s| match s {
            Value::String(k) => Some(JsonPathSegment::Key(k.clone())),
            Value::Number(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= u32::MAX as f64 => {
                Some(JsonPathSegment::Index(*n as u32))
            }
            _ => None,
        })
        .collect()
}

/// Whether the program has a JSON path Rust can't write.
fn has_unwritable_path(v: &Value) -> bool {
    match v {
        Value::Object(o) => {
            if let Some(c) = o.get("cmpJson").and_then(Value::as_array)
                && c.get(1).is_some_and(|p| path(p).is_none())
            {
                return true;
            }
            if let Some(p) = o.get("path")
                && p.as_array().is_some()
                && path(p).is_none()
            {
                return true;
            }
            o.values().any(has_unwritable_path)
        }
        Value::Array(a) => a.iter().any(has_unwritable_path),
        _ => false,
    }
}

fn apply(q: AnyQuery, op: &Value) -> Result<AnyQuery, String> {
    let error = RefCell::new(None);
    let field = || op.str("field");
    let opt_path = || -> Result<Option<Vec<JsonPathSegment>>, String> {
        match op.get("path") {
            None | Some(Value::Null) => Ok(None),
            Some(p) => path(p)
                .map(Some)
                .ok_or_else(|| format!("unwritable path {p}")),
        }
    };
    let q = match op.str("op")? {
        "where" => {
            let value = op.get("value").cloned().unwrap_or(Value::Null);
            let cmp = match op.get("cmp") {
                None => SimpleOperator::Eq,
                Some(c) => operator(c)?,
            };
            q.where_(field()?, cmp, value)
        }
        "whereExpr" => {
            let expr = op.field("expr")?;
            q.where_expr(|eb| expression(eb, expr, &error))
        }
        "whereExists" => {
            let rel = op.str("rel")?;
            q.where_exists(rel, exists_options(op), sub(op.get("sub"), &error))
        }
        "related" => q.related(op.str("rel")?, sub(op.get("sub"), &error)),
        "orderBy" => {
            let dir = Direction::parse(op.str("dir")?).ok_or("unknown direction")?;
            q.order_by(field()?, dir)
        }
        "limit" => {
            let n = op.num("n")?;
            if n.fract() != 0.0 {
                return Err(format!("op {op}: the limit is not an integer"));
            }
            q.limit(n as i64)
        }
        "start" => {
            let inclusive = op
                .get("inclusive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            q.start(op.obj("row")?.clone(), inclusive)
        }
        "one" => q.one(),
        "count" => q.count(),
        f @ ("sum" | "avg" | "min" | "max") => {
            let fn_ = AggregateFunction::parse(f).ok_or("unknown function")?;
            aggregate(q, fn_, field()?, opt_path()?)
        }
        "distinct" => q.distinct(strings(op.field("columns")?)?),
        "groupBy" => {
            let grouped = q.group_by(strings(op.field("columns")?)?);
            let fn_ = AggregateFunction::parse(op.str("fn")?).ok_or("unknown function")?;
            let f = op.get("field").and_then(Value::as_str);
            let p = opt_path()?;
            match (fn_, f, p) {
                (AggregateFunction::Count, _, _) => grouped.count(),
                (_, None, _) => return Err(format!("op {op}: no field")),
                (AggregateFunction::Sum, Some(f), None) => grouped.sum(f),
                (AggregateFunction::Sum, Some(f), Some(p)) => grouped.sum_json(f, p),
                (AggregateFunction::Avg, Some(f), None) => grouped.avg(f),
                (AggregateFunction::Avg, Some(f), Some(p)) => grouped.avg_json(f, p),
                (AggregateFunction::Min, Some(f), None) => grouped.min(f),
                (AggregateFunction::Min, Some(f), Some(p)) => grouped.min_json(f, p),
                (AggregateFunction::Max, Some(f), None) => grouped.max(f),
                (AggregateFunction::Max, Some(f), Some(p)) => grouped.max_json(f, p),
            }
        }
        other => return Err(format!("unknown op {other}")),
    };
    match error.into_inner() {
        Some(e) => Err(e),
        None => Ok(q),
    }
}

fn aggregate(
    q: AnyQuery,
    fn_: AggregateFunction,
    field: &str,
    path: Option<Vec<JsonPathSegment>>,
) -> AnyQuery {
    match (fn_, path) {
        (AggregateFunction::Sum, None) => q.sum(field),
        (AggregateFunction::Sum, Some(p)) => q.sum_json(field, p),
        (AggregateFunction::Avg, None) => q.avg(field),
        (AggregateFunction::Avg, Some(p)) => q.avg_json(field, p),
        (AggregateFunction::Min, None) => q.min(field),
        (AggregateFunction::Min, Some(p)) => q.min_json(field, p),
        (AggregateFunction::Max, None) => q.max(field),
        (AggregateFunction::Max, Some(p)) => q.max_json(field, p),
        (AggregateFunction::Count, _) => q.count(),
    }
}

fn strings(v: &Value) -> Result<Vec<String>, String> {
    v.as_array()
        .and_then(|a| a.iter().map(|s| s.as_str().map(str::to_owned)).collect())
        .ok_or_else(|| format!("{v} is not a list of strings"))
}

/// `buildExpr` in the harness. A malformed expression is recorded and
/// stands in as an empty `and`.
fn expression(eb: &ExpressionBuilder, e: &Value, error: &RefCell<Option<String>>) -> Condition {
    let built = (|| -> Result<Condition, String> {
        let list = |key: &str| e.get(key).and_then(Value::as_array);
        if let Some(c) = list("cmp") {
            let field = c.first().and_then(Value::as_str).ok_or("cmp field")?;
            let op = operator(c.get(1).ok_or("cmp op")?)?;
            return Ok(eb.cmp(field, op, c.get(2).cloned().unwrap_or(Value::Null)));
        }
        if let Some(c) = list("cmpJson") {
            let field = c.first().and_then(Value::as_str).ok_or("cmpJson field")?;
            let p = c.get(1).and_then(path).ok_or("unwritable path")?;
            let op = operator(c.get(2).ok_or("cmpJson op")?)?;
            let value = c.get(3).cloned().unwrap_or(Value::Null);
            return Ok(eb.cmp_json(&eb.json(field, p), op, value));
        }
        if let Some(c) = list("and") {
            return Ok(eb.and(c.iter().map(|x| expression(eb, x, error)).collect()));
        }
        if let Some(c) = list("or") {
            return Ok(eb.or(c.iter().map(|x| expression(eb, x, error)).collect()));
        }
        if let Some(c) = e.get("not") {
            return Ok(eb.not(expression(eb, c, error)));
        }
        let rel = e.str("exists")?;
        Ok(eb.exists(rel, exists_options(e), sub(e.get("sub"), error)))
    })();
    built.unwrap_or_else(|m| {
        error
            .borrow_mut()
            .get_or_insert(format!("malformed expression {e}: {m}"));
        Condition::And(Vec::new())
    })
}
