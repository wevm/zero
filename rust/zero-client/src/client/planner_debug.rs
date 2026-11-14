//! The planner's debug events as text (`formatPlannerEvents` in
//! `packages/zql/src/planner/planner-debug.ts`), for
//! [`Inspector::format_planner_events`](super::Inspector::format_planner_events).

use super::inspector::PlanDebugEvent;
use crate::ast::{Condition, ValuePosition, format_json_path};
use crate::decode::JsonCodable;
use crate::json::{self, Value, number};

/// `formatPlannerEvents`: each planning attempt's connections and joins
/// with their costs, then the plan chosen.
pub(crate) fn format_planner_events(events: &[PlanDebugEvent]) -> String {
    let mut lines: Vec<String> = Vec::new();
    // By attempt, in order of first appearance (a JS `Map`).
    let mut by_attempt: Vec<(f64, Vec<&Value>)> = Vec::new();
    let mut best: Option<&Value> = None;
    for event in events {
        let e = &event.0;
        if let Some(attempt) = e.get("attemptNumber") {
            if let Some(n) = attempt.as_f64() {
                // `SameValueZero`.
                let slot = by_attempt
                    .iter_mut()
                    .find(|(a, _)| *a == n || (a.is_nan() && n.is_nan()));
                match slot {
                    Some((_, v)) => v.push(e),
                    None => by_attempt.push((n, vec![e])),
                }
            }
        } else if ty(e) == "best-plan-selected" {
            best = Some(e);
        }
    }
    for (attempt, events) in &by_attempt {
        lines.extend(attempt_summary(*attempt, events));
        lines.push(String::new());
    }
    if let Some(b) = best {
        let rule = "─".repeat(60);
        lines.push(rule.clone());
        lines.push(format!(
            "✓ Best plan: Attempt {} (cost={})",
            number::format(num(b, "bestAttemptNumber") + 1.0),
            to_fixed(num(b, "totalCost"), 2)
        ));
        let states = b
            .get("joinStates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !states.is_empty() {
            lines.push("  Join types:".into());
            for j in &states {
                lines.push(format!("    {}: {}", text(j, "join"), text(j, "type")));
            }
        }
        lines.push(rule);
    }
    lines.join("\n")
}

fn ty(e: &Value) -> &str {
    e.get("type").and_then(Value::as_str).unwrap_or("")
}

fn num(e: &Value, key: &str) -> f64 {
    e.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

fn text<'a>(e: &'a Value, key: &str) -> &'a str {
    e.get(key).and_then(Value::as_str).unwrap_or("undefined")
}

fn attempt_summary(attempt: f64, events: &[&Value]) -> Vec<String> {
    let mut lines = Vec::new();
    let total = events
        .iter()
        .find(|e| ty(e) == "attempt-start")
        .and_then(|e| e.get("totalAttempts").and_then(Value::as_f64));
    let bits = match total {
        // `Math.ceil(Math.log2(totalAttempts)) || 1`.
        Some(t) => {
            let b = t.log2().ceil();
            if b == 0.0 || b.is_nan() { 1.0 } else { b }
        }
        None => 1.0,
    };
    let pattern = binary(attempt);
    let width = if bits.is_finite() && bits > 0.0 {
        bits as usize
    } else {
        0
    };
    let pad = width.saturating_sub(pattern.encode_utf16().count());
    lines.push(format!(
        "[Attempt {}/{}] Pattern {} ({}{pattern})",
        number::format(attempt + 1.0),
        total.map_or("?".to_owned(), number::format),
        number::format(attempt),
        "0".repeat(pad),
    ));

    let node_events = |event: &str, node_type: &str| -> Vec<&Value> {
        events
            .iter()
            .copied()
            .filter(|e| {
                ty(e) == event && e.get("nodeType").and_then(Value::as_str) == Some(node_type)
            })
            .collect()
    };
    let connection_costs = node_events("node-cost", "connection");
    let connection_constraints = node_events("node-constraint", "connection");
    if !connection_costs.is_empty() {
        lines.push("  Connections:".into());
        for cost in connection_costs {
            let pattern = branch_pattern(cost);
            let constraint = connection_constraints
                .iter()
                .find(|c| c.get("node") == cost.get("node") && branch_pattern(c) == pattern)
                .and_then(|c| c.get("constraint"));
            let estimate = cost.get("costEstimate").cloned().unwrap_or_default();
            lines.push(format!("    {}:", text(cost, "node")));
            lines.extend(cost_lines(&estimate, cost));
            lines.push(format!(
                "      constraints={}",
                format_constraint(constraint)
            ));
            let filters = cost
                .get("filters")
                .and_then(|f| Condition::from_json(f).ok());
            lines.push(format!(
                "      filters={}",
                filters.as_ref().map_or("none".into(), format_filter)
            ));
            lines.push(format!(
                "      ordering={}",
                format_ordering(cost.get("ordering"))
            ));
        }
    }
    let joins = node_events("node-cost", "join");
    if !joins.is_empty() {
        lines.push("  Joins:".into());
        for cost in joins {
            let join_type = cost
                .get("joinType")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map_or(String::new(), |t| format!(" ({t})"));
            let estimate = cost.get("costEstimate").cloned().unwrap_or_default();
            lines.push(format!("    {}{join_type}:", text(cost, "node")));
            lines.extend(cost_lines(&estimate, cost));
        }
    }
    let complete = events.iter().find(|e| ty(e) == "plan-complete");
    let failed = events.iter().find(|e| ty(e) == "plan-failed");
    if let Some(c) = complete {
        lines.push(format!(
            "  ✓ Plan complete: total cost = {}",
            to_fixed(num(c, "totalCost"), 2)
        ));
    } else if let Some(f) = failed {
        lines.push(format!("  ✗ Plan failed: {}", text(f, "reason")));
    }
    lines
}

fn cost_lines(estimate: &Value, cost: &Value) -> [String; 3] {
    let limit = match estimate.get("limit").and_then(Value::as_f64) {
        Some(l) => number::format(l),
        None => "none".into(),
    };
    [
        format!(
            "      cost={}, startup={}, scan={}",
            to_fixed(num(estimate, "cost"), 2),
            to_fixed(num(estimate, "startupCost"), 2),
            to_fixed(num(estimate, "scanEst"), 2)
        ),
        format!(
            "      rows={}, selectivity={}, limit={limit}",
            to_fixed(num(estimate, "returnedRows"), 2),
            to_fixed(num(estimate, "selectivity"), 8)
        ),
        format!(
            "      downstreamChildSelectivity={}",
            to_fixed(num(cost, "downstreamChildSelectivity"), 8)
        ),
    ]
}

/// `branchPattern.join(',')`.
fn branch_pattern(e: &Value) -> String {
    e.get("branchPattern")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|n| n.as_f64().map_or(String::new(), number::format))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

fn format_constraint(constraint: Option<&Value>) -> String {
    match constraint.and_then(Value::as_object) {
        Some(o) if !o.is_empty() => {
            let keys: Vec<&str> = o.keys().map(String::as_str).collect();
            format!("{{{}}}", keys.join(", "))
        }
        _ => "{}".into(),
    }
}

fn format_value_position(v: &ValuePosition) -> String {
    match v {
        ValuePosition::Column(name) => name.clone(),
        ValuePosition::Json { column, path } => format_json_path(column, path),
        ValuePosition::Literal(Value::String(s)) => format!("'{s}'"),
        ValuePosition::Literal(l) => json::stringify(l),
        ValuePosition::Static { anchor, field } => {
            let field = match field {
                crate::ast::StaticField::Single(f) => f.clone(),
                crate::ast::StaticField::Path(p) => p.join("."),
            };
            format!("@{}.{field}", anchor.as_str())
        }
    }
}

fn format_filter(c: &Condition) -> String {
    match c {
        Condition::Simple { op, left, right } => format!(
            "{} {} {}",
            format_value_position(left),
            op.as_str(),
            format_value_position(right)
        ),
        Condition::And(cs) => format!(
            "({})",
            cs.iter()
                .map(format_filter)
                .collect::<Vec<_>>()
                .join(" AND ")
        ),
        Condition::Or(cs) => format!(
            "({})",
            cs.iter()
                .map(format_filter)
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
        Condition::CorrelatedSubquery { related, .. } => {
            format!("EXISTS({})", related.subquery.table)
        }
    }
}

fn format_ordering(ordering: Option<&Value>) -> String {
    let parts: Vec<String> = ordering
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|p| {
                    let field = p.at(0).and_then(Value::as_str).unwrap_or_default();
                    let dir = p.at(1).and_then(Value::as_str).unwrap_or_default();
                    format!("{field} {dir}")
                })
                .collect()
        })
        .unwrap_or_default();
    if parts.is_empty() {
        return "none".into();
    }
    parts.join(", ")
}

/// `Number.prototype.toString(2)` of an integer; other numbers as
/// `toString()`, which the planner never numbers attempts with.
fn binary(n: f64) -> String {
    if n.is_finite() && n == n.trunc() && n.abs() < 9_007_199_254_740_992.0 {
        let s = format!("{:b}", n.abs() as u64);
        return if n < 0.0 { format!("-{s}") } else { s };
    }
    number::format(n)
}

/// `Number.prototype.toFixed(digits)`: the exact value rounded half up.
pub(crate) fn to_fixed(x: f64, digits: usize) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.abs() >= 1e21 {
        return number::format(x);
    }
    if x < 0.0 {
        let s = to_fixed(-x, digits);
        return format!("-{s}");
    }
    // -0 is not below 0, and prints as 0.
    let x = if x == 0.0 { 0.0 } else { x };
    // A double's exact expansion has at most 1074 fraction digits.
    let exact = format!("{:.1075}", x);
    let (int, frac) = exact.split_once('.').expect("a fraction");
    let mut kept: Vec<u8> = int.bytes().chain(frac.bytes().take(digits)).collect();
    if frac.as_bytes()[digits] >= b'5' {
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, b'1');
                break;
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    let split = kept.len() - digits;
    let (int, frac) = kept.split_at(split);
    let int = std::str::from_utf8(int).expect("digits");
    if digits == 0 {
        return int.to_owned();
    }
    format!("{int}.{}", std::str::from_utf8(frac).expect("digits"))
}

#[cfg(test)]
mod tests {
    use super::to_fixed;

    #[test]
    fn to_fixed_rounds_the_exact_value_half_up() {
        assert_eq!(to_fixed(0.125, 2), "0.13");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(9.995, 2), "9.99");
        assert_eq!(to_fixed(99.996, 2), "100.00");
        assert_eq!(to_fixed(-0.001, 2), "-0.00");
        assert_eq!(to_fixed(-0.0, 2), "0.00");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(1e21, 2), "1e+21");
        assert_eq!(to_fixed(0.3, 8), "0.30000000");
    }
}
