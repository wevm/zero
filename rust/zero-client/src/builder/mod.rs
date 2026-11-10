//! Builds IVM pipelines from ASTs (`packages/zql/src/builder/builder.ts`).
//!
//! This is the compute mode, where every operator runs on the client's own
//! rows. The synced client's mode, which reads aggregates and groups from
//! the rows the server streams for them, comes with the client engine.

mod pushdown;

pub use pushdown::*;

use crate::ast::{
    Aggregate as AggregateSpec, AggregateFunction, Ast, Condition, CorrelatedSubquery,
    CorrelatedSubqueryOperator, Direction, OrderPart, System,
};
use crate::ivm::*;
use crate::json::number;
use crate::keys::AGGREGATE_TABLE_PREFIX;
use crate::map::OrderedMap;
use crate::schema::Column;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

/// Connects a pipeline being built to its environment (`BuilderDelegate`).
pub trait BuilderDelegate {
    fn get_source(&self, table_name: &str) -> Option<Rc<dyn Source>>;
    /// Apply `where` filters even when the source applies them all.
    fn apply_filters_anyway(&self) -> bool {
        false
    }
    /// Allow NOT EXISTS. Only the server does: a client cannot tell a row
    /// that does not exist from one that is not synced.
    fn enable_not_exists(&self) -> bool {
        false
    }
    /// Do not copy a parent's conditions on correlation columns into its
    /// subqueries.
    fn disable_correlated_predicate_pushdown(&self) -> bool {
        false
    }
    fn map_ast(&self, ast: &Ast) -> Ast {
        ast.clone()
    }
}

/// The limit put on EXISTS subqueries: one row is enough, a few more keep a
/// removal from emptying it.
pub const EXISTS_LIMIT: f64 = 3.0;
pub const PERMISSIONS_EXISTS_LIMIT: f64 = 1.0;

type Result<T> = std::result::Result<T, PipelineError>;

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(PipelineError::new(message))
}

/// Builds a pipeline from an AST (`buildPipeline`).
pub fn build_pipeline(
    ast: &Ast,
    delegate: &dyn BuilderDelegate,
    query_id: &str,
) -> Result<Rc<dyn Input>> {
    let mut ast = delegate.map_ast(ast);
    if ast.group_by.is_some() && ast.aggregate.as_ref().is_some_and(|a| a.distinct.is_some()) {
        return err("a count of distinct values per group is not supported yet");
    }
    if ast.group_by.is_some() || ast.aggregate.as_ref().is_some_and(|a| a.distinct.is_some()) {
        // A grouping's input is sorted by its columns first, so the rows of
        // a group are adjacent (see GroupBy).
        ast.order_by = Some(group_ordering(&ast));
    }
    ast = complete_ordering(&ast, &|table| match delegate.get_source(table) {
        Some(s) => Ok(s.table_schema().primary_key.clone()),
        None => err(format!("Source not found: {table}")),
    })?;
    if !delegate.disable_correlated_predicate_pushdown() {
        let columns_of = |table: &str| -> OrderedMap<Column> {
            delegate
                .get_source(table)
                .map(|s| s.table_schema().columns.clone())
                .unwrap_or_default()
        };
        ast = push_down_correlated_predicates(&ast, &columns_of);
    }
    Builder { delegate, query_id }.build(&ast, "", None, false)
}

/// Fails if `condition` has a NOT EXISTS outside nested subqueries.
fn assert_no_not_exists(condition: &Condition) -> Result<()> {
    match condition {
        Condition::Simple { .. } => Ok(()),
        Condition::CorrelatedSubquery {
            op: CorrelatedSubqueryOperator::NotExists,
            ..
        } => err(
            "not(exists()) is not supported on the client - see https://bugs.rocicorp.dev/issue/3438",
        ),
        Condition::CorrelatedSubquery { .. } => Ok(()),
        Condition::And(cs) | Condition::Or(cs) => cs.iter().try_for_each(assert_no_not_exists),
    }
}

struct CsqCondition {
    related: CorrelatedSubquery,
    op: CorrelatedSubqueryOperator,
    flip: Option<bool>,
}

struct Builder<'a> {
    delegate: &'a dyn BuilderDelegate,
    query_id: &'a str,
}

impl Builder<'_> {
    fn build(
        &self,
        ast_in: &Ast,
        name: &str,
        partition_key: Option<&[String]>,
        is_non_flipped_exists_child: bool,
    ) -> Result<Rc<dyn Input>> {
        if ast_in.group_by.is_some() && !name.is_empty() {
            return err("groupBy is only supported at the top level");
        }
        let Some(source) = self.delegate.get_source(&ast_in.table) else {
            return err(format!("Source not found: {}", ast_in.table));
        };
        let ast = uniquify_correlated_subquery_condition_aliases(ast_in);
        if !self.delegate.enable_not_exists()
            && let Some(w) = &ast.where_
        {
            assert_no_not_exists(w)?;
        }

        let csq_conditions = gather_correlated_subquery_conditions(ast.where_.as_ref());
        let mut split_edit_keys: HashSet<String> =
            partition_key.unwrap_or_default().iter().cloned().collect();
        for csq in &csq_conditions {
            split_edit_keys.extend(csq.related.correlation.parent_field.iter().cloned());
        }
        for csq in ast.related.iter().flatten() {
            split_edit_keys.extend(csq.correlation.parent_field.iter().cloned());
        }
        if is_non_flipped_exists_child {
            // JS asserts these, which throws.
            if ast.start.is_some() {
                return err("EXISTS subqueries must not have start");
            }
            if ast.related.is_some() {
                return err("EXISTS subqueries must not have related");
            }
        }

        // Cap needs an unordered connection, but a flipped subquery in the
        // where builds a UnionFanIn, which needs order. Fall back to Take
        // then.
        let use_cap = is_non_flipped_exists_child
            && !ast
                .where_
                .as_ref()
                .is_some_and(condition_includes_flipped_subquery_at_any_level);

        let sort = if use_cap {
            None
        } else {
            Some(ast.order_by.clone().expect("ordering is complete"))
        };
        let conn = source.connect(sort, ast.where_.as_ref(), Some(&split_edit_keys))?;
        let fully_applied_filters = conn.fully_applied_filters();
        let mut end: Rc<dyn Input> = conn;

        if let Some(start) = &ast.start {
            end = Skip::new(end, start);
        }

        let mut take_gate: Option<Rc<TakeGate>> = None;
        if ast.limit.is_some() && !use_cap {
            let gate = TakeGate::new(end);
            end = gate.clone();
            take_gate = Some(gate);
        }
        let bound_provider = take_gate.clone().map(|g| g as Rc<dyn TakeBoundProvider>);

        // The parent indexes of the EXISTS joins, kept by a JoinIndexTap
        // after the limit.
        let mut exists_parent_indexes: Vec<Rc<JoinIndex>> = Vec::new();
        for csq in csq_conditions.iter().filter(|c| c.flip != Some(true)) {
            let parent_index = JoinIndex::new(
                csq.related.correlation.parent_field.clone(),
                end.get_schema().primary_key.clone(),
                None,
            );
            exists_parent_indexes.push(parent_index.clone());
            let mut related = csq.related.clone();
            related.subquery.limit = Some(if related.system == Some(System::Permissions) {
                PERMISSIONS_EXISTS_LIMIT
            } else {
                EXISTS_LIMIT
            });
            end = self.apply_correlated_subquery(
                &related,
                end,
                name,
                true,
                None,
                bound_provider.clone(),
                Some(ExistsJoin {
                    op: csq.op,
                    parent_index,
                }),
            )?;
        }

        if let Some(w) = &ast.where_
            && (!fully_applied_filters || self.delegate.apply_filters_anyway())
        {
            end = self.apply_where(
                end,
                w,
                name,
                partition_key,
                bound_provider.clone(),
                &mut exists_parent_indexes,
                true,
            )?;
        }

        if let Some(limit_value) = ast.limit {
            if !(limit_value >= 0.0 && limit_value.fract() == 0.0) {
                return err(format!("Invalid limit {}", number::format(limit_value)));
            }
            let limit = limit_value as usize;
            if use_cap {
                end = Cap::new(end, limit, partition_key.map(<[String]>::to_vec));
            } else {
                let take = Take::new(end, limit, partition_key.map(<[String]>::to_vec))?;
                if let Some(gate) = &take_gate {
                    gate.set_bound_provider(Rc::downgrade(&take) as Weak<dyn TakeBoundProvider>);
                    take.set_take_gate(gate);
                }
                end = take;
            }
        }

        if !exists_parent_indexes.is_empty() {
            end = JoinIndexTap::new(end, exists_parent_indexes);
        }

        if ast.group_by.is_some() {
            // A grouping, compute mode: a distinct is a GroupBy with a count,
            // which a view doesn't show.
            if ast.start.is_some() || ast.limit.is_some() || ast.related.is_some() {
                return err("groupBy does not support start, limit or related");
            }
            return Ok(GroupBy::new(
                end,
                &group_ordering(&ast),
                group_table_name(self.query_id),
                reducer_of(&ast),
            )?);
        }

        if let Some(aggregate) = &ast.aggregate {
            // A top-level aggregate, compute mode: one row, emitted to
            // `aggregate:<queryID>`, reducing the rows that survive `where`,
            // `start` and `limit`, so `limit(10).count()` counts at most 10
            // rows, as in SQL.
            let count = AggregateSpec::new(AggregateFunction::Count);
            if aggregate.distinct.is_some() {
                // A count of the distinct combinations counts a GroupBy's
                // groups.
                if ast.start.is_some() || ast.limit.is_some() {
                    return err("a count of distinct values does not support start or limit");
                }
                end = GroupBy::new(
                    end,
                    &group_ordering(&ast),
                    group_table_name(self.query_id),
                    count.clone(),
                )?;
            }
            let reduced = if aggregate.distinct.is_none() {
                aggregate.clone()
            } else {
                count
            };
            return Ok(Aggregate::new(
                end,
                Vec::new(),
                reduced,
                Some(top_level_aggregate_table_name(self.query_id)),
            )?);
        }

        if let Some(related) = &ast.related {
            // Dedupe by alias, last one wins, keeping the first one's
            // position.
            let mut by_alias: Vec<(&str, &CorrelatedSubquery)> = Vec::new();
            for csq in related {
                let alias = csq.subquery.alias.as_deref().unwrap_or("");
                match by_alias.iter_mut().find(|(a, _)| *a == alias) {
                    Some(entry) => entry.1 = csq,
                    None => by_alias.push((alias, csq)),
                }
            }
            for (_, csq) in by_alias {
                end = self.apply_correlated_subquery(
                    csq,
                    end,
                    name,
                    false,
                    partition_key,
                    None,
                    None,
                )?;
            }
        }
        Ok(end)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_where(
        &self,
        input: Rc<dyn Input>,
        condition: &Condition,
        name: &str,
        parent_partition_key: Option<&[String]>,
        bound_provider: Option<Rc<dyn TakeBoundProvider>>,
        exists_parent_indexes: &mut Vec<Rc<JoinIndex>>,
        track_indexes: bool,
    ) -> Result<Rc<dyn Input>> {
        if !condition_includes_flipped_subquery_at_any_level(condition) {
            return build_filter_pipeline(
                input,
                |f| self.apply_filter(f, condition, name),
                transform_filters(Some(condition)).0,
            );
        }
        self.apply_filter_with_flips(
            input,
            condition,
            name,
            parent_partition_key,
            bound_provider,
            exists_parent_indexes,
            track_indexes,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_filter_with_flips(
        &self,
        input: Rc<dyn Input>,
        condition: &Condition,
        name: &str,
        parent_partition_key: Option<&[String]>,
        bound_provider: Option<Rc<dyn TakeBoundProvider>>,
        exists_parent_indexes: &mut Vec<Rc<JoinIndex>>,
        track_indexes: bool,
    ) -> Result<Rc<dyn Input>> {
        let mut end = input;
        match condition {
            Condition::Simple { .. } => panic!("Simple conditions cannot have flips"),
            Condition::And(conditions) => {
                let (with_flipped, without_flipped): (Vec<_>, Vec<_>) = conditions
                    .iter()
                    .cloned()
                    .partition(condition_includes_flipped_subquery_at_any_level);
                if !without_flipped.is_empty() {
                    let and = Condition::And(without_flipped.clone());
                    end = build_filter_pipeline(
                        end,
                        |f| self.apply_and(f, &without_flipped, name),
                        transform_filters(Some(&and)).0,
                    )?;
                }
                assert!(!with_flipped.is_empty(), "Impossible to have no flips here");
                for cond in &with_flipped {
                    end = self.apply_filter_with_flips(
                        end,
                        cond,
                        name,
                        parent_partition_key,
                        bound_provider.clone(),
                        exists_parent_indexes,
                        track_indexes,
                    )?;
                }
            }
            Condition::Or(conditions) => {
                let (with_flipped, without_flipped): (Vec<_>, Vec<_>) = conditions
                    .iter()
                    .cloned()
                    .partition(condition_includes_flipped_subquery_at_any_level);
                assert!(!with_flipped.is_empty(), "Impossible to have no flips here");
                let ufo = UnionFanOut::new(end);
                let ufo_input: Rc<dyn Input> = ufo.clone();
                let mut branches: Vec<Rc<dyn Input>> = Vec::new();
                if !without_flipped.is_empty() {
                    let or = Condition::Or(without_flipped.clone());
                    branches.push(build_filter_pipeline(
                        ufo_input.clone(),
                        |f| self.apply_or(f, &without_flipped, name),
                        transform_filters(Some(&or)).0,
                    )?);
                }
                for cond in &with_flipped {
                    branches.push(self.apply_filter_with_flips(
                        ufo_input.clone(),
                        cond,
                        name,
                        parent_partition_key,
                        bound_provider.clone(),
                        exists_parent_indexes,
                        track_indexes,
                    )?);
                }
                end = UnionFanIn::new(&ufo, branches);
            }
            Condition::CorrelatedSubquery { related: sq, .. } => {
                let alias = sq
                    .subquery
                    .alias
                    .clone()
                    .expect("Subquery must have an alias");
                let child = Builder {
                    delegate: self.delegate,
                    query_id: "",
                }
                .build(
                    &sq.subquery,
                    &format!("{name}.{alias}"),
                    Some(&sq.correlation.child_field),
                    false,
                )?;
                let parent_index = track_indexes.then(|| {
                    let index = JoinIndex::new(
                        sq.correlation.parent_field.clone(),
                        end.get_schema().primary_key.clone(),
                        parent_partition_key.map(<[String]>::to_vec),
                    );
                    exists_parent_indexes.push(index.clone());
                    index
                });
                end = FlippedJoin::new(FlippedJoinArgs {
                    parent: end,
                    child,
                    parent_key: sq.correlation.parent_field.clone(),
                    child_key: sq.correlation.child_field.clone(),
                    relationship_name: alias,
                    hidden: sq.hidden.unwrap_or(false),
                    system: sq.system.unwrap_or(System::Client),
                    parent_partition_key: parent_partition_key.map(<[String]>::to_vec),
                    bound_provider,
                    parent_index,
                });
            }
        }
        Ok(end)
    }

    fn apply_filter(
        &self,
        input: Rc<dyn FilterInput>,
        condition: &Condition,
        name: &str,
    ) -> Result<Rc<dyn FilterInput>> {
        match condition {
            Condition::And(cs) => self.apply_and(input, cs, name),
            Condition::Or(cs) => self.apply_or(input, cs, name),
            Condition::CorrelatedSubquery { related, op, .. } => {
                Ok(apply_correlated_subquery_condition(input, related, *op))
            }
            Condition::Simple { .. } => Ok(Filter::new(input, create_predicate(condition)?)),
        }
    }

    fn apply_and(
        &self,
        input: Rc<dyn FilterInput>,
        conditions: &[Condition],
        name: &str,
    ) -> Result<Rc<dyn FilterInput>> {
        let mut input = input;
        for c in conditions {
            input = self.apply_filter(input, c, name)?;
        }
        Ok(input)
    }

    fn apply_or(
        &self,
        input: Rc<dyn FilterInput>,
        conditions: &[Condition],
        name: &str,
    ) -> Result<Rc<dyn FilterInput>> {
        let (other, subquery): (Vec<_>, Vec<_>) = conditions
            .iter()
            .cloned()
            .partition(is_not_and_does_not_contain_subquery);
        // Without subqueries no fan-out is needed.
        if subquery.is_empty() {
            return Ok(Filter::new(input, create_predicate(&Condition::Or(other))?));
        }
        let fan_out = FanOut::new(input);
        let fan_out_input: Rc<dyn FilterInput> = fan_out.clone();
        let mut branches: Vec<Rc<dyn FilterInput>> = Vec::new();
        for c in &subquery {
            branches.push(self.apply_filter(fan_out_input.clone(), c, name)?);
        }
        if !other.is_empty() {
            branches.push(Filter::new(
                fan_out_input,
                create_predicate(&Condition::Or(other))?,
            ));
        }
        let fan_in = FanIn::new(&fan_out, branches);
        fan_out.set_fan_in(&fan_in);
        Ok(fan_in)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_correlated_subquery(
        &self,
        sq: &CorrelatedSubquery,
        end: Rc<dyn Input>,
        name: &str,
        from_condition: bool,
        parent_partition_key: Option<&[String]>,
        bound_provider: Option<Rc<dyn TakeBoundProvider>>,
        exists: Option<ExistsJoin>,
    ) -> Result<Rc<dyn Input>> {
        // Only omitted for conditions: `related` with `limit(0)` still gets
        // an empty array.
        if sq.subquery.limit == Some(0.0) && from_condition {
            return Ok(end);
        }
        let alias = sq
            .subquery
            .alias
            .clone()
            .expect("Subquery must have an alias");
        let mut child = self.build(
            &sq.subquery,
            &child_pipeline_name(name, sq),
            Some(&sq.correlation.child_field),
            from_condition,
        )?;
        if let Some(aggregate) = &sq.aggregate {
            // An aggregate relationship, compute mode: an Aggregate reduces
            // the child rows to one row per parent, carrying the correlation
            // key, so the Join routes it as it would the children.
            if aggregate.distinct.is_some() {
                return err("a count of distinct values is not supported in a relationship yet");
            }
            let mut aggregated = aggregate.clone();
            if let Some(dest) = sq.subquery.related.as_ref().and_then(|r| r.first()) {
                // A junction aggregate: the field lives on the destination,
                // one hop past the junction row. LiftField copies it (or its
                // JSON leaf) onto the junction row. `count` never takes this
                // path: it is a plain count over the junction.
                let (Some(dest_alias), Some(field)) = (&dest.subquery.alias, &aggregate.field)
                else {
                    panic!("a junction aggregate needs a destination alias and a field");
                };
                child = LiftField::new(
                    child,
                    dest_alias.clone(),
                    field.clone(),
                    aggregate.path.is_some().then(|| aggregate.clone()),
                );
                aggregated = AggregateSpec {
                    field: Some(LIFTED_FIELD_COLUMN.to_owned()),
                    path: None,
                    ..AggregateSpec::new(aggregate.fn_)
                };
            }
            child = Aggregate::new(
                child,
                sq.correlation.child_field.clone(),
                aggregated,
                Some(aggregate_table_name(self.query_id, sq, name)),
            )?;
        }
        Ok(Join::new(JoinArgs {
            parent: end,
            child,
            parent_key: sq.correlation.parent_field.clone(),
            child_key: sq.correlation.child_field.clone(),
            relationship_name: alias,
            hidden: sq.hidden.unwrap_or(false),
            system: sq.system.unwrap_or(System::Client),
            parent_partition_key: if from_condition {
                None
            } else {
                parent_partition_key.map(<[String]>::to_vec)
            },
            bound_provider,
            exists,
        }))
    }
}

fn build_filter_pipeline(
    input: Rc<dyn Input>,
    pipeline: impl FnOnce(Rc<dyn FilterInput>) -> Result<Rc<dyn FilterInput>>,
    condition: Option<Condition>,
) -> Result<Rc<dyn Input>> {
    let start = FilterStart::new(input, condition);
    let middle = pipeline(start.clone())?;
    Ok(FilterEnd::new(start, middle))
}

fn is_not_and_does_not_contain_subquery(condition: &Condition) -> bool {
    match condition {
        Condition::CorrelatedSubquery { .. } => false,
        Condition::Simple { .. } => true,
        Condition::And(cs) | Condition::Or(cs) => {
            cs.iter().all(is_not_and_does_not_contain_subquery)
        }
    }
}

fn apply_correlated_subquery_condition(
    input: Rc<dyn FilterInput>,
    related: &CorrelatedSubquery,
    op: CorrelatedSubqueryOperator,
) -> Rc<dyn FilterInput> {
    if related.subquery.limit == Some(0.0) {
        let result = op != CorrelatedSubqueryOperator::Exists;
        return Filter::new(input, Rc::new(move |_: &Row| result));
    }
    let alias = related
        .subquery
        .alias
        .clone()
        .expect("Subquery must have an alias");
    Exists::new(input, alias, related.correlation.parent_field.clone(), op)
}

fn gather_correlated_subquery_conditions(condition: Option<&Condition>) -> Vec<CsqCondition> {
    fn gather(c: &Condition, out: &mut Vec<CsqCondition>) {
        match c {
            Condition::CorrelatedSubquery {
                related, op, flip, ..
            } => out.push(CsqCondition {
                related: related.clone(),
                op: *op,
                flip: *flip,
            }),
            Condition::And(cs) | Condition::Or(cs) => cs.iter().for_each(|x| gather(x, out)),
            Condition::Simple { .. } => {}
        }
    }
    let mut out = Vec::new();
    if let Some(c) = condition {
        gather(c, &mut out);
    }
    out
}

/// Suffixes the aliases of a where's subqueries with a counter so that each
/// gets its own relationship.
fn uniquify_correlated_subquery_condition_aliases(ast: &Ast) -> Ast {
    let Some(w @ (Condition::And(_) | Condition::Or(_))) = &ast.where_ else {
        return ast.clone();
    };
    fn uniquify(c: &Condition, count: &mut usize) -> Condition {
        match c {
            Condition::Simple { .. } => c.clone(),
            Condition::CorrelatedSubquery {
                related,
                op,
                flip,
                scalar,
            } => {
                let mut related = related.clone();
                let alias = related.subquery.alias.take().unwrap_or_default();
                related.subquery.alias = Some(format!("{alias}_{count}"));
                *count += 1;
                Condition::CorrelatedSubquery {
                    related,
                    op: *op,
                    flip: *flip,
                    scalar: *scalar,
                }
            }
            Condition::And(cs) => Condition::And(cs.iter().map(|x| uniquify(x, count)).collect()),
            Condition::Or(cs) => Condition::Or(cs.iter().map(|x| uniquify(x, count)).collect()),
        }
    }
    let mut result = ast.clone();
    result.where_ = Some(uniquify(w, &mut 0));
    result
}

fn condition_includes_flipped_subquery_at_any_level(c: &Condition) -> bool {
    match c {
        Condition::CorrelatedSubquery { flip, .. } => flip.unwrap_or(false),
        Condition::And(cs) | Condition::Or(cs) => cs
            .iter()
            .any(condition_includes_flipped_subquery_at_any_level),
        Condition::Simple { .. } => false,
    }
}

type PrimaryKeyOf<'a> = dyn Fn(&str) -> Result<Vec<String>> + 'a;

/// `completeOrdering`: appends missing primary key columns to every
/// ordering in the AST.
pub fn complete_ordering(ast: &Ast, primary_key_of: &PrimaryKeyOf) -> Result<Ast> {
    let primary_key = primary_key_of(&ast.table)?;
    let mut result = ast.clone();
    if let Some(related) = &ast.related {
        let mut out = Vec::new();
        for r in related {
            let mut c = r.clone();
            c.subquery = Box::new(complete_ordering(&r.subquery, primary_key_of)?);
            out.push(c);
        }
        result.related = Some(out);
    }
    if let Some(w) = &ast.where_ {
        result.where_ = Some(complete_ordering_in_condition(w, primary_key_of)?);
    }
    result.order_by = Some(add_primary_keys(&primary_key, ast.order_by.as_deref()));
    Ok(result)
}

fn complete_ordering_in_condition(
    c: &Condition,
    primary_key_of: &PrimaryKeyOf,
) -> Result<Condition> {
    Ok(match c {
        Condition::Simple { .. } => c.clone(),
        Condition::CorrelatedSubquery {
            related,
            op,
            flip,
            scalar,
        } => {
            let mut related = related.clone();
            related.subquery = Box::new(complete_ordering(&related.subquery, primary_key_of)?);
            Condition::CorrelatedSubquery {
                related,
                op: *op,
                flip: *flip,
                scalar: *scalar,
            }
        }
        Condition::And(cs) => Condition::And(
            cs.iter()
                .map(|x| complete_ordering_in_condition(x, primary_key_of))
                .collect::<Result<_>>()?,
        ),
        Condition::Or(cs) => Condition::Or(
            cs.iter()
                .map(|x| complete_ordering_in_condition(x, primary_key_of))
                .collect::<Result<_>>()?,
        ),
    })
}

fn add_primary_keys(primary_key: &[String], order_by: Option<&[OrderPart]>) -> Vec<OrderPart> {
    let order_by = order_by.unwrap_or_default();
    let mut to_add: Vec<&String> = Vec::new();
    for k in primary_key {
        if !to_add.contains(&k) && !order_by.iter().any(|p| &p.field == k) {
            to_add.push(k);
        }
    }
    let mut result = order_by.to_vec();
    result.extend(to_add.into_iter().map(|k| OrderPart {
        field: k.clone(),
        direction: Direction::Asc,
    }));
    result
}

/// `childPipelineName`: the parent's pipeline name, a `.`, and the escaped
/// alias. An alias can be any string, so it is escaped to keep the path
/// unambiguous and free of the `/` and `:` a table name must not contain.
fn child_pipeline_name(name: &str, sq: &CorrelatedSubquery) -> String {
    format!(
        "{name}.{}",
        encode_uri_component(sq.subquery.alias.as_deref().unwrap_or(""), true)
    )
}

/// `aggregateTableName`: `aggregate:<queryID>:<alias path>`, the table of an
/// aggregate relationship's rows. `parent_name` is the parent's pipeline
/// name.
pub fn aggregate_table_name(query_id: &str, sq: &CorrelatedSubquery, parent_name: &str) -> String {
    let path = child_pipeline_name(parent_name, sq);
    format!("{AGGREGATE_TABLE_PREFIX}{query_id}:{}", &path[1..])
}

/// `topLevelAggregateTableName`: `aggregate:<queryID>`.
pub fn top_level_aggregate_table_name(query_id: &str) -> String {
    format!("{AGGREGATE_TABLE_PREFIX}{query_id}")
}

/// `groupTableName`: `aggregate:<queryID>:#group`, the table of a grouped
/// query's rows. An escaped alias never has a `#`.
pub fn group_table_name(query_id: &str) -> String {
    format!("{AGGREGATE_TABLE_PREFIX}{query_id}:#group")
}

/// `reducerOf`: what a grouped query reduces each group to, its aggregate
/// or a count.
fn reducer_of(ast: &Ast) -> AggregateSpec {
    ast.aggregate
        .clone()
        .unwrap_or_else(|| AggregateSpec::new(AggregateFunction::Count))
}

/// `groupViewInput`: the input of a grouped query's view: its rows reduced
/// to the group columns and, for a `groupBy()` with an aggregate, the value
/// under the function's name, by the names of the pipeline's AST.
pub fn group_view_input(
    input: Rc<dyn Input>,
    ast: &Ast,
    delegate: &dyn BuilderDelegate,
) -> Result<Rc<dyn Input>> {
    let view_names = delegate.map_ast(ast).group_by.unwrap_or_default();
    let columns = view_names
        .into_iter()
        .map(|n| ProjectedColumn {
            out: n.clone(),
            from: n,
        })
        .collect();
    Ok(GroupProjection::new(
        input,
        columns,
        ast.aggregate.as_ref().map(|a| a.fn_.as_str().to_owned()),
    )?)
}

/// JS's `encodeURIComponent`: every UTF-8 byte but the letters, digits and
/// `-_.!~*'()` as `%XX`; with `escaping_dots`, `.` too.
pub fn encode_uri_component(s: &str, escaping_dots: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(b as char),
            b'.' if !escaping_dots => out.push('.'),
            b'-' | b'_' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => out.push(b as char),
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
        }
    }
    out
}
