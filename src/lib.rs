//! # SPQR Tree, triconnectivity decomposition
//!
//! Computes SPQR trees of biconnected multigraphs using a DFS based
//! triconnected components algorithm (Hopcroft Tarjan with corrections
//! by Gutwenger and Mutzel in 2001)
//!

#![deny(unsafe_code)]
#![allow(clippy::needless_range_loop)]

pub mod biconnected;
pub mod connected;
#[allow(unsafe_code)]
pub mod ffi;
pub mod spqr_format;
pub mod verify;

pub use biconnected::{BCNodeId, BCNodeType, BCTree, Block};
pub use connected::{
    connected_components, connected_components_simple, count_connected_components,
    ConnectedComponents,
};

use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreeNodeId(pub u32);

impl Default for NodeId {
    fn default() -> Self {
        NodeId::INVALID
    }
}
impl Default for EdgeId {
    fn default() -> Self {
        EdgeId::INVALID
    }
}
impl Default for TreeNodeId {
    fn default() -> Self {
        TreeNodeId::INVALID
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}
impl fmt::Debug for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}
impl fmt::Debug for TreeNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

pub const INVALID: u32 = u32::MAX;

impl NodeId {
    pub const INVALID: NodeId = NodeId(INVALID);
    #[inline(always)]
    pub fn is_valid(self) -> bool {
        self.0 != INVALID
    }
    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}
impl EdgeId {
    pub const INVALID: EdgeId = EdgeId(INVALID);
    #[inline(always)]
    pub fn is_valid(self) -> bool {
        self.0 != INVALID
    }
    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}
impl TreeNodeId {
    pub const INVALID: TreeNodeId = TreeNodeId(INVALID);
    #[inline(always)]
    pub fn is_valid(self) -> bool {
        self.0 != INVALID
    }
    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}

// Graph (adjacency-list multigraph)

#[derive(Clone, Debug)]
struct HalfEdge {
    target: NodeId,
    edge_id: EdgeId,
    next: u32,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub src: NodeId,
    pub dst: NodeId,
}

#[derive(Clone)]
pub struct Graph {
    heads: Vec<u32>,
    half_edges: Vec<HalfEdge>,
    edges: Vec<Edge>,
}

impl Graph {
    pub fn with_capacity(n: usize, m: usize) -> Self {
        Graph {
            heads: Vec::with_capacity(n),
            half_edges: Vec::with_capacity(2 * m),
            edges: Vec::with_capacity(m),
        }
    }

    pub fn from_edge_arrays(num_nodes: usize, src: &[u32], dst: &[u32]) -> Self {
        debug_assert_eq!(src.len(), dst.len());
        let num_edges = src.len();
        let mut heads = vec![INVALID; num_nodes];
        let mut half_edges = Vec::with_capacity(2 * num_edges);
        let mut edges = Vec::with_capacity(num_edges);
        for i in 0..num_edges {
            let u = src[i];
            let v = dst[i];
            let eid = i as u32;
            edges.push(Edge {
                src: NodeId(u),
                dst: NodeId(v),
            });
            let idx_uv = half_edges.len() as u32;
            let idx_vu = idx_uv + 1;
            half_edges.push(HalfEdge {
                target: NodeId(v),
                edge_id: EdgeId(eid),
                next: heads[u as usize],
            });
            heads[u as usize] = idx_uv;
            half_edges.push(HalfEdge {
                target: NodeId(u),
                edge_id: EdgeId(eid),
                next: heads[v as usize],
            });
            heads[v as usize] = idx_vu;
        }
        Graph {
            heads,
            half_edges,
            edges,
        }
    }

    pub fn from_edge_pairs(num_nodes: usize, pairs: &[u32]) -> Self {
        debug_assert_eq!(pairs.len() % 2, 0);
        let num_edges = pairs.len() / 2;
        let mut heads = vec![INVALID; num_nodes];
        let mut half_edges = Vec::with_capacity(2 * num_edges);
        let mut edges = Vec::with_capacity(num_edges);
        for i in 0..num_edges {
            let u = pairs[i * 2];
            let v = pairs[i * 2 + 1];
            let eid = i as u32;
            edges.push(Edge {
                src: NodeId(u),
                dst: NodeId(v),
            });
            let idx_uv = half_edges.len() as u32;
            let idx_vu = idx_uv + 1;
            half_edges.push(HalfEdge {
                target: NodeId(v),
                edge_id: EdgeId(eid),
                next: heads[u as usize],
            });
            heads[u as usize] = idx_uv;
            half_edges.push(HalfEdge {
                target: NodeId(u),
                edge_id: EdgeId(eid),
                next: heads[v as usize],
            });
            heads[v as usize] = idx_vu;
        }
        Graph {
            heads,
            half_edges,
            edges,
        }
    }
    pub fn add_node(&mut self) -> NodeId {
        let id = NodeId(self.heads.len() as u32);
        self.heads.push(INVALID);
        id
    }
    pub fn add_nodes(&mut self, n: usize) -> Vec<NodeId> {
        let start = self.heads.len() as u32;
        self.heads.resize(self.heads.len() + n, INVALID);
        (start..start + n as u32).map(NodeId).collect()
    }
    pub fn add_nodes_fast(&mut self, n: usize) {
        self.heads.resize(self.heads.len() + n, INVALID);
    }
    #[inline]
    pub fn num_nodes(&self) -> usize {
        self.heads.len()
    }
    #[inline]
    pub fn num_edges(&self) -> usize {
        self.edges.len()
    }
    pub fn add_edge(&mut self, u: NodeId, v: NodeId) -> EdgeId {
        let eid = EdgeId(self.edges.len() as u32);
        self.edges.push(Edge { src: u, dst: v });
        let idx_uv = self.half_edges.len() as u32;
        let idx_vu = idx_uv + 1;
        self.half_edges.push(HalfEdge {
            target: v,
            edge_id: eid,
            next: self.heads[u.idx()],
        });
        self.heads[u.idx()] = idx_uv;
        self.half_edges.push(HalfEdge {
            target: u,
            edge_id: eid,
            next: self.heads[v.idx()],
        });
        self.heads[v.idx()] = idx_vu;
        eid
    }
    #[inline]
    pub fn edge(&self, eid: EdgeId) -> &Edge {
        &self.edges[eid.idx()]
    }
    pub fn neighbors(&self, u: NodeId) -> NeighborIter<'_> {
        NeighborIter {
            graph: self,
            current: self.heads[u.idx()],
        }
    }
    pub fn degree(&self, u: NodeId) -> usize {
        self.neighbors(u).count()
    }
    /// reverse all adjacency lists so iteration order matches insertion order
    pub fn reverse_adj_lists(&mut self) {
        for v in 0..self.heads.len() {
            let mut prev = INVALID;
            let mut cur = self.heads[v];
            while cur != INVALID {
                let next = self.half_edges[cur as usize].next;
                self.half_edges[cur as usize].next = prev;
                prev = cur;
                cur = next;
            }
            self.heads[v] = prev;
        }
    }
    #[inline(always)]
    pub fn adj_cursor(&self, u: NodeId) -> u32 {
        self.heads[u.idx()]
    }
    #[inline(always)]
    pub fn adj_next(&self, cursor: u32) -> Option<(NodeId, EdgeId, u32)> {
        if cursor == INVALID {
            return None;
        }
        let he = &self.half_edges[cursor as usize];
        Some((he.target, he.edge_id, he.next))
    }
}

pub struct NeighborIter<'a> {
    graph: &'a Graph,
    current: u32,
}
impl<'a> Iterator for NeighborIter<'a> {
    type Item = (NodeId, EdgeId);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.current == INVALID {
            return None;
        }
        let he = &self.graph.half_edges[self.current as usize];
        self.current = he.next;
        Some((he.target, he.edge_id))
    }
}

// SPQR tree structures

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpqrNodeType {
    S,
    P,
    R,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct SkeletonEdge {
    pub src: NodeId,
    pub dst: NodeId,
    pub real_edge: EdgeId,
    pub virtual_id: u32,
    pub twin_tree_node: TreeNodeId,
    pub twin_edge_idx: u32,
}

impl Default for SkeletonEdge {
    fn default() -> Self {
        SkeletonEdge {
            src: NodeId::INVALID,
            dst: NodeId::INVALID,
            real_edge: EdgeId::INVALID,
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        }
    }
}

pub struct SkeletonView<'a> {
    pub num_nodes: u32,
    pub edges: &'a [SkeletonEdge],
    pub node_to_original: &'a [NodeId],
}

impl<'a> SkeletonView<'a> {
    pub fn poles(&self) -> (NodeId, NodeId) {
        (self.node_to_original[0], self.node_to_original[1])
    }
    pub fn num_edges(&self) -> usize {
        self.edges.len()
    }
}

pub struct SpqrTreeNodeView<'a> {
    pub node_type: SpqrNodeType,
    pub skeleton: SkeletonView<'a>,
    pub parent: TreeNodeId,
    pub children: &'a [TreeNodeId],
}

pub struct SpqrTree {
    pub root: TreeNodeId,
    pub node_types: Vec<SpqrNodeType>,
    pub node_parents: Vec<TreeNodeId>,
    pub children_offsets: Vec<u32>,
    pub children: Vec<TreeNodeId>,
    pub skeleton_offsets: Vec<u32>,
    pub skeleton_edges: Vec<SkeletonEdge>,
    pub node_mapping_offsets: Vec<u32>,
    pub node_mapping: Vec<NodeId>,
    pub skeleton_num_nodes: Vec<u32>,
    pub edge_to_tree_node: Vec<TreeNodeId>,
}

impl SpqrTree {
    pub fn len(&self) -> usize {
        self.node_types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.node_types.is_empty()
    }

    #[inline]
    pub fn node_type(&self, id: TreeNodeId) -> SpqrNodeType {
        self.node_types[id.idx()]
    }

    #[inline]
    pub fn parent(&self, id: TreeNodeId) -> TreeNodeId {
        self.node_parents[id.idx()]
    }

    #[inline]
    pub fn children_slice(&self, id: TreeNodeId) -> &[TreeNodeId] {
        let start = self.children_offsets[id.idx()] as usize;
        let end = self.children_offsets[id.idx() + 1] as usize;
        &self.children[start..end]
    }

    #[inline]
    pub fn skeleton_edges_slice(&self, id: TreeNodeId) -> &[SkeletonEdge] {
        let start = self.skeleton_offsets[id.idx()] as usize;
        let end = self.skeleton_offsets[id.idx() + 1] as usize;
        &self.skeleton_edges[start..end]
    }

    #[inline]
    pub fn skeleton_edges_slice_mut(&mut self, id: TreeNodeId) -> &mut [SkeletonEdge] {
        let start = self.skeleton_offsets[id.idx()] as usize;
        let end = self.skeleton_offsets[id.idx() + 1] as usize;
        &mut self.skeleton_edges[start..end]
    }

    #[inline]
    pub fn skeleton_edge_mut(
        &mut self,
        tree_node: TreeNodeId,
        edge_idx: usize,
    ) -> &mut SkeletonEdge {
        let start = self.skeleton_offsets[tree_node.idx()] as usize;
        &mut self.skeleton_edges[start + edge_idx]
    }

    #[inline]
    pub fn node_mapping_slice(&self, id: TreeNodeId) -> &[NodeId] {
        let start = self.node_mapping_offsets[id.idx()] as usize;
        let end = self.node_mapping_offsets[id.idx() + 1] as usize;
        &self.node_mapping[start..end]
    }

    #[inline]
    pub fn skeleton_num_nodes(&self, id: TreeNodeId) -> u32 {
        self.skeleton_num_nodes[id.idx()]
    }

    pub fn node(&self, id: TreeNodeId) -> SpqrTreeNodeView<'_> {
        SpqrTreeNodeView {
            node_type: self.node_types[id.idx()],
            skeleton: SkeletonView {
                num_nodes: self.skeleton_num_nodes[id.idx()],
                edges: self.skeleton_edges_slice(id),
                node_to_original: self.node_mapping_slice(id),
            },
            parent: self.node_parents[id.idx()],
            children: self.children_slice(id),
        }
    }

    pub fn tree_node_of_edge(&self, eid: EdgeId) -> TreeNodeId {
        self.edge_to_tree_node[eid.idx()]
    }

    pub fn count_by_type(&self) -> (usize, usize, usize) {
        let (mut s, mut p, mut r) = (0, 0, 0);
        for &t in &self.node_types {
            match t {
                SpqrNodeType::S => s += 1,
                SpqrNodeType::P => p += 1,
                SpqrNodeType::R => r += 1,
            }
        }
        (s, p, r)
    }

    pub fn iter(&self) -> impl Iterator<Item = TreeNodeId> + '_ {
        (0..self.len()).map(|i| TreeNodeId(i as u32))
    }

    fn empty(num_edges: usize) -> Self {
        SpqrTree {
            root: TreeNodeId::INVALID,
            node_types: Vec::new(),
            node_parents: Vec::new(),
            children_offsets: vec![0],
            children: Vec::new(),
            skeleton_offsets: vec![0],
            skeleton_edges: Vec::new(),
            node_mapping_offsets: vec![0],
            node_mapping: Vec::new(),
            skeleton_num_nodes: Vec::new(),
            edge_to_tree_node: vec![TreeNodeId::INVALID; num_edges],
        }
    }

    fn single_node(
        num_edges: usize,
        node_type: SpqrNodeType,
        num_skel_nodes: u32,
        edges: Vec<SkeletonEdge>,
        node_to_original: Vec<NodeId>,
    ) -> Self {
        let mut edge_to_tree_node = vec![TreeNodeId::INVALID; num_edges];
        for edge in &edges {
            if edge.real_edge.is_valid() {
                edge_to_tree_node[edge.real_edge.idx()] = TreeNodeId(0);
            }
        }

        SpqrTree {
            root: TreeNodeId(0),
            node_types: vec![node_type],
            node_parents: vec![TreeNodeId::INVALID],
            children_offsets: vec![0, 0],
            children: Vec::new(),
            skeleton_offsets: vec![0, edges.len() as u32],
            skeleton_edges: edges,
            node_mapping_offsets: vec![0, node_to_original.len() as u32],
            node_mapping: node_to_original,
            skeleton_num_nodes: vec![num_skel_nodes],
            edge_to_tree_node,
        }
    }
}

/// result of an SPQR decomposition.
///
/// any self-loops present in the input graph are collected in self_loops
///
/// for self loop edges, tree.tree_node_of_edge() returns TreeNodeId::INVALID.
pub struct SpqrResult {
    pub tree: SpqrTree,
    /// Selfloop edges (v,v) stripped before decomposition
    pub self_loops: Vec<EdgeId>,
}

// Triconnectivity decomposition

struct SpqrTreeBuilder {
    node_types: Vec<SpqrNodeType>,
    node_parents: Vec<TreeNodeId>,
    skeleton_num_nodes: Vec<u32>,
    skeleton_offsets: Vec<u32>,
    skeleton_edges: Vec<SkeletonEdge>,
    node_mapping_offsets: Vec<u32>,
    node_mapping: Vec<NodeId>,
    edge_to_tree_node: Vec<TreeNodeId>,
}

impl SpqrTreeBuilder {
    fn new(num_edges: usize) -> Self {
        SpqrTreeBuilder {
            node_types: Vec::new(),
            node_parents: Vec::new(),
            skeleton_num_nodes: Vec::new(),
            skeleton_offsets: vec![0],
            skeleton_edges: Vec::new(),
            node_mapping_offsets: vec![0],
            node_mapping: Vec::new(),
            edge_to_tree_node: vec![TreeNodeId::INVALID; num_edges],
        }
    }

    fn add_node(
        &mut self,
        node_type: SpqrNodeType,
        num_nodes: u32,
        edges: Vec<SkeletonEdge>,
        node_to_original: Vec<NodeId>,
    ) -> TreeNodeId {
        let tid = TreeNodeId(self.node_types.len() as u32);

        // Mark real edges
        for edge in &edges {
            if edge.real_edge.is_valid() {
                self.edge_to_tree_node[edge.real_edge.idx()] = tid;
            }
        }

        self.node_types.push(node_type);
        self.node_parents.push(TreeNodeId::INVALID);
        self.skeleton_num_nodes.push(num_nodes);

        // Skeleton edges
        self.skeleton_edges.extend(edges);
        self.skeleton_offsets.push(self.skeleton_edges.len() as u32);

        // Node mapping
        self.node_mapping.extend(node_to_original);
        self.node_mapping_offsets
            .push(self.node_mapping.len() as u32);

        tid
    }

    fn skeleton_edge_mut(&mut self, tree_node: TreeNodeId, edge_idx: usize) -> &mut SkeletonEdge {
        let start = self.skeleton_offsets[tree_node.idx()] as usize;
        &mut self.skeleton_edges[start + edge_idx]
    }

    fn skeleton_edges_len(&self, tree_node: TreeNodeId) -> usize {
        let start = self.skeleton_offsets[tree_node.idx()] as usize;
        let end = self.skeleton_offsets[tree_node.idx() + 1] as usize;
        end - start
    }

    fn num_nodes(&self) -> usize {
        self.node_types.len()
    }

    fn finalize_with_children(
        self,
        root: TreeNodeId,
        children_offsets: Vec<u32>,
        children: Vec<TreeNodeId>,
    ) -> SpqrTree {
        SpqrTree {
            root,
            node_types: self.node_types,
            node_parents: self.node_parents,
            children_offsets,
            children,
            skeleton_offsets: self.skeleton_offsets,
            skeleton_edges: self.skeleton_edges,
            node_mapping_offsets: self.node_mapping_offsets,
            node_mapping: self.node_mapping,
            skeleton_num_nodes: self.skeleton_num_nodes,
            edge_to_tree_node: self.edge_to_tree_node,
        }
    }

    fn finalize_empty(self) -> SpqrTree {
        SpqrTree {
            root: TreeNodeId::INVALID,
            node_types: Vec::new(),
            node_parents: Vec::new(),
            children_offsets: vec![0],
            children: Vec::new(),
            skeleton_offsets: vec![0],
            skeleton_edges: Vec::new(),
            node_mapping_offsets: vec![0],
            node_mapping: Vec::new(),
            skeleton_num_nodes: Vec::new(),
            edge_to_tree_node: self.edge_to_tree_node,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct StackEdge {
    src: u32,
    dst: u32,
    eid: u32,
}

#[derive(Clone, Debug)]
struct SplitComponent {
    edges: Vec<StackEdge>,
    pole_a: u32,
    pole_b: u32,
}

// PUBLIC ENTRY POINTS HERE

/// Build an SPQR tree
///
/// we returns here an SpqrResult whose tree is a SPQR tree and whose self_loops contains any (v,v) edges found in the input
pub fn build_spqr(graph: &Graph) -> SpqrResult {
    let m = graph.num_edges();

    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u32));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u32));
        }
    }

    let tree = build_spqr_tree_filtered(graph, &is_self_loop);
    SpqrResult { tree, self_loops }
}

/// Build an SPQR tree from a graph known to contain no self loops
///
/// we set panics in debug mode if a self loop is found.  For graphs that may contain self loops, use build_spqr instead.
pub fn build_spqr_tree(graph: &Graph) -> SpqrTree {
    let m = graph.num_edges();
    debug_assert!(
        (0..m).all(|i| {
            let e = graph.edge(EdgeId(i as u32));
            e.src != e.dst
        }),
        "Graph contains self-loops; use build_spqr() instead"
    );
    let no_self_loops = vec![false; m];
    build_spqr_tree_filtered(graph, &no_self_loops)
}

fn build_spqr_tree_filtered(graph: &Graph, is_self_loop: &[bool]) -> SpqrTree {
    let n = graph.num_nodes();
    let m = graph.num_edges();
    let m_real = m - is_self_loop.iter().filter(|&&b| b).count();

    if n == 0 || m_real == 0 {
        return SpqrTree::empty(m);
    }
    if n == 1 {
        return SpqrTree::empty(m);
    }
    if m_real == 1 {
        let mut eid_real = 0;
        for i in 0..m {
            if !is_self_loop[i] {
                eid_real = i;
                break;
            }
        }
        let e = graph.edge(EdgeId(eid_real as u32));
        let edges = vec![SkeletonEdge {
            src: NodeId(0),
            dst: NodeId(1),
            real_edge: EdgeId(eid_real as u32),
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        }];
        return SpqrTree::single_node(m, SpqrNodeType::P, 2, edges, vec![e.src, e.dst]);
    }

    // Count distinct non self loop endpoints
    let mut has_non_loop_node = [false, false];
    let mut all_between_01 = true;
    for i in 0..m {
        if is_self_loop[i] {
            continue;
        }
        let e = graph.edge(EdgeId(i as u32));
        let (a, b) = (e.src.0.min(e.dst.0), e.src.0.max(e.dst.0));
        if a == 0 && b == 1 {
            has_non_loop_node[0] = true;
            has_non_loop_node[1] = true;
        } else {
            all_between_01 = false;
            break;
        }
    }
    if n == 2 || (all_between_01 && has_non_loop_node[0]) {
        return build_parallel_case(graph, is_self_loop);
    }

    let mut next_virtual = m as u32;
    let (multi_comps, synthetic, consumed) =
        split_multi_edges(graph, &mut next_virtual, is_self_loop);

    // Build working graph (exclude self loops AND consumed multi-edges)
    let real_count = (0..m).filter(|&i| !consumed[i] && !is_self_loop[i]).count();
    let total = real_count + synthetic.len();
    let mut wg = Graph::with_capacity(n, total);
    wg.add_nodes(n);
    let mut weid_to_label: Vec<u32> = Vec::with_capacity(total);
    for i in 0..m {
        if !consumed[i] && !is_self_loop[i] {
            let e = graph.edge(EdgeId(i as u32));
            wg.add_edge(e.src, e.dst);
            weid_to_label.push(i as u32);
        }
    }
    for &(a, b, vid) in &synthetic {
        wg.add_edge(NodeId(a), NodeId(b));
        weid_to_label.push(vid);
    }

    let wg_m = wg.num_edges();
    let ref_weid = {
        let mut found = EdgeId::INVALID;
        for i in 0..wg_m {
            let e = wg.edge(EdgeId(i as u32));
            if e.src != e.dst {
                found = EdgeId(i as u32);
                break;
            }
        }
        found
    };
    let used_triconn = n >= 3 && wg_m >= 3 && ref_weid.is_valid();

    let empty_consumed: Vec<bool> = Vec::new();
    let mut wcomps = if used_triconn {
        triconn_decompose(&wg, ref_weid, &mut next_virtual, &empty_consumed)
    } else {
        let edges: Vec<StackEdge> = (0..wg_m)
            .map(|i| {
                let e = wg.edge(EdgeId(i as u32));
                StackEdge {
                    src: e.src.0,
                    dst: e.dst.0,
                    eid: i as u32,
                }
            })
            .collect();
        if edges.is_empty() {
            Vec::new()
        } else {
            let (pa, pb) = (edges[0].src, edges[0].dst);
            vec![SplitComponent {
                edges,
                pole_a: pa,
                pole_b: pb,
            }]
        }
    };

    for comp in &mut wcomps {
        for se in &mut comp.edges {
            let i = se.eid as usize;
            if i < weid_to_label.len() {
                se.eid = weid_to_label[i];
            }
        }
    }

    let mut all = combine_components(multi_comps, wcomps, &mut next_virtual);
    merge_same_type_components(&mut all, m);

    assemble_spqr_tree(graph, &all, next_virtual)
}

fn build_parallel_case(graph: &Graph, is_self_loop: &[bool]) -> SpqrTree {
    let m = graph.num_edges();
    let mut edges = Vec::new();
    let mut edge_to_tree_node = vec![TreeNodeId::INVALID; m];

    for i in 0..m {
        if is_self_loop[i] {
            continue;
        }
        let e = graph.edge(EdgeId(i as u32));
        edges.push(SkeletonEdge {
            src: if e.src == NodeId(0) {
                NodeId(0)
            } else {
                NodeId(1)
            },
            dst: if e.src == NodeId(0) {
                NodeId(1)
            } else {
                NodeId(0)
            },
            real_edge: EdgeId(i as u32),
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        });
        edge_to_tree_node[i] = TreeNodeId(0);
    }

    SpqrTree {
        root: TreeNodeId(0),
        node_types: vec![SpqrNodeType::P],
        node_parents: vec![TreeNodeId::INVALID],
        children_offsets: vec![0, 0],
        children: Vec::new(),
        skeleton_offsets: vec![0, edges.len() as u32],
        skeleton_edges: edges,
        node_mapping_offsets: vec![0, 2],
        node_mapping: vec![NodeId(0), NodeId(1)],
        skeleton_num_nodes: vec![2],
        edge_to_tree_node,
    }
}

// Multi edge preprocessing

#[allow(clippy::type_complexity)]
fn split_multi_edges(
    graph: &Graph,
    next_virtual: &mut u32,
    is_self_loop: &[bool],
) -> (Vec<SplitComponent>, Vec<(u32, u32, u32)>, Vec<bool>) {
    let m = graph.num_edges();
    let mut pair_to_eids: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for i in 0..m {
        if is_self_loop[i] {
            continue;
        }
        let e = graph.edge(EdgeId(i as u32));
        let (a, b) = if e.src.0 <= e.dst.0 {
            (e.src.0, e.dst.0)
        } else {
            (e.dst.0, e.src.0)
        };
        pair_to_eids.entry((a, b)).or_default().push(i as u32);
    }
    let mut p_comps = Vec::new();
    let mut consumed = vec![false; m];
    let mut synthetic = Vec::new();
    for (&(a, b), eids) in &pair_to_eids {
        if eids.len() >= 2 {
            let vid = *next_virtual;
            *next_virtual += 1;
            let mut edges: Vec<StackEdge> = eids
                .iter()
                .map(|&eid| {
                    let e = graph.edge(EdgeId(eid));
                    StackEdge {
                        src: e.src.0,
                        dst: e.dst.0,
                        eid,
                    }
                })
                .collect();
            edges.push(StackEdge {
                src: a,
                dst: b,
                eid: vid,
            });
            p_comps.push(SplitComponent {
                edges,
                pole_a: a,
                pole_b: b,
            });
            for &eid in eids {
                consumed[eid as usize] = true;
            }
            synthetic.push((a, b, vid));
        }
    }
    (p_comps, synthetic, consumed)
}

fn triconn_decompose(
    graph: &Graph,
    reference_eid: EdgeId,
    next_virtual: &mut u32,
    consumed: &[bool],
) -> Vec<SplitComponent> {
    let n = graph.num_nodes();
    let m = graph.num_edges();
    assert!(reference_eid.is_valid() && reference_eid.idx() < m);

    let mut me_src: Vec<u32> = Vec::with_capacity(m * 2);
    let mut me_dst: Vec<u32> = Vec::with_capacity(m * 2);
    let mut me_orig: Vec<u32> = Vec::with_capacity(m * 2);
    let mut me_etype: Vec<u8> = Vec::with_capacity(m * 2);
    let mut me_start: Vec<bool> = Vec::with_capacity(m * 2);
    let mut me_adj_v: Vec<u32> = Vec::with_capacity(m * 2);
    let mut me_adj_p: Vec<u32> = Vec::with_capacity(m * 2);
    let mut me_hi_slot: Vec<u32> = Vec::with_capacity(m * 2);

    macro_rules! new_edge {
        ($src:expr, $dst:expr, $orig:expr, $et:expr) => {{
            let i = me_src.len() as u32;
            me_src.push($src);
            me_dst.push($dst);
            me_orig.push($orig);
            me_etype.push($et);
            me_start.push(false);
            me_adj_v.push(INVALID);
            me_adj_p.push(INVALID);
            me_hi_slot.push(INVALID);
            i
        }};
    }

    let mut al_edge: Vec<u32> = Vec::with_capacity(m);
    let mut al_next: Vec<u32> = Vec::with_capacity(m);
    let mut al_prev: Vec<u32> = Vec::with_capacity(m);
    let mut ah_head: Vec<u32> = vec![INVALID; n];
    let mut ah_tail: Vec<u32> = vec![INVALID; n];
    let mut ah_count: Vec<i32> = vec![0; n];
    struct HpEntry {
        val: i32,
        next: u32,
        deleted: bool,
    }
    let mut hp_arena: Vec<HpEntry> = Vec::with_capacity(m);
    let mut hp_head: Vec<u32> = vec![INVALID; n];
    let mut hp_tail: Vec<u32> = vec![INVALID; n];

    let mut degree = vec![0i32; n];
    let mut father: Vec<i32> = vec![-1; n];
    let mut tree_arc = vec![INVALID; n];
    let mut newnum = vec![0i32; n];
    let mut lp1 = vec![0i32; n];
    let mut lp2 = vec![0i32; n];
    let mut nd_arr = vec![1i32; n];
    let mut nodeat = vec![0u32; n + 1];

    for v in 0..n {
        degree[v] = graph.degree(NodeId(v as u32)) as i32;
    }
    for i in 0..consumed.len().min(m) {
        if consumed[i] {
            let e = graph.edge(EdgeId(i as u32));
            degree[e.src.idx()] -= 1;
            degree[e.dst.idx()] -= 1;
        }
    }

    let mut number = vec![0i32; n];
    let mut etype_orig = vec![0u8; m];
    {
        let mut nc = 0i32;
        let mut seen = vec![false; m];
        for i in 0..consumed.len().min(m) {
            if consumed[i] {
                seen[i] = true;
                etype_orig[i] = 3;
            }
        }
        let s0 = 0u32;
        nc += 1;
        number[s0 as usize] = nc;
        lp1[s0 as usize] = nc;
        lp2[s0 as usize] = nc;
        struct F {
            v: u32,
            he: u32,
        }
        let mut stk = vec![F {
            v: s0,
            he: graph.heads[s0 as usize],
        }];
        while let Some(fr) = stk.last_mut() {
            let v = fr.v;
            if fr.he == INVALID {
                stk.pop();
                if let Some(p) = stk.last() {
                    let pv = p.v as usize;
                    nd_arr[pv] += nd_arr[v as usize];
                    let (a, b) = (lp1[v as usize], lp2[v as usize]);
                    if a < lp1[pv] {
                        lp2[pv] = std::cmp::min(lp1[pv], b);
                        lp1[pv] = a;
                    } else if a == lp1[pv] {
                        lp2[pv] = std::cmp::min(lp2[pv], b);
                    } else {
                        lp2[pv] = std::cmp::min(lp2[pv], a);
                    }
                }
                continue;
            }
            let he = &graph.half_edges[fr.he as usize];
            fr.he = he.next;
            let w = he.target.0;
            let ei = he.edge_id.0 as usize;
            if seen[ei] {
                continue;
            }
            seen[ei] = true;
            if number[w as usize] == 0 {
                etype_orig[ei] = 1;
                nc += 1;
                number[w as usize] = nc;
                father[w as usize] = v as i32;
                lp1[w as usize] = nc;
                lp2[w as usize] = nc;
                tree_arc[w as usize] = ei as u32;
                stk.push(F {
                    v: w,
                    he: graph.heads[w as usize],
                });
            } else {
                etype_orig[ei] = 2;
                let nw = number[w as usize];
                if nw < lp1[v as usize] {
                    lp2[v as usize] = lp1[v as usize];
                    lp1[v as usize] = nw;
                } else if nw > lp1[v as usize] {
                    lp2[v as usize] = std::cmp::min(lp2[v as usize], nw);
                }
            }
        }
        assert!(nc as usize == n, "not connected: {} / {}", nc, n);
    }

    let mut esrc = vec![0u32; m];
    let mut edst = vec![0u32; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u32));
        let (s, t) = (e.src.0, e.dst.0);
        let up = number[t as usize] > number[s as usize];
        if (up && etype_orig[i] == 2) || (!up && etype_orig[i] == 1) {
            esrc[i] = t;
            edst[i] = s;
        } else {
            esrc[i] = s;
            edst[i] = t;
        }
    }

    let maxb = 3 * n as i32 + 2;

    // Build oadj in CSR format without Vec<Vec>
    // Pass 1: Count edges per phi bucket and per source node
    let mut phi_count: Vec<u32> = vec![0; (maxb + 2) as usize];
    let mut oadj_count: Vec<u32> = vec![0; n];
    let mut edge_phi: Vec<i32> = vec![0; m];

    for i in 0..m {
        if etype_orig[i] == 0 || etype_orig[i] == 3 {
            continue;
        }
        let w = edst[i];
        let vs = esrc[i];
        let phi = if etype_orig[i] == 2 {
            3 * number[w as usize] + 1
        } else if lp2[w as usize] < number[vs as usize] {
            3 * lp1[w as usize]
        } else {
            3 * lp1[w as usize] + 2
        };
        if phi >= 1 && phi <= maxb {
            edge_phi[i] = phi;
            phi_count[phi as usize] += 1;
            oadj_count[esrc[i] as usize] += 1;
        }
    }

    // Build phi bucket offsets
    let mut phi_offsets: Vec<u32> = vec![0; (maxb + 2) as usize];
    for i in 1..=(maxb as usize + 1) {
        phi_offsets[i] = phi_offsets[i - 1] + phi_count[i - 1];
    }
    let total_edges = phi_offsets[maxb as usize + 1] as usize;

    // Build oadj offsets
    let mut oadj_offsets: Vec<u32> = vec![0; n + 1];
    for i in 0..n {
        oadj_offsets[i + 1] = oadj_offsets[i] + oadj_count[i];
    }

    // Pass 2: Place edges into phi buckets
    let mut bkt_flat: Vec<u32> = vec![0; total_edges];
    let mut phi_write: Vec<u32> = phi_offsets[..=(maxb as usize)].to_vec();
    for i in 0..m {
        let phi = edge_phi[i];
        if phi >= 1 && phi <= maxb {
            let pos = phi_write[phi as usize] as usize;
            bkt_flat[pos] = i as u32;
            phi_write[phi as usize] += 1;
        }
    }
    drop(phi_write);
    drop(phi_count);
    drop(edge_phi);

    // Pass 3: Build oadj_flat from sorted buckets
    let mut oadj_flat: Vec<u32> = vec![0; total_edges];
    let mut oadj_write: Vec<u32> = oadj_offsets[..n].to_vec();
    for phi in 1..=(maxb as usize) {
        let start = phi_offsets[phi] as usize;
        let end = phi_offsets[phi + 1] as usize;
        for idx in start..end {
            let ei = bkt_flat[idx];
            let src = esrc[ei as usize] as usize;
            let pos = oadj_write[src] as usize;
            oadj_flat[pos] = ei;
            oadj_write[src] += 1;
        }
    }
    drop(bkt_flat);
    drop(phi_offsets);
    drop(oadj_write);

    let mut startf = vec![false; m];
    let mut hp_init: Vec<Vec<(i32, u32)>> = vec![Vec::new(); n];
    {
        let mut nc = n as i32;
        let mut np = true;
        let s0 = 0u32;
        newnum[s0 as usize] = nc - nd_arr[s0 as usize] + 1;
        struct PF {
            v: u32,
            idx: usize,
            pend: bool,
        }
        let mut pfs = vec![PF {
            v: s0,
            idx: 0,
            pend: false,
        }];
        while let Some(fr) = pfs.last_mut() {
            if fr.pend {
                fr.pend = false;
                nc -= 1;
            }
            let v = fr.v as usize;
            let oadj_len = (oadj_offsets[v + 1] - oadj_offsets[v]) as usize;
            if fr.idx >= oadj_len {
                pfs.pop();
                continue;
            }
            let ei = oadj_flat[oadj_offsets[v] as usize + fr.idx] as usize;
            fr.idx += 1;
            let w = edst[ei];
            if np {
                np = false;
                startf[ei] = true;
            }
            if etype_orig[ei] == 1 {
                fr.pend = true;
                newnum[w as usize] = nc - nd_arr[w as usize] + 1;
                pfs.push(PF {
                    v: w,
                    idx: 0,
                    pend: false,
                });
            } else {
                hp_init[w as usize].push((newnum[fr.v as usize], ei as u32));
                np = true;
            }
        }
    }

    let mut o2n = vec![0i32; n + 1];
    for v in 0..n {
        o2n[number[v] as usize] = newnum[v];
    }
    for v in 0..n {
        lp1[v] = o2n[lp1[v] as usize];
        lp2[v] = o2n[lp2[v] as usize];
    }
    for v in 0..n {
        nodeat[newnum[v] as usize] = v as u32;
    }

    for i in 0..m {
        let idx = new_edge!(esrc[i], edst[i], i as u32, etype_orig[i]);
        me_start[idx as usize] = startf[i];
    }
    for v in 0..n {
        for idx in oadj_offsets[v] as usize..oadj_offsets[v + 1] as usize {
            let ei = oadj_flat[idx];
            let slot = al_edge.len() as u32;
            al_edge.push(ei);
            al_next.push(INVALID);
            al_prev.push(ah_tail[v]);
            if ah_tail[v] != INVALID {
                al_next[ah_tail[v] as usize] = slot;
            } else {
                ah_head[v] = slot;
            }
            ah_tail[v] = slot;
            ah_count[v] += 1;
            me_adj_v[ei as usize] = v as u32;
            me_adj_p[ei as usize] = slot;
        }
    }
    for v in 0..n {
        for &(val, eidx) in &hp_init[v] {
            let slot = hp_arena.len() as u32;
            hp_arena.push(HpEntry {
                val,
                next: INVALID,
                deleted: false,
            });
            if hp_tail[v] != INVALID {
                hp_arena[hp_tail[v] as usize].next = slot;
            } else {
                hp_head[v] = slot;
            }
            hp_tail[v] = slot;
            me_hi_slot[eidx as usize] = slot;
        }
    }

    macro_rules! high {
        ($v:expr) => {{
            let __vi = $v as usize;
            while hp_head[__vi] != INVALID && hp_arena[hp_head[__vi] as usize].deleted {
                hp_head[__vi] = hp_arena[hp_head[__vi] as usize].next;
            }
            if hp_head[__vi] == INVALID {
                0i32
            } else {
                hp_arena[hp_head[__vi] as usize].val
            }
        }};
    }

    macro_rules! adj_front {
        ($v:expr) => {{
            let h = ah_head[$v as usize];
            if h == INVALID {
                None
            } else {
                Some((al_edge[h as usize], h))
            }
        }};
    }
    macro_rules! adj_count {
        ($v:expr) => {
            ah_count[$v as usize]
        };
    }
    macro_rules! next_slot {
        ($after:expr) => {{
            let ns = al_next[$after as usize];
            if ns == INVALID {
                None
            } else {
                let __ei = al_edge[ns as usize];
                let __w = me_dst[__ei as usize];
                Some((ns, __ei, __w, newnum[__w as usize]))
            }
        }};
    }

    macro_rules! del_adj {
        ($ei:expr) => {
            let __v = me_adj_v[$ei as usize] as usize;
            let __s = me_adj_p[$ei as usize];
            if __v != INVALID as usize {
                let __prev = al_prev[__s as usize];
                let __next = al_next[__s as usize];
                if __prev != INVALID {
                    al_next[__prev as usize] = __next;
                } else {
                    ah_head[__v] = __next;
                }
                if __next != INVALID {
                    al_prev[__next as usize] = __prev;
                } else {
                    ah_tail[__v] = __prev;
                }
                ah_count[__v] -= 1;
            }
        };
    }
    macro_rules! del_adj_slot {
        ($v:expr, $slot:expr) => {{
            let __v2 = $v as usize;
            let __s2 = $slot as usize;
            let __prev = al_prev[__s2];
            let __next = al_next[__s2];
            if __prev != INVALID {
                al_next[__prev as usize] = __next;
            } else {
                ah_head[__v2] = __next;
            }
            if __next != INVALID {
                al_prev[__next as usize] = __prev;
            } else {
                ah_tail[__v2] = __prev;
            }
            ah_count[__v2] -= 1;
        }};
    }
    macro_rules! del_high {
        ($ei:expr) => {
            let slot = me_hi_slot[$ei as usize];
            if slot != INVALID && (slot as usize) < hp_arena.len() {
                hp_arena[slot as usize].deleted = true;
            }
        };
    }
    macro_rules! replace_adj {
        ($v:expr, $slot:expr, $new_ei:expr) => {
            al_edge[$slot as usize] = $new_ei;
            me_adj_v[$new_ei as usize] = $v;
            me_adj_p[$new_ei as usize] = $slot;
        };
    }
    macro_rules! se {
        ($ei:expr) => {
            StackEdge {
                src: me_src[$ei as usize],
                dst: me_dst[$ei as usize],
                eid: me_orig[$ei as usize],
            }
        };
    }

    let tsz = 2 * (m + n) + 2;
    let mut th = vec![0i32; tsz];
    let mut ta = vec![0i32; tsz];
    let mut tb = vec![0i32; tsz];
    let mut top: usize = 0;
    ta[0] = -1;

    let mut estack: Vec<u32> = Vec::with_capacity(m + n);
    let mut comps: Vec<SplitComponent> = Vec::new();

    struct PS {
        v: u32,
        vn: i32,
        outv: i32,
        cur: u32,
        after: bool,
        ei: u32,
        w: u32,
        wn: i32,
        it: u32,
        tei: u32,
    }

    let s0 = 0u32;
    let (fei, fpos) = adj_front!(s0).expect("start vertex has no adj");
    let fw = me_dst[fei as usize];
    let mut cs: Vec<PS> = vec![PS {
        v: s0,
        vn: newnum[s0 as usize],
        outv: adj_count!(s0),
        cur: fpos,
        after: false,
        ei: fei,
        w: fw,
        wn: newnum[fw as usize],
        it: fpos,
        tei: INVALID,
    }];

    while !cs.is_empty() {
        let idx = cs.len() - 1;

        if !cs[idx].after && me_etype[cs[idx].ei as usize] == 1 {
            let ei = cs[idx].ei;
            let w = cs[idx].w;
            let vn = cs[idx].vn;
            if me_start[ei as usize] {
                if ta[top] > lp1[w as usize] {
                    let mut y = 0i32;
                    let mut bv;
                    loop {
                        y = std::cmp::max(y, th[top]);
                        bv = tb[top];
                        top -= 1;
                        if ta[top] <= lp1[w as usize] {
                            break;
                        }
                    }
                    top += 1;
                    th[top] = y;
                    ta[top] = lp1[w as usize];
                    tb[top] = bv;
                } else {
                    top += 1;
                    th[top] = newnum[w as usize] + nd_arr[w as usize] - 1;
                    ta[top] = lp1[w as usize];
                    tb[top] = vn;
                }
                top += 1;
                ta[top] = -1;
            }
            cs[idx].after = true;
            cs[idx].it = cs[idx].cur;
            cs[idx].tei = ei;
            if let Some((ce, cp)) = adj_front!(w) {
                let cw = me_dst[ce as usize];
                cs.push(PS {
                    v: w,
                    vn: newnum[w as usize],
                    outv: adj_count!(w),
                    cur: cp,
                    after: false,
                    ei: ce,
                    w: cw,
                    wn: newnum[cw as usize],
                    it: cp,
                    tei: INVALID,
                });
            }
            continue;
        } else if cs[idx].after {
            let v = cs[idx].v;
            let vn = cs[idx].vn;
            let itp = cs[idx].it;
            let tei = cs[idx].tei;
            let mut w = cs[idx].w;
            let mut wn = cs[idx].wn;

            estack.push(tree_arc[w as usize]);

            while vn != 1
                && (ta[top] == vn
                    || (degree[w as usize] == 2
                        && adj_front!(w)
                            .map_or(false, |(fe, _)| newnum[me_dst[fe as usize] as usize] > wn)))
            {
                let a = ta[top];
                let b = tb[top];
                if a == vn && father[nodeat[b as usize] as usize] == nodeat[a as usize] as i32 {
                    top -= 1;
                } else {
                    let mut eab: Option<u32> = None;

                    if degree[w as usize] == 2
                        && adj_front!(w)
                            .map_or(false, |(fe, _)| newnum[me_dst[fe as usize] as usize] > wn)
                    {
                        let e1 = estack.pop().unwrap();
                        let e2 = estack.pop().unwrap();
                        del_adj!(e2);
                        let x = me_dst[e2 as usize];
                        degree[x as usize] -= 1;
                        degree[v as usize] -= 1;
                        let vid = *next_virtual;
                        *next_virtual += 1;
                        let ev = new_edge!(v, x, vid, 1);
                        comps.push(SplitComponent {
                            edges: vec![
                                se!(e1),
                                se!(e2),
                                StackEdge {
                                    src: v,
                                    dst: x,
                                    eid: vid,
                                },
                            ],
                            pole_a: v,
                            pole_b: x,
                        });
                        if let Some(&et) = estack.last() {
                            if me_src[et as usize] == x && me_dst[et as usize] == v {
                                let eab2 = estack.pop().unwrap();
                                del_adj!(eab2);
                                del_high!(eab2);
                                eab = Some(eab2);
                            }
                        }
                        let mut cur_virt = ev;
                        let cur_vid = vid;
                        if let Some(eab_v) = eab {
                            let vid2 = *next_virtual;
                            *next_virtual += 1;
                            let nv2 = new_edge!(v, x, vid2, 1);
                            comps.push(SplitComponent {
                                edges: vec![
                                    se!(eab_v),
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: cur_vid,
                                    },
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: vid2,
                                    },
                                ],
                                pole_a: v,
                                pole_b: x,
                            });
                            degree[x as usize] -= 1;
                            degree[v as usize] -= 1;
                            cur_virt = nv2;
                        }
                        estack.push(cur_virt);
                        replace_adj!(v, itp, cur_virt);
                        degree[x as usize] += 1;
                        degree[v as usize] += 1;
                        father[x as usize] = v as i32;
                        tree_arc[x as usize] = cur_virt;
                        me_etype[cur_virt as usize] = 1;
                        w = x;
                        wn = newnum[w as usize];
                    } else {
                        let h = th[top];
                        top -= 1;
                        let mut ce: Vec<StackEdge> = Vec::new();
                        while let Some(&et) = estack.last() {
                            let nx = newnum[me_src[et as usize] as usize];
                            let ny = newnum[me_dst[et as usize] as usize];
                            if !(a <= nx && nx <= h && a <= ny && ny <= h) {
                                break;
                            }
                            if (nx == a && ny == b) || (ny == a && nx == b) {
                                let eab2 = estack.pop().unwrap();
                                del_adj!(eab2);
                                del_high!(eab2);
                                eab = Some(eab2);
                            } else {
                                let eh = estack.pop().unwrap();
                                if !(me_adj_v[eh as usize] == v && me_adj_p[eh as usize] == itp) {
                                    del_adj!(eh);
                                    del_high!(eh);
                                }
                                ce.push(se!(eh));
                                degree[me_src[eh as usize] as usize] -= 1;
                                degree[me_dst[eh as usize] as usize] -= 1;
                            }
                        }
                        let pa = nodeat[a as usize];
                        let pb = nodeat[b as usize];
                        let vid = *next_virtual;
                        *next_virtual += 1;
                        let ev = new_edge!(pa, pb, vid, 1);
                        ce.push(StackEdge {
                            src: pa,
                            dst: pb,
                            eid: vid,
                        });
                        comps.push(SplitComponent {
                            edges: ce,
                            pole_a: pa,
                            pole_b: pb,
                        });
                        let x = pb;
                        let mut cur_virt = ev;
                        let cur_vid = vid;
                        if let Some(eab_v) = eab {
                            let vid2 = *next_virtual;
                            *next_virtual += 1;
                            let nv2 = new_edge!(v, x, vid2, 1);
                            comps.push(SplitComponent {
                                edges: vec![
                                    se!(eab_v),
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: cur_vid,
                                    },
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: vid2,
                                    },
                                ],
                                pole_a: v,
                                pole_b: x,
                            });
                            degree[x as usize] -= 1;
                            degree[v as usize] -= 1;
                            cur_virt = nv2;
                        }
                        estack.push(cur_virt);
                        replace_adj!(v, itp, cur_virt);
                        degree[x as usize] += 1;
                        degree[v as usize] += 1;
                        father[x as usize] = v as i32;
                        tree_arc[x as usize] = cur_virt;
                        me_etype[cur_virt as usize] = 1;
                        w = x;
                        wn = newnum[w as usize];
                    }
                }
            }

            if lp2[w as usize] >= vn
                && lp1[w as usize] < vn
                && (father[v as usize] != s0 as i32 || cs[idx].outv >= 2)
            {
                let l1 = lp1[w as usize];
                let mut ce: Vec<StackEdge> = Vec::new();
                let mut xx = 0i32;
                let mut yy = 0i32;
                while let Some(&et) = estack.last() {
                    xx = newnum[me_src[et as usize] as usize];
                    yy = newnum[me_dst[et as usize] as usize];
                    if !((wn <= xx && xx < wn + nd_arr[w as usize])
                        || (wn <= yy && yy < wn + nd_arr[w as usize]))
                    {
                        break;
                    }
                    let eh = estack.pop().unwrap();
                    del_high!(eh);
                    ce.push(se!(eh));
                    degree[nodeat[xx as usize] as usize] -= 1;
                    degree[nodeat[yy as usize] as usize] -= 1;
                }
                let pl = nodeat[l1 as usize];
                let vid = *next_virtual;
                *next_virtual += 1;
                let mut ev = new_edge!(v, pl, vid, 1);
                let cur_vid = vid;
                ce.push(StackEdge {
                    src: v,
                    dst: pl,
                    eid: vid,
                });
                comps.push(SplitComponent {
                    edges: ce,
                    pole_a: v,
                    pole_b: pl,
                });

                if (xx == vn && yy == l1) || (yy == vn && xx == l1) {
                    if let Some(eh) = estack.pop() {
                        if !(me_adj_v[eh as usize] == v && me_adj_p[eh as usize] == itp) {
                            del_adj!(eh);
                        }
                        let vid2 = *next_virtual;
                        *next_virtual += 1;
                        let nv2 = new_edge!(v, pl, vid2, 1);
                        comps.push(SplitComponent {
                            edges: vec![
                                se!(eh),
                                StackEdge {
                                    src: v,
                                    dst: pl,
                                    eid: cur_vid,
                                },
                                StackEdge {
                                    src: v,
                                    dst: pl,
                                    eid: vid2,
                                },
                            ],
                            pole_a: v,
                            pole_b: pl,
                        });
                        me_hi_slot[nv2 as usize] = me_hi_slot[eh as usize];
                        degree[v as usize] -= 1;
                        degree[pl as usize] -= 1;
                        ev = nv2;
                        me_etype[nv2 as usize] = 1;
                    }
                }

                if pl as i32 != father[v as usize] {
                    estack.push(ev);
                    replace_adj!(v, itp, ev);
                    if me_hi_slot[ev as usize] == INVALID && high!(pl) < vn {
                        let slot = hp_arena.len() as u32;
                        hp_arena.push(HpEntry {
                            val: vn,
                            next: hp_head[pl as usize],
                            deleted: false,
                        });
                        hp_head[pl as usize] = slot;
                        if hp_tail[pl as usize] == INVALID {
                            hp_tail[pl as usize] = slot;
                        }
                        me_hi_slot[ev as usize] = slot;
                    }
                    degree[v as usize] += 1;
                    degree[pl as usize] += 1;
                } else {
                    del_adj_slot!(v, itp);
                    let tav = tree_arc[v as usize];
                    let vid2 = *next_virtual;
                    *next_virtual += 1;
                    let nv2 = new_edge!(pl, v, vid2, 1);
                    comps.push(SplitComponent {
                        edges: vec![
                            StackEdge {
                                src: v,
                                dst: pl,
                                eid: cur_vid,
                            },
                            StackEdge {
                                src: pl,
                                dst: v,
                                eid: vid2,
                            },
                            se!(tav),
                        ],
                        pole_a: pl,
                        pole_b: v,
                    });
                    tree_arc[v as usize] = nv2;
                    me_etype[nv2 as usize] = 1;
                    if me_adj_v[tav as usize] != INVALID {
                        replace_adj!(me_adj_v[tav as usize], me_adj_p[tav as usize], nv2);
                    }
                }
            }

            if me_start[tei as usize] {
                while ta[top] != -1 {
                    top -= 1;
                }
                top -= 1;
            }
            while ta[top] != -1 && tb[top] != vn && high!(v) > th[top] {
                top -= 1;
            }

            cs[idx].outv -= 1;
            cs[idx].after = false;
        } else {
            let ei = cs[idx].ei;
            let wn = cs[idx].wn;
            let vn = cs[idx].vn;
            if me_start[ei as usize] {
                if ta[top] > wn {
                    let mut y = 0i32;
                    let mut bv;
                    loop {
                        y = std::cmp::max(y, th[top]);
                        bv = tb[top];
                        top -= 1;
                        if ta[top] <= wn {
                            break;
                        }
                    }
                    top += 1;
                    th[top] = y;
                    ta[top] = wn;
                    tb[top] = bv;
                } else {
                    top += 1;
                    th[top] = vn;
                    ta[top] = wn;
                    tb[top] = vn;
                }
            }
            estack.push(ei);
        }

        let idx = cs.len() - 1;
        if let Some((np, ne, nw, nwn)) = next_slot!(cs[idx].cur) {
            cs[idx].cur = np;
            cs[idx].ei = ne;
            cs[idx].w = nw;
            cs[idx].wn = nwn;
        } else {
            cs.pop();
        }
    }

    if !estack.is_empty() {
        let mut rem: Vec<StackEdge> = Vec::new();
        while let Some(ei) = estack.pop() {
            rem.push(se!(ei));
        }
        let (pa, pb) = (rem[0].src, rem[0].dst);
        comps.push(SplitComponent {
            edges: rem,
            pole_a: pa,
            pole_b: pb,
        });
    }

    comps
}

fn combine_components(
    multi: Vec<SplitComponent>,
    work: Vec<SplitComponent>,
    next_virtual: &mut u32,
) -> Vec<SplitComponent> {
    let mut out = multi;
    let mut pending: Vec<SplitComponent> = work;
    while let Some(comp) = pending.pop() {
        let parts = split_internal_parallels(comp, next_virtual);
        if parts.len() == 1 {
            out.push(parts.into_iter().next().unwrap());
        } else {
            pending.extend(parts);
        }
    }
    out
}

fn split_internal_parallels(comp: SplitComponent, next_virtual: &mut u32) -> Vec<SplitComponent> {
    let mut verts: HashMap<u32, ()> = HashMap::new();
    for e in &comp.edges {
        verts.insert(e.src, ());
        verts.insert(e.dst, ());
    }
    if verts.len() <= 2 {
        return vec![comp];
    }
    let mut groups: HashMap<(u32, u32), Vec<StackEdge>> = HashMap::new();
    for &e in &comp.edges {
        let (a, b) = if e.src <= e.dst {
            (e.src, e.dst)
        } else {
            (e.dst, e.src)
        };
        groups.entry((a, b)).or_default().push(e);
    }
    if !groups.values().any(|g| g.len() >= 2) {
        return vec![comp];
    }
    let mut result: Vec<SplitComponent> = Vec::new();
    let mut remainder_edges: Vec<StackEdge> = Vec::new();
    for (&(a, b), es) in groups.iter() {
        if es.len() >= 2 {
            let vid = *next_virtual;
            *next_virtual += 1;
            let mut bond_edges = es.clone();
            bond_edges.push(StackEdge {
                src: a,
                dst: b,
                eid: vid,
            });
            result.push(SplitComponent {
                edges: bond_edges,
                pole_a: a,
                pole_b: b,
            });
            remainder_edges.push(StackEdge {
                src: a,
                dst: b,
                eid: vid,
            });
        } else {
            remainder_edges.push(es[0]);
        }
    }
    result.push(SplitComponent {
        edges: remainder_edges,
        pole_a: comp.pole_a,
        pole_b: comp.pole_b,
    });
    result
}

fn classify_component(comp: &SplitComponent) -> SpqrNodeType {
    let mut deg: HashMap<u32, u32> = HashMap::new();
    for e in &comp.edges {
        *deg.entry(e.src).or_default() += 1;
        *deg.entry(e.dst).or_default() += 1;
    }
    let v = deg.len();
    let e = comp.edges.len();
    if v == 2 && e >= 2 {
        return SpqrNodeType::P;
    }
    if e == v && e >= 3 && deg.values().all(|&d| d == 2) {
        return SpqrNodeType::S;
    }
    SpqrNodeType::R
}

fn merge_same_type_components(comps: &mut Vec<SplitComponent>, m: usize) {
    let ctype: Vec<SpqrNodeType> = comps.iter().map(classify_component).collect();

    let mut comp1: HashMap<u32, usize> = HashMap::new();
    let mut comp2: HashMap<u32, usize> = HashMap::new();

    for (ci, comp) in comps.iter().enumerate() {
        for e in &comp.edges {
            if (e.eid as usize) >= m {
                if let std::collections::hash_map::Entry::Vacant(e) = comp1.entry(e.eid) {
                    e.insert(ci);
                } else {
                    comp2.insert(e.eid, ci);
                }
            }
        }
    }

    let mut visited = vec![false; comps.len()];

    for i in 0..comps.len() {
        visited[i] = true;
        if comps[i].edges.is_empty() {
            continue;
        }

        let ti = ctype[i];
        if ti != SpqrNodeType::P && ti != SpqrNodeType::S {
            continue;
        }

        let mut ei = 0;
        while ei < comps[i].edges.len() {
            let eid = comps[i].edges[ei].eid;
            if (eid as usize) < m {
                ei += 1;
                continue;
            }

            let c1 = comp1.get(&eid).copied();
            let c2 = comp2.get(&eid).copied();
            let j = match (c1, c2) {
                (Some(a), Some(b)) if a == i && !visited[b] => b,
                (Some(a), Some(b)) if b == i && !visited[a] => a,
                _ => {
                    ei += 1;
                    continue;
                }
            };

            if comps[j].edges.is_empty() || ctype[j] != ti {
                ei += 1;
                continue;
            }

            visited[j] = true;

            let mut j_edges = std::mem::take(&mut comps[j].edges);
            j_edges.retain(|e| e.eid != eid);

            for e in &j_edges {
                if (e.eid as usize) >= m {
                    if comp1.get(&e.eid) == Some(&j) {
                        comp1.insert(e.eid, i);
                    }
                    if comp2.get(&e.eid) == Some(&j) {
                        comp2.insert(e.eid, i);
                    }
                }
            }

            comps[i].edges.swap_remove(ei);
            comps[i].edges.append(&mut j_edges);
        }
    }

    comps.retain(|c| !c.edges.is_empty());
}

fn assemble_spqr_tree(graph: &Graph, components: &[SplitComponent], next_virtual: u32) -> SpqrTree {
    let m = graph.num_edges();
    let base = m as u32;

    // Use builder for flat construction
    let mut builder = SpqrTreeBuilder::new(m);

    for comp in components {
        let nt = classify_component(comp);
        let mut lid: HashMap<u32, u32> = HashMap::new();
        let mut n2o: Vec<NodeId> = Vec::new();
        let mut local_of = |v: u32| -> u32 {
            if let Some(&id) = lid.get(&v) {
                id
            } else {
                let id = n2o.len() as u32;
                lid.insert(v, id);
                n2o.push(NodeId(v));
                id
            }
        };
        local_of(comp.pole_a);
        local_of(comp.pole_b);
        let mut se = Vec::with_capacity(comp.edges.len());
        for edge in &comp.edges {
            let ls = NodeId(local_of(edge.src));
            let ld = NodeId(local_of(edge.dst));
            let is_real = (edge.eid as usize) < m;
            se.push(SkeletonEdge {
                src: ls,
                dst: ld,
                real_edge: if is_real {
                    EdgeId(edge.eid)
                } else {
                    EdgeId::INVALID
                },
                virtual_id: if is_real { INVALID } else { edge.eid },
                twin_tree_node: TreeNodeId::INVALID,
                twin_edge_idx: INVALID,
            });
        }
        builder.add_node(nt, n2o.len() as u32, se, n2o);
    }

    let num_nodes = builder.num_nodes();
    if num_nodes == 0 {
        return builder.finalize_empty();
    }

    // Link virtual edges
    let num_virtual = next_virtual.saturating_sub(base) as usize;
    let mut first: Vec<Option<(usize, u32)>> = vec![None; num_virtual];
    let mut pairs: Vec<(usize, u32, usize, u32)> = Vec::new();

    for ti in 0..num_nodes {
        for ei in 0..builder.skeleton_edges_len(TreeNodeId(ti as u32)) {
            let vid = builder
                .skeleton_edge_mut(TreeNodeId(ti as u32), ei)
                .virtual_id;
            if vid == INVALID {
                continue;
            }
            assert!(vid >= base, "virtual_id {} < base {}", vid, base);
            let idx = (vid - base) as usize;
            assert!(idx < first.len(), "virtual_id {} out of range", vid);
            if let Some((tj, ej)) = first[idx].take() {
                pairs.push((ti, ei as u32, tj, ej));
            } else {
                first[idx] = Some((ti, ei as u32));
            }
        }
    }

    // Clear unpaired virtual edges
    for entry in &first {
        if let Some(&(ti, ei)) = entry.as_ref() {
            let edge = builder.skeleton_edge_mut(TreeNodeId(ti as u32), ei as usize);
            edge.virtual_id = INVALID;
            edge.twin_tree_node = TreeNodeId::INVALID;
            edge.twin_edge_idx = INVALID;
        }
    }

    // Build tree adjacency in CSR format
    let mut tree_adj_count: Vec<u32> = vec![0; num_nodes];
    for &(a, _, b, _) in &pairs {
        tree_adj_count[a] += 1;
        tree_adj_count[b] += 1;
    }
    let mut tree_adj_offsets: Vec<u32> = vec![0; num_nodes + 1];
    for i in 0..num_nodes {
        tree_adj_offsets[i + 1] = tree_adj_offsets[i] + tree_adj_count[i];
    }
    let tree_adj_total = tree_adj_offsets[num_nodes] as usize;
    let mut tree_adj_flat: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; tree_adj_total];
    let mut tree_adj_write: Vec<u32> = tree_adj_offsets[..num_nodes].to_vec();

    for (a, ea, b, eb) in pairs {
        assert!(a != b);
        let ta = TreeNodeId(a as u32);
        let tb = TreeNodeId(b as u32);

        builder.skeleton_edge_mut(ta, ea as usize).twin_tree_node = tb;
        builder.skeleton_edge_mut(ta, ea as usize).twin_edge_idx = eb;
        builder.skeleton_edge_mut(tb, eb as usize).twin_tree_node = ta;
        builder.skeleton_edge_mut(tb, eb as usize).twin_edge_idx = ea;

        tree_adj_flat[tree_adj_write[a] as usize] = tb;
        tree_adj_write[a] += 1;
        tree_adj_flat[tree_adj_write[b] as usize] = ta;
        tree_adj_write[b] += 1;
    }
    drop(tree_adj_write);
    drop(tree_adj_count);

    // Build parent-child relationships via BFS
    let root = TreeNodeId(0);
    let mut par = vec![TreeNodeId::INVALID; num_nodes];
    let mut vis = vec![false; num_nodes];
    let mut st = vec![root];
    vis[0] = true;

    while let Some(v) = st.pop() {
        for idx in tree_adj_offsets[v.idx()] as usize..tree_adj_offsets[v.idx() + 1] as usize {
            let u = tree_adj_flat[idx];
            if vis[u.idx()] {
                continue;
            }
            vis[u.idx()] = true;
            par[u.idx()] = v;
            st.push(u);
        }
    }
    drop(tree_adj_flat);
    drop(tree_adj_offsets);

    // Handle disconnected nodes
    for (i, &v) in vis.iter().enumerate() {
        if !v {
            par[i] = root;
        }
    }

    // Set parents in builder
    builder.node_parents[..num_nodes].copy_from_slice(&par[..num_nodes]);

    // Build children CSR directly
    let mut children_count: Vec<u32> = vec![0; num_nodes];
    for i in 0..num_nodes {
        let p = par[i];
        if p.is_valid() {
            children_count[p.idx()] += 1;
        }
    }
    let mut children_offsets: Vec<u32> = vec![0; num_nodes + 1];
    for i in 0..num_nodes {
        children_offsets[i + 1] = children_offsets[i] + children_count[i];
    }
    let children_total = children_offsets[num_nodes] as usize;
    let mut children_flat: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; children_total];
    let mut children_write: Vec<u32> = children_offsets[..num_nodes].to_vec();
    for i in 0..num_nodes {
        let p = par[i];
        if p.is_valid() {
            children_flat[children_write[p.idx()] as usize] = TreeNodeId(i as u32);
            children_write[p.idx()] += 1;
        }
    }
    drop(children_write);
    drop(children_count);

    builder.finalize_with_children(root, children_offsets, children_flat)
}

impl SpqrTree {
    pub fn normalize(&mut self) {
        // For flat structure, normalize by rebuilding with merged same-type adjacent nodes
        // This is O(n) time and space, same as original

        let n = self.len();
        if n == 0 {
            return;
        }

        // Track which nodes are absorbed into others
        let mut absorbed_into: Vec<Option<TreeNodeId>> = vec![None; n];
        let mut changed = true;

        while changed {
            changed = false;

            for i in 0..n {
                if absorbed_into[i].is_some() {
                    continue;
                }
                let num_edges = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
                if num_edges == 0 {
                    continue;
                }

                let t = self.node_types[i];
                if t != SpqrNodeType::S && t != SpqrNodeType::P {
                    continue;
                }

                // Check children for same-type nodes to merge
                let children_start = self.children_offsets[i] as usize;
                let children_end = self.children_offsets[i + 1] as usize;

                for ci in children_start..children_end {
                    let child = self.children[ci];
                    if !child.is_valid() || absorbed_into[child.idx()].is_some() {
                        continue;
                    }

                    let child_num_edges =
                        self.skeleton_offsets[child.idx() + 1] - self.skeleton_offsets[child.idx()];
                    if child_num_edges == 0 {
                        continue;
                    }

                    if self.node_types[child.idx()] == t {
                        absorbed_into[child.idx()] = Some(TreeNodeId(i as u32));
                        changed = true;
                    }
                }
            }
        }

        // If nothing was absorbed, we're done
        if absorbed_into.iter().all(|x| x.is_none()) {
            return;
        }

        // Rebuild the tree with merged nodes
        self.rebuild_with_merges(&absorbed_into);
    }

    fn rebuild_with_merges(&mut self, absorbed_into: &[Option<TreeNodeId>]) {
        let n = self.len();

        // Build mapping from old node ID to new node ID
        let mut old_to_new: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n];
        let mut new_idx = 0u32;
        for i in 0..n {
            if absorbed_into[i].is_none() {
                old_to_new[i] = TreeNodeId(new_idx);
                new_idx += 1;
            }
        }

        // For absorbed nodes, map to the node they were absorbed into
        for i in 0..n {
            if let Some(parent) = absorbed_into[i] {
                old_to_new[i] = old_to_new[parent.idx()];
            }
        }

        let new_count = new_idx as usize;
        if new_count == n {
            return; // Nothing absorbed
        }

        // ===== PASS 1: Count sizes for each new node =====
        // Simple u32 arrays instead of Vec<Vec<T>> - much less memory!
        let mut edge_counts: Vec<u32> = vec![0; new_count];
        let mut child_counts: Vec<u32> = vec![0; new_count];
        let mut mapping_counts: Vec<u32> = vec![0; new_count];

        for i in 0..n {
            let new_i = old_to_new[i].idx();

            // Count edges (excluding virtual edges to merged nodes)
            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;
            for ei in edge_start..edge_end {
                let e = &self.skeleton_edges[ei];
                if e.twin_tree_node.is_valid() {
                    let twin_new = old_to_new[e.twin_tree_node.idx()];
                    if twin_new == TreeNodeId(new_i as u32) {
                        continue; // Skip - virtual edge to merged node
                    }
                }
                edge_counts[new_i] += 1;
            }

            // Count mappings (only for surviving nodes)
            if absorbed_into[i].is_none() {
                let map_len = self.node_mapping_offsets[i + 1] - self.node_mapping_offsets[i];
                mapping_counts[new_i] = map_len;

                // Count direct children
                let cs = self.children_offsets[i] as usize;
                let ce = self.children_offsets[i + 1] as usize;
                for ci in cs..ce {
                    let child = self.children[ci];
                    if child.is_valid() && absorbed_into[child.idx()].is_none() {
                        child_counts[new_i] += 1;
                    }
                }
            }
        }

        // Count children from absorbed nodes
        for j in 0..n {
            if let Some(parent_tid) = absorbed_into[j] {
                let new_i = old_to_new[parent_tid.idx()].idx();
                let cs = self.children_offsets[j] as usize;
                let ce = self.children_offsets[j + 1] as usize;
                for ci in cs..ce {
                    let child = self.children[ci];
                    if child.is_valid() && absorbed_into[child.idx()].is_none() {
                        child_counts[new_i] += 1;
                    }
                }
            }
        }

        // ===== PASS 2: Build offset arrays =====
        let total_edges: usize = edge_counts.iter().map(|&x| x as usize).sum();
        let total_children: usize = child_counts.iter().map(|&x| x as usize).sum();
        let total_mapping: usize = mapping_counts.iter().map(|&x| x as usize).sum();

        let mut new_skeleton_offsets: Vec<u32> = Vec::with_capacity(new_count + 1);
        let mut new_children_offsets: Vec<u32> = Vec::with_capacity(new_count + 1);
        let mut new_mapping_offsets: Vec<u32> = Vec::with_capacity(new_count + 1);

        new_skeleton_offsets.push(0);
        new_children_offsets.push(0);
        new_mapping_offsets.push(0);

        for i in 0..new_count {
            new_skeleton_offsets.push(new_skeleton_offsets[i] + edge_counts[i]);
            new_children_offsets.push(new_children_offsets[i] + child_counts[i]);
            new_mapping_offsets.push(new_mapping_offsets[i] + mapping_counts[i]);
        }

        // ===== PASS 3: Allocate final arrays with exact sizes =====
        let mut new_node_types: Vec<SpqrNodeType> = vec![SpqrNodeType::R; new_count];
        let mut new_node_parents: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; new_count];
        let mut new_skeleton_num_nodes: Vec<u32> = vec![0; new_count];
        let mut new_skeleton_edges: Vec<SkeletonEdge> = vec![SkeletonEdge::default(); total_edges];
        let mut new_children: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; total_children];
        let mut new_node_mapping: Vec<NodeId> = vec![NodeId::INVALID; total_mapping];

        // Track write positions
        let mut edge_write_pos: Vec<u32> = new_skeleton_offsets[..new_count].to_vec();
        let mut child_write_pos: Vec<u32> = new_children_offsets[..new_count].to_vec();

        // ===== PASS 4: Fill data =====
        for i in 0..n {
            if absorbed_into[i].is_some() {
                continue;
            }

            let new_i = old_to_new[i].idx();

            // Node info
            new_node_types[new_i] = self.node_types[i];
            let parent = self.node_parents[i];
            new_node_parents[new_i] = if parent.is_valid() {
                old_to_new[parent.idx()]
            } else {
                TreeNodeId::INVALID
            };
            new_skeleton_num_nodes[new_i] = self.skeleton_num_nodes[i];

            // Copy node mapping directly
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            let new_map_start = new_mapping_offsets[new_i] as usize;
            for (j, &val) in self.node_mapping[map_start..map_end].iter().enumerate() {
                new_node_mapping[new_map_start + j] = val;
            }

            // Copy direct children
            let cs = self.children_offsets[i] as usize;
            let ce = self.children_offsets[i + 1] as usize;
            for ci in cs..ce {
                let child = self.children[ci];
                if child.is_valid() && absorbed_into[child.idx()].is_none() {
                    let pos = child_write_pos[new_i] as usize;
                    new_children[pos] = old_to_new[child.idx()];
                    child_write_pos[new_i] += 1;
                }
            }
        }

        // Copy children from absorbed nodes
        for j in 0..n {
            if let Some(parent_tid) = absorbed_into[j] {
                let new_i = old_to_new[parent_tid.idx()].idx();
                let cs = self.children_offsets[j] as usize;
                let ce = self.children_offsets[j + 1] as usize;
                for ci in cs..ce {
                    let child = self.children[ci];
                    if child.is_valid() && absorbed_into[child.idx()].is_none() {
                        let pos = child_write_pos[new_i] as usize;
                        new_children[pos] = old_to_new[child.idx()];
                        child_write_pos[new_i] += 1;
                    }
                }
            }
        }

        // Copy edges from all nodes (including absorbed)
        for i in 0..n {
            let new_i = old_to_new[i].idx();
            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;

            for ei in edge_start..edge_end {
                let mut e = self.skeleton_edges[ei];

                if e.twin_tree_node.is_valid() {
                    let twin_new = old_to_new[e.twin_tree_node.idx()];
                    if twin_new == TreeNodeId(new_i as u32) {
                        continue; // Skip merged virtual edge
                    }
                    e.twin_tree_node = twin_new;
                }

                let pos = edge_write_pos[new_i] as usize;
                new_skeleton_edges[pos] = e;
                edge_write_pos[new_i] += 1;
            }
        }

        // Update edge_to_tree_node
        for tn in &mut self.edge_to_tree_node {
            if tn.is_valid() {
                *tn = old_to_new[tn.idx()];
            }
        }

        // Update root
        if self.root.is_valid() {
            self.root = old_to_new[self.root.idx()];
        }

        // Replace arrays
        self.node_types = new_node_types;
        self.node_parents = new_node_parents;
        self.skeleton_offsets = new_skeleton_offsets;
        self.skeleton_edges = new_skeleton_edges;
        self.node_mapping_offsets = new_mapping_offsets;
        self.node_mapping = new_node_mapping;
        self.skeleton_num_nodes = new_skeleton_num_nodes;
        self.children_offsets = new_children_offsets;
        self.children = new_children;
    }

    pub fn compact(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }

        // Find nodes with edges (non-empty)
        let mut is_alive: Vec<bool> = vec![false; n];
        for i in 0..n {
            let num_edges = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
            is_alive[i] = num_edges > 0;
        }

        // Build mapping from old to new IDs
        let mut old_to_new: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n];
        let mut new_idx = 0u32;
        for i in 0..n {
            if is_alive[i] {
                old_to_new[i] = TreeNodeId(new_idx);
                new_idx += 1;
            }
        }

        if new_idx as usize == n {
            // No compaction needed
            return;
        }

        let new_count = new_idx as usize;

        // Build new arrays
        let mut new_node_types: Vec<SpqrNodeType> = Vec::with_capacity(new_count);
        let mut new_node_parents: Vec<TreeNodeId> = Vec::with_capacity(new_count);
        let mut new_skeleton_num_nodes: Vec<u32> = Vec::with_capacity(new_count);
        let mut new_skeleton_offsets: Vec<u32> = vec![0];
        let mut new_skeleton_edges: Vec<SkeletonEdge> = Vec::new();
        let mut new_node_mapping_offsets: Vec<u32> = vec![0];
        let mut new_node_mapping: Vec<NodeId> = Vec::new();
        let mut new_children_offsets: Vec<u32> = vec![0];
        let mut new_children: Vec<TreeNodeId> = Vec::new();

        for i in 0..n {
            if !is_alive[i] {
                continue;
            }

            new_node_types.push(self.node_types[i]);

            let parent = self.node_parents[i];
            new_node_parents.push(if parent.is_valid() {
                old_to_new[parent.idx()]
            } else {
                TreeNodeId::INVALID
            });

            new_skeleton_num_nodes.push(self.skeleton_num_nodes[i]);

            // Copy skeleton edges, updating twin references
            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;
            for ei in edge_start..edge_end {
                let mut e = self.skeleton_edges[ei];
                if e.twin_tree_node.is_valid() {
                    e.twin_tree_node = old_to_new[e.twin_tree_node.idx()];
                }
                new_skeleton_edges.push(e);
            }
            new_skeleton_offsets.push(new_skeleton_edges.len() as u32);

            // Copy node mapping
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            new_node_mapping.extend_from_slice(&self.node_mapping[map_start..map_end]);
            new_node_mapping_offsets.push(new_node_mapping.len() as u32);

            // Copy children, filtering out dead nodes
            let children_start = self.children_offsets[i] as usize;
            let children_end = self.children_offsets[i + 1] as usize;
            for ci in children_start..children_end {
                let child = self.children[ci];
                if child.is_valid() && is_alive[child.idx()] {
                    new_children.push(old_to_new[child.idx()]);
                }
            }
            new_children_offsets.push(new_children.len() as u32);
        }

        // Update edge_to_tree_node
        for tn in &mut self.edge_to_tree_node {
            if tn.is_valid() && old_to_new[tn.idx()].is_valid() {
                *tn = old_to_new[tn.idx()];
            } else if tn.is_valid() {
                *tn = TreeNodeId::INVALID;
            }
        }

        // Update root
        if self.root.is_valid() {
            self.root = old_to_new[self.root.idx()];
        }

        // Replace arrays
        self.node_types = new_node_types;
        self.node_parents = new_node_parents;
        self.skeleton_num_nodes = new_skeleton_num_nodes;
        self.skeleton_offsets = new_skeleton_offsets;
        self.skeleton_edges = new_skeleton_edges;
        self.node_mapping_offsets = new_node_mapping_offsets;
        self.node_mapping = new_node_mapping;
        self.children_offsets = new_children_offsets;
        self.children = new_children;
    }
}

impl fmt::Display for SpqrTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "SPQR Tree ({} nodes):", self.len())?;
        for i in 0..self.len() {
            let t = match self.node_types[i] {
                SpqrNodeType::S => "S",
                SpqrNodeType::P => "P",
                SpqrNodeType::R => "R",
            };
            let num_edges = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
            let num_children = self.children_offsets[i + 1] - self.children_offsets[i];
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            let poles = if map_end - map_start >= 2 {
                (
                    self.node_mapping[map_start],
                    self.node_mapping[map_start + 1],
                )
            } else {
                (NodeId::INVALID, NodeId::INVALID)
            };
            writeln!(
                f,
                "  [{}] {}: {} edges, {} children, poles={:?}",
                i, t, num_edges, num_children, poles
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_k4() -> Graph {
        let mut g = Graph::with_capacity(4, 6);
        g.add_nodes(4);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(2));
        g.add_edge(NodeId(0), NodeId(3));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(1), NodeId(3));
        g.add_edge(NodeId(2), NodeId(3));
        g
    }
    fn make_bond() -> Graph {
        let mut g = Graph::with_capacity(2, 3);
        g.add_nodes(2);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(1));
        g
    }
    fn make_cycle(n: usize) -> Graph {
        let mut g = Graph::with_capacity(n, n);
        g.add_nodes(n);
        for i in 0..n {
            g.add_edge(NodeId(i as u32), NodeId(((i + 1) % n) as u32));
        }
        g
    }
    #[test]
    fn test_k4() {
        let t = build_spqr_tree(&make_k4());
        let (_s, _p, r) = t.count_by_type();
        assert!(r >= 1, "K4 needs R-node");
        println!("{}", t);
    }
    #[test]
    fn test_bond() {
        let t = build_spqr_tree(&make_bond());
        assert_eq!(t.node_types[0], SpqrNodeType::P);
    }
    #[test]
    fn test_cycle() {
        let t = build_spqr_tree(&make_cycle(5));
        let (s, _, _) = t.count_by_type();
        assert!(s >= 1, "Cycle needs S-node");
        println!("{}", t);
    }
    #[test]
    fn test_single_edge() {
        let mut g = Graph::with_capacity(2, 1);
        g.add_nodes(2);
        g.add_edge(NodeId(0), NodeId(1));
        assert_eq!(build_spqr_tree(&g).len(), 1);
    }
    #[test]
    fn test_edge_partition() {
        let g = make_k4();
        let t = build_spqr_tree(&g);
        for i in 0..g.num_edges() {
            assert!(
                t.tree_node_of_edge(EdgeId(i as u32)).is_valid(),
                "Edge {} unmapped",
                i
            );
        }
    }
    #[test]
    fn test_triangle_single_s() {
        let g = make_cycle(3);
        let t = build_spqr_tree(&g);
        let report = verify::verify_spqr_tree_with_options(
            &g,
            &t,
            verify::VerifyOptions {
                require_reduced: false,
            },
        );
        assert!(report.is_ok(), "{:?}", report.errors);
        let (s, p, r) = t.count_by_type();
        assert_eq!((s, p, r), (1, 0, 0), "triangle should be one S-node");
    }
    #[test]
    fn test_two_triangles_shared_edge() {
        let mut g = Graph::with_capacity(4, 5);
        g.add_nodes(4);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(2), NodeId(0));
        g.add_edge(NodeId(0), NodeId(3));
        g.add_edge(NodeId(3), NodeId(1));
        let t = build_spqr_tree(&g);
        let report = verify::verify_spqr_tree_with_options(
            &g,
            &t,
            verify::VerifyOptions {
                require_reduced: false,
            },
        );
        assert!(report.is_ok(), "{:?}", report.errors);
    }

    #[test]
    fn test_self_loops_separated() {
        let mut g = Graph::with_capacity(3, 5);
        g.add_nodes(3);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(2), NodeId(0));
        g.add_edge(NodeId(0), NodeId(0));
        g.add_edge(NodeId(1), NodeId(1));
        let res = build_spqr(&g);
        assert_eq!(res.self_loops.len(), 2);
        assert_eq!(res.self_loops[0], EdgeId(3));
        assert_eq!(res.self_loops[1], EdgeId(4));
        let _report = verify::verify_spqr_tree_with_options(
            &g,
            &res.tree,
            verify::VerifyOptions {
                require_reduced: false,
            },
        );
        let (s, p, r) = res.tree.count_by_type();
        assert_eq!((s, p, r), (1, 0, 0), "triangle → one S-node");
        assert!(!res.tree.tree_node_of_edge(EdgeId(3)).is_valid());
        assert!(!res.tree.tree_node_of_edge(EdgeId(4)).is_valid());
        for i in 0..3 {
            assert!(res.tree.tree_node_of_edge(EdgeId(i)).is_valid());
        }
    }

    #[test]
    fn test_only_self_loops() {
        let mut g = Graph::with_capacity(1, 3);
        g.add_nodes(1);
        g.add_edge(NodeId(0), NodeId(0));
        g.add_edge(NodeId(0), NodeId(0));
        g.add_edge(NodeId(0), NodeId(0));
        let res = build_spqr(&g);
        assert_eq!(res.self_loops.len(), 3);
        assert!(res.tree.is_empty());
    }

    #[test]
    fn test_self_loops_with_multi_edges() {
        let mut g = Graph::with_capacity(2, 5);
        g.add_nodes(2);
        g.add_edge(NodeId(0), NodeId(1)); // e0
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(0));
        g.add_edge(NodeId(1), NodeId(1)); // e3 self loop
        g.add_edge(NodeId(0), NodeId(0)); // e4 self loop
        let res = build_spqr(&g);
        assert_eq!(res.self_loops.len(), 3);
        assert_eq!(res.tree.len(), 1);
        assert_eq!(res.tree.node_types[0], SpqrNodeType::P);
        assert!(res.tree.tree_node_of_edge(EdgeId(0)).is_valid());
        assert!(res.tree.tree_node_of_edge(EdgeId(1)).is_valid());
        assert!(!res.tree.tree_node_of_edge(EdgeId(2)).is_valid());
    }

    #[test]
    fn test_build_spqr_tree_debug_assert() {
        let g = make_k4();
        let t = build_spqr_tree(&g);
        assert!(!t.is_empty());
    }

    #[test]
    fn test_graph_from_edge_pairs() {
        // K4 as flat pairs
        let pairs: Vec<u32> = vec![0, 1, 0, 2, 0, 3, 1, 2, 1, 3, 2, 3];
        let g = Graph::from_edge_pairs(4, &pairs);
        assert_eq!(g.num_nodes(), 4);
        assert_eq!(g.num_edges(), 6);

        // Verify the graph works with SPQR
        let t = build_spqr_tree(&g);
        let (_, _, r) = t.count_by_type();
        assert!(r >= 1, "K4 needs R-node");
    }

    #[test]
    fn test_graph_from_edge_arrays() {
        let src: Vec<u32> = vec![0, 0, 0, 1, 1, 2];
        let dst: Vec<u32> = vec![1, 2, 3, 2, 3, 3];
        let g = Graph::from_edge_arrays(4, &src, &dst);
        assert_eq!(g.num_nodes(), 4);
        assert_eq!(g.num_edges(), 6);

        // Verify same result as make_k4
        let t = build_spqr_tree(&g);
        let (_, _, r) = t.count_by_type();
        assert!(r >= 1, "K4 needs R-node");
    }

    #[test]
    fn test_graph_construction_equivalence() {
        // Build same graph three ways
        let g1 = make_k4();

        let pairs: Vec<u32> = vec![0, 1, 0, 2, 0, 3, 1, 2, 1, 3, 2, 3];
        let g2 = Graph::from_edge_pairs(4, &pairs);

        let src: Vec<u32> = vec![0, 0, 0, 1, 1, 2];
        let dst: Vec<u32> = vec![1, 2, 3, 2, 3, 3];
        let g3 = Graph::from_edge_arrays(4, &src, &dst);

        // All should have same structure
        assert_eq!(g1.num_nodes(), g2.num_nodes());
        assert_eq!(g1.num_nodes(), g3.num_nodes());
        assert_eq!(g1.num_edges(), g2.num_edges());
        assert_eq!(g1.num_edges(), g3.num_edges());

        // All should produce valid SPQR trees
        let t1 = build_spqr_tree(&g1);
        let t2 = build_spqr_tree(&g2);
        let t3 = build_spqr_tree(&g3);

        assert_eq!(t1.len(), t2.len());
        assert_eq!(t1.len(), t3.len());
    }
}
