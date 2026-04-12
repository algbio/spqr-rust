//! CC algorithm

use crate::{Graph, NodeId, INVALID};

#[derive(Debug, Clone)]
pub struct ConnectedComponents {
    /// Number of connected components
    pub num_components: u32,
    pub component: Vec<u32>,
}

impl ConnectedComponents {
    #[inline(always)]
    pub fn component_of(&self, node: NodeId) -> u32 {
        self.component[node.idx()]
    }

    #[inline]
    pub fn nodes_in_iter(&self, component_id: u32) -> impl Iterator<Item = NodeId> + '_ {
        self.component
            .iter()
            .enumerate()
            .filter(move |(_, &c)| c == component_id)
            .map(|(i, _)| NodeId(i as u32))
    }

    #[inline]
    pub fn count_in(&self, component_id: u32) -> usize {
        self.component
            .iter()
            .filter(|&&c| c == component_id)
            .count()
    }

    pub fn nodes_in(&self, component_id: u32) -> Vec<NodeId> {
        self.nodes_in_iter(component_id).collect()
    }
}

#[inline]
pub fn connected_components(graph: &Graph) -> ConnectedComponents {
    let n = graph.num_nodes();
    if n == 0 {
        return ConnectedComponents {
            num_components: 0,
            component: Vec::new(),
        };
    }

    let mut component = vec![INVALID; n];
    let mut current_component: u32 = 0;

    let mut stack: Vec<NodeId> = Vec::with_capacity(n.min(4096));

    for start in 0..n {
        if component[start] != INVALID {
            continue; // Already visited
        }

        // BFS/DFS from this node
        stack.push(NodeId(start as u32));
        component[start] = current_component;

        while let Some(u) = stack.pop() {
            for (v, _edge_id) in graph.neighbors(u) {
                let vi = v.idx();
                if component[vi] == INVALID {
                    component[vi] = current_component;
                    stack.push(v);
                }
            }
        }

        current_component += 1;
    }

    ConnectedComponents {
        num_components: current_component,
        component,
    }
}

#[inline]
pub fn count_connected_components(graph: &Graph) -> u32 {
    let n = graph.num_nodes();
    if n == 0 {
        return 0;
    }

    let words = n.div_ceil(64);
    let mut visited = vec![0u64; words];
    let mut count: u32 = 0;
    let mut stack: Vec<NodeId> = Vec::with_capacity(n.min(4096));

    #[inline(always)]
    fn is_visited(visited: &[u64], v: usize) -> bool {
        (visited[v / 64] & (1u64 << (v % 64))) != 0
    }

    #[inline(always)]
    fn mark_visited(visited: &mut [u64], v: usize) {
        visited[v / 64] |= 1u64 << (v % 64);
    }

    for start in 0..n {
        if is_visited(&visited, start) {
            continue;
        }

        stack.push(NodeId(start as u32));
        mark_visited(&mut visited, start);

        while let Some(u) = stack.pop() {
            for (v, _) in graph.neighbors(u) {
                let vi = v.idx();
                if !is_visited(&visited, vi) {
                    mark_visited(&mut visited, vi);
                    stack.push(v);
                }
            }
        }

        count += 1;
    }

    count
}

#[inline]
pub fn connected_components_simple(graph: &Graph) -> (u32, Vec<u32>) {
    let cc = connected_components(graph);
    (cc.num_components, cc.component)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Graph;

    fn make_graph(n: usize, edges: &[(u32, u32)]) -> Graph {
        let mut g = Graph::with_capacity(n, edges.len());
        g.add_nodes(n);
        for &(u, v) in edges {
            g.add_edge(NodeId(u), NodeId(v));
        }
        g
    }

    #[test]
    fn test_single_component() {
        let g = make_graph(3, &[(0, 1), (1, 2), (2, 0)]);
        let cc = connected_components(&g);
        assert_eq!(cc.num_components, 1);
        assert_eq!(cc.component_of(NodeId(0)), 0);
        assert_eq!(cc.component_of(NodeId(1)), 0);
        assert_eq!(cc.component_of(NodeId(2)), 0);
    }

    #[test]
    fn test_two_components() {
        let g = make_graph(4, &[(0, 1), (2, 3)]);
        let cc = connected_components(&g);
        assert_eq!(cc.num_components, 2);
        assert_eq!(cc.component_of(NodeId(0)), cc.component_of(NodeId(1)));
        assert_eq!(cc.component_of(NodeId(2)), cc.component_of(NodeId(3)));
        assert_ne!(cc.component_of(NodeId(0)), cc.component_of(NodeId(2)));
    }

    #[test]
    fn test_isolated_nodes() {
        let g = make_graph(4, &[]);
        let cc = connected_components(&g);
        assert_eq!(cc.num_components, 4);
    }

    #[test]
    fn test_empty_graph() {
        let g = Graph::with_capacity(0, 0);
        let cc = connected_components(&g);
        assert_eq!(cc.num_components, 0);
    }

    #[test]
    fn test_count_only() {
        let g = make_graph(6, &[(0, 1), (2, 3), (4, 5)]);
        assert_eq!(count_connected_components(&g), 3);
    }

    #[test]
    fn test_nodes_in_iter() {
        let g = make_graph(4, &[(0, 1), (2, 3)]);
        let cc = connected_components(&g);

        let comp0_nodes: Vec<_> = cc.nodes_in_iter(0).collect();
        assert_eq!(comp0_nodes.len(), 2);

        assert_eq!(cc.count_in(0), 2);
        assert_eq!(cc.count_in(1), 2);
    }
}
