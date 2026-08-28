use std::{collections::BTreeSet, ffi::OsString};

use crate::session::SessionKey;

/// Identity of either a resumable session or a relation-only node.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NodeKey {
    Session(SessionKey),
    Related(RelatedNodeKey),
}

/// Kinds of nodes which exist only to preserve recorded relation evidence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RelatedNodeKind {
    AgentExecution,
    MissingSession,
}

/// Adapter-owned opaque identity for a relation-only node.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RelatedNodeKey {
    pub kind: RelatedNodeKind,
    pub agent: OsString,
    pub native_locator: OsString,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RelationKind {
    Spawned,
    ForkedFrom,
    ImportedFrom,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EvidenceSource {
    NativeTranscript,
    NativeDatabase,
    NativeLayout,
}

/// A recorded parent-to-child relation. Direction is structural, independent
/// of how the native format names the relation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RelationEdge {
    pub parent: NodeKey,
    pub child: NodeKey,
    pub kind: RelationKind,
    pub source: EvidenceSource,
}

/// Unified graph containing only explicit native evidence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RelationGraph {
    nodes: BTreeSet<NodeKey>,
    edges: BTreeSet<RelationEdge>,
}

impl RelationGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: NodeKey) -> bool {
        self.nodes.insert(node)
    }

    /// Inserts both endpoints and returns false only for an exact duplicate.
    pub fn add_edge(&mut self, edge: RelationEdge) -> bool {
        self.nodes.insert(edge.parent.clone());
        self.nodes.insert(edge.child.clone());
        self.edges.insert(edge)
    }

    pub fn nodes(&self) -> impl ExactSizeIterator<Item = &NodeKey> {
        self.nodes.iter()
    }

    pub fn edges(&self) -> impl ExactSizeIterator<Item = &RelationEdge> {
        self.edges.iter()
    }

    pub fn contains_node(&self, node: &NodeKey) -> bool {
        self.nodes.contains(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn related(kind: RelatedNodeKind, locator: &str) -> NodeKey {
        NodeKey::Related(RelatedNodeKey {
            kind,
            agent: "test".into(),
            native_locator: locator.into(),
        })
    }

    #[test]
    fn exact_duplicate_edges_dedupe_but_parallel_edges_remain() {
        let parent = related(RelatedNodeKind::MissingSession, "parent");
        let child = related(RelatedNodeKind::AgentExecution, "child");
        let edge = RelationEdge {
            parent,
            child,
            kind: RelationKind::Spawned,
            source: EvidenceSource::NativeTranscript,
        };
        let mut graph = RelationGraph::new();

        assert!(graph.add_edge(edge.clone()));
        assert!(!graph.add_edge(edge.clone()));
        assert!(graph.add_edge(RelationEdge {
            source: EvidenceSource::NativeDatabase,
            ..edge
        }));
        assert_eq!(graph.edges().len(), 2);
    }

    #[test]
    fn missing_endpoint_is_preserved_as_a_node() {
        let missing = related(RelatedNodeKind::MissingSession, "gone");
        let mut graph = RelationGraph::new();
        graph.add_edge(RelationEdge {
            parent: missing.clone(),
            child: related(RelatedNodeKind::AgentExecution, "child"),
            kind: RelationKind::Spawned,
            source: EvidenceSource::NativeLayout,
        });

        assert!(graph.contains_node(&missing));
        assert_eq!(graph.nodes().len(), 2);
    }
}
