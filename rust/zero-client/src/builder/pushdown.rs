//! Correlated predicate pushdown
//! (`packages/zql/src/builder/correlated-predicate-pushdown.ts`).

use crate::ast::{Ast, Condition, CorrelatedSubquery, SimpleOperator, ValuePosition};
use crate::json::Value;
use crate::map::OrderedMap;
use crate::query::simplify_condition;
use crate::schema::{Column, ValueType};

/// The longest `IN` list copied into a child.
pub const MAX_PUSHED_IN_VALUES: usize = 32;

/// Copies a parent's conditions on correlation columns into its subqueries.
/// The result is the same; child rows that cannot join to a parent that
/// matters are filtered out early.
pub fn push_down_correlated_predicates(
    ast: &Ast,
    columns_of: &dyn Fn(&str) -> OrderedMap<Column>,
) -> Ast {
    Pushdown { columns_of }.visit(ast)
}

struct Pushdown<'a> {
    columns_of: &'a dyn Fn(&str) -> OrderedMap<Column>,
}

impl Pushdown<'_> {
    fn visit(&self, ast: &Ast) -> Ast {
        let where_ = ast
            .where_
            .as_ref()
            .map(|w| self.visit_condition(w, &ast.table, &[]));
        let facts = where_.as_ref().map(simple_conjuncts).unwrap_or_default();
        let mut result = ast.clone();
        result.related = ast.related.as_ref().map(|related| {
            related
                .iter()
                .map(|csq| {
                    let mut c = csq.clone();
                    c.subquery = Box::new(self.visit(&self.push_into(csq, &facts, &ast.table)));
                    c
                })
                .collect()
        });
        result.where_ = where_;
        result
    }

    fn visit_condition(&self, c: &Condition, table: &str, facts: &[Condition]) -> Condition {
        match c {
            Condition::Simple { .. } => c.clone(),
            Condition::And(_) => {
                let mut all = facts.to_vec();
                all.extend(simple_conjuncts(c));
                self.visit_conjunction(c, table, &all)
            }
            Condition::Or(cs) => Condition::Or(
                cs.iter()
                    .map(|x| self.visit_condition(x, table, facts))
                    .collect(),
            ),
            Condition::CorrelatedSubquery {
                related,
                op,
                flip,
                scalar,
            } => {
                let mut related = related.clone();
                related.subquery = Box::new(self.visit(&self.push_into(&related, facts, table)));
                Condition::CorrelatedSubquery {
                    related,
                    op: *op,
                    flip: *flip,
                    scalar: *scalar,
                }
            }
        }
    }

    /// `facts` already has the simple conjuncts of `c` and its nested ANDs.
    fn visit_conjunction(&self, c: &Condition, table: &str, facts: &[Condition]) -> Condition {
        let Condition::And(cs) = c else {
            return c.clone();
        };
        Condition::And(
            cs.iter()
                .map(|x| match x {
                    Condition::And(_) => self.visit_conjunction(x, table, facts),
                    _ => self.visit_condition(x, table, facts),
                })
                .collect(),
        )
    }

    fn push_into(&self, csq: &CorrelatedSubquery, facts: &[Condition], parent_table: &str) -> Ast {
        let parent_field = &csq.correlation.parent_field;
        let child_field = &csq.correlation.child_field;
        let subquery = &*csq.subquery;
        let mut conjuncts = subquery
            .where_
            .as_ref()
            .map(simple_conjuncts)
            .unwrap_or_default();
        let mut added: Vec<Condition> = Vec::new();
        let parent_columns = (self.columns_of)(parent_table);
        let child_columns = (self.columns_of)(&subquery.table);
        for fact in facts {
            if !is_pushable(fact) {
                continue;
            }
            let Condition::Simple {
                op,
                left: ValuePosition::Column(name),
                right,
            } = fact
            else {
                continue;
            };
            for (p, c) in parent_field.iter().zip(child_field) {
                if p == name && have_same_scalar_type(parent_columns.get(p), child_columns.get(c)) {
                    let copy = Condition::Simple {
                        op: *op,
                        left: ValuePosition::Column(c.clone()),
                        right: right.clone(),
                    };
                    if !conjuncts.iter().any(|x| is_same_condition(x, &copy)) {
                        conjuncts.push(copy.clone());
                        added.push(copy);
                    }
                }
            }
        }
        if added.is_empty() {
            return subquery.clone();
        }
        let mut result = subquery.clone();
        let mut all: Vec<Condition> = subquery.where_.iter().cloned().collect();
        all.extend(added);
        result.where_ = Some(simplify_condition(Condition::And(all)));
        result
    }
}

/// The simple conjuncts of `c`, including those of nested ANDs.
fn simple_conjuncts(c: &Condition) -> Vec<Condition> {
    match c {
        Condition::Simple { .. } => vec![c.clone()],
        Condition::And(cs) => cs.iter().flat_map(simple_conjuncts).collect(),
        _ => Vec::new(),
    }
}

fn is_pushable(c: &Condition) -> bool {
    let Condition::Simple {
        op,
        left: ValuePosition::Column(_),
        right: ValuePosition::Literal(value),
    } = c
    else {
        return false;
    };
    match op {
        SimpleOperator::Eq | SimpleOperator::Is => true,
        SimpleOperator::In => matches!(value, Value::Array(a) if a.len() <= MAX_PUSHED_IN_VALUES),
        _ => false,
    }
}

fn is_same_condition(a: &Condition, b: &Condition) -> bool {
    match (a, b) {
        (
            Condition::Simple {
                op: op_a,
                left: ValuePosition::Column(name_a),
                right: ValuePosition::Literal(va),
            },
            Condition::Simple {
                op: op_b,
                left: ValuePosition::Column(name_b),
                right: ValuePosition::Literal(vb),
            },
        ) => op_a == op_b && name_a == name_b && va == vb,
        _ => false,
    }
}

fn have_same_scalar_type(parent: Option<&Column>, child: Option<&Column>) -> bool {
    match (parent, child) {
        (Some(p), Some(c)) => p.type_ == c.type_ && p.type_ != ValueType::Json,
        _ => false,
    }
}
