use std::collections::{BTreeMap, BTreeSet};

use crate::relation::{EvidenceSource, NodeKey, RelationEdge, RelationGraph, RelationKind};

/// Deterministic forest view of a relation graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeProjection {
    pub roots: Vec<TreeNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeNode {
    Node {
        key: NodeKey,
        children: Vec<TreeBranch>,
    },
    /// Target was already expanded elsewhere, including an ancestor cycle.
    Reference(NodeKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeBranch {
    pub kind: RelationKind,
    pub source: EvidenceSource,
    pub node: TreeNode,
}

/// Projects every graph node exactly once as a full node. Later occurrences
/// become references. Root, child, and parallel-edge order follows key order.
/// Components without a natural root (cycles) start at their smallest key.
pub fn project(graph: &RelationGraph) -> TreeProjection {
    let mut incoming = BTreeSet::new();
    let mut children: BTreeMap<&NodeKey, Vec<&RelationEdge>> = BTreeMap::new();
    for edge in graph.edges() {
        incoming.insert(&edge.child);
        children.entry(&edge.parent).or_default().push(edge);
    }

    let mut expanded = BTreeSet::new();
    let mut roots = Vec::new();
    for key in graph.nodes().filter(|key| !incoming.contains(key)) {
        roots.push(expand(key, &children, &mut expanded));
    }
    for key in graph.nodes() {
        if !expanded.contains(key) {
            roots.push(expand(key, &children, &mut expanded));
        }
    }
    TreeProjection { roots }
}

fn expand(
    key: &NodeKey,
    children: &BTreeMap<&NodeKey, Vec<&RelationEdge>>,
    expanded: &mut BTreeSet<NodeKey>,
) -> TreeNode {
    if !expanded.insert(key.clone()) {
        return TreeNode::Reference(key.clone());
    }

    let branches = children
        .get(key)
        .into_iter()
        .flat_map(|edges| edges.iter())
        .map(|edge| TreeBranch {
            kind: edge.kind,
            source: edge.source,
            node: expand(&edge.child, children, expanded),
        })
        .collect();
    TreeNode::Node {
        key: key.clone(),
        children: branches,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::relation::{RelatedNodeKey, RelatedNodeKind};

    fn key(locator: &str) -> NodeKey {
        NodeKey::Related(RelatedNodeKey {
            kind: RelatedNodeKind::AgentExecution,
            agent: OsString::from("test"),
            native_locator: OsString::from(locator),
        })
    }

    fn edge(parent: &str, child: &str) -> RelationEdge {
        RelationEdge {
            parent: key(parent),
            child: key(child),
            kind: RelationKind::Spawned,
            source: EvidenceSource::NativeTranscript,
        }
    }

    fn node(key: &str, children: Vec<TreeBranch>) -> TreeNode {
        TreeNode::Node {
            key: self::key(key),
            children,
        }
    }

    fn branch(node: TreeNode) -> TreeBranch {
        TreeBranch {
            kind: RelationKind::Spawned,
            source: EvidenceSource::NativeTranscript,
            node,
        }
    }

    #[test]
    fn second_parent_gets_reference() {
        let mut graph = RelationGraph::new();
        graph.add_edge(edge("a", "shared"));
        graph.add_edge(edge("b", "shared"));

        assert_eq!(
            project(&graph).roots,
            vec![
                node("a", vec![branch(node("shared", vec![]))]),
                node(
                    "b",
                    vec![branch(TreeNode::Reference(key("shared")))],
                ),
            ]
        );
    }

    #[test]
    fn cycle_terminates_with_reference() {
        let mut graph = RelationGraph::new();
        graph.add_edge(edge("a", "b"));
        graph.add_edge(edge("b", "a"));

        assert_eq!(
            project(&graph).roots,
            vec![node(
                "a",
                vec![branch(node(
                    "b",
                    vec![branch(TreeNode::Reference(key("a")))],
                ))],
            )]
        );
    }

    #[test]
    fn ordering_is_stable_across_insertion_order() {
        let edges = [edge("root", "c"), edge("root", "a"), edge("root", "b")];
        let mut forward = RelationGraph::new();
        let mut reverse = RelationGraph::new();
        for edge in &edges {
            forward.add_edge(edge.clone());
        }
        for edge in edges.iter().rev() {
            reverse.add_edge(edge.clone());
        }

        let expected = vec![node(
            "root",
            vec![
                branch(node("a", vec![])),
                branch(node("b", vec![])),
                branch(node("c", vec![])),
            ],
        )];
        assert_eq!(project(&forward).roots, expected);
        assert_eq!(project(&forward), project(&reverse));
    }
}
