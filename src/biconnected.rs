//! biconnected components and BC-Tree (Tarjan algorithm)

use crate::{EdgeId, Graph, NodeId, INVALID};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
pub struct BCNodeId(pub u32);

impl BCNodeId {
    pub const INVALID: BCNodeId = BCNodeId(INVALID);

    #[inline(always)]
    pub fn is_valid(self) -> bool {
        self.0 != INVALID
    }

    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BCNodeType {
    Block = 0,
    CutVertex = 1,
}

#[derive(Clone, Debug)]
#[repr(C)]
pub struct Block {
    pub node_start: u32,
    pub node_count: u32,
    pub edge_start: u32,
    pub edge_count: u32,
}

#[derive(Clone, Debug)]
pub struct BCTree {
    blocks: Vec<Block>,
    /// array of all block nodes (indexed by Block::node_start)
    block_nodes_flat: Vec<NodeId>,
    /// ------------------ edges (---------- Block::edge_start)
    block_edges_flat: Vec<EdgeId>,
    /// cut vertices (sorted for binary search)
    cut_vertices: Vec<NodeId>,
    /// bitset cut vertex check (bit i = 1 if node i is cut vertex)
    is_cut: Vec<u64>,
    /// Number of original graph nodes (for bounds checking)
    num_nodes: u32,
    pub num_components: u32,
}

impl BCTree {
    pub fn build(graph: &Graph) -> Self {
        let n = graph.num_nodes();
        if n == 0 {
            return BCTree {
                blocks: Vec::new(),
                block_nodes_flat: Vec::new(),
                block_edges_flat: Vec::new(),
                cut_vertices: Vec::new(),
                is_cut: Vec::new(),
                num_nodes: 0,
                num_components: 0,
            };
        }

        let mut builder = BCTreeBuilder::new(graph);
        builder.build();
        builder.into_bc_tree()
    }

    #[inline]
    pub fn is_cut_vertex(&self, node: NodeId) -> bool {
        let idx = node.idx();
        if idx >= self.num_nodes as usize {
            return false;
        }
        let word = idx / 64;
        let bit = idx % 64;
        word < self.is_cut.len() && (self.is_cut[word] & (1u64 << bit)) != 0
    }

    #[inline(always)]
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }

    #[inline(always)]
    pub fn num_cut_vertices(&self) -> usize {
        self.cut_vertices.len()
    }

    #[inline]
    pub fn is_biconnected(&self) -> bool {
        self.blocks.len() == 1 && self.cut_vertices.is_empty()
    }

    #[inline]
    pub fn block(&self, idx: usize) -> &Block {
        &self.blocks[idx]
    }

    #[inline]
    pub fn block_nodes(&self, idx: usize) -> &[NodeId] {
        let b = &self.blocks[idx];
        let start = b.node_start as usize;
        let end = start + b.node_count as usize;
        &self.block_nodes_flat[start..end]
    }

    #[inline]
    pub fn block_edges(&self, idx: usize) -> &[EdgeId] {
        let b = &self.blocks[idx];
        let start = b.edge_start as usize;
        let end = start + b.edge_count as usize;
        &self.block_edges_flat[start..end]
    }

    #[inline]
    pub fn cut_vertices(&self) -> &[NodeId] {
        &self.cut_vertices
    }

    #[inline]
    pub fn iter_blocks(&self) -> impl Iterator<Item = (usize, &Block)> {
        self.blocks.iter().enumerate()
    }

    #[inline]
    pub fn blocks_raw(&self) -> &[Block] {
        &self.blocks
    }

    #[inline]
    pub fn nodes_flat_raw(&self) -> &[NodeId] {
        &self.block_nodes_flat
    }

    #[inline]
    pub fn edges_flat_raw(&self) -> &[EdgeId] {
        &self.block_edges_flat
    }
}

struct BCTreeBuilder<'a> {
    graph: &'a Graph,
    n: usize,

    // disc[v] == 0 && v != dfs_root means unvisited
    disc: Vec<u32>,
    low: Vec<u32>,
    parent: Vec<u32>,
    parent_edge: Vec<u32>,
    time: u32,

    edge_stack: Vec<(u32, u32, u32)>, // (u, v, edge_id)

    blocks: Vec<Block>,
    block_nodes_flat: Vec<NodeId>,
    block_edges_flat: Vec<EdgeId>,
    is_cut: Vec<u64>,

    // Bit i = 1 means node i is already in current block
    temp_in_block: Vec<u64>,

    num_components: u32,
}

impl<'a> BCTreeBuilder<'a> {
    fn new(graph: &'a Graph) -> Self {
        let n = graph.num_nodes();
        let bitset_words = n.div_ceil(64);

        BCTreeBuilder {
            graph,
            n,
            disc: vec![0; n],
            low: vec![0; n],
            parent: vec![INVALID; n],
            parent_edge: vec![INVALID; n],
            time: 0,
            edge_stack: Vec::with_capacity(graph.num_edges()),
            blocks: Vec::new(),
            block_nodes_flat: Vec::new(),
            block_edges_flat: Vec::new(),
            is_cut: vec![0u64; bitset_words],
            temp_in_block: vec![0u64; bitset_words],
            num_components: 0,
        }
    }

    #[inline(always)]
    fn mark_cut(&mut self, v: usize) {
        let word = v / 64;
        let bit = v % 64;
        self.is_cut[word] |= 1u64 << bit;
    }

    #[inline(always)]
    fn is_in_block(&self, v: usize) -> bool {
        let word = v / 64;
        let bit = v % 64;
        (self.temp_in_block[word] & (1u64 << bit)) != 0
    }

    #[inline(always)]
    fn mark_in_block(&mut self, v: usize) {
        let word = v / 64;
        let bit = v % 64;
        self.temp_in_block[word] |= 1u64 << bit;
    }

    #[inline(always)]
    fn clear_in_block(&mut self, v: usize) {
        let word = v / 64;
        let bit = v % 64;
        self.temp_in_block[word] &= !(1u64 << bit);
    }

    #[inline(always)]
    fn push_edge(&mut self, u: u32, v: u32, eid: u32) {
        self.edge_stack.push((u, v, eid));
    }

    #[inline(always)]
    fn pop_edge(&mut self) -> Option<(u32, u32, u32)> {
        self.edge_stack.pop()
    }

    fn build(&mut self) {
        for start in 0..self.n {
            if self.disc[start] == 0 {
                self.time += 1;
                self.dfs_iterative(start as u32);
                self.num_components += 1;
            }
        }
    }

    fn dfs_iterative(&mut self, root: u32) {
        let mut stack: Vec<(u32, u32, u32)> = Vec::with_capacity(self.n.min(4096));

        self.disc[root as usize] = self.time;
        self.low[root as usize] = self.time;
        self.time += 1;

        let root_cursor = self.graph.adj_cursor(NodeId(root));
        stack.push((root, root_cursor, 0));

        while let Some(&(u, cursor, _children)) = stack.last() {
            let u_idx = u as usize;

            if let Some((v, eid, next_cursor)) = self.graph.adj_next(cursor) {
                let v_idx = v.idx();
                let v_u32 = v.0;

                stack.last_mut().unwrap().1 = next_cursor;

                if self.disc[v_idx] == 0 {
                    self.parent[v_idx] = u;
                    self.parent_edge[v_idx] = eid.0;
                    self.push_edge(u, v_u32, eid.0);

                    self.disc[v_idx] = self.time;
                    self.low[v_idx] = self.time;
                    self.time += 1;

                    stack.last_mut().unwrap().2 += 1;
                    let v_cursor = self.graph.adj_cursor(v);
                    stack.push((v_u32, v_cursor, 0));
                } else if eid.0 != self.parent_edge[u_idx] && self.disc[v_idx] < self.disc[u_idx] {
                    // Back edge
                    self.low[u_idx] = self.low[u_idx].min(self.disc[v_idx]);
                    self.push_edge(u, v_u32, eid.0);
                }
            } else {
                let (u, _, children) = stack.pop().unwrap();
                let u_idx = u as usize;

                if let Some(&(p, _, _)) = stack.last() {
                    let p_idx = p as usize;

                    self.low[p_idx] = self.low[p_idx].min(self.low[u_idx]);

                    // Check articulation point
                    if self.low[u_idx] >= self.disc[p_idx] {
                        self.extract_block(self.parent_edge[u_idx]);
                        // p is cut vertex unless it's root with single child
                        if self.parent[p_idx] != INVALID {
                            self.mark_cut(p_idx);
                        }
                    }
                } else {
                    // Root node
                    if children > 1 {
                        self.mark_cut(u_idx);
                    }
                }
            }
        }
    }

    fn extract_block(&mut self, tree_eid: u32) {
        let node_start = self.block_nodes_flat.len() as u32;
        let edge_start = self.block_edges_flat.len() as u32;

        let nodes_start_idx = self.block_nodes_flat.len();

        while let Some((x, y, eid)) = self.pop_edge() {
            self.block_edges_flat.push(EdgeId(eid));

            let x_idx = x as usize;
            let y_idx = y as usize;

            if !self.is_in_block(x_idx) {
                self.mark_in_block(x_idx);
                self.block_nodes_flat.push(NodeId(x));
            }
            if !self.is_in_block(y_idx) {
                self.mark_in_block(y_idx);
                self.block_nodes_flat.push(NodeId(y));
            }

            if eid == tree_eid {
                break;
            }
        }

        for i in nodes_start_idx..self.block_nodes_flat.len() {
            let node_idx = self.block_nodes_flat[i].idx();
            self.clear_in_block(node_idx);
        }

        let node_count = (self.block_nodes_flat.len() as u32) - node_start;
        let edge_count = (self.block_edges_flat.len() as u32) - edge_start;

        self.blocks.push(Block {
            node_start,
            node_count,
            edge_start,
            edge_count,
        });
    }

    fn into_bc_tree(self) -> BCTree {
        let mut cut_vertices = Vec::new();
        for (word_idx, &word) in self.is_cut.iter().enumerate() {
            if word == 0 {
                continue;
            }
            let base = word_idx * 64;
            let mut w = word;
            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                let node_idx = base + bit;
                if node_idx < self.n {
                    cut_vertices.push(NodeId(node_idx as u32));
                }
                w &= w - 1; // Clear lowest set bit
            }
        }

        BCTree {
            blocks: self.blocks,
            block_nodes_flat: self.block_nodes_flat,
            block_edges_flat: self.block_edges_flat,
            cut_vertices,
            is_cut: self.is_cut,
            num_nodes: self.n as u32,
            num_components: self.num_components,
        }
    }
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
    fn test_triangle() {
        let g = make_graph(3, &[(0, 1), (1, 2), (2, 0)]);
        let bc = BCTree::build(&g);
        assert_eq!(bc.num_blocks(), 1);
        assert_eq!(bc.num_cut_vertices(), 0);
        assert!(bc.is_biconnected());
        assert_eq!(bc.block_nodes(0).len(), 3);
        assert_eq!(bc.block_edges(0).len(), 3);
    }

    #[test]
    fn test_two_triangles_shared_vertex() {
        let g = make_graph(5, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 2)]);
        let bc = BCTree::build(&g);
        assert_eq!(bc.num_blocks(), 2);
        assert_eq!(bc.num_cut_vertices(), 1);
        assert!(bc.is_cut_vertex(NodeId(2)));
        assert!(!bc.is_cut_vertex(NodeId(0)));
    }

    #[test]
    fn test_path() {
        let g = make_graph(5, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let bc = BCTree::build(&g);
        assert_eq!(bc.num_blocks(), 4);
        assert_eq!(bc.num_cut_vertices(), 3);
        assert!(bc.is_cut_vertex(NodeId(1)));
        assert!(bc.is_cut_vertex(NodeId(2)));
        assert!(bc.is_cut_vertex(NodeId(3)));
    }

    #[test]
    fn test_single_edge() {
        let g = make_graph(2, &[(0, 1)]);
        let bc = BCTree::build(&g);
        assert_eq!(bc.num_blocks(), 1);
        assert!(bc.is_biconnected());
    }

    #[test]
    fn test_k4() {
        let g = make_graph(4, &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)]);
        let bc = BCTree::build(&g);
        assert!(bc.is_biconnected());
    }

    #[test]
    fn test_two_components() {
        let g = make_graph(6, &[(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3)]);
        let bc = BCTree::build(&g);
        assert_eq!(bc.num_blocks(), 2);
        assert_eq!(bc.num_cut_vertices(), 0);
        assert_eq!(bc.num_components, 2);
    }

    #[test]
    fn test_empty_graph() {
        let g = Graph::with_capacity(0, 0);
        let bc = BCTree::build(&g);
        assert_eq!(bc.num_blocks(), 0);
    }

    #[test]
    fn test_block_slices() {
        let g = make_graph(5, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 2)]);
        let bc = BCTree::build(&g);

        for i in 0..bc.num_blocks() {
            let nodes = bc.block_nodes(i);
            let edges = bc.block_edges(i);
            assert!(nodes.len() >= 2);
            assert!(!edges.is_empty());
        }
    }
}
