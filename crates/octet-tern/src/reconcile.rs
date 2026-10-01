//! Keyed reconciliation of native trees without replacing unchanged subtrees.

use std::collections::HashMap;

use serde_json::Value;

use crate::wire::{Node, Op, Props};

/// Reconcile ordered children of `parent`. The previous tree must be the last
/// successfully sent tree, not a projection that was skipped for lack of credit.
/// Node identities are unique within a surface and survive insertion/reordering.
pub fn children(parent: &str, previous: &[Node], next: &[Node], ops: &mut Vec<Op>) {
    let old: HashMap<&str, &Node> = previous
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let new: HashMap<&str, &Node> = next.iter().map(|node| (node.id.as_str(), node)).collect();
    for node in previous {
        if !new.contains_key(node.id.as_str()) {
            ops.push(Op::Del {
                id: node.id.clone(),
            });
        }
    }
    // Insertions/removals alone never require moving existing siblings. In
    // particular, appending a turn must not detach the entire visible history.
    let reordered = previous
        .iter()
        .filter(|node| {
            new.get(node.id.as_str())
                .is_some_and(|next| next.k == node.k)
        })
        .map(|n| &n.id)
        .ne(next
            .iter()
            .filter(|node| {
                old.get(node.id.as_str())
                    .is_some_and(|prior| prior.k == node.k)
            })
            .map(|n| &n.id));
    // Insert/move from the end: the `before` target already exists even when
    // several new siblings were prepended in this frame.
    let mut before = None;
    for node in next.iter().rev() {
        match old.get(node.id.as_str()) {
            Some(prior) if prior.k == node.k => {
                let props = changed_props(prior.p.as_ref(), node.p.as_ref());
                if !props.is_empty() {
                    ops.push(Op::Set {
                        id: node.id.clone(),
                        props,
                    });
                }
                children(
                    &node.id,
                    prior.c.as_deref().unwrap_or_default(),
                    node.c.as_deref().unwrap_or_default(),
                    ops,
                );
                if reordered {
                    ops.push(Op::Move {
                        id: node.id.clone(),
                        parent: parent.to_owned(),
                        before: before.clone(),
                    });
                }
            }
            prior => {
                if prior.is_some() {
                    ops.push(Op::Del {
                        id: node.id.clone(),
                    });
                }
                ops.push(Op::Add {
                    id: node.id.clone(),
                    parent: parent.to_owned(),
                    before: before.clone(),
                    node: node.clone(),
                });
            }
        }
        before = Some(node.id.clone());
    }
}

fn changed_props(previous: Option<&Props>, next: Option<&Props>) -> Props {
    let empty = Props::new();
    let old = previous.unwrap_or(&empty).as_map();
    let new = next.unwrap_or(&empty).as_map();
    let mut changed = serde_json::Map::new();
    for (key, value) in new {
        if old.get(key) != Some(value) {
            changed.insert(key.clone(), value.clone());
        }
    }
    for key in old.keys() {
        if !new.contains_key(key) {
            changed.insert(key.clone(), Value::Null);
        }
    }
    Props::from_value(Value::Object(changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Kind;

    fn text(id: &str, value: &str) -> Node {
        Node::new(id, Kind::Md, Props::new().set("text", value))
    }

    #[test]
    fn streaming_patches_only_the_changed_leaf() {
        let old = vec![text("history", "keep"), text("live", "hello")];
        let new = vec![text("history", "keep"), text("live", "hello world")];
        let mut ops = Vec::new();
        children("main", &old, &new, &mut ops);
        assert_eq!(
            ops,
            vec![Op::Set {
                id: "live".into(),
                props: Props::new().set("text", "hello world")
            }]
        );
        ops.clear();
        children("main", &new, &new, &mut ops);
        assert!(ops.is_empty());
    }

    #[test]
    fn prepend_preserves_existing_nodes_and_orders_new_siblings() {
        let old = vec![text("b", "b")];
        let new = vec![text("x", "x"), text("a", "a"), text("b", "b")];
        let mut ops = Vec::new();
        children("main", &old, &new, &mut ops);
        assert!(!ops.iter().any(|op| matches!(op, Op::Del { .. })));
        assert!(
            matches!(&ops[0], Op::Add { id, before: Some(before), .. } if id == "a" && before == "b")
        );
        assert!(
            matches!(&ops[1], Op::Add { id, before: Some(before), .. } if id == "x" && before == "a")
        );
    }

    #[test]
    fn removal_and_property_clearing_are_explicit() {
        let old = vec![
            Node::new(
                "a",
                Kind::Md,
                Props::new().set("stream", true).set("text", "done"),
            ),
            text("b", "removed"),
        ];
        let new = vec![text("a", "done")];
        let mut ops = Vec::new();
        children("main", &old, &new, &mut ops);
        assert!(matches!(&ops[0], Op::Del { id } if id == "b"));
        assert!(
            matches!(&ops[1], Op::Set { id, props } if id == "a" && props.as_map()["stream"].is_null())
        );
    }
}
