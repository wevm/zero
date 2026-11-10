//! The `inspect` messages (`inspect-up.ts`, `inspect-down.ts`, and
//! `analyze-query-result.ts`), kept as JSON, checked and stripped as valita
//! does: the client has no inspector yet.

use super::valita::{Field, Schema as S};
use super::*;
use crate::ast::{Ast, Condition, strict};
use std::sync::LazyLock;

/// An `inspect` request from the client (`inspectUpBodySchema`).
#[derive(Clone, Debug, PartialEq)]
pub struct InspectUpBody(pub Value);

/// An `inspect` response from zero-cache (`inspectDownBodySchema`).
#[derive(Clone, Debug, PartialEq)]
pub struct InspectDownBody(pub Value);

impl JsonCodable for InspectUpBody {
    fn from_json(v: &Value) -> Result<Self> {
        UP.parse(v).map(InspectUpBody)
    }

    fn to_json(&self) -> Value {
        self.0.clone()
    }
}

impl JsonCodable for InspectDownBody {
    fn from_json(v: &Value) -> Result<Self> {
        DOWN.parse(v).map(InspectDownBody)
    }

    fn to_json(&self) -> Value {
        self.0.clone()
    }
}

fn b(s: S) -> Box<S> {
    Box::new(s)
}

fn req(key: &'static str, s: S) -> (&'static str, Field) {
    (key, Field::Required(s))
}

fn opt(key: &'static str, s: S) -> (&'static str, Field) {
    (key, Field::Optional(s))
}

fn op(name: &'static str, mut fields: Vec<(&'static str, Field)>) -> S {
    fields.push(req("id", S::String));
    fields.push(req("op", S::Literal(name)));
    S::Object(fields)
}

fn event(ty: &'static str, mut fields: Vec<(&'static str, Field)>) -> S {
    fields.push(req("type", S::Literal(ty)));
    S::Object(fields)
}

fn ast(v: &Value) -> Result<Value> {
    Ast::from_json(v).map(|a| a.to_json())
}

fn strict_ast(v: &Value) -> Result<Value> {
    strict::decode(v).map(|a| a.to_json())
}

fn condition(v: &Value) -> Result<Value> {
    Condition::from_json(v).map(|c| c.to_json())
}

static UP: LazyLock<S> = LazyLock::new(|| {
    S::Union(vec![
        op("queries", vec![opt("clientID", S::String)]),
        op("metrics", vec![]),
        op("version", vec![]),
        op("authenticate", vec![req("value", S::String)]),
        op(
            "analyze-query",
            vec![
                opt("value", S::Custom(strict_ast)),
                opt(
                    "options",
                    S::Object(vec![
                        opt("vendedRows", S::Boolean),
                        opt("syncedRows", S::Boolean),
                        opt("joinPlans", S::Boolean),
                    ]),
                ),
                opt("ast", S::Custom(strict_ast)),
                opt("name", S::String),
                opt("args", S::Array(b(S::Any))),
            ],
        ),
    ])
});

static DOWN: LazyLock<S> = LazyLock::new(|| {
    // `tdigestSchema` (`packages/shared/src/tdigest-schema.ts`).
    let tdigest = S::NonEmptyArray(b(S::Number));
    let row = S::Record(b(S::Any));
    let ordering = S::Array(b(S::Tuple(vec![
        S::String,
        S::Union(vec![S::Literal("asc"), S::Literal("desc")]),
    ])));
    let query_server_metrics = S::Object(vec![
        opt("query-hydration-server-ms", S::Number),
        req("query-update-server", tdigest.clone()),
    ]);
    let inspect_query_row = S::Object(vec![
        req("clientID", S::String),
        req("queryID", S::String),
        req("ast", S::Nullable(b(S::Custom(ast)))),
        req("name", S::Nullable(b(S::String))),
        req("args", S::Nullable(b(S::Array(b(S::Any))))),
        req("got", S::Boolean),
        req("deleted", S::Boolean),
        req("ttl", S::Number),
        req("inactivatedAt", S::Nullable(b(S::Number))),
        req("rowCount", S::Number),
        opt("metrics", S::Nullable(b(query_server_metrics))),
    ]);
    let server_metrics = S::Object(vec![
        req("query-materialization-server", tdigest.clone()),
        req("query-update-server", tdigest),
    ]);
    let row_counts_by_source = S::Record(b(S::Record(b(S::Number))));
    let rows_by_source = S::Record(b(S::Record(b(S::Array(b(row.clone()))))));
    let cost_estimate = S::Object(vec![
        req("startupCost", S::Number),
        req("scanEst", S::Number),
        req("cost", S::Number),
        req("returnedRows", S::Number),
        req("selectivity", S::Number),
        opt("limit", S::Number),
    ]);
    // `plannerConstraintSchema`, a record of `v.union(v.unknown(), v.null())`.
    let planner_constraint = S::Record(b(S::Any));
    let constraints = S::Record(b(S::Nullable(b(planner_constraint.clone()))));
    let join_states = S::Array(b(S::Object(vec![
        req("join", S::String),
        req("type", S::String),
    ])));
    let plan_debug_event = S::Union(vec![
        event(
            "attempt-start",
            vec![
                req("attemptNumber", S::Number),
                req("totalAttempts", S::Number),
            ],
        ),
        event(
            "connection-costs",
            vec![
                req("attemptNumber", S::Number),
                req(
                    "costs",
                    S::Array(b(S::Object(vec![
                        req("connection", S::String),
                        req("cost", S::Number),
                        req("costEstimate", cost_estimate.clone()),
                        req("pinned", S::Boolean),
                        req("constraints", constraints.clone()),
                        req("constraintCosts", S::Record(b(cost_estimate.clone()))),
                    ]))),
                ),
            ],
        ),
        event(
            "connection-selected",
            vec![
                req("attemptNumber", S::Number),
                req("connection", S::String),
                req("cost", S::Number),
                req("isRoot", S::Boolean),
            ],
        ),
        event(
            "constraints-propagated",
            vec![
                req("attemptNumber", S::Number),
                req(
                    "connectionConstraints",
                    S::Array(b(S::Object(vec![
                        req("connection", S::String),
                        req("constraints", constraints),
                        req("constraintCosts", S::Record(b(cost_estimate.clone()))),
                    ]))),
                ),
            ],
        ),
        event(
            "plan-complete",
            vec![
                req("attemptNumber", S::Number),
                req("totalCost", S::Number),
                req("flipPattern", S::Number),
                req("joinStates", join_states.clone()),
            ],
        ),
        event(
            "plan-failed",
            vec![req("attemptNumber", S::Number), req("reason", S::String)],
        ),
        event(
            "best-plan-selected",
            vec![
                req("bestAttemptNumber", S::Number),
                req("totalCost", S::Number),
                req("flipPattern", S::Number),
                req("joinStates", join_states),
            ],
        ),
        event(
            "node-cost",
            vec![
                opt("attemptNumber", S::Number),
                req("nodeType", S::String),
                req("node", S::String),
                req("branchPattern", S::Array(b(S::Number))),
                req("downstreamChildSelectivity", S::Number),
                req("costEstimate", cost_estimate),
                opt("filters", S::Custom(condition)),
                opt("ordering", ordering),
                opt("joinType", S::String),
            ],
        ),
        event(
            "node-constraint",
            vec![
                opt("attemptNumber", S::Number),
                req("nodeType", S::String),
                req("node", S::String),
                req("branchPattern", S::Array(b(S::Number))),
                opt("constraint", S::Nullable(b(planner_constraint))),
                req("from", S::String),
            ],
        ),
    ]);
    // `analyzeQueryResultSchema`.
    let analyze_query_result = S::Object(vec![
        req("warnings", S::Array(b(S::String))),
        opt("syncedRows", S::Record(b(S::Array(b(row))))),
        req("syncedRowCount", S::Number),
        req("start", S::Number),
        req("end", S::Number),
        opt("elapsed", S::Number),
        opt("afterPermissions", S::String),
        opt("vendedRowCounts", row_counts_by_source.clone()),
        opt("vendedRows", rows_by_source.clone()),
        opt("sqlitePlans", S::Record(b(S::Array(b(S::String))))),
        opt("readRows", rows_by_source),
        opt("readRowCountsByQuery", row_counts_by_source.clone()),
        opt("readRowCount", S::Number),
        opt("dbScansByQuery", row_counts_by_source),
        opt("joinPlans", S::Array(b(plan_debug_event))),
    ]);
    S::Union(vec![
        op(
            "queries",
            vec![req("value", S::Array(b(inspect_query_row)))],
        ),
        op("metrics", vec![req("value", server_metrics)]),
        op("version", vec![req("value", S::String)]),
        op("authenticated", vec![req("value", S::Boolean)]),
        op("analyze-query", vec![req("value", analyze_query_result)]),
        op("error", vec![req("value", S::String)]),
    ])
});
