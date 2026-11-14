//! An AST as the query builder code that makes it (`astToZQL` in
//! `packages/ast-to-zql/src/ast-to-zql.ts`), for the inspector.

use super::*;
use crate::json::{self, number};
use crate::query::SUBQUERY_ALIAS_PREFIX;

/// The builder calls after the table that make `ast`, such as
/// `.where('id', '=', 123)`: `astToZQL`. The inspector prefixes the table
/// name. With `redact_literals`, every literal and the `start` row read
/// `?`, so the code can be logged without row data or arguments.
pub fn ast_to_zql(ast: &Ast, redact_literals: bool) -> String {
    let mut code = String::new();
    if let Some(w) = &ast.where_ {
        code += &condition(w, Prefix::Where, &mut Vec::new(), redact_literals);
    }
    if let Some(related) = &ast.related {
        for r in related {
            if r.hidden == Some(true) {
                if let Some(nested) = r.subquery.related.as_ref().and_then(|n| n.first()) {
                    code += &transform_related(nested, redact_literals);
                }
            } else {
                code += &transform_related(r, redact_literals);
            }
        }
    }
    let distinct = ast.aggregate.as_ref().and_then(|a| a.distinct.as_ref());
    if let Some(group_by) = &ast.group_by {
        let cols = string_list(group_by);
        code += &match &ast.aggregate {
            Some(a) => format!(".groupBy({cols}).{}({})", a.fn_.as_str(), aggregate_arg(a)),
            None => format!(".distinct({cols})"),
        };
    } else if let Some(d) = distinct {
        code += &format!(".distinct({})", string_list(d));
    }
    if let Some(order_by) = ast.order_by.as_ref().filter(|o| !o.is_empty()) {
        code += &ordering(order_by);
    }
    if let Some(limit) = ast.limit {
        code += &format!(".limit({})", number::format(limit));
    }
    if let Some(start) = &ast.start {
        let row = if redact_literals {
            "?".to_owned()
        } else {
            json::stringify(&Value::Object(start.row.clone()))
        };
        let inclusive = if start.exclusive {
            ""
        } else {
            ", { inclusive: true }"
        };
        code += &format!(".start({row}{inclusive})");
    }
    if distinct.is_some() {
        code += ".count()";
    }
    code
}

#[derive(Clone, Copy, PartialEq)]
enum Prefix {
    Where,
    Cmp,
}

impl Prefix {
    fn as_str(self) -> &'static str {
        match self {
            Prefix::Where => ".where",
            Prefix::Cmp => "cmp",
        }
    }
}

/// The names a builder callback destructures, in the order first needed
/// (a JS `Set`).
type Args = Vec<&'static str>;

fn add(args: &mut Args, name: &'static str) {
    if !args.contains(&name) {
        args.push(name);
    }
}

/// `code` at `prefix`: a callback of its own at the top level, or the
/// enclosing callback's args extended inside one.
fn wrap(prefix: Prefix, args: &mut Args, needed: &[&'static str], code: String) -> String {
    if prefix == Prefix::Where {
        let mut sorted = needed.to_vec();
        sorted.sort_unstable();
        return format!(".where(({{{}}}) => {code})", sorted.join(", "));
    }
    for name in needed {
        add(args, name);
    }
    code
}

fn condition(c: &Condition, prefix: Prefix, args: &mut Args, redact: bool) -> String {
    match c {
        Condition::Simple { op, left, right } => {
            let l = value_position(left, redact);
            let r = value_position(right, redact);
            let code = if *op == SimpleOperator::Eq {
                format!("{l}, {r}")
            } else {
                format!("{l}, '{}', {r}", op.as_str())
            };
            if matches!(left, ValuePosition::Json { .. }) {
                return wrap(prefix, args, &["cmp", "json"], format!("cmp({code})"));
            }
            format!("{}({code})", prefix.as_str())
        }
        Condition::And(cs) | Condition::Or(cs) => {
            let is_and = matches!(c, Condition::And(_));
            if cs.len() == 1 {
                return condition(&cs[0], prefix, args, redact);
            }
            if is_and {
                let parts: Vec<String> = cs
                    .iter()
                    .map(|c| condition(c, prefix, args, redact))
                    .collect();
                if prefix == Prefix::Where {
                    return parts.concat();
                }
                add(args, "and");
                return format!("and({})", parts.join(", "));
            }
            let mut own = Args::new();
            let inner = if prefix == Prefix::Where {
                &mut own
            } else {
                &mut *args
            };
            let code: Vec<String> = cs
                .iter()
                .map(|c| condition(c, Prefix::Cmp, inner, redact))
                .collect();
            let mut needed = inner.clone();
            add(&mut needed, "cmp");
            add(&mut needed, "or");
            wrap(prefix, args, &needed, format!("or({})", code.join(", ")))
        }
        Condition::CorrelatedSubquery {
            related,
            op,
            flip,
            scalar,
        } => exists(related, *op, *flip, *scalar, prefix, args, redact),
    }
}

/// Whether a subquery has more than its table (JS's truthiness: an empty
/// `orderBy` counts, a `limit` of 0 doesn't).
fn has_props(ast: &Ast) -> bool {
    ast.where_.is_some()
        || ast.related.as_ref().is_some_and(|r| !r.is_empty())
        || ast.order_by.is_some()
        || ast.limit.is_some_and(|l| l != 0.0 && !l.is_nan())
}

fn exists(
    related: &CorrelatedSubquery,
    op: CorrelatedSubqueryOperator,
    flip: Option<bool>,
    scalar: Option<bool>,
    prefix: Prefix,
    args: &mut Args,
    redact: bool,
) -> String {
    let alias = related.subquery.alias.as_deref().unwrap_or_default();
    let relationship = alias.strip_prefix(SUBQUERY_ALIAS_PREFIX).unwrap_or(alias);
    let next = next_exists_subquery(related);
    let props = has_props(next);
    let mut parts = Vec::new();
    if let Some(f) = flip {
        parts.push(format!("flip: {f}"));
    }
    if let Some(s) = scalar {
        parts.push(format!("scalar: {s}"));
    }
    let options = if parts.is_empty() {
        String::new()
    } else {
        format!(", {{{}}}", parts.join(", "))
    };
    if op == CorrelatedSubqueryOperator::Exists {
        let sub = if props {
            format!(", q => q{}", ast_to_zql(next, redact))
        } else {
            String::new()
        };
        if prefix == Prefix::Where {
            return format!(".whereExists('{relationship}'{sub}{options})");
        }
        add(args, "exists");
        return format!("exists('{relationship}'{sub}{options})");
    }
    let sub = if props {
        format!(", q => q{}", ast_to_zql(next, redact))
    } else {
        String::new()
    };
    wrap(
        prefix,
        args,
        &["exists", "not"],
        format!("not(exists('{relationship}'{sub}{options}))"),
    )
}

/// Past the hidden hop of a junction relationship, whose two hops are both
/// EXISTS conditions.
fn next_exists_subquery(related: &CorrelatedSubquery) -> &Ast {
    if let Some(Condition::CorrelatedSubquery { related: inner, .. }) = &related.subquery.where_
        && inner
            .subquery
            .alias
            .as_deref()
            .is_some_and(|a| a.contains(&format!("{SUBQUERY_ALIAS_PREFIX}zhidden_")))
    {
        return next_exists_subquery(inner);
    }
    &related.subquery
}

fn transform_related(related: &CorrelatedSubquery, redact: bool) -> String {
    let Some(alias) = &related.subquery.alias else {
        return String::new();
    };
    let mut code = format!(".related('{alias}'");
    if has_props(&related.subquery) {
        code += ", q => q";
        code += &ast_to_zql(&related.subquery, redact);
    }
    code.push(')');
    code
}

fn ordering(order_by: &[OrderPart]) -> String {
    order_by
        .iter()
        .map(|o| format!(".orderBy('{}', '{}')", o.field, o.direction.as_str()))
        .collect()
}

fn value_position(v: &ValuePosition, redact: bool) -> String {
    match v {
        ValuePosition::Literal(_) if redact => "?".into(),
        ValuePosition::Literal(l) => literal(l),
        ValuePosition::Column(name) => format!("'{name}'"),
        ValuePosition::Json { column, path } => {
            format!("json('{column}', {})", path_segments(path))
        }
        ValuePosition::Static { field, .. } => {
            let field = match field {
                StaticField::Single(f) => format!("'{f}'"),
                StaticField::Path(p) => {
                    let parts: Vec<String> = p.iter().map(|f| format!("'{f}'")).collect();
                    format!("[{}]", parts.join(", "))
                }
            };
            format!("authParam({field})")
        }
    }
}

fn path_segments(path: &[JsonPathSegment]) -> String {
    let parts: Vec<String> = path.iter().map(|s| literal(&s.to_json())).collect();
    parts.join(", ")
}

fn literal(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Array(_) => json::stringify(v),
        Value::String(s) => js_string_literal(s),
        Value::Number(n) => number::format(*n),
        Value::Bool(b) => b.to_string(),
        Value::Object(_) => json::stringify(v),
    }
}

/// A single-quoted JS string literal for `s`, escaped as `JSON.stringify`
/// escapes it.
fn js_string_literal(s: &str) -> String {
    let quoted = json::stringify(&Value::String(s.to_owned()));
    let inner = &quoted[1..quoted.len() - 1];
    format!("'{}'", inner.replace("\\\"", "\"").replace('\'', "\\'"))
}

fn string_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| js_string_literal(s)).collect();
    parts.join(", ")
}

fn aggregate_arg(a: &Aggregate) -> String {
    let Some(field) = &a.field else {
        return String::new();
    };
    let name = js_string_literal(field);
    match &a.path {
        None => name,
        Some(path) => format!("({{json}}) => json({name}, {})", path_segments(path)),
    }
}
