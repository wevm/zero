//! The untyped query builder (`QueryImpl` in
//! `packages/zql/src/query/query-impl.ts`) and the builder of `where`
//! conditions (`ExpressionBuilder` in `packages/zql/src/query/expression.ts`).
//!
//! The AST is always normalized: each step builds its part of the AST in
//! normalized form. Misuse that JS reports by throwing from the builder,
//! such as a negative `limit` or a `limit` inside a junction relationship,
//! leaves that step out and makes the query invalid: [`AnyQuery::invalid`]
//! holds JS's message. An unknown relationship is a programming error and
//! panics.

use crate::ast::*;
use crate::format::Format;
use crate::hash::query::hash_of_query_internals;
use crate::json::{self, Object, Value};
use crate::schema::{Cardinality, Relationship, RelationshipHop, Schema, ValueType};
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::Arc;

/// A query a `Zero` materializes, runs, and preloads: an [`AnyQuery`], a
/// typed `Query`, or (with the `serde` feature) a `QueryRequest` of a
/// `define_query` definition, which is built with the client's context.
pub trait ZeroQuery<C> {
    /// The query, as `context` sees it.
    fn any_query(&self, context: &C) -> Cow<'_, AnyQuery>;
}

impl<C> ZeroQuery<C> for AnyQuery {
    fn any_query(&self, _: &C) -> Cow<'_, AnyQuery> {
        Cow::Borrowed(self)
    }
}

/// The prefix of the aliases the builder gives `exists` subqueries
/// (`SUBQ_PREFIX` in `packages/zero-protocol/src/ast.ts`).
pub const SUBQUERY_ALIAS_PREFIX: &str = "zsubq_";

/// The name and arguments of the named query a query was built by.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomQueryId {
    /// The query's name.
    pub name: String,
    /// Its arguments.
    pub args: Vec<Value>,
}

/// A value taken from the context a query runs in, such as the auth data
/// (`staticParam` in `packages/zql/src/query/static-param.ts`).
#[derive(Clone, Debug, PartialEq)]
pub struct StaticParameter {
    /// What the value is taken from.
    pub anchor: Anchor,
    /// The field of it.
    pub field: StaticField,
}

impl StaticParameter {
    /// The field `field` of `anchor`.
    pub fn new(anchor: Anchor, field: impl Into<String>) -> Self {
        StaticParameter {
            anchor,
            field: StaticField::Single(field.into()),
        }
    }

    /// The value at `path` in `anchor`.
    pub fn path(anchor: Anchor, path: Vec<String>) -> Self {
        StaticParameter {
            anchor,
            field: StaticField::Path(path),
        }
    }

    fn position(&self) -> ValuePosition {
        ValuePosition::Static {
            anchor: self.anchor,
            field: self.field.clone(),
        }
    }
}

/// The options of `exists` and `whereExists`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExistsOptions {
    /// Runs the subquery first and looks up the parents of its rows.
    pub flip: Option<bool>,
    /// Lets the server resolve the subquery to a value once.
    pub scalar: Option<bool>,
}

/// A query over a table, named by strings.
#[derive(Clone, Debug)]
pub struct AnyQuery {
    schema: Arc<Schema>,
    table: String,
    ast: Ast,
    format: Format,
    system: System,
    custom_query_id: Option<CustomQueryId>,
    /// The two-hop relationship this query is the destination of, if any.
    current_junction: Option<String>,
    /// Why the query is invalid: the first misuse JS's builder throws for.
    invalid: Option<String>,
}

impl AnyQuery {
    /// All rows of `table`, for the system `system` (an app uses
    /// `System::Client`).
    pub fn new(schema: Arc<Schema>, table: impl Into<String>, system: System) -> Self {
        let table = table.into();
        AnyQuery {
            schema,
            ast: Ast::new(table.clone()),
            table,
            format: Format::default(),
            system,
            custom_query_id: None,
            current_junction: None,
            invalid: None,
        }
    }

    /// The schema the query is built on.
    pub fn schema(&self) -> &Arc<Schema> {
        &self.schema
    }

    /// The table the query reads.
    pub fn table(&self) -> &str {
        &self.table
    }

    /// The query's AST.
    pub fn ast(&self) -> &Ast {
        &self.ast
    }

    /// How the query's view shapes its rows.
    pub fn format(&self) -> &Format {
        &self.format
    }

    /// The system the query is built for.
    pub fn system(&self) -> System {
        self.system
    }

    /// The name and arguments the query is named by, for a named query.
    pub fn custom_query_id(&self) -> Option<&CustomQueryId> {
        self.custom_query_id.as_ref()
    }

    /// Why the query is invalid: the message JS's builder throws.
    pub fn invalid(&self) -> Option<&str> {
        self.invalid.as_deref()
    }

    /// The query's hash as the client and server know it.
    pub fn hash(&self) -> String {
        hash_of_query_internals(
            &self.ast,
            &crate::decode::JsonCodable::to_json(&self.format),
            self.system,
            self.current_junction.as_deref(),
            self.custom_query_id
                .as_ref()
                .map(|c| (c.name.as_str(), Some(c.args.as_slice()))),
        )
    }

    fn with(mut self, ast: Ast, format: Option<Format>) -> Self {
        self.ast = ast;
        if let Some(format) = format {
            self.format = format;
        }
        self
    }

    /// This query, unchanged but invalid: a builder step JS throws for.
    fn invalidated(self, message: impl Into<String>) -> Self {
        self.with_invalid(Some(message.into()))
    }

    fn with_invalid(mut self, invalid: Option<String>) -> Self {
        if self.invalid.is_none() {
            self.invalid = invalid;
        }
        self
    }

    fn base(&self, table: &str, alias: String, format: Format, junction: Option<&str>) -> Self {
        let mut ast = Ast::new(table);
        ast.alias = Some(alias);
        AnyQuery {
            schema: self.schema.clone(),
            table: table.to_owned(),
            ast,
            format,
            system: self.system,
            custom_query_id: self.custom_query_id.clone(),
            current_junction: junction.map(str::to_owned),
            invalid: None,
        }
    }

    /// Names the query, as JS's `defineQueries` does: the server runs its own
    /// query by `name` with `args`, and the client syncs what that returns.
    pub fn name_and_args(mut self, name: impl Into<String>, args: Vec<Value>) -> Self {
        self.custom_query_id = Some(CustomQueryId {
            name: name.into(),
            args,
        });
        self
    }

    /// At most one row; the view holds the row or nothing rather than a list.
    pub fn one(self) -> Self {
        if let Some(m) = self
            .not_aggregated("one")
            .or_else(|| self.not_distinct("one"))
        {
            return self.invalidated(m);
        }
        let mut ast = self.ast.clone();
        ast.limit = Some(1.0);
        let mut format = self.format.clone();
        format.singular = true;
        self.with(ast, Some(format))
    }

    /// Rows whose `field` compares to `value` by `op`.
    pub fn where_(
        self,
        field: impl Into<String>,
        op: SimpleOperator,
        value: impl Into<Value>,
    ) -> Self {
        self.where_condition(cmp_condition(
            field.into(),
            op,
            ValuePosition::Literal(value.into()),
        ))
    }

    /// Rows whose `field` compares to a value of the context by `op`.
    pub fn where_param(
        self,
        field: impl Into<String>,
        op: SimpleOperator,
        param: &StaticParameter,
    ) -> Self {
        self.where_condition(cmp_condition(field.into(), op, param.position()))
    }

    /// Rows that match the condition `factory` builds.
    pub fn where_expr(self, factory: impl FnOnce(&ExpressionBuilder) -> Condition) -> Self {
        let builder = ExpressionBuilder::new(&self);
        let condition = factory(&builder);
        let invalid = builder.invalid.into_inner();
        self.where_condition(condition).with_invalid(invalid)
    }

    /// Rows that match `condition`, and any earlier `where`.
    pub fn where_condition(self, condition: Condition) -> Self {
        let merged = match &self.ast.where_ {
            Some(w) => and_conditions(vec![w.clone(), condition]),
            None => condition,
        };
        let mut ast = self.ast.clone();
        ast.where_ = Some(normalize_condition(&simplify_condition(merged)));
        self.with(ast, None)
    }

    /// Rows with a related row in `relationship` that `sub` matches.
    pub fn where_exists(
        self,
        relationship: &str,
        options: ExistsOptions,
        sub: impl FnOnce(AnyQuery) -> AnyQuery,
    ) -> Self {
        self.where_expr(|eb| eb.exists(relationship, options, sub))
    }

    /// Starts after the row with these columns, or at it when `inclusive`.
    pub fn start(self, row: Object, inclusive: bool) -> Self {
        if let Some(m) = self
            .not_aggregated("start")
            .or_else(|| self.not_distinct("start"))
        {
            return self.invalidated(m);
        }
        let mut ast = self.ast.clone();
        ast.start = Some(Bound {
            row,
            exclusive: !inclusive,
        });
        self.with(ast, None)
    }

    /// At most `limit` rows.
    pub fn limit(self, limit: i64) -> Self {
        if let Some(m) = self
            .not_aggregated("limit")
            .or_else(|| self.not_distinct("limit"))
        {
            return self.invalidated(m);
        }
        if limit < 0 {
            return self.invalidated("Limit must be non-negative");
        }
        if i32::try_from(limit).is_err() {
            return self.invalidated("Limit must be an integer");
        }
        if let Some(j) = &self.current_junction {
            let m = format!(
                "Limit is not supported in junction relationships yet. Junction relationship being limited: {j}"
            );
            return self.invalidated(m);
        }
        let mut ast = self.ast.clone();
        ast.limit = Some(limit as f64);
        self.with(ast, None)
    }

    /// Orders the rows by `field`, after any earlier `order_by`; the primary
    /// key breaks ties.
    pub fn order_by(self, field: impl Into<String>, direction: Direction) -> Self {
        let field = field.into();
        if let Some(j) = &self.current_junction {
            let m = format!(
                "Order by is not supported in junction relationships yet. Junction relationship being ordered: {j}"
            );
            return self.invalidated(m);
        }
        if let Some(group_by) = &self.ast.group_by
            && let Some(m) = not_group_column(&field, group_by)
        {
            return self.invalidated(m);
        }
        let mut ast = self.ast.clone();
        ast.order_by
            .get_or_insert_with(Vec::new)
            .push(OrderPart { field, direction });
        self.with(ast, None)
    }

    /// Includes the rows of `relationship`, as `sub` shapes them.
    pub fn related(self, relationship: &str, sub: impl FnOnce(AnyQuery) -> AnyQuery) -> Self {
        assert!(
            !relationship.starts_with(SUBQUERY_ALIAS_PREFIX),
            "Relationship names may not start with \"{SUBQUERY_ALIAS_PREFIX}\". That is a reserved prefix."
        );
        if let Some(m) = self.not_distinct("related") {
            return self.invalidated(m);
        }
        let hops = self.relationship_hops(relationship);
        let mut ast = self.ast.clone();
        let mut format = self.format.clone();
        match hops.as_slice() {
            [hop] => {
                let q = sub(self.base(
                    &hop.dest_schema,
                    relationship.to_owned(),
                    singular(hop),
                    None,
                ));
                if let Some(m) = q
                    .invalid
                    .clone()
                    .or_else(|| q.not_grouped_subquery("related"))
                {
                    return self.invalidated(m);
                }
                // The builder and the name mapper read a `related` under an
                // aggregate as a junction's destination, so a direct one
                // can't nest one.
                if q.format.aggregate.is_some() && q.ast.related.is_some() {
                    return self.invalidated(
                        "an aggregate relationship does not support related() on its subquery",
                    );
                }
                // A relationship's aggregate lives on its correlated
                // subquery, not on the subquery's AST, which would reduce
                // itself to a scalar too.
                let mut subquery = q.ast;
                subquery.aggregate = None;
                insert_related(
                    &mut ast.related,
                    CorrelatedSubquery {
                        correlation: correlation(hop),
                        subquery: Box::new(subquery),
                        system: Some(self.system),
                        hidden: None,
                        aggregate: q.format.aggregate.clone(),
                    },
                );
                format.set_relationship(relationship, q.format);
            }
            [first, second] => {
                let q = sub(self.base(
                    &second.dest_schema,
                    relationship.to_owned(),
                    singular(second),
                    Some(relationship),
                ));
                if let Some(m) = q
                    .invalid
                    .clone()
                    .or_else(|| q.not_grouped_subquery("related"))
                {
                    return self.invalidated(m);
                }
                if let Some(aggregate) = q.format.aggregate.clone() {
                    // An aggregate over a junction. A `where` on the
                    // destination is fine; nesting and bounds are not.
                    if q.ast.related.is_some() || q.ast.limit.is_some() || q.ast.start.is_some() {
                        return self.invalidated(
                            "an aggregate over a junction relationship does not yet support related/limit/start on the destination",
                        );
                    }
                    let mut dest = q.ast;
                    dest.aggregate = None;
                    let dest_csq = CorrelatedSubquery {
                        correlation: correlation(second),
                        subquery: Box::new(dest),
                        system: Some(self.system),
                        hidden: None,
                        aggregate: None,
                    };
                    let mut junction = Ast::new(first.dest_schema.clone());
                    junction.alias = Some(relationship.to_owned());
                    let is_count = aggregate.fn_ == AggregateFunction::Count;
                    if is_count {
                        // A count over the junction rows whose destination
                        // exists (where the server applies its read
                        // permissions), never reading the destination.
                        junction.where_ =
                            Some(normalize_condition(&Condition::CorrelatedSubquery {
                                related: dest_csq,
                                op: CorrelatedSubqueryOperator::Exists,
                                flip: None,
                                scalar: None,
                            }));
                    } else {
                        // sum, avg, min and max need the destination's
                        // field: the builder lifts it onto the junction row.
                        junction.related = Some(vec![dest_csq]);
                    }
                    insert_related(
                        &mut ast.related,
                        CorrelatedSubquery {
                            correlation: correlation(first),
                            subquery: Box::new(junction),
                            system: Some(self.system),
                            hidden: None,
                            aggregate: Some(if is_count {
                                Aggregate::new(AggregateFunction::Count)
                            } else {
                                aggregate
                            }),
                        },
                    );
                    format.set_relationship(relationship, q.format);
                } else {
                    let mut junction = Ast::new(first.dest_schema.clone());
                    junction.alias = Some(relationship.to_owned());
                    junction.related = Some(vec![CorrelatedSubquery {
                        correlation: correlation(second),
                        subquery: Box::new(q.ast),
                        system: Some(self.system),
                        hidden: None,
                        aggregate: None,
                    }]);
                    insert_related(
                        &mut ast.related,
                        CorrelatedSubquery {
                            correlation: correlation(first),
                            subquery: Box::new(junction),
                            system: Some(self.system),
                            hidden: Some(true),
                            aggregate: None,
                        },
                    );
                    format.set_relationship(relationship, q.format);
                }
            }
            _ => panic!("Invalid relationship {relationship}"),
        }
        self.with(ast, Some(format))
    }

    /// `#exists`: the condition `exists(relationship)` adds, and why the
    /// subquery is invalid, if it is.
    fn exists_condition(
        &self,
        relationship: &str,
        options: ExistsOptions,
        sub: impl FnOnce(AnyQuery) -> AnyQuery,
    ) -> (Condition, Option<String>) {
        let hops = self.relationship_hops(relationship);
        let exists = |related, scalar| Condition::CorrelatedSubquery {
            related,
            op: CorrelatedSubqueryOperator::Exists,
            flip: options.flip,
            scalar,
        };
        match hops.as_slice() {
            [hop] => {
                let q = sub(self.base(
                    &hop.dest_schema,
                    format!("{SUBQUERY_ALIAS_PREFIX}{relationship}"),
                    Format::default(),
                    None,
                ));
                let invalid = q.invalid.clone().or_else(|| q.not_exists_subquery());
                let related = CorrelatedSubquery {
                    correlation: correlation(hop),
                    subquery: Box::new(q.ast),
                    system: Some(self.system),
                    hidden: None,
                    aggregate: None,
                };
                (exists(related, options.scalar), invalid)
            }
            [first, second] => {
                let q = sub(self.base(
                    &second.dest_schema,
                    format!("{SUBQUERY_ALIAS_PREFIX}zhidden_{relationship}"),
                    Format::default(),
                    Some(relationship),
                ));
                let invalid = q.invalid.clone().or_else(|| q.not_exists_subquery());
                let inner = exists(
                    CorrelatedSubquery {
                        correlation: correlation(second),
                        subquery: Box::new(q.ast),
                        system: Some(self.system),
                        hidden: None,
                        aggregate: None,
                    },
                    options.scalar,
                );
                let mut junction = Ast::new(first.dest_schema.clone());
                junction.alias = Some(format!("{SUBQUERY_ALIAS_PREFIX}{relationship}"));
                junction.where_ = Some(normalize_condition(&inner));
                // The junction hop carries `flip` but not `scalar`, as in JS.
                let related = CorrelatedSubquery {
                    correlation: correlation(first),
                    subquery: Box::new(junction),
                    system: Some(self.system),
                    hidden: None,
                    aggregate: None,
                };
                (exists(related, None), invalid)
            }
            _ => panic!("Invalid relationship {relationship}"),
        }
    }

    // Aggregates and groupings.

    /// The number of rows: a query of one value rather than rows.
    pub fn count(self) -> Self {
        self.aggregate(AggregateFunction::Count, None, None)
    }

    /// The sum of `field`'s non-null values; null when there are none.
    pub fn sum(self, field: impl Into<String>) -> Self {
        self.aggregate(AggregateFunction::Sum, Some(field.into()), None)
    }

    /// The sum of the numbers at a JSON path in the `json` column `column`.
    pub fn sum_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> Self {
        self.aggregate(AggregateFunction::Sum, Some(column.into()), Some(path))
    }

    /// The average of `field`'s non-null values; null when there are none.
    pub fn avg(self, field: impl Into<String>) -> Self {
        self.aggregate(AggregateFunction::Avg, Some(field.into()), None)
    }

    /// Reduces the query to the average of the numbers at `path` in the JSON
    /// `column`.
    pub fn avg_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> Self {
        self.aggregate(AggregateFunction::Avg, Some(column.into()), Some(path))
    }

    /// The least of `field`'s non-null values; null when there are none.
    pub fn min(self, field: impl Into<String>) -> Self {
        self.aggregate(AggregateFunction::Min, Some(field.into()), None)
    }

    /// The least number or string at a JSON path; numbers sort first.
    pub fn min_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> Self {
        self.aggregate(AggregateFunction::Min, Some(column.into()), Some(path))
    }

    /// The greatest of `field`'s non-null values; null when there are none.
    pub fn max(self, field: impl Into<String>) -> Self {
        self.aggregate(AggregateFunction::Max, Some(field.into()), None)
    }

    /// Reduces the query to the greatest value at `path` in the JSON `column`.
    pub fn max_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> Self {
        self.aggregate(AggregateFunction::Max, Some(column.into()), Some(path))
    }

    /// `#aggregate`. On a top-level query the view holds the value; in a
    /// `related` subquery the relationship does.
    fn aggregate(
        self,
        fn_: AggregateFunction,
        field: Option<String>,
        path: Option<Vec<JsonPathSegment>>,
    ) -> Self {
        let name = fn_.as_str();
        if let Some(m) = self.not_aggregated(name) {
            return self.invalidated(m);
        }
        // A count of a distinct() counts its rows. A groupBy() has its
        // aggregate already.
        let distinct = self.ast.group_by.clone();
        if distinct.is_some()
            && (fn_ != AggregateFunction::Count || self.ast.aggregate.is_some())
            && let Some(m) = self.not_distinct(name)
        {
            return self.invalidated(m);
        }
        // The server runs a named query's own definition, so the aggregate
        // would never reach it.
        if let Some(c) = &self.custom_query_id {
            let m = format!(
                "{name}() cannot be applied to the named query \"{}\": put it in the query's definition",
                c.name
            );
            return self.invalidated(m);
        }
        if let (Some(path), Some(field)) = (&path, &field)
            && let Some(m) = invalid_json_path(field, path)
        {
            return self.invalidated(m);
        }
        let aggregate = match distinct {
            Some(columns) => Aggregate {
                distinct: Some(columns),
                ..Aggregate::new(fn_)
            },
            None => Aggregate {
                field,
                path,
                ..Aggregate::new(fn_)
            },
        };
        let mut ast = self.ast.clone();
        ast.group_by = None;
        ast.aggregate = Some(aggregate.clone());
        let mut format = self.format.clone();
        format.singular = true;
        format.aggregate = Some(aggregate);
        self.with(ast, Some(format))
    }

    /// The distinct combinations of `columns`' values, as `SELECT DISTINCT`:
    /// rows of those columns. `count()` after it counts the combinations.
    pub fn distinct(self, columns: Vec<String>) -> Self {
        if let Some(m) = self.not_groupable("distinct", &columns) {
            return self.invalidated(m);
        }
        let mut ast = self.ast.clone();
        ast.group_by = Some(columns);
        self.with(ast, None)
    }

    /// Groups the rows by `columns`, as `GROUP BY`; an aggregate of the
    /// result gives one row per group, with the value under the function's
    /// name.
    pub fn group_by(self, columns: Vec<String>) -> AnyGroupedQuery {
        let invalid = self.not_groupable("groupBy", &columns);
        AnyGroupedQuery {
            query: self,
            columns,
            invalid,
        }
    }

    /// `groupAggregate`: the rows of `groupBy(columns)` reduced by `fn_`.
    fn group_aggregate(
        self,
        columns: Vec<String>,
        fn_: AggregateFunction,
        field: Option<String>,
        path: Option<Vec<JsonPathSegment>>,
    ) -> Self {
        let name = fn_.as_str();
        if columns.iter().any(|c| c == name) {
            return self.invalidated(format!(
                "groupBy(): the column \"{name}\" has the same name as the {name}() value"
            ));
        }
        if let (Some(path), Some(field)) = (&path, &field)
            && let Some(m) = invalid_json_path(field, path)
        {
            return self.invalidated(m);
        }
        let mut ast = self.ast.clone();
        ast.group_by = Some(columns);
        ast.aggregate = Some(Aggregate {
            field,
            path,
            ..Aggregate::new(fn_)
        });
        self.with(ast, None)
    }

    /// `#assertNotAggregated`: a bound goes in the same AST fields before or
    /// after an aggregate, so it must come first.
    fn not_aggregated(&self, method: &str) -> Option<String> {
        let aggregate = self.format.aggregate.as_ref()?;
        Some(format!(
            "{method}() must come before {}(): it bounds the rows the aggregate reduces",
            aggregate.fn_.as_str()
        ))
    }

    /// `#assertNotDistinct`.
    fn not_distinct(&self, method: &str) -> Option<String> {
        self.ast.group_by.as_ref()?;
        let kind = if self.ast.aggregate.is_some() {
            "groupBy()"
        } else {
            "distinct()"
        };
        Some(format!("{method}() does not support {kind} yet"))
    }

    /// `#assertGroupable`.
    fn not_groupable(&self, method: &str, columns: &[String]) -> Option<String> {
        if let Some(m) = self
            .not_aggregated(method)
            .or_else(|| self.not_distinct(method))
        {
            return Some(m);
        }
        if let Some(c) = &self.custom_query_id {
            return Some(format!(
                "{method}() cannot be applied to the named query \"{}\": put it in the query's definition",
                c.name
            ));
        }
        if columns.is_empty() {
            return Some(format!("{method}() needs at least one column"));
        }
        let table_columns = self.schema.tables.get(&self.table).map(|t| &t.columns);
        for (i, column) in columns.iter().enumerate() {
            let Some(column_schema) = table_columns.and_then(|c| c.get(column)) else {
                return Some(format!(
                    "{method}(): \"{column}\" is not a column of \"{}\"",
                    self.table
                ));
            };
            if column_schema.type_ == ValueType::Json {
                return Some(format!(
                    "{method}() does not support the json column \"{column}\""
                ));
            }
            if columns.iter().position(|c| c == column) != Some(i) {
                return Some(format!("{method}(): \"{column}\" is named twice"));
            }
            if is_synthetic_column(column) {
                return Some(format!(
                    "{method}(): \"{column}\" is the name of a synthetic column, which a grouped row has too"
                ));
            }
        }
        if self.ast.limit.is_some() || self.ast.start.is_some() {
            return Some(format!(
                "{method}() does not support limit(), start() or one() yet"
            ));
        }
        if self.ast.related.is_some() {
            return Some(format!("{method}() does not support related() yet"));
        }
        for part in self.ast.order_by.iter().flatten() {
            if let Some(m) = not_group_column(&part.field, columns) {
                return Some(m);
            }
        }
        None
    }

    /// `assertSubqueryNotGrouped`.
    fn not_grouped_subquery(&self, method: &str) -> Option<String> {
        let distinct = self
            .ast
            .aggregate
            .as_ref()
            .is_some_and(|a| a.distinct.is_some());
        if self.ast.group_by.is_none() && !distinct {
            return None;
        }
        let kind = if self.ast.group_by.is_some() && self.ast.aggregate.is_some() {
            "groupBy()"
        } else {
            "distinct()"
        };
        Some(format!("{method}() does not support a {kind} subquery yet"))
    }

    fn not_exists_subquery(&self) -> Option<String> {
        if self.ast.aggregate.is_some() {
            return Some("exists() does not support an aggregate subquery".to_owned());
        }
        self.not_grouped_subquery("exists")
    }

    fn relationship_hops(&self, relationship: &str) -> Relationship {
        let Some(hops) = self.schema.relationship(&self.table, relationship) else {
            panic!("Invalid relationship {}.{relationship}", self.table);
        };
        for hop in hops {
            assert!(
                !hop.source_field.is_empty() && hop.source_field.len() == hop.dest_field.len(),
                "Invalid relationship {}.{relationship}",
                self.table
            );
        }
        hops.clone()
    }
}

/// A query grouped by columns, waiting for the aggregate that reduces each
/// group (`GroupedQuery`).
#[derive(Clone, Debug)]
pub struct AnyGroupedQuery {
    query: AnyQuery,
    columns: Vec<String>,
    invalid: Option<String>,
}

impl AnyGroupedQuery {
    fn reduce(
        self,
        fn_: AggregateFunction,
        field: Option<String>,
        path: Option<Vec<JsonPathSegment>>,
    ) -> AnyQuery {
        match self.invalid {
            Some(m) => self.query.invalidated(m),
            None => self.query.group_aggregate(self.columns, fn_, field, path),
        }
    }

    /// One row per group: its columns and `count`.
    pub fn count(self) -> AnyQuery {
        self.reduce(AggregateFunction::Count, None, None)
    }

    /// Each group's sum of `field`.
    pub fn sum(self, field: impl Into<String>) -> AnyQuery {
        self.reduce(AggregateFunction::Sum, Some(field.into()), None)
    }

    /// Each group's sum of the numbers at `path` in the JSON `column`.
    pub fn sum_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> AnyQuery {
        self.reduce(AggregateFunction::Sum, Some(column.into()), Some(path))
    }

    /// Each group's average of `field`.
    pub fn avg(self, field: impl Into<String>) -> AnyQuery {
        self.reduce(AggregateFunction::Avg, Some(field.into()), None)
    }

    /// Each group's average of the numbers at `path` in the JSON `column`.
    pub fn avg_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> AnyQuery {
        self.reduce(AggregateFunction::Avg, Some(column.into()), Some(path))
    }

    /// Each group's least value of `field`.
    pub fn min(self, field: impl Into<String>) -> AnyQuery {
        self.reduce(AggregateFunction::Min, Some(field.into()), None)
    }

    /// Each group's least value at `path` in the JSON `column`.
    pub fn min_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> AnyQuery {
        self.reduce(AggregateFunction::Min, Some(column.into()), Some(path))
    }

    /// Each group's greatest value of `field`.
    pub fn max(self, field: impl Into<String>) -> AnyQuery {
        self.reduce(AggregateFunction::Max, Some(field.into()), None)
    }

    /// Each group's greatest value at `path` in the JSON `column`.
    pub fn max_json(self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> AnyQuery {
        self.reduce(AggregateFunction::Max, Some(column.into()), Some(path))
    }
}

/// Builds the conditions of a `where` (`ExpressionBuilder` in
/// `packages/zql/src/query/expression.ts`).
pub struct ExpressionBuilder<'a> {
    query: &'a AnyQuery,
    /// The first misuse JS's builder throws for, for `where`.
    invalid: RefCell<Option<String>>,
}

impl<'a> ExpressionBuilder<'a> {
    fn new(query: &'a AnyQuery) -> Self {
        ExpressionBuilder {
            query,
            invalid: RefCell::new(None),
        }
    }

    fn record(&self, message: String) {
        let mut invalid = self.invalid.borrow_mut();
        if invalid.is_none() {
            *invalid = Some(message);
        }
    }

    /// `field` compares to `value` by `op`.
    pub fn cmp(
        &self,
        field: impl Into<String>,
        op: SimpleOperator,
        value: impl Into<Value>,
    ) -> Condition {
        cmp_condition(field.into(), op, ValuePosition::Literal(value.into()))
    }

    /// `field` compares to a value of the context by `op`.
    pub fn cmp_param(
        &self,
        field: impl Into<String>,
        op: SimpleOperator,
        param: &StaticParameter,
    ) -> Condition {
        cmp_condition(field.into(), op, param.position())
    }

    /// A value inside the `json` column `column`, for [`Self::cmp_json`]:
    /// `path` is object keys and array indices, applied in order. At least
    /// one segment is needed, and an index must be at most
    /// [`MAX_JSON_PATH_INDEX`].
    pub fn json(&self, column: impl Into<String>, path: Vec<JsonPathSegment>) -> JsonPathReference {
        let column = column.into();
        if let Some(m) = invalid_json_path(&column, &path) {
            self.record(m);
        }
        JsonPathReference { column, path }
    }

    /// Compares the value at a JSON path with a value of the same JSON type;
    /// a value of another type never equals it. An `IN` list must hold
    /// values of one type.
    pub fn cmp_json(
        &self,
        reference: &JsonPathReference,
        op: SimpleOperator,
        value: impl Into<Value>,
    ) -> Condition {
        let value = value.into();
        if matches!(op, SimpleOperator::In | SimpleOperator::NotIn)
            && let Value::Array(list) = &value
            && !is_homogeneous(list)
        {
            self.record(format!(
                "{} against a JSON path requires a list of values of one type",
                op.as_str()
            ));
        }
        Condition::Simple {
            op,
            left: reference.position(),
            right: ValuePosition::Literal(value),
        }
    }

    /// The value at the JSON path compares to a value of the context by `op`.
    pub fn cmp_json_param(
        &self,
        reference: &JsonPathReference,
        op: SimpleOperator,
        param: &StaticParameter,
    ) -> Condition {
        Condition::Simple {
            op,
            left: reference.position(),
            right: param.position(),
        }
    }

    /// Compares two literals, neither of them a column.
    pub fn cmp_lit(
        &self,
        left: impl Into<Value>,
        op: SimpleOperator,
        right: impl Into<Value>,
    ) -> Condition {
        Condition::Simple {
            op,
            left: ValuePosition::Literal(left.into()),
            right: ValuePosition::Literal(right.into()),
        }
    }

    /// Compares a value of the context with a literal.
    pub fn cmp_lit_param(
        &self,
        left: &StaticParameter,
        op: SimpleOperator,
        right: impl Into<Value>,
    ) -> Condition {
        Condition::Simple {
            op,
            left: left.position(),
            right: ValuePosition::Literal(right.into()),
        }
    }

    /// Rows that every condition matches.
    pub fn and(&self, conditions: Vec<Condition>) -> Condition {
        and_conditions(conditions)
    }

    /// Rows that any condition matches.
    pub fn or(&self, conditions: Vec<Condition>) -> Condition {
        or_conditions(conditions)
    }

    /// Rows that `condition` doesn't match.
    pub fn not(&self, condition: Condition) -> Condition {
        negate_condition(condition)
    }

    /// Whether a related row in `relationship` matches `sub`.
    pub fn exists(
        &self,
        relationship: &str,
        options: ExistsOptions,
        sub: impl FnOnce(AnyQuery) -> AnyQuery,
    ) -> Condition {
        let (condition, invalid) = self.query.exists_condition(relationship, options, sub);
        if let Some(m) = invalid {
            self.record(m);
        }
        condition
    }
}

/// A value inside a `json` column, made by [`ExpressionBuilder::json`]
/// (`ColumnRef` in `packages/zql/src/query/expression.ts`).
#[derive(Clone, Debug, PartialEq)]
pub struct JsonPathReference {
    /// The JSON column.
    pub column: String,
    /// The path in its value.
    pub path: Vec<JsonPathSegment>,
}

impl JsonPathReference {
    fn position(&self) -> ValuePosition {
        ValuePosition::Json {
            column: self.column.clone(),
            path: self.path.clone(),
        }
    }
}

/// `TRUE`: an empty AND.
pub const TRUE_CONDITION: Condition = Condition::And(Vec::new());
/// `FALSE`: an empty OR.
pub const FALSE_CONDITION: Condition = Condition::Or(Vec::new());

fn is_always_true(c: &Condition) -> bool {
    matches!(c, Condition::And(c) if c.is_empty())
}

fn is_always_false(c: &Condition) -> bool {
    matches!(c, Condition::Or(c) if c.is_empty())
}

fn cmp_condition(field: String, op: SimpleOperator, right: ValuePosition) -> Condition {
    Condition::Simple {
        op,
        left: ValuePosition::Column(field),
        right,
    }
}

/// `and(...)`: drops the always-true conditions; one left is itself.
fn and_conditions(conditions: Vec<Condition>) -> Condition {
    let mut expressions: Vec<Condition> = conditions
        .into_iter()
        .filter(|c| !is_always_true(c))
        .collect();
    if expressions.len() == 1 {
        return expressions.pop().expect("one");
    }
    if expressions.iter().any(is_always_false) {
        return FALSE_CONDITION;
    }
    Condition::And(expressions)
}

/// `or(...)`: drops the always-false conditions; one left is itself.
fn or_conditions(conditions: Vec<Condition>) -> Condition {
    let mut expressions: Vec<Condition> = conditions
        .into_iter()
        .filter(|c| !is_always_false(c))
        .collect();
    if expressions.len() == 1 {
        return expressions.pop().expect("one");
    }
    if expressions.iter().any(is_always_true) {
        return TRUE_CONDITION;
    }
    Condition::Or(expressions)
}

/// `not`: pushes the negation down to the leaves (De Morgan).
fn negate_condition(condition: Condition) -> Condition {
    match condition {
        Condition::And(c) => Condition::Or(c.into_iter().map(negate_condition).collect()),
        Condition::Or(c) => Condition::And(c.into_iter().map(negate_condition).collect()),
        Condition::CorrelatedSubquery {
            related,
            op,
            flip,
            scalar,
        } => Condition::CorrelatedSubquery {
            related,
            op: match op {
                CorrelatedSubqueryOperator::Exists => CorrelatedSubqueryOperator::NotExists,
                CorrelatedSubqueryOperator::NotExists => CorrelatedSubqueryOperator::Exists,
            },
            flip,
            scalar,
        },
        Condition::Simple { op, left, right } => Condition::Simple {
            op: op.negated(),
            left,
            right,
        },
    }
}

impl SimpleOperator {
    /// The operator that matches exactly the rows this one does not.
    pub fn negated(self) -> SimpleOperator {
        use SimpleOperator::*;
        match self {
            Eq => Ne,
            Ne => Eq,
            Lt => Ge,
            Gt => Le,
            Ge => Lt,
            Le => Gt,
            In => NotIn,
            NotIn => In,
            Like => NotLike,
            NotLike => Like,
            Ilike => NotIlike,
            NotIlike => Ilike,
            Is => IsNot,
            IsNot => Is,
        }
    }
}

/// `simplifyCondition` (`packages/zql/src/query/expression.ts`).
pub fn simplify_condition(c: Condition) -> Condition {
    let (is_and, conditions) = match c {
        Condition::And(c) => (true, c),
        Condition::Or(c) => (false, c),
        c => return c,
    };
    if conditions.len() == 1 {
        return simplify_condition(conditions.into_iter().next().expect("one"));
    }
    let mut flattened = Vec::with_capacity(conditions.len());
    for c in conditions.into_iter().map(simplify_condition) {
        match (is_and, c) {
            (true, Condition::And(inner)) | (false, Condition::Or(inner)) => {
                flattened.extend(inner)
            }
            (_, c) => flattened.push(c),
        }
    }
    if is_and {
        if flattened.iter().any(is_always_false) {
            return FALSE_CONDITION;
        }
        Condition::And(flattened)
    } else {
        if flattened.iter().any(is_always_true) {
            return TRUE_CONDITION;
        }
        Condition::Or(flattened)
    }
}

/// `insertRelated`: adds `subquery` to a normalized `related`, after the
/// entries it compares equal to.
fn insert_related(related: &mut Option<Vec<CorrelatedSubquery>>, subquery: CorrelatedSubquery) {
    let related = related.get_or_insert_with(Vec::new);
    let mut i = related.len();
    while i > 0 && cmp_related(&related[i - 1], &subquery).is_gt() {
        i -= 1;
    }
    related.insert(i, subquery);
}

fn singular(hop: &RelationshipHop) -> Format {
    Format {
        singular: hop.cardinality == Cardinality::One,
        ..Format::default()
    }
}

fn correlation(hop: &RelationshipHop) -> Correlation {
    Correlation {
        parent_field: hop.source_field.clone(),
        child_field: hop.dest_field.clone(),
    }
}

/// `#assertGroupColumn`.
fn not_group_column(column: &str, columns: &[String]) -> Option<String> {
    if columns.iter().any(|c| c == column) {
        return None;
    }
    let names: Vec<String> = columns
        .iter()
        .map(|c| json::stringify(&Value::String(c.clone())))
        .collect();
    Some(format!(
        "orderBy(\"{column}\"): the rows can only be ordered by the grouped columns ({})",
        names.join(", ")
    ))
}

/// `makeColumnRef`'s checks of a JSON path.
fn invalid_json_path(column: &str, path: &[JsonPathSegment]) -> Option<String> {
    if path.is_empty() {
        return Some(format!(
            "json('{column}') requires at least one path segment"
        ));
    }
    path.iter().find_map(|s| match s {
        JsonPathSegment::Index(i) if *i > MAX_JSON_PATH_INDEX => Some(format!(
            "Invalid JSON path segment {i} in json('{column}', ...): a numeric segment is an array index and must be a non-negative integer no larger than {MAX_JSON_PATH_INDEX}"
        )),
        _ => None,
    })
}

/// `isSyntheticColumn` (`packages/zql/src/ivm/aggregate.ts`): the columns
/// the aggregate operators add to a row.
fn is_synthetic_column(column: &str) -> bool {
    matches!(
        column,
        "_0_key" | "_0_value" | "_0_sum" | "_0_count" | "_0_rows"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `issue.labels` through the junction `issueLabel`, as in the zql test
    /// schema.
    fn schema() -> Arc<Schema> {
        let text = r#"{
          "tables": {
            "issue": {"name": "issue", "columns": {"id": {"type": "string"}}, "primaryKey": ["id"]},
            "issueLabel": {"name": "issueLabel", "columns": {"issueId": {"type": "string"}, "labelId": {"type": "string"}}, "primaryKey": ["issueId", "labelId"]},
            "label": {"name": "label", "columns": {"id": {"type": "string"}}, "primaryKey": ["id"]}
          },
          "relationships": {"issue": {"labels": [
            {"sourceField": ["id"], "destField": ["issueId"], "destSchema": "issueLabel", "cardinality": "many"},
            {"sourceField": ["labelId"], "destField": ["id"], "destSchema": "label", "cardinality": "many"}
          ]}}
        }"#;
        Arc::new(Schema::from_json(&json::parse(text).unwrap()).unwrap())
    }

    /// No vector reaches these: JS's generator never bounds or orders a
    /// junction's destination.
    #[test]
    fn junction_destinations_take_no_limit_or_order() {
        let q = AnyQuery::new(schema(), "issue", System::Client);
        let limited = q.clone().related("labels", |l| l.limit(1));
        assert_eq!(
            limited.invalid(),
            Some(
                "Limit is not supported in junction relationships yet. Junction relationship being limited: labels"
            )
        );
        let ordered = q
            .clone()
            .related("labels", |l| l.order_by("id", Direction::Asc).limit(1));
        assert_eq!(
            ordered.invalid(),
            Some(
                "Order by is not supported in junction relationships yet. Junction relationship being ordered: labels"
            )
        );
        // The step JS throws for is left out; the rest of the query stands.
        let unbound = q.clone().limit(-1).limit(2);
        assert_eq!(unbound.invalid(), Some("Limit must be non-negative"));
        assert_eq!(unbound.ast().limit, Some(2.0));
        assert_eq!(q.related("labels", |l| l).invalid(), None);
    }
}
