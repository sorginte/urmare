//! Generic directed graph operations used by Urmare.
//!
//! Nodes are addressed by compact, stable IDs. The graph stores both edge
//! orientations so reverse impact traversal does not scan unrelated edges.

use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::fmt;

/// A stable node identifier within one [`DirectedGraph`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(usize);

impl NodeId {
    /// Returns the zero-based index backing this ID.
    pub const fn index(self) -> usize {
        self.0
    }
}

/// An attempt to use an ID that was not allocated by this graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNodeId(pub NodeId);

impl fmt::Display for InvalidNodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "node ID {} does not exist in this graph",
            self.0.index()
        )
    }
}

impl Error for InvalidNodeId {}

/// A directed graph with efficient traversal in both directions.
#[derive(Clone, Debug, Default)]
pub struct DirectedGraph {
    forward: Vec<HashSet<NodeId>>,
    reverse: Vec<HashSet<NodeId>>,
    edge_count: usize,
}

impl DirectedGraph {
    /// Creates an empty graph.
    pub const fn new() -> Self {
        Self {
            forward: Vec::new(),
            reverse: Vec::new(),
            edge_count: 0,
        }
    }

    /// Adds a node and returns its stable ID.
    pub fn add_node(&mut self) -> NodeId {
        let id = NodeId(self.forward.len());
        self.forward.push(HashSet::new());
        self.reverse.push(HashSet::new());
        id
    }

    /// Adds `from -> to`, returning whether a new edge was inserted.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId) -> Result<bool, InvalidNodeId> {
        self.validate(from)?;
        self.validate(to)?;

        let inserted = self.forward[from.index()].insert(to);
        if inserted {
            self.reverse[to.index()].insert(from);
            self.edge_count += 1;
        }
        Ok(inserted)
    }

    /// Returns the number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.forward.len()
    }

    /// Returns the number of unique directed edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edge_count
    }

    /// Returns nodes directly depended on by `node`.
    pub fn forward_neighbors(&self, node: NodeId) -> Result<&HashSet<NodeId>, InvalidNodeId> {
        self.forward.get(node.index()).ok_or(InvalidNodeId(node))
    }

    /// Returns nodes that directly depend on `node`.
    pub fn reverse_neighbors(&self, node: NodeId) -> Result<&HashSet<NodeId>, InvalidNodeId> {
        self.reverse.get(node.index()).ok_or(InvalidNodeId(node))
    }

    /// Finds every direct and transitive dependent of `start`.
    ///
    /// `start` itself is excluded, including when it participates in a cycle.
    pub fn reverse_transitive_closure(
        &self,
        start: NodeId,
    ) -> Result<HashSet<NodeId>, InvalidNodeId> {
        self.validate(start)?;

        let mut visited = HashSet::from([start]);
        let mut queue = VecDeque::from([start]);

        while let Some(current) = queue.pop_front() {
            for &dependent in &self.reverse[current.index()] {
                if visited.insert(dependent) {
                    queue.push_back(dependent);
                }
            }
        }

        visited.remove(&start);
        Ok(visited)
    }

    /// Finds one shortest path from `dependent` to `dependency`.
    ///
    /// Paths follow forward edges, so they read naturally as “A depends on B”.
    /// Neighbor IDs are sorted before traversal to make path choice deterministic.
    pub fn dependency_path(
        &self,
        dependent: NodeId,
        dependency: NodeId,
    ) -> Result<Option<Vec<NodeId>>, InvalidNodeId> {
        self.validate(dependent)?;
        self.validate(dependency)?;

        if dependent == dependency {
            return Ok(Some(vec![dependent]));
        }

        let mut parents = vec![None; self.node_count()];
        let mut visited = HashSet::from([dependent]);
        let mut queue = VecDeque::from([dependent]);

        while let Some(current) = queue.pop_front() {
            let mut neighbors: Vec<_> = self.forward[current.index()].iter().copied().collect();
            neighbors.sort_unstable();

            for neighbor in neighbors {
                if !visited.insert(neighbor) {
                    continue;
                }
                parents[neighbor.index()] = Some(current);

                if neighbor == dependency {
                    return Ok(Some(reconstruct_path(dependent, dependency, &parents)));
                }
                queue.push_back(neighbor);
            }
        }

        Ok(None)
    }

    fn validate(&self, node: NodeId) -> Result<(), InvalidNodeId> {
        if node.index() < self.node_count() {
            Ok(())
        } else {
            Err(InvalidNodeId(node))
        }
    }
}

fn reconstruct_path(start: NodeId, end: NodeId, parents: &[Option<NodeId>]) -> Vec<NodeId> {
    let mut path = vec![end];
    let mut current = end;

    while current != start {
        // Every visited node other than `start` receives a parent before it is
        // queued. Avoid `unwrap` here so malformed internal state degrades to a
        // partial path rather than panicking.
        let Some(parent) = parents[current.index()] else {
            break;
        };
        path.push(parent);
        current = parent;
    }

    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::DirectedGraph;

    #[test]
    fn preserves_dependency_orientation_and_reverse_neighbors() {
        let mut graph = DirectedGraph::new();
        let api = graph.add_node();
        let service = graph.add_node();

        graph.add_edge(api, service).expect("valid IDs");

        assert_eq!(graph.forward_neighbors(api).expect("valid ID").len(), 1);
        assert!(
            graph
                .forward_neighbors(api)
                .expect("valid ID")
                .contains(&service)
        );
        assert!(
            graph
                .reverse_neighbors(service)
                .expect("valid ID")
                .contains(&api)
        );
    }

    #[test]
    fn reverse_closure_handles_transitive_edges_and_cycles() {
        let mut graph = DirectedGraph::new();
        let test = graph.add_node();
        let api = graph.add_node();
        let service = graph.add_node();

        graph.add_edge(test, api).expect("valid IDs");
        graph.add_edge(api, service).expect("valid IDs");
        graph.add_edge(service, api).expect("valid IDs");

        let affected = graph.reverse_transitive_closure(service).expect("valid ID");
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&api));
        assert!(affected.contains(&test));
        assert!(!affected.contains(&service));
    }

    #[test]
    fn dependency_path_is_shortest_and_reads_dependent_to_dependency() {
        let mut graph = DirectedGraph::new();
        let test = graph.add_node();
        let api = graph.add_node();
        let service = graph.add_node();
        let changed = graph.add_node();

        graph.add_edge(test, api).expect("valid IDs");
        graph.add_edge(api, service).expect("valid IDs");
        graph.add_edge(service, changed).expect("valid IDs");

        assert_eq!(
            graph
                .dependency_path(test, changed)
                .expect("valid IDs")
                .expect("path exists"),
            vec![test, api, service, changed]
        );
        assert_eq!(
            graph.dependency_path(changed, test).expect("valid IDs"),
            None
        );
    }

    #[test]
    fn duplicate_edges_are_counted_once() {
        let mut graph = DirectedGraph::new();
        let first = graph.add_node();
        let second = graph.add_node();

        assert!(graph.add_edge(first, second).expect("valid IDs"));
        assert!(!graph.add_edge(first, second).expect("valid IDs"));
        assert_eq!(graph.edge_count(), 1);
    }
}
