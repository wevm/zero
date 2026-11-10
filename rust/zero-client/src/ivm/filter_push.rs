//! `filterPush` (`packages/zql/src/ivm/filter-push.ts`).

use super::{Change, InputBase, Node, Output, RowPredicate};

/// Pushes `change` to `output` if its row passes `predicate`; an edit whose
/// row enters or leaves the predicate becomes an add or a remove.
pub fn filter_push(
    change: Change,
    output: &dyn Output,
    pusher: &dyn InputBase,
    predicate: Option<&RowPredicate>,
) {
    let Some(predicate) = predicate else {
        return output.push(change, pusher);
    };
    match change {
        Change::Edit { node, old_node } => {
            maybe_split_and_push_edit_change(node, old_node, predicate, output, pusher)
        }
        change => {
            if predicate(&change.node().row) {
                output.push(change, pusher)
            }
        }
    }
}

/// `maybeSplitAndPushEditChange`.
pub fn maybe_split_and_push_edit_change(
    node: Node,
    old_node: Node,
    predicate: &RowPredicate,
    output: &dyn Output,
    pusher: &dyn InputBase,
) {
    let old_was_present = predicate(&old_node.row);
    let new_is_present = predicate(&node.row);
    match (old_was_present, new_is_present) {
        (true, true) => output.push(Change::Edit { node, old_node }, pusher),
        (true, false) => output.push(Change::Remove(old_node), pusher),
        (false, true) => output.push(Change::Add(node), pusher),
        (false, false) => {}
    }
}
