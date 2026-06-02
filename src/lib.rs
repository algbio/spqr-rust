//! # SPQR Tree, triconnectivity decomposition
//!
//! Computes SPQR trees of biconnected multigraphs using a DFS based
//! triconnected components algorithm (Hopcroft Tarjan with corrections
//! by Gutwenger and Mutzel in 2001)
//!

#![deny(unsafe_code)]
#![allow(clippy::needless_range_loop)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::thread;

pub static CANONICALIZE_ROOT_ENABLED: AtomicBool = AtomicBool::new(true);

static SPQR_THREAD_COUNT: OnceLock<usize> = OnceLock::new();

pub(crate) fn spqr_thread_count() -> usize {
    *SPQR_THREAD_COUNT.get_or_init(|| {
        let from_env = |name: &str| -> Option<usize> {
            let raw = std::env::var(name).ok()?;
            let first = raw.split(',').next().unwrap_or(raw.as_str()).trim();
            let value = first.parse::<usize>().ok()?;
            (value > 0).then_some(value)
        };
        let requested = from_env("OMP_NUM_THREADS")
            .or_else(|| from_env("RAYON_NUM_THREADS"))
            .unwrap_or(1);
        let available = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        requested.min(available).max(1)
    })
}

pub mod biconnected;
pub mod connected;
#[allow(unsafe_code)]
pub mod ffi;
pub mod sp_compress;
pub mod spqr_format;
pub mod verify;

pub use biconnected::{BCNodeId, BCNodeType, BCTree, Block};
pub use connected::{
    connected_components, connected_components_simple, count_connected_components,
    ConnectedComponents,
};

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

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

thread_local! {
    static SKIP_ASSEMBLE_SPQRTREE: Cell<bool> = const { Cell::new(false) };
    static SPQRA_CORE_CHILD_REFS: RefCell<Option<Vec<u32>>> = const { RefCell::new(None) };
}

pub(crate) fn with_skip_assemble_spqr<T, F: FnOnce() -> T>(skip: bool, f: F) -> T {
    SKIP_ASSEMBLE_SPQRTREE.with(|cell| {
        let old = cell.replace(skip);
        let out = f();
        cell.set(old);
        out
    })
}

pub(crate) fn with_spqra_core_child_refs<T, F: FnOnce() -> T>(refs: Option<Vec<u32>>, f: F) -> T {
    SPQRA_CORE_CHILD_REFS.with(|cell| {
        let old = cell.replace(refs);
        let out = f();
        cell.replace(old);
        out
    })
}

fn spqra_core_child_ref(edge_id: usize) -> Option<u32> {
    SPQRA_CORE_CHILD_REFS.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|refs| refs.get(edge_id).copied())
    })
}

fn skip_assemble_spqr_requested() -> bool {
    SKIP_ASSEMBLE_SPQRTREE.with(|cell| cell.get())
}

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
    pub fn add_edges_flat(&mut self, pairs: &[u32]) {
        let num_edges = pairs.len() / 2;
        self.edges.reserve(num_edges);
        self.half_edges.reserve(num_edges * 2);
        for i in 0..num_edges {
            let u = NodeId(pairs[i * 2]);
            let v = NodeId(pairs[i * 2 + 1]);
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
        }
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

#[derive(Clone)]
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
    pub min_real_per_node: Vec<u32>,
    pub preassembly_scad_export:
        Option<std::sync::Arc<crate::sp_compress::integration::CoreScadExport>>,
    pub preassembly_minimizer_sidecar:
        Option<std::sync::Arc<crate::sp_compress::integration::SpqraMinimizerSidecar>>,
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
            min_real_per_node: Vec::new(),
            preassembly_scad_export: None,
            preassembly_minimizer_sidecar: None,
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
        let mut min_real: u32 = u32::MAX;
        for edge in &edges {
            if edge.real_edge.is_valid() {
                edge_to_tree_node[edge.real_edge.idx()] = TreeNodeId(0);
                if edge.real_edge.0 < min_real {
                    min_real = edge.real_edge.0;
                }
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
            min_real_per_node: vec![min_real],
            preassembly_scad_export: None,
            preassembly_minimizer_sidecar: None,
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
    min_real_per_node: Vec<u32>,
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
            min_real_per_node: Vec::new(),
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

        let mut min_real: u32 = u32::MAX;
        for edge in &edges {
            if edge.real_edge.is_valid() {
                self.edge_to_tree_node[edge.real_edge.idx()] = tid;
                if edge.real_edge.0 < min_real {
                    min_real = edge.real_edge.0;
                }
            }
        }
        self.min_real_per_node.push(min_real);

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
            min_real_per_node: self.min_real_per_node,
            preassembly_scad_export: None,
            preassembly_minimizer_sidecar: None,
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
            min_real_per_node: Vec::new(),
            preassembly_scad_export: None,
            preassembly_minimizer_sidecar: None,
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
    origin: u8,
}

const SPLIT_ORIGIN_PRETRICONN_P_BOND: u8 = 1 << 1;

#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct SpqrRawBuildTimings {
    pub t_self_loop_scan_us: u64,
    pub t_tree_total_us: u64,
    pub t_precheck_us: u64,
    pub t_split_multi_edges_us: u64,
    pub t_work_graph_us: u64,
    pub t_triconn_us: u64,
    pub t_relabel_us: u64,
    pub t_combine_us: u64,
    pub t_merge_us: u64,
    pub t_assemble_us: u64,

    pub c_multi_components: u64,
    pub c_triconn_components: u64,
    pub c_precombine_components: u64,
    pub c_combined_components: u64,
    pub c_merged_components: u64,
    pub c_merged_real_edges: u64,
    pub c_merged_virtual_incidences: u64,
    pub c_virtual_id_span: u64,
    pub c_tree_nodes: u64,
    pub c_tree_edges: u64,
    pub c_tree_skeleton_edges: u64,
    pub c_tree_virtual_incidences: u64,
}

struct PretriconnChainAnalysis {
    quotient: Graph,
    quotient_node_inv: Vec<u32>,
    quotient_labels: Vec<u32>,
    chain_components: Vec<SplitComponent>,
    chain_atom_components: Vec<SplitComponent>,
}

fn extract_parallel_p_bonds_from_quotient(
    quotient_pairs: &mut Vec<u32>,
    quotient_labels: &mut Vec<u32>,
    chain_components: &mut Vec<SplitComponent>,
    chain_atom_components: &mut Vec<SplitComponent>,
    atomizable_virtuals: &mut HashMap<u32, ()>,
    next_virtual: &mut u32,
    atomize_p_bonds: bool,
) {
    if quotient_labels.len() <= 1 || quotient_pairs.len() != quotient_labels.len().saturating_mul(2)
    {
        return;
    }

    let pre_p_pairs = std::mem::take(quotient_pairs);
    let pre_p_labels = std::mem::take(quotient_labels);
    let mut keyed_edges: Vec<(u32, u32, usize)> = Vec::with_capacity(pre_p_labels.len());
    for i in 0..pre_p_labels.len() {
        let u = pre_p_pairs[i * 2];
        let v = pre_p_pairs[i * 2 + 1];
        let (a, b) = if u <= v { (u, v) } else { (v, u) };
        keyed_edges.push((a, b, i));
    }
    keyed_edges.sort_unstable();

    let mut consumed_qedge = vec![false; pre_p_labels.len()];
    let mut pos = 0usize;
    while pos < keyed_edges.len() {
        let a = keyed_edges[pos].0;
        let b = keyed_edges[pos].1;
        let mut end_pos = pos + 1;
        while end_pos < keyed_edges.len()
            && keyed_edges[end_pos].0 == a
            && keyed_edges[end_pos].1 == b
        {
            end_pos += 1;
        }

        if end_pos - pos <= 1 {
            pos = end_pos;
            continue;
        }

        let vid = *next_virtual;
        *next_virtual = (*next_virtual).saturating_add(1);
        let mut comp_edges = Vec::with_capacity(end_pos - pos + 1);
        for item in &keyed_edges[pos..end_pos] {
            let idx = item.2;
            consumed_qedge[idx] = true;
            comp_edges.push(StackEdge {
                src: pre_p_pairs[idx * 2],
                dst: pre_p_pairs[idx * 2 + 1],
                eid: pre_p_labels[idx],
            });
        }
        comp_edges.push(StackEdge {
            src: a,
            dst: b,
            eid: vid,
        });
        let can_atomize = atomize_p_bonds
            && keyed_edges[pos..end_pos].iter().all(|item| {
                let label = pre_p_labels[item.2];
                spqra_core_child_ref(label as usize).is_some()
                    || atomizable_virtuals.contains_key(&label)
            });
        let comp = SplitComponent {
            edges: comp_edges,
            pole_a: a,
            pole_b: b,
            origin: SPLIT_ORIGIN_PRETRICONN_P_BOND,
        };
        if can_atomize {
            atomizable_virtuals.insert(vid, ());
            chain_atom_components.push(comp);
        } else {
            chain_components.push(comp);
        }
        quotient_pairs.push(a);
        quotient_pairs.push(b);
        quotient_labels.push(vid);

        pos = end_pos;
    }

    for i in 0..pre_p_labels.len() {
        if consumed_qedge[i] {
            continue;
        }
        quotient_pairs.push(pre_p_pairs[i * 2]);
        quotient_pairs.push(pre_p_pairs[i * 2 + 1]);
        quotient_labels.push(pre_p_labels[i]);
    }
}

fn group_parallel_p_bonds_only_before_triconn(
    graph: &Graph,
    edge_labels: Option<&[u32]>,
    next_virtual: &mut u32,
) -> PretriconnChainAnalysis {
    let n = graph.num_nodes();
    let m = graph.num_edges();
    let label_of = |eid: usize| -> u32 {
        edge_labels
            .and_then(|labels| labels.get(eid).copied())
            .unwrap_or(eid as u32)
    };

    let mut quotient_pairs = Vec::with_capacity(m.saturating_mul(2));
    let mut quotient_labels = Vec::with_capacity(m);
    for eid in 0..m {
        let e = graph.edge(EdgeId(eid as u32));
        quotient_pairs.push(e.src.0);
        quotient_pairs.push(e.dst.0);
        quotient_labels.push(label_of(eid));
    }

    let mut chain_components = Vec::new();
    let mut chain_atom_components = Vec::new();
    let mut atomizable_virtuals: HashMap<u32, ()> = HashMap::new();
    let atomize_p_bonds = skip_assemble_spqr_requested();
    extract_parallel_p_bonds_from_quotient(
        &mut quotient_pairs,
        &mut quotient_labels,
        &mut chain_components,
        &mut chain_atom_components,
        &mut atomizable_virtuals,
        next_virtual,
        atomize_p_bonds,
    );

    let mut node_remap = vec![u32::MAX; n];
    let mut quotient_node_inv = Vec::new();
    let mut dense_pairs = Vec::with_capacity(quotient_pairs.len());
    for &v in &quotient_pairs {
        let slot = &mut node_remap[v as usize];
        let dense = if *slot != u32::MAX {
            *slot
        } else {
            let id = quotient_node_inv.len() as u32;
            quotient_node_inv.push(v);
            *slot = id;
            id
        };
        dense_pairs.push(dense);
    }

    PretriconnChainAnalysis {
        quotient: Graph::from_edge_pairs(quotient_node_inv.len(), &dense_pairs),
        quotient_node_inv,
        quotient_labels,
        chain_components,
        chain_atom_components,
    }
}

#[derive(Clone, Copy)]
struct SelfLoopFlags<'a> {
    flags: Option<&'a [bool]>,
}

impl<'a> SelfLoopFlags<'a> {
    #[inline(always)]
    fn none() -> Self {
        Self { flags: None }
    }

    #[inline(always)]
    fn from_slice(flags: &'a [bool]) -> Self {
        Self { flags: Some(flags) }
    }

    #[inline(always)]
    fn is_loop(self, idx: usize) -> bool {
        match self.flags {
            Some(flags) => flags[idx],
            None => false,
        }
    }

    #[inline]
    fn count(self) -> usize {
        match self.flags {
            Some(flags) => flags.iter().filter(|&&b| b).count(),
            None => 0,
        }
    }

    #[inline]
    fn any(self) -> bool {
        match self.flags {
            Some(flags) => flags.iter().any(|&b| b),
            None => false,
        }
    }
}

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

    let mut tree = build_spqr_tree_filtered(graph, &is_self_loop);
    tree.canonicalize_root();
    tree.canonicalize_skeleton_node_order();
    tree.canonicalize_skeleton_edge_orientation();
    tree.move_root_to_zero();
    SpqrResult { tree, self_loops }
}

pub(crate) fn build_spqr_raw(graph: &Graph) -> SpqrResult {
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

pub(crate) fn build_spqr_raw_no_self_loops(graph: &Graph) -> SpqrResult {
    debug_assert!((0..graph.num_edges()).all(|i| {
        let e = graph.edge(EdgeId(i as u32));
        e.src != e.dst
    }));
    let tree = build_spqr_tree_filtered_impl(graph, SelfLoopFlags::none(), None, false);
    SpqrResult {
        tree,
        self_loops: Vec::new(),
    }
}

pub(crate) fn build_spqr_raw_no_multi_edges(graph: &Graph) -> SpqrResult {
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

    let tree =
        build_spqr_tree_filtered_impl(graph, SelfLoopFlags::from_slice(&is_self_loop), None, true);
    SpqrResult { tree, self_loops }
}

pub(crate) fn build_spqr_raw_timed(graph: &Graph) -> (SpqrResult, SpqrRawBuildTimings) {
    let m = graph.num_edges();
    let mut timings = SpqrRawBuildTimings::default();

    let t0 = Instant::now();
    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u32));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u32));
        }
    }
    timings.t_self_loop_scan_us = t0.elapsed().as_micros() as u64;

    let t1 = Instant::now();
    let tree = build_spqr_tree_filtered_impl(
        graph,
        SelfLoopFlags::from_slice(&is_self_loop),
        Some(&mut timings),
        false,
    );
    timings.t_tree_total_us = t1.elapsed().as_micros() as u64;

    (SpqrResult { tree, self_loops }, timings)
}

pub(crate) fn build_spqr_raw_no_self_loops_timed(
    graph: &Graph,
) -> (SpqrResult, SpqrRawBuildTimings) {
    debug_assert!((0..graph.num_edges()).all(|i| {
        let e = graph.edge(EdgeId(i as u32));
        e.src != e.dst
    }));
    let mut timings = SpqrRawBuildTimings::default();
    let t1 = Instant::now();
    let tree =
        build_spqr_tree_filtered_impl(graph, SelfLoopFlags::none(), Some(&mut timings), false);
    timings.t_tree_total_us = t1.elapsed().as_micros() as u64;

    (
        SpqrResult {
            tree,
            self_loops: Vec::new(),
        },
        timings,
    )
}

pub(crate) fn build_spqr_raw_no_multi_edges_timed(
    graph: &Graph,
) -> (SpqrResult, SpqrRawBuildTimings) {
    let m = graph.num_edges();
    let mut timings = SpqrRawBuildTimings::default();

    let t0 = Instant::now();
    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u32));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u32));
        }
    }
    timings.t_self_loop_scan_us = t0.elapsed().as_micros() as u64;

    let t1 = Instant::now();
    let tree = build_spqr_tree_filtered_impl(
        graph,
        SelfLoopFlags::from_slice(&is_self_loop),
        Some(&mut timings),
        true,
    );
    timings.t_tree_total_us = t1.elapsed().as_micros() as u64;

    (SpqrResult { tree, self_loops }, timings)
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
    build_spqr_tree_filtered_impl(graph, SelfLoopFlags::none(), None, false)
}

fn build_spqr_tree_filtered(graph: &Graph, is_self_loop: &[bool]) -> SpqrTree {
    build_spqr_tree_filtered_impl(graph, SelfLoopFlags::from_slice(is_self_loop), None, false)
}

fn build_spqr_tree_filtered_impl(
    graph: &Graph,
    self_loops: SelfLoopFlags<'_>,
    mut timings: Option<&mut SpqrRawBuildTimings>,
    assume_no_non_loop_multi_edges: bool,
) -> SpqrTree {
    macro_rules! add_timing {
        ($field:ident, $start:expr) => {
            if let Some(t) = timings.as_mut() {
                t.$field += $start.elapsed().as_micros() as u64;
            }
        };
    }

    let t_precheck = Instant::now();
    let n = graph.num_nodes();
    let m = graph.num_edges();
    let m_real = m - self_loops.count();

    if n == 0 || m_real == 0 {
        add_timing!(t_precheck_us, t_precheck);
        return SpqrTree::empty(m);
    }
    if n == 1 {
        add_timing!(t_precheck_us, t_precheck);
        return SpqrTree::empty(m);
    }
    if m_real == 1 {
        let mut eid_real = 0;
        for i in 0..m {
            if !self_loops.is_loop(i) {
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
        add_timing!(t_precheck_us, t_precheck);
        return SpqrTree::single_node(m, SpqrNodeType::P, 2, edges, vec![e.src, e.dst]);
    }

    // Count distinct non self loop endpoints
    let mut has_non_loop_node = [false, false];
    let mut all_between_01 = true;
    for i in 0..m {
        if self_loops.is_loop(i) {
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
        add_timing!(t_precheck_us, t_precheck);
        return build_parallel_case(graph, self_loops);
    }

    if let Some(tree) = try_build_simple_cycle(graph, self_loops) {
        add_timing!(t_precheck_us, t_precheck);
        return tree;
    }
    add_timing!(t_precheck_us, t_precheck);

    let mut next_virtual = m as u32;
    let mut multi_comps;
    let mut owned_wg: Option<Graph> = None;
    let mut weid_to_label: Vec<u32> = Vec::new();
    let mut relabel_edges;

    if assume_no_non_loop_multi_edges && m_real == m {
        multi_comps = Vec::new();
        relabel_edges = false;
    } else {
        let t_split = Instant::now();
        let (split_multi_comps, synthetic, consumed) = if assume_no_non_loop_multi_edges {
            (Vec::new(), Vec::new(), vec![false; m])
        } else {
            split_multi_edges(graph, &mut next_virtual, self_loops)
        };
        multi_comps = split_multi_comps;
        if let Some(t) = timings.as_mut() {
            t.c_multi_components = multi_comps.len() as u64;
        }
        add_timing!(t_split_multi_edges_us, t_split);

        let t_work_graph = Instant::now();
        let real_count = (0..m)
            .filter(|&i| !consumed[i] && !self_loops.is_loop(i))
            .count();
        let total = real_count + synthetic.len();
        let mut work = Graph::with_capacity(n, total);
        work.add_nodes(n);
        weid_to_label = Vec::with_capacity(total);
        for i in 0..m {
            if !consumed[i] && !self_loops.is_loop(i) {
                let e = graph.edge(EdgeId(i as u32));
                work.add_edge(e.src, e.dst);
                weid_to_label.push(i as u32);
            }
        }
        for &(a, b, vid) in &synthetic {
            work.add_edge(NodeId(a), NodeId(b));
            weid_to_label.push(vid);
        }
        add_timing!(t_work_graph_us, t_work_graph);
        owned_wg = Some(work);
        relabel_edges = true;
    }

    let mut pretriconn_chain_comps: Vec<SplitComponent> = Vec::new();
    let mut pretriconn_chain_atom_comps: Vec<SplitComponent> = Vec::new();
    let mut pretriconn_quotient_node_inv: Option<Vec<u32>> = None;
    let base_wg = owned_wg.as_ref().unwrap_or(graph);
    let labels = if relabel_edges {
        Some(weid_to_label.as_slice())
    } else {
        None
    };
    let mut analysis_next_virtual = next_virtual;
    let analysis =
        group_parallel_p_bonds_only_before_triconn(base_wg, labels, &mut analysis_next_virtual);
    if !analysis.chain_components.is_empty() || !analysis.chain_atom_components.is_empty() {
        owned_wg = Some(analysis.quotient);
        pretriconn_quotient_node_inv = Some(analysis.quotient_node_inv);
        weid_to_label = analysis.quotient_labels;
        relabel_edges = true;
        next_virtual = analysis_next_virtual;
        pretriconn_chain_comps = analysis.chain_components;
        pretriconn_chain_atom_comps = analysis.chain_atom_components;
    }

    let wg = owned_wg.as_ref().unwrap_or(graph);
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
    let t_triconn = Instant::now();
    let mut wcomps = if used_triconn {
        triconn_decompose(wg, ref_weid, &mut next_virtual, &empty_consumed)
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
                origin: 0,
            }]
        }
    };
    if let Some(t) = timings.as_mut() {
        t.c_triconn_components = wcomps.len() as u64;
        t.c_precombine_components = t.c_multi_components.saturating_add(wcomps.len() as u64);
    }
    add_timing!(t_triconn_us, t_triconn);

    let t_relabel = Instant::now();
    if relabel_edges {
        for comp in &mut wcomps {
            for se in &mut comp.edges {
                let i = se.eid as usize;
                if i < weid_to_label.len() {
                    se.eid = weid_to_label[i];
                }
            }
        }
    }
    if let Some(node_inv) = pretriconn_quotient_node_inv.as_ref() {
        for comp in &mut wcomps {
            if (comp.pole_a as usize) < node_inv.len() {
                comp.pole_a = node_inv[comp.pole_a as usize];
            }
            if (comp.pole_b as usize) < node_inv.len() {
                comp.pole_b = node_inv[comp.pole_b as usize];
            }
            for se in &mut comp.edges {
                if (se.src as usize) < node_inv.len() {
                    se.src = node_inv[se.src as usize];
                }
                if (se.dst as usize) < node_inv.len() {
                    se.dst = node_inv[se.dst as usize];
                }
            }
        }
    }
    add_timing!(t_relabel_us, t_relabel);

    let t_combine = Instant::now();
    if !pretriconn_chain_comps.is_empty() {
        multi_comps.extend(pretriconn_chain_comps);
    }
    let mut all = combine_components(multi_comps, wcomps, &mut next_virtual);
    if let Some(t) = timings.as_mut() {
        t.c_combined_components = all.len() as u64;
    }
    add_timing!(t_combine_us, t_combine);

    let t_merge = Instant::now();
    let merged_component_types = Some(merge_same_type_components(&mut all, m));
    if let Some(t) = timings.as_mut() {
        t.c_merged_components = all.len() as u64;
        t.c_virtual_id_span = next_virtual.saturating_sub(m as u32) as u64;
        let mut real_edges = 0u64;
        let mut virtual_incidences = 0u64;
        for comp in &all {
            for e in &comp.edges {
                if (e.eid as usize) < m {
                    real_edges = real_edges.saturating_add(1);
                } else {
                    virtual_incidences = virtual_incidences.saturating_add(1);
                }
            }
        }
        t.c_merged_real_edges = real_edges;
        t.c_merged_virtual_incidences = virtual_incidences;
    }
    add_timing!(t_merge_us, t_merge);

    let mut preassembly_minimizer_sidecar =
        if crate::sp_compress::integration::wants_spqra_minimizer_sidecar() {
            let sidecar = std::sync::Arc::new(build_spqra_minimizer_sidecar_from_split_components(
                graph,
                &all,
                &pretriconn_chain_atom_comps,
                next_virtual,
                merged_component_types.as_deref(),
            ));
            Some(sidecar)
        } else {
            None
        };

    let mut preassembly_scad_export = if crate::sp_compress::integration::wants_core_scad_export() {
        export_core_scad_from_split_components(graph, &all, next_virtual).map(std::sync::Arc::new)
    } else {
        None
    };

    if skip_assemble_spqr_requested()
        && (preassembly_scad_export.is_some() || preassembly_minimizer_sidecar.is_some())
    {
        if let Some(t) = timings.as_mut() {
            if let Some(export) = preassembly_scad_export.as_ref() {
                t.c_tree_nodes = export.components.len() as u64;
                t.c_tree_edges = (export.incidences.len() / 2) as u64;
                t.c_tree_skeleton_edges = export.edges.len() as u64;
                t.c_tree_virtual_incidences = export.incidences.len() as u64;
            } else if let Some(sidecar) = preassembly_minimizer_sidecar.as_ref() {
                t.c_tree_nodes = sidecar.components.len() as u64;
                t.c_tree_edges = sidecar.children.len() as u64;
                t.c_tree_skeleton_edges = sidecar.edges.len() as u64;
                t.c_tree_virtual_incidences = sidecar.children.len().saturating_mul(2) as u64;
            }
        }
        let mut tree = SpqrTree::empty(m);
        tree.preassembly_scad_export = preassembly_scad_export.take();
        tree.preassembly_minimizer_sidecar = preassembly_minimizer_sidecar.take();
        return tree;
    }

    let t_assemble = Instant::now();
    let mut tree = assemble_spqr_tree(graph, &all, next_virtual);
    tree.preassembly_scad_export = preassembly_scad_export;
    tree.preassembly_minimizer_sidecar = preassembly_minimizer_sidecar;
    if let Some(t) = timings.as_mut() {
        t.c_tree_nodes = tree.len() as u64;
        t.c_tree_edges = tree.children.len() as u64;
        t.c_tree_skeleton_edges = tree.skeleton_edges.len() as u64;
        t.c_tree_virtual_incidences = tree
            .skeleton_edges
            .iter()
            .filter(|e| e.virtual_id != INVALID)
            .count() as u64;
    }
    add_timing!(t_assemble_us, t_assemble);
    tree
}

fn export_core_scad_from_split_components(
    graph: &Graph,
    components: &[SplitComponent],
    next_virtual: u32,
) -> Option<crate::sp_compress::integration::CoreScadExport> {
    if components.is_empty() {
        return Some(crate::sp_compress::integration::CoreScadExport {
            components: Vec::new(),
            edges: Vec::new(),
            incidences: Vec::new(),
            node_mapping: Vec::new(),
        });
    }

    let m = graph.num_edges();
    let base = m as u32;
    let component_types = classify_components_parallel(components);
    let total_edges = components.iter().map(|c| c.edges.len()).sum::<usize>();
    let mut out_components: Vec<crate::sp_compress::integration::FfiScadComponent> =
        Vec::with_capacity(components.len());
    let mut out_edges: Vec<crate::sp_compress::integration::FfiScadEdge> =
        Vec::with_capacity(total_edges);
    let mut out_incidences: Vec<crate::sp_compress::integration::FfiScadIncidence> = Vec::new();
    let mut out_node_mapping = Vec::new();
    let mut incidence_first = vec![u32::MAX; next_virtual.saturating_sub(base) as usize];

    let mut dense_seen = vec![0u32; graph.num_nodes()];
    let mut dense_local = vec![0u32; graph.num_nodes()];
    let mut dense_stamp = 1u32;

    for (component_id, (comp, &nt)) in components.iter().zip(component_types.iter()).enumerate() {
        let edge_begin = out_edges.len() as u32;
        let node_begin = out_node_mapping.len() as u32;

        let mut n2o: Vec<NodeId> = Vec::new();
        let local_of = |v: u32,
                        seen: &mut [u32],
                        local: &mut [u32],
                        stamp: u32,
                        n2o: &mut Vec<NodeId>|
         -> u32 {
            let vi = v as usize;
            if vi < seen.len() && seen[vi] == stamp {
                local[vi]
            } else {
                let id = n2o.len() as u32;
                if vi < seen.len() {
                    seen[vi] = stamp;
                    local[vi] = id;
                }
                n2o.push(NodeId(v));
                id
            }
        };

        local_of(
            comp.pole_a,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut n2o,
        );
        local_of(
            comp.pole_b,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut n2o,
        );

        for edge in &comp.edges {
            let src_local = local_of(
                edge.src,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut n2o,
            );
            let dst_local = local_of(
                edge.dst,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut n2o,
            );
            let is_real = (edge.eid as usize) < m;
            let local_edge_id = (out_edges.len() as u32).saturating_sub(edge_begin);
            out_edges.push(crate::sp_compress::integration::FfiScadEdge {
                kind: if is_real { 1 } else { 3 },
                _pad: [0; 3],
                src_local,
                dst_local,
                src_core: n2o.get(src_local as usize).map(|v| v.0).unwrap_or(INVALID),
                dst_core: n2o.get(dst_local as usize).map(|v| v.0).unwrap_or(INVALID),
                original_edge_id: if is_real { edge.eid } else { INVALID },
                macro_id: INVALID,
                virtual_id: if is_real { INVALID } else { edge.eid },
            });

            if !is_real {
                let inc_idx = out_incidences.len() as u32;
                let mut twin_incidence = u32::MAX;
                if edge.eid >= base {
                    let vid_idx = (edge.eid - base) as usize;
                    if vid_idx < incidence_first.len() {
                        let first = incidence_first[vid_idx];
                        if first == u32::MAX {
                            incidence_first[vid_idx] = inc_idx;
                        } else {
                            twin_incidence = first;
                            out_incidences[first as usize].twin_incidence = inc_idx;
                            incidence_first[vid_idx] = u32::MAX;
                        }
                    }
                }
                out_incidences.push(crate::sp_compress::integration::FfiScadIncidence {
                    virtual_id: edge.eid,
                    component_id: component_id as u32,
                    local_edge_id,
                    twin_incidence,
                    sep_u: INVALID,
                    sep_v: INVALID,
                });
            }
        }

        let edge_end = out_edges.len() as u32;
        let inc_begin = 0u32;
        let inc_end = out_incidences.len() as u32;
        out_node_mapping.extend(n2o.iter().map(|v| v.0));
        let node_end = out_node_mapping.len() as u32;
        out_components.push(crate::sp_compress::integration::FfiScadComponent {
            raw_component_id: component_id as u32,
            kind: match nt {
                SpqrNodeType::S => 1,
                SpqrNodeType::P => 2,
                SpqrNodeType::R => 3,
            },
            _pad: [0; 3],
            edge_begin,
            edge_end,
            inc_begin,
            inc_end,
            node_begin,
            node_end,
        });

        dense_stamp = dense_stamp.wrapping_add(1);
        if dense_stamp == 0 {
            dense_seen.fill(0);
            dense_stamp = 1;
        }
    }

    Some(crate::sp_compress::integration::CoreScadExport {
        components: out_components,
        edges: out_edges,
        incidences: out_incidences,
        node_mapping: out_node_mapping,
    })
}

fn build_spqra_minimizer_sidecar_from_split_components(
    graph: &Graph,
    components_in: &[SplitComponent],
    atom_components: &[SplitComponent],
    next_virtual: u32,
    component_types_hint: Option<&[SpqrNodeType]>,
) -> crate::sp_compress::integration::SpqraMinimizerSidecar {
    use crate::sp_compress::integration::{
        FfiSpqraBehaviorAtom, FfiSpqraBehaviorAtomItem, FfiSpqraMinimizerComponent,
        FfiSpqraMinimizerEdge, FfiSpqraMinimizerSummary, SpqraMinimizerSidecar,
        SPQRA_MIN_ATOM_ITEM_BEHAVIOR_ATOM, SPQRA_MIN_ATOM_ITEM_CHILD_REF,
        SPQRA_MIN_EDGE_HAS_BEHAVIOR_ATOM, SPQRA_MIN_EDGE_HAS_CHILD_REF, SPQRA_MIN_EDGE_VIRTUAL,
    };

    let n = components_in.len();
    let root = if n == 0 { INVALID } else { 0 };
    let m = graph.num_edges();
    let base = m as u32;
    let component_types: std::borrow::Cow<'_, [SpqrNodeType]> =
        if let Some(types) = component_types_hint {
            if types.len() == n {
                std::borrow::Cow::Borrowed(types)
            } else {
                std::borrow::Cow::Owned(classify_components_parallel(components_in))
            }
        } else {
            std::borrow::Cow::Owned(classify_components_parallel(components_in))
        };
    let total_edges = components_in.iter().map(|c| c.edges.len()).sum::<usize>();

    let mut components = Vec::with_capacity(n);
    let mut edges: Vec<FfiSpqraMinimizerEdge> = Vec::with_capacity(total_edges);
    let mut behavior_atoms: Vec<FfiSpqraBehaviorAtom> =
        Vec::with_capacity(atom_components.len().saturating_add(n));
    let mut behavior_atom_items: Vec<FfiSpqraBehaviorAtomItem> =
        Vec::with_capacity(total_edges.saturating_mul(2));
    let mut node_mapping = Vec::new();
    let mut tree_pairs: Vec<(u32, u32)> = Vec::new();
    let mut virtual_first =
        vec![(INVALID, INVALID, INVALID); next_virtual.saturating_sub(base) as usize];
    let mut atom_by_virtual = vec![INVALID; next_virtual.saturating_sub(base) as usize];
    let mut attachment_by_virtual = vec![false; next_virtual.saturating_sub(base) as usize];
    let mut rap_r_leaf_folded = vec![false; n];
    for atom_comp in atom_components {
        let atom_vid = atom_comp.edges.last().map(|e| e.eid).unwrap_or(INVALID);
        if atom_vid < base {
            continue;
        }
        let atom_idx = behavior_atoms.len() as u32;
        let item_begin = behavior_atom_items.len() as u32;
        for e in atom_comp
            .edges
            .iter()
            .take(atom_comp.edges.len().saturating_sub(1))
        {
            if let Some(child_ref) = spqra_core_child_ref(e.eid as usize) {
                behavior_atom_items.push(FfiSpqraBehaviorAtomItem {
                    child_ref,
                    flags: SPQRA_MIN_ATOM_ITEM_CHILD_REF,
                    src_core: e.src,
                    dst_core: e.dst,
                });
            } else if e.eid >= base {
                let vidx = (e.eid - base) as usize;
                let atom_ref = atom_by_virtual.get(vidx).copied().unwrap_or(INVALID);
                if atom_ref != INVALID {
                    behavior_atom_items.push(FfiSpqraBehaviorAtomItem {
                        child_ref: atom_ref,
                        flags: SPQRA_MIN_ATOM_ITEM_BEHAVIOR_ATOM,
                        src_core: e.src,
                        dst_core: e.dst,
                    });
                }
            }
        }
        let item_end = behavior_atom_items.len() as u32;
        if item_end == item_begin {
            continue;
        }
        let atom_kind = if (atom_comp.origin & SPLIT_ORIGIN_PRETRICONN_P_BOND) != 0 {
            2
        } else {
            1
        };
        behavior_atoms.push(FfiSpqraBehaviorAtom {
            kind: atom_kind,
            _pad: [atom_comp.origin, 0, 0],
            item_begin,
            item_end,
            port0_core: atom_comp.pole_a,
            port1_core: atom_comp.pole_b,
        });
        let vidx = (atom_vid - base) as usize;
        if vidx < atom_by_virtual.len() {
            atom_by_virtual[vidx] = atom_idx;
        }
    }
    for (component_id, (comp, &nt)) in components_in.iter().zip(component_types.iter()).enumerate()
    {
        if nt != SpqrNodeType::R {
            continue;
        }
        let mut boundary_vid = INVALID;
        let mut topology_virtuals = 0u32;
        let mut item_count = 0u32;
        let mut foldable = true;
        for e in &comp.edges {
            if (e.eid as usize) < m {
                if spqra_core_child_ref(e.eid as usize).is_some() {
                    item_count = item_count.saturating_add(1);
                } else {
                    foldable = false;
                    break;
                }
            } else if e.eid >= base {
                let vidx = (e.eid - base) as usize;
                let atom_ref = atom_by_virtual.get(vidx).copied().unwrap_or(INVALID);
                if atom_ref != INVALID {
                    item_count = item_count.saturating_add(1);
                } else {
                    topology_virtuals = topology_virtuals.saturating_add(1);
                    boundary_vid = e.eid;
                }
            } else {
                foldable = false;
                break;
            }
        }
        if foldable
            && topology_virtuals == 1
            && boundary_vid >= base
            && ((boundary_vid - base) as usize) < atom_by_virtual.len()
            && item_count > 0
        {
            attachment_by_virtual[(boundary_vid - base) as usize] = true;
            rap_r_leaf_folded[component_id] = true;
        }
    }
    let mut component_sidecar_id = vec![INVALID; n];
    let mut sidecar_component_count = 0u32;
    for cid in 0..n {
        if rap_r_leaf_folded.get(cid).copied().unwrap_or(false) {
            continue;
        }
        component_sidecar_id[cid] = sidecar_component_count;
        sidecar_component_count = sidecar_component_count.saturating_add(1);
    }
    let mut summary = FfiSpqraMinimizerSummary {
        root: component_sidecar_id
            .get(root as usize)
            .copied()
            .filter(|&r| r != INVALID)
            .unwrap_or(0),
        ..FfiSpqraMinimizerSummary::default()
    };

    let mut dense_seen = vec![0u32; graph.num_nodes()];
    let mut dense_local = vec![0u32; graph.num_nodes()];
    let mut dense_stamp = 1u32;
    let mut virtual_inc_count = 0u32;

    for (component_id, (comp, &nt)) in components_in.iter().zip(component_types.iter()).enumerate()
    {
        let sidecar_component_id = component_sidecar_id
            .get(component_id)
            .copied()
            .unwrap_or(INVALID);
        let edge_begin = edges.len() as u32;
        let node_begin = node_mapping.len() as u32;
        if sidecar_component_id == INVALID {
            continue;
        }

        let mut n2o: Vec<NodeId> = Vec::new();
        let local_of = |v: u32,
                        seen: &mut [u32],
                        local: &mut [u32],
                        stamp: u32,
                        n2o: &mut Vec<NodeId>|
         -> u32 {
            let vi = v as usize;
            if vi < seen.len() && seen[vi] == stamp {
                local[vi]
            } else {
                let id = n2o.len() as u32;
                if vi < seen.len() {
                    seen[vi] = stamp;
                    local[vi] = id;
                }
                n2o.push(NodeId(v));
                id
            }
        };

        local_of(
            comp.pole_a,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut n2o,
        );
        local_of(
            comp.pole_b,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut n2o,
        );

        let kind = match nt {
            SpqrNodeType::S => 1,
            SpqrNodeType::P => 2,
            SpqrNodeType::R => 3,
        };

        for edge in &comp.edges {
            let src_local = local_of(
                edge.src,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut n2o,
            );
            let dst_local = local_of(
                edge.dst,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut n2o,
            );
            let local_edge_id = (edges.len() as u32).saturating_sub(edge_begin);
            let global_edge_id = edges.len() as u32;
            let is_real = (edge.eid as usize) < m;
            let mut min_edge = FfiSpqraMinimizerEdge {
                src_local,
                dst_local,
                flags: if is_real { 0 } else { SPQRA_MIN_EDGE_VIRTUAL },
                ..FfiSpqraMinimizerEdge::default()
            };

            if is_real {
                if let Some(child_ref) = spqra_core_child_ref(edge.eid as usize) {
                    min_edge.child_ref = child_ref;
                    min_edge.flags |= SPQRA_MIN_EDGE_HAS_CHILD_REF;
                }
            } else if edge.eid >= base {
                let vidx = (edge.eid - base) as usize;
                let has_attachment = attachment_by_virtual.get(vidx).copied().unwrap_or(false);
                if has_attachment {
                    min_edge.flags = 0;
                } else {
                    let atom_idx = atom_by_virtual.get(vidx).copied().unwrap_or(INVALID);
                    if atom_idx != INVALID {
                        min_edge.flags = SPQRA_MIN_EDGE_HAS_BEHAVIOR_ATOM;
                        min_edge.child_ref = atom_idx;
                    } else {
                        virtual_inc_count = virtual_inc_count.saturating_add(1);
                        if vidx < virtual_first.len() {
                            let (first_component, first_local, first_global) = virtual_first[vidx];
                            if first_component == INVALID {
                                virtual_first[vidx] =
                                    (sidecar_component_id, local_edge_id, global_edge_id);
                            } else {
                                min_edge.twin_component = first_component;
                                min_edge.twin_local_edge = first_local;
                                if let Some(first_edge) = edges.get_mut(first_global as usize) {
                                    first_edge.twin_component = sidecar_component_id;
                                    first_edge.twin_local_edge = local_edge_id;
                                    if first_component == sidecar_component_id {
                                        summary.bad_twin_count =
                                            summary.bad_twin_count.saturating_add(1);
                                    } else {
                                        tree_pairs.push((first_component, sidecar_component_id));
                                    }
                                } else {
                                    summary.bad_twin_count =
                                        summary.bad_twin_count.saturating_add(1);
                                }
                                virtual_first[vidx] = (INVALID, INVALID, INVALID);
                            }
                        } else {
                            summary.bad_twin_count = summary.bad_twin_count.saturating_add(1);
                        }
                    }
                }
            } else {
                summary.bad_twin_count = summary.bad_twin_count.saturating_add(1);
            }
            edges.push(min_edge);
        }

        node_mapping.extend(n2o.iter().map(|v| v.0));
        let node_end = node_mapping.len() as u32;
        let edge_end = edges.len() as u32;
        components.push(FfiSpqraMinimizerComponent {
            kind,
            _pad: [comp.origin, 0, 0],
            raw_component_id: sidecar_component_id,
            parent: INVALID,
            child_begin: 0,
            child_end: 0,
            edge_begin,
            edge_end,
            inc_begin: 0,
            inc_end: 0,
            node_begin,
            node_end,
            port0_core: INVALID,
            port1_core: INVALID,
        });

        dense_stamp = dense_stamp.wrapping_add(1);
        if dense_stamp == 0 {
            dense_seen.fill(0);
            dense_stamp = 1;
        }
    }
    for &(component_id, _, _) in &virtual_first {
        if component_id != INVALID {
            summary.bad_twin_count = summary.bad_twin_count.saturating_add(1);
        }
    }
    let active_n = components.len();

    let mut parents = vec![INVALID; active_n];
    let children = if active_n == 0 {
        Vec::new()
    } else {
        let mut adj_count = vec![0u32; active_n];
        for &(a, b) in &tree_pairs {
            let ai = a as usize;
            let bi = b as usize;
            if ai < active_n && bi < active_n {
                adj_count[ai] = adj_count[ai].saturating_add(1);
                adj_count[bi] = adj_count[bi].saturating_add(1);
            }
        }
        let mut adj_offsets = vec![0u32; active_n + 1];
        for i in 0..active_n {
            adj_offsets[i + 1] = adj_offsets[i].saturating_add(adj_count[i]);
        }
        let mut adj = vec![INVALID; adj_offsets[active_n] as usize];
        let mut write = adj_offsets[..active_n].to_vec();
        for &(a, b) in &tree_pairs {
            let ai = a as usize;
            let bi = b as usize;
            if ai < active_n && bi < active_n {
                adj[write[ai] as usize] = b;
                write[ai] = write[ai].saturating_add(1);
                adj[write[bi] as usize] = a;
                write[bi] = write[bi].saturating_add(1);
            }
        }

        let mut seen = vec![false; active_n];
        let mut stack = vec![0u32];
        if !seen.is_empty() {
            seen[0] = true;
        }
        while let Some(v) = stack.pop() {
            let vi = v as usize;
            for ai in adj_offsets[vi] as usize..adj_offsets[vi + 1] as usize {
                let u = adj[ai];
                if u == INVALID {
                    continue;
                }
                let ui = u as usize;
                if ui >= n || seen[ui] {
                    continue;
                }
                seen[ui] = true;
                parents[ui] = v;
                stack.push(u);
            }
        }
        for i in 1..active_n {
            if !seen[i] {
                parents[i] = 0;
            }
        }

        let mut child_count = vec![0u32; active_n];
        for &p in &parents {
            if p != INVALID && (p as usize) < active_n {
                child_count[p as usize] = child_count[p as usize].saturating_add(1);
            }
        }
        let mut child_offsets = vec![0u32; active_n + 1];
        for i in 0..active_n {
            child_offsets[i + 1] = child_offsets[i].saturating_add(child_count[i]);
        }
        let mut children = vec![INVALID; child_offsets[active_n] as usize];
        write = child_offsets[..active_n].to_vec();
        for (child, &p) in parents.iter().enumerate() {
            if p != INVALID && (p as usize) < active_n {
                children[write[p as usize] as usize] = child as u32;
                write[p as usize] = write[p as usize].saturating_add(1);
            }
        }
        for i in 0..active_n {
            components[i].parent = parents[i];
            components[i].child_begin = child_offsets[i];
            components[i].child_end = child_offsets[i + 1];
        }
        children
    };

    let mut postorder = Vec::with_capacity(active_n);
    if active_n > 0 {
        let mut entered = vec![false; active_n];
        let mut stack = vec![0u32];
        while let Some(&tn) = stack.last() {
            let ti = tn as usize;
            if ti >= active_n {
                stack.pop();
                continue;
            }
            if !entered[ti] {
                entered[ti] = true;
                for ci in components[ti].child_begin as usize..components[ti].child_end as usize {
                    stack.push(children[ci]);
                }
            } else {
                stack.pop();
                postorder.push(tn);
            }
        }
    }

    SpqraMinimizerSidecar {
        components,
        edges,
        behavior_atoms,
        behavior_atom_items,
        node_mapping,
        children,
        postorder,
        summary,
    }
}

fn build_parallel_case(graph: &Graph, self_loops: SelfLoopFlags<'_>) -> SpqrTree {
    let m = graph.num_edges();
    let mut edges = Vec::new();
    let mut edge_to_tree_node = vec![TreeNodeId::INVALID; m];
    let mut min_real: u32 = u32::MAX;

    for i in 0..m {
        if self_loops.is_loop(i) {
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
        if (i as u32) < min_real {
            min_real = i as u32;
        }
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
        min_real_per_node: vec![min_real],
        preassembly_scad_export: None,
        preassembly_minimizer_sidecar: None,
    }
}

pub static FAST_CYCLE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static FAST_CYCLE_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn try_build_simple_cycle(graph: &Graph, self_loops: SelfLoopFlags<'_>) -> Option<SpqrTree> {
    FAST_CYCLE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let n = graph.num_nodes();
    let m = graph.num_edges();

    if n < 3 || m != n {
        return None;
    }
    // No self-loops allowed on the simple-cycle path.
    if self_loops.any() {
        return None;
    }
    // Every vertex has degree exactly 2.
    for v in 0..n {
        if graph.degree(NodeId(v as u32)) != 2 {
            return None;
        }
    }

    // walk the cycle starting at vertex 0
    let mut order: Vec<u32> = Vec::with_capacity(n);
    let mut edge_order: Vec<u32> = Vec::with_capacity(n);
    let mut visited = vec![false; n];

    let mut current: u32 = 0;
    let mut prev_edge: u32 = u32::MAX;
    visited[0] = true;
    order.push(0);

    for step in 0..n {
        // Pick the incident edge that isn'tprev_edge
        let mut next_node: u32 = u32::MAX;
        let mut next_edge: u32 = u32::MAX;
        for (nb, eid) in graph.neighbors(NodeId(current)) {
            if eid.0 != prev_edge {
                next_node = nb.0;
                next_edge = eid.0;
                break;
            }
        }
        if next_edge == u32::MAX {
            return None;
        }
        edge_order.push(next_edge);

        if step == n - 1 {
            if next_node != 0 {
                return None;
            }
            break;
        }
        if (next_node as usize) >= n || visited[next_node as usize] {
            return None;
        }
        visited[next_node as usize] = true;
        order.push(next_node);
        prev_edge = next_edge;
        current = next_node;
    }

    debug_assert_eq!(order.len(), n);
    debug_assert_eq!(edge_order.len(), n);

    let mut edges: Vec<SkeletonEdge> = Vec::with_capacity(n);
    for i in 0..n {
        edges.push(SkeletonEdge {
            src: NodeId(i as u32),
            dst: NodeId(((i + 1) % n) as u32),
            real_edge: EdgeId(edge_order[i]),
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        });
    }
    let node_mapping: Vec<NodeId> = order.into_iter().map(NodeId).collect();

    FAST_CYCLE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some(SpqrTree::single_node(
        m,
        SpqrNodeType::S,
        n as u32,
        edges,
        node_mapping,
    ))
}

#[allow(clippy::type_complexity)]
fn split_multi_edges(
    graph: &Graph,
    next_virtual: &mut u32,
    self_loops: SelfLoopFlags<'_>,
) -> (Vec<SplitComponent>, Vec<(u32, u32, u32)>, Vec<bool>) {
    #[derive(Clone, Copy)]
    struct PairEdge {
        key: u64,
        eid: u32,
    }

    #[inline]
    fn pair_key(a: u32, b: u32) -> u64 {
        let lo = a.min(b) as u64;
        let hi = a.max(b) as u64;
        (lo << 32) | hi
    }

    #[inline]
    fn key_endpoints(key: u64) -> (u32, u32) {
        ((key >> 32) as u32, key as u32)
    }

    fn emit_parallel_group(
        key: u64,
        eids: &[u32],
        graph: &Graph,
        next_virtual: &mut u32,
        p_comps: &mut Vec<SplitComponent>,
        consumed: &mut [bool],
        synthetic: &mut Vec<(u32, u32, u32)>,
    ) {
        if eids.len() < 2 {
            return;
        }
        let (a, b) = key_endpoints(key);
        let vid = *next_virtual;
        *next_virtual += 1;
        let mut edges: Vec<StackEdge> = Vec::with_capacity(eids.len() + 1);
        for &eid in eids {
            let e = graph.edge(EdgeId(eid));
            edges.push(StackEdge {
                src: e.src.0,
                dst: e.dst.0,
                eid,
            });
            consumed[eid as usize] = true;
        }
        edges.push(StackEdge {
            src: a,
            dst: b,
            eid: vid,
        });
        p_comps.push(SplitComponent {
            edges,
            pole_a: a,
            pole_b: b,
            origin: 0,
        });
        synthetic.push((a, b, vid));
    }

    let m = graph.num_edges();
    let mut pairs: Vec<PairEdge> = Vec::with_capacity(m);
    for i in 0..m {
        if self_loops.is_loop(i) {
            continue;
        }
        let e = graph.edge(EdgeId(i as u32));
        pairs.push(PairEdge {
            key: pair_key(e.src.0, e.dst.0),
            eid: i as u32,
        });
    }
    let mut p_comps = Vec::new();
    let mut consumed = vec![false; m];
    let mut synthetic = Vec::new();

    const MIN_PAR_SORT_EDGES: usize = 1_000_000;
    let threads = spqr_thread_count().min(pairs.len().max(1));
    if threads > 1 && pairs.len() >= MIN_PAR_SORT_EDGES {
        let chunk = pairs.len().div_ceil(threads);
        thread::scope(|scope| {
            for chunk_pairs in pairs.chunks_mut(chunk) {
                scope.spawn(move || {
                    chunk_pairs.sort_unstable_by_key(|pe| (pe.key, pe.eid));
                });
            }
        });

        let mut ranges = Vec::new();
        let mut start = 0usize;
        while start < pairs.len() {
            let end = (start + chunk).min(pairs.len());
            ranges.push((start, end));
            start = end;
        }

        let mut heap = std::collections::BinaryHeap::new();
        for (chunk_id, &(start, end)) in ranges.iter().enumerate() {
            if start < end {
                let pe = pairs[start];
                heap.push(std::cmp::Reverse((pe.key, pe.eid, chunk_id, start)));
            }
        }

        let mut current_key: Option<u64> = None;
        let mut current_eids: Vec<u32> = Vec::new();
        while let Some(std::cmp::Reverse((key, eid, chunk_id, pos))) = heap.pop() {
            if current_key != Some(key) {
                if let Some(prev_key) = current_key {
                    emit_parallel_group(
                        prev_key,
                        &current_eids,
                        graph,
                        next_virtual,
                        &mut p_comps,
                        &mut consumed,
                        &mut synthetic,
                    );
                }
                current_key = Some(key);
                current_eids.clear();
            }
            current_eids.push(eid);

            let next_pos = pos + 1;
            let (_, end) = ranges[chunk_id];
            if next_pos < end {
                let pe = pairs[next_pos];
                heap.push(std::cmp::Reverse((pe.key, pe.eid, chunk_id, next_pos)));
            }
        }
        if let Some(prev_key) = current_key {
            emit_parallel_group(
                prev_key,
                &current_eids,
                graph,
                next_virtual,
                &mut p_comps,
                &mut consumed,
                &mut synthetic,
            );
        }
    } else {
        pairs.sort_unstable_by_key(|pe| (pe.key, pe.eid));
        let mut i = 0usize;
        while i < pairs.len() {
            let key = pairs[i].key;
            let start = i;
            i += 1;
            while i < pairs.len() && pairs[i].key == key {
                i += 1;
            }
            if i - start >= 2 {
                let mut eids = Vec::with_capacity(i - start);
                for pe in &pairs[start..i] {
                    eids.push(pe.eid);
                }
                emit_parallel_group(
                    key,
                    &eids,
                    graph,
                    next_virtual,
                    &mut p_comps,
                    &mut consumed,
                    &mut synthetic,
                );
            }
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
                    match a.cmp(&lp1[pv]) {
                        std::cmp::Ordering::Less => {
                            lp2[pv] = std::cmp::min(lp1[pv], b);
                            lp1[pv] = a;
                        }
                        std::cmp::Ordering::Equal => {
                            lp2[pv] = std::cmp::min(lp2[pv], b);
                        }
                        std::cmp::Ordering::Greater => {
                            lp2[pv] = std::cmp::min(lp2[pv], a);
                        }
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
                match nw.cmp(&lp1[v as usize]) {
                    std::cmp::Ordering::Less => {
                        lp2[v as usize] = lp1[v as usize];
                        lp1[v as usize] = nw;
                    }
                    std::cmp::Ordering::Equal => {}
                    std::cmp::Ordering::Greater => {
                        lp2[v as usize] = std::cmp::min(lp2[v as usize], nw);
                    }
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
    let mut hp_init: Vec<(u32, i32, u32)> =
        Vec::with_capacity(m.saturating_sub(n.saturating_sub(1)));
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
                hp_init.push((w, newnum[fr.v as usize], ei as u32));
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
    for &(v, val, eidx) in &hp_init {
        let v = v as usize;
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
                            origin: 0,
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
                                origin: 0,
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
                            origin: 0,
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
                                origin: 0,
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
                    origin: 0,
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
                            origin: 0,
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
                        origin: 0,
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
            origin: 0,
        });
    }

    comps
}

fn combine_components(
    multi: Vec<SplitComponent>,
    work: Vec<SplitComponent>,
    next_virtual: &mut u32,
) -> Vec<SplitComponent> {
    combine_components_parallel(multi, work, next_virtual)
}

fn combine_one_component(comp: SplitComponent, next_virtual: &mut u32) -> Vec<SplitComponent> {
    let mut out = Vec::new();
    let mut pending: Vec<SplitComponent> = vec![comp];
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

struct CombinedComponentBatch {
    parts: Vec<SplitComponent>,
    local_virtuals: u32,
}

const LOCAL_VIRTUAL_BASE: u32 = 0xC000_0000;

fn combine_components_parallel(
    multi: Vec<SplitComponent>,
    work: Vec<SplitComponent>,
    next_virtual: &mut u32,
) -> Vec<SplitComponent> {
    const MIN_PAR_COMBINE_COMPONENTS: usize = 1024;

    let n = work.len();
    let threads = spqr_thread_count().min(n.max(1));
    if threads <= 1 || n < MIN_PAR_COMBINE_COMPONENTS || *next_virtual >= LOCAL_VIRTUAL_BASE {
        let mut out = multi;
        for comp in work.into_iter().rev() {
            out.extend(combine_one_component(comp, next_virtual));
        }
        return out;
    }

    let chunk = n.div_ceil(threads);
    let mut chunks: Vec<Vec<(usize, SplitComponent)>> = Vec::new();
    let mut cur: Vec<(usize, SplitComponent)> = Vec::with_capacity(chunk);
    for (idx, comp) in work.into_iter().enumerate() {
        cur.push((idx, comp));
        if cur.len() == chunk {
            chunks.push(cur);
            cur = Vec::with_capacity(chunk);
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }

    let mut joined = Vec::with_capacity(chunks.len());
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(chunks.len());
        for chunk_jobs in chunks {
            handles.push(scope.spawn(move || {
                let mut local_results = Vec::with_capacity(chunk_jobs.len());
                for (idx, comp) in chunk_jobs {
                    let mut local_next = LOCAL_VIRTUAL_BASE;
                    let parts = combine_one_component(comp, &mut local_next);
                    local_results.push((
                        idx,
                        CombinedComponentBatch {
                            parts,
                            local_virtuals: local_next - LOCAL_VIRTUAL_BASE,
                        },
                    ));
                }
                local_results
            }));
        }
        for handle in handles {
            joined.push(
                handle
                    .join()
                    .expect("parallel SPQR combine worker panicked"),
            );
        }
    });

    let mut batches: Vec<Option<CombinedComponentBatch>> = (0..n).map(|_| None).collect();
    for local_results in joined {
        for (idx, batch) in local_results {
            batches[idx] = Some(batch);
        }
    }

    let mut bases = vec![0u32; n];
    let mut next = *next_virtual;
    for idx in (0..n).rev() {
        bases[idx] = next;
        let count = batches[idx]
            .as_ref()
            .expect("missing parallel SPQR combine batch")
            .local_virtuals;
        next = next.saturating_add(count);
    }
    *next_virtual = next;

    let mut out = multi;
    for idx in (0..n).rev() {
        let mut batch = batches[idx]
            .take()
            .expect("missing parallel SPQR combine batch");
        let base = bases[idx];
        for comp in &mut batch.parts {
            for edge in &mut comp.edges {
                if edge.eid >= LOCAL_VIRTUAL_BASE {
                    edge.eid = base + (edge.eid - LOCAL_VIRTUAL_BASE);
                }
            }
        }
        out.extend(batch.parts);
    }
    out
}

fn split_internal_parallels(comp: SplitComponent, next_virtual: &mut u32) -> Vec<SplitComponent> {
    if comp.edges.len() <= 64 {
        let mut verts = [0u32; 128];
        let mut vert_len = 0usize;
        for e in &comp.edges {
            if !verts[..vert_len].contains(&e.src) {
                verts[vert_len] = e.src;
                vert_len += 1;
            }
            if !verts[..vert_len].contains(&e.dst) {
                verts[vert_len] = e.dst;
                vert_len += 1;
            }
        }
        if vert_len <= 2 {
            return vec![comp];
        }

        let mut pairs = [(0u32, 0u32); 64];
        for (idx, e) in comp.edges.iter().enumerate() {
            pairs[idx] = if e.src <= e.dst {
                (e.src, e.dst)
            } else {
                (e.dst, e.src)
            };
        }
        let pair_slice = &mut pairs[..comp.edges.len()];
        pair_slice.sort_unstable();
        let has_internal_parallel = pair_slice.windows(2).any(|w| w[0] == w[1]);
        if !has_internal_parallel {
            return vec![comp];
        }
    }

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
                origin: comp.origin,
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
        origin: comp.origin,
    });
    result
}

fn classify_component(comp: &SplitComponent) -> SpqrNodeType {
    if comp.edges.len() <= 64 {
        let mut deg = [(0u32, 0u32); 128];
        let mut deg_len = 0usize;
        for e in &comp.edges {
            for node in [e.src, e.dst] {
                if let Some(pos) = deg[..deg_len].iter().position(|&(v, _)| v == node) {
                    deg[pos].1 = deg[pos].1.saturating_add(1);
                } else {
                    deg[deg_len] = (node, 1);
                    deg_len += 1;
                }
            }
        }
        let v = deg_len;
        let e = comp.edges.len();
        if v == 2 && e >= 2 {
            return SpqrNodeType::P;
        }
        if e == v && e >= 3 && deg[..deg_len].iter().all(|&(_, d)| d == 2) {
            return SpqrNodeType::S;
        }
        return SpqrNodeType::R;
    }

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

fn classify_components_parallel(comps: &[SplitComponent]) -> Vec<SpqrNodeType> {
    const MIN_PAR_COMPONENTS: usize = 4096;
    let n = comps.len();
    let threads = spqr_thread_count().min(n.max(1));
    if threads <= 1 || n < MIN_PAR_COMPONENTS {
        return comps.iter().map(classify_component).collect();
    }

    let mut out = vec![SpqrNodeType::R; n];
    let chunk = n.div_ceil(threads);
    thread::scope(|scope| {
        for (out_chunk, comp_chunk) in out.chunks_mut(chunk).zip(comps.chunks(chunk)) {
            scope.spawn(move || {
                for (dst, comp) in out_chunk.iter_mut().zip(comp_chunk.iter()) {
                    *dst = classify_component(comp);
                }
            });
        }
    });
    out
}

fn merge_same_type_components(comps: &mut Vec<SplitComponent>, m: usize) -> Vec<SpqrNodeType> {
    let ctype = classify_components_parallel(comps);

    let mut max_virtual_eid = m as u32;
    for comp in comps.iter() {
        for e in &comp.edges {
            if (e.eid as usize) >= m && e.eid > max_virtual_eid {
                max_virtual_eid = e.eid;
            }
        }
    }

    if (max_virtual_eid as usize) < m {
        let mut new_comps = Vec::with_capacity(comps.len());
        let mut new_types = Vec::with_capacity(comps.len());
        for (comp, ty) in std::mem::take(comps).into_iter().zip(ctype) {
            if !comp.edges.is_empty() {
                new_comps.push(comp);
                new_types.push(ty);
            }
        }
        *comps = new_comps;
        return new_types;
    }

    const INVALID_COMP: usize = usize::MAX;
    let virtual_count = (max_virtual_eid as usize) - m + 1;
    let mut comp1: Vec<usize> = vec![INVALID_COMP; virtual_count];
    let mut comp2: Vec<usize> = vec![INVALID_COMP; virtual_count];

    for (ci, comp) in comps.iter().enumerate() {
        for e in &comp.edges {
            if (e.eid as usize) >= m {
                let idx = (e.eid as usize) - m;
                if comp1[idx] == INVALID_COMP {
                    comp1[idx] = ci;
                } else {
                    comp2[idx] = ci;
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
            let vidx = (eid as usize) - m;
            if vidx >= comp1.len() {
                ei += 1;
                continue;
            }

            let c1 = comp1[vidx];
            let c2 = comp2[vidx];
            let j = match (c1, c2) {
                (a, b) if a != INVALID_COMP && b != INVALID_COMP && a == i && !visited[b] => b,
                (a, b) if a != INVALID_COMP && b != INVALID_COMP && b == i && !visited[a] => a,
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
            comps[i].origin |= comps[j].origin;

            let mut j_edges = std::mem::take(&mut comps[j].edges);
            j_edges.retain(|e| e.eid != eid);

            for e in &j_edges {
                if (e.eid as usize) >= m {
                    let idx = (e.eid as usize) - m;
                    if idx >= comp1.len() {
                        continue;
                    }
                    if comp1[idx] == j {
                        comp1[idx] = i;
                    }
                    if comp2[idx] == j {
                        comp2[idx] = i;
                    }
                }
            }

            comps[i].edges.swap_remove(ei);
            comps[i].edges.append(&mut j_edges);
        }
    }

    let mut new_comps = Vec::with_capacity(comps.len());
    let mut new_types = Vec::with_capacity(comps.len());
    for (comp, ty) in std::mem::take(comps).into_iter().zip(ctype) {
        if !comp.edges.is_empty() {
            new_comps.push(comp);
            new_types.push(ty);
        }
    }
    *comps = new_comps;
    new_types
}

fn assemble_spqr_tree(graph: &Graph, components: &[SplitComponent], next_virtual: u32) -> SpqrTree {
    let m = graph.num_edges();
    let base = m as u32;

    let mut builder = SpqrTreeBuilder::new(m);
    let component_types = classify_components_parallel(components);
    let mut dense_seen = vec![0u32; graph.num_nodes()];
    let mut dense_local = vec![0u32; graph.num_nodes()];
    let mut dense_stamp = 1u32;

    for (comp, &nt) in components.iter().zip(component_types.iter()) {
        let mut n2o: Vec<NodeId> = Vec::new();
        let local_of = |v: u32,
                        seen: &mut [u32],
                        local: &mut [u32],
                        stamp: u32,
                        n2o: &mut Vec<NodeId>|
         -> u32 {
            let vi = v as usize;
            if vi < seen.len() && seen[vi] == stamp {
                local[vi]
            } else {
                let id = n2o.len() as u32;
                if vi < seen.len() {
                    seen[vi] = stamp;
                    local[vi] = id;
                }
                n2o.push(NodeId(v));
                id
            }
        };
        local_of(
            comp.pole_a,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut n2o,
        );
        local_of(
            comp.pole_b,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut n2o,
        );
        let mut se = Vec::with_capacity(comp.edges.len());
        for edge in &comp.edges {
            let ls_val = local_of(
                edge.src,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut n2o,
            );
            let ld_val = local_of(
                edge.dst,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut n2o,
            );
            let ls = NodeId(ls_val);
            let ld = NodeId(ld_val);
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
        let num_nodes = n2o.len() as u32;
        for (i, e) in se.iter().enumerate() {
            assert!(
                e.src.0 < num_nodes && e.dst.0 < num_nodes,
                "Edge {} has invalid indices: src={}, dst={}, num_nodes={}, tree_node_idx={}",
                i,
                e.src.0,
                e.dst.0,
                num_nodes,
                builder.num_nodes()
            );
        }
        builder.add_node(nt, num_nodes, se, n2o);
        dense_stamp = dense_stamp.wrapping_add(1);
        if dense_stamp == 0 {
            dense_seen.fill(0);
            dense_stamp = 1;
        }
    }

    let num_nodes = builder.num_nodes();
    if num_nodes == 0 {
        return builder.finalize_empty();
    }

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

#[inline]
fn mix64(state: u64, x: u64) -> u64 {
    let mut h = state ^ x;
    h = h.wrapping_mul(0x100000001b3);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc2b2ae3d27d4eb4f);
    h ^= h >> 29;
    h
}

impl SpqrTree {
    pub fn canonicalize_skeleton_node_order(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }
        for tn in 0..n {
            let nm_s = self.node_mapping_offsets[tn] as usize;
            let nm_e = self.node_mapping_offsets[tn + 1] as usize;
            let k = nm_e - nm_s;
            if k <= 1 {
                continue;
            }
            let mut perm: Vec<u32> = (0..k as u32).collect();
            perm.sort_by_key(|&old_local| self.node_mapping[nm_s + old_local as usize].0);
            let mut inv = vec![0u32; k];
            for new_local in 0..k {
                inv[perm[new_local] as usize] = new_local as u32;
            }
            if (0..k).all(|i| perm[i] == i as u32) {
                continue;
            }
            let new_nm: Vec<crate::NodeId> = (0..k)
                .map(|new_local| self.node_mapping[nm_s + perm[new_local] as usize])
                .collect();
            self.node_mapping[nm_s..(nm_s + k)].copy_from_slice(&new_nm[..k]);
            let s = self.skeleton_offsets[tn] as usize;
            let e = self.skeleton_offsets[tn + 1] as usize;
            for i in s..e {
                let edge = &mut self.skeleton_edges[i];
                edge.src = crate::NodeId(inv[edge.src.0 as usize]);
                edge.dst = crate::NodeId(inv[edge.dst.0 as usize]);
            }
        }
    }

    pub fn canonicalize_skeleton_edge_orientation(&mut self) {
        for edge in &mut self.skeleton_edges {
            if edge.src.0 > edge.dst.0 {
                std::mem::swap(&mut edge.src, &mut edge.dst);
            }
        }
    }

    pub fn move_root_to_zero(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }
        let r = self.root.idx();
        if r == 0 {
            return;
        }
        self.node_types.swap(0, r);
        self.skeleton_num_nodes.swap(0, r);
        if self.min_real_per_node.len() == n {
            self.min_real_per_node.swap(0, r);
        }

        let mut new_nm_offsets = vec![0u32; n + 1];
        let mut new_nm: Vec<NodeId> = Vec::with_capacity(self.node_mapping.len());
        let perm = |i: usize| -> usize {
            if i == 0 {
                r
            } else if i == r {
                0
            } else {
                i
            }
        };
        for i in 0..n {
            let src_i = perm(i);
            let s = self.node_mapping_offsets[src_i] as usize;
            let e = self.node_mapping_offsets[src_i + 1] as usize;
            new_nm.extend_from_slice(&self.node_mapping[s..e]);
            new_nm_offsets[i + 1] = new_nm.len() as u32;
        }
        self.node_mapping_offsets = new_nm_offsets;
        self.node_mapping = new_nm;

        let mut new_sk_offsets = vec![0u32; n + 1];
        let mut new_sk: Vec<SkeletonEdge> = Vec::with_capacity(self.skeleton_edges.len());
        for i in 0..n {
            let src_i = perm(i);
            let s = self.skeleton_offsets[src_i] as usize;
            let e = self.skeleton_offsets[src_i + 1] as usize;
            new_sk.extend_from_slice(&self.skeleton_edges[s..e]);
            new_sk_offsets[i + 1] = new_sk.len() as u32;
        }
        self.skeleton_offsets = new_sk_offsets;
        self.skeleton_edges = new_sk;

        let mut new_ch_offsets = vec![0u32; n + 1];
        let mut new_ch: Vec<TreeNodeId> = Vec::with_capacity(self.children.len());
        for i in 0..n {
            let src_i = perm(i);
            let s = self.children_offsets[src_i] as usize;
            let e = self.children_offsets[src_i + 1] as usize;
            new_ch.extend_from_slice(&self.children[s..e]);
            new_ch_offsets[i + 1] = new_ch.len() as u32;
        }
        self.children_offsets = new_ch_offsets;
        self.children = new_ch;

        self.node_parents.swap(0, r);
        for p in &mut self.node_parents {
            if !p.is_valid() {
                continue;
            }
            let pi = p.idx();
            if pi == 0 {
                *p = TreeNodeId(r as u32);
            } else if pi == r {
                *p = TreeNodeId(0);
            }
        }

        for c in &mut self.children {
            if !c.is_valid() {
                continue;
            }
            let ci = c.idx();
            if ci == 0 {
                *c = TreeNodeId(r as u32);
            } else if ci == r {
                *c = TreeNodeId(0);
            }
        }

        for e in &mut self.skeleton_edges {
            if !e.twin_tree_node.is_valid() {
                continue;
            }
            let ti = e.twin_tree_node.idx();
            if ti == 0 {
                e.twin_tree_node = TreeNodeId(r as u32);
            } else if ti == r {
                e.twin_tree_node = TreeNodeId(0);
            }
        }

        for tn in &mut self.edge_to_tree_node {
            if !tn.is_valid() {
                continue;
            }
            let ti = tn.idx();
            if ti == 0 {
                *tn = TreeNodeId(r as u32);
            } else if ti == r {
                *tn = TreeNodeId(0);
            }
        }

        self.root = TreeNodeId(0);
    }

    pub fn recompute_min_real_per_node(&mut self) {
        let n = self.len();
        self.min_real_per_node = vec![u32::MAX; n];
        for tn in 0..n {
            let s = self.skeleton_offsets[tn] as usize;
            let e = self.skeleton_offsets[tn + 1] as usize;
            let mut local_min = u32::MAX;
            for i in s..e {
                let re = self.skeleton_edges[i].real_edge;
                if re.is_valid() && re.0 < local_min {
                    local_min = re.0;
                }
            }
            self.min_real_per_node[tn] = local_min;
        }
    }

    pub fn canonicalize_root(&mut self) {
        if !CANONICALIZE_ROOT_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let n = self.len();
        if n == 0 {
            return;
        }

        let min_real: &[u32] = if self.min_real_per_node.len() == n {
            &self.min_real_per_node
        } else {
            self.recompute_min_real_per_node();
            &self.min_real_per_node
        };

        let mut new_root = 0usize;
        for tn in 1..n {
            if min_real[tn] < min_real[new_root]
                || (min_real[tn] == min_real[new_root] && tn < new_root)
            {
                new_root = tn;
            }
        }

        let new_root_id = TreeNodeId(new_root as u32);
        if self.root == new_root_id {
            return;
        }

        let mut adj_count = vec![0u32; n];
        for tn in 0..n {
            let p = self.node_parents[tn];
            if p.is_valid() && (p.idx()) != tn {
                adj_count[tn] += 1;
                adj_count[p.idx()] += 1;
            }
        }
        let mut adj_off = vec![0u32; n + 1];
        for i in 0..n {
            adj_off[i + 1] = adj_off[i] + adj_count[i];
        }
        let total = adj_off[n] as usize;
        let mut adj_flat = vec![TreeNodeId::INVALID; total];
        let mut cursor = adj_off[..n].to_vec();
        for tn in 0..n {
            let p = self.node_parents[tn];
            if p.is_valid() && (p.idx()) != tn {
                adj_flat[cursor[tn] as usize] = p;
                cursor[tn] += 1;
                adj_flat[cursor[p.idx()] as usize] = TreeNodeId(tn as u32);
                cursor[p.idx()] += 1;
            }
        }

        let mut new_parents: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n];
        let mut visited = vec![false; n];
        let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
        queue.push_back(new_root);
        visited[new_root] = true;
        while let Some(v) = queue.pop_front() {
            let s = adj_off[v] as usize;
            let e = adj_off[v + 1] as usize;
            for i in s..e {
                let u = adj_flat[i];
                if u.is_valid() && !visited[u.idx()] {
                    visited[u.idx()] = true;
                    new_parents[u.idx()] = TreeNodeId(v as u32);
                    queue.push_back(u.idx());
                }
            }
        }

        self.root = new_root_id;
        self.node_parents = new_parents;

        let mut child_count = vec![0u32; n];
        for tn in 0..n {
            let p = self.node_parents[tn];
            if p.is_valid() {
                child_count[p.idx()] += 1;
            }
        }
        let mut new_offsets = vec![0u32; n + 1];
        for i in 0..n {
            new_offsets[i + 1] = new_offsets[i] + child_count[i];
        }
        let total_children = new_offsets[n] as usize;
        let mut new_children = vec![TreeNodeId::INVALID; total_children];
        let mut write = new_offsets[..n].to_vec();
        for tn in 0..n {
            let p = self.node_parents[tn];
            if p.is_valid() {
                new_children[write[p.idx()] as usize] = TreeNodeId(tn as u32);
                write[p.idx()] += 1;
            }
        }
        self.children_offsets = new_offsets;
        self.children = new_children;

        self.canonicalize_skeleton_edge_order();
    }

    fn canonicalize_skeleton_edge_order(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }

        for _iter in 0..32 {
            let hashes = self.compute_canonical_hashes();

            let mut any_change = false;
            for tn in 0..n {
                let s = self.skeleton_offsets[tn] as usize;
                let e = self.skeleton_offsets[tn + 1] as usize;
                let k = e - s;
                if k <= 1 {
                    continue;
                }

                #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
                struct Key {
                    is_virtual: u8,
                    a: u64,
                    b: u32,
                }
                let mut keyed: Vec<(u32, Key)> = (0..k as u32)
                    .map(|i| {
                        let edge = &self.skeleton_edges[s + i as usize];
                        let key = if edge.real_edge.is_valid() {
                            Key {
                                is_virtual: 0,
                                a: edge.real_edge.0 as u64,
                                b: 0,
                            }
                        } else {
                            Key {
                                is_virtual: 1,
                                a: hashes[edge.twin_tree_node.idx()],
                                b: edge.twin_edge_idx,
                            }
                        };
                        (i, key)
                    })
                    .collect();
                keyed.sort_by_key(|(_, k)| *k);

                let mut new_pos = vec![0u32; k];
                for (new_idx, (old_idx, _)) in keyed.iter().enumerate() {
                    new_pos[*old_idx as usize] = new_idx as u32;
                }
                let is_identity = (0..k).all(|i| new_pos[i] == i as u32);
                if is_identity {
                    continue;
                }
                any_change = true;

                let new_edges: Vec<crate::SkeletonEdge> = keyed
                    .iter()
                    .map(|(old_idx, _)| self.skeleton_edges[s + *old_idx as usize])
                    .collect();
                self.skeleton_edges[s..(s + k)].copy_from_slice(&new_edges[..k]);

                for new_idx in 0..k {
                    let edge = self.skeleton_edges[s + new_idx];
                    if !edge.real_edge.is_valid() {
                        let twin_tn = edge.twin_tree_node.idx();
                        let twin_idx = edge.twin_edge_idx as usize;
                        let twin_so = self.skeleton_offsets[twin_tn] as usize;
                        self.skeleton_edges[twin_so + twin_idx].twin_edge_idx = new_idx as u32;
                    }
                }
            }
            if !any_change {
                break;
            }
        }
    }

    fn compute_canonical_hashes(&self) -> Vec<u64> {
        let n = self.len();
        let mut hashes: Vec<u64> = vec![0u64; n];

        for tn in 0..n {
            let s = self.skeleton_offsets[tn] as usize;
            let e = self.skeleton_offsets[tn + 1] as usize;
            let mut real_eids: Vec<u32> = Vec::new();
            let mut n_virt: u32 = 0;
            for i in s..e {
                let ed = &self.skeleton_edges[i];
                if ed.real_edge.is_valid() {
                    real_eids.push(ed.real_edge.0);
                } else {
                    n_virt += 1;
                }
            }
            real_eids.sort();
            let mut h: u64 = 0xcbf29ce484222325;
            let ty_byte = match self.node_types[tn] {
                crate::SpqrNodeType::S => 0u8,
                crate::SpqrNodeType::P => 1,
                crate::SpqrNodeType::R => 2,
            };
            h = mix64(h, ty_byte as u64);
            h = mix64(h, n_virt as u64);
            for r in &real_eids {
                h = mix64(h, *r as u64);
            }
            hashes[tn] = h;
        }

        for _iter in 0..16 {
            let mut new_hashes = hashes.clone();
            for tn in 0..n {
                let s = self.skeleton_offsets[tn] as usize;
                let e = self.skeleton_offsets[tn + 1] as usize;
                let mut neigh_hashes: Vec<u64> = Vec::new();
                for i in s..e {
                    let ed = &self.skeleton_edges[i];
                    if !ed.real_edge.is_valid() {
                        neigh_hashes.push(hashes[ed.twin_tree_node.idx()]);
                    }
                }
                neigh_hashes.sort();
                let mut h = hashes[tn];
                for nh in &neigh_hashes {
                    h = mix64(h, *nh);
                }
                new_hashes[tn] = h;
            }
            if new_hashes == hashes {
                break;
            }
            hashes = new_hashes;
        }
        hashes
    }

    pub fn normalize(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }

        let mut parent_set: Vec<u32> = (0..n as u32).collect();

        fn find(p: &mut [u32], x: u32) -> u32 {
            let mut r = x;
            while p[r as usize] != r {
                r = p[r as usize];
            }
            let mut cur = x;
            while p[cur as usize] != r {
                let next = p[cur as usize];
                p[cur as usize] = r;
                cur = next;
            }
            r
        }

        for i in 0..n {
            let p_id = self.node_parents[i];
            if !p_id.is_valid() || p_id.idx() == i {
                continue;
            }
            let pi = p_id.idx();

            let t = self.node_types[i];
            if t != SpqrNodeType::S && t != SpqrNodeType::P {
                continue;
            }
            if self.node_types[pi] != t {
                continue;
            }

            let i_num = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
            if i_num == 0 {
                continue;
            }
            let p_num = self.skeleton_offsets[pi + 1] - self.skeleton_offsets[pi];
            if p_num == 0 {
                continue;
            }

            let r_x = find(&mut parent_set, i as u32);
            let r_p = find(&mut parent_set, pi as u32);
            if r_x != r_p {
                parent_set[r_x as usize] = r_p;
            }
        }

        let mut absorbed_into: Vec<Option<TreeNodeId>> = vec![None; n];
        let mut any_absorbed = false;
        for i in 0..n {
            let r = find(&mut parent_set, i as u32) as usize;
            if r != i {
                absorbed_into[i] = Some(TreeNodeId(r as u32));
                any_absorbed = true;
            }
        }

        if !any_absorbed {
            return;
        }

        self.rebuild_with_merges(&absorbed_into);
    }

    fn rebuild_with_merges(&mut self, absorbed_into: &[Option<TreeNodeId>]) {
        let n = self.len();

        let mut old_to_new: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n];
        let mut new_idx = 0u32;
        for i in 0..n {
            if absorbed_into[i].is_none() {
                old_to_new[i] = TreeNodeId(new_idx);
                new_idx += 1;
            }
        }

        for i in 0..n {
            if let Some(parent) = absorbed_into[i] {
                old_to_new[i] = old_to_new[parent.idx()];
            }
        }

        let new_count = new_idx as usize;
        if new_count == n {
            return;
        }

        let mut new_local_maps: Vec<std::collections::HashMap<u32, u32>> =
            vec![std::collections::HashMap::new(); new_count];

        for i in 0..n {
            let new_i = old_to_new[i].idx();
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;

            for local_idx in 0..(map_end - map_start) {
                let orig_node = self.node_mapping[map_start + local_idx].0;
                let next_idx = new_local_maps[new_i].len() as u32;
                new_local_maps[new_i].entry(orig_node).or_insert(next_idx);
            }
        }

        let mut local_remaps: Vec<Vec<u32>> = Vec::with_capacity(n);
        for i in 0..n {
            let new_i = old_to_new[i].idx();
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            let num_local = map_end - map_start;

            let mut remap = vec![0u32; num_local];
            for local_idx in 0..num_local {
                let orig_node = self.node_mapping[map_start + local_idx].0;
                remap[local_idx] = new_local_maps[new_i][&orig_node];
            }
            local_remaps.push(remap);
        }

        let mut edge_counts: Vec<u32> = vec![0; new_count];
        let mut child_counts: Vec<u32> = vec![0; new_count];

        for i in 0..n {
            let new_i = old_to_new[i].idx();

            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;
            for ei in edge_start..edge_end {
                let e = &self.skeleton_edges[ei];
                if e.twin_tree_node.is_valid() {
                    let twin_new = old_to_new[e.twin_tree_node.idx()];
                    if twin_new == TreeNodeId(new_i as u32) {
                        continue;
                    }
                }
                edge_counts[new_i] += 1;
            }

            if absorbed_into[i].is_none() {
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

        let total_edges: usize = edge_counts.iter().map(|&x| x as usize).sum();
        let total_children: usize = child_counts.iter().map(|&x| x as usize).sum();

        let mut new_skeleton_offsets: Vec<u32> = Vec::with_capacity(new_count + 1);
        let mut new_children_offsets: Vec<u32> = Vec::with_capacity(new_count + 1);
        let mut new_mapping_offsets: Vec<u32> = Vec::with_capacity(new_count + 1);

        new_skeleton_offsets.push(0);
        new_children_offsets.push(0);
        new_mapping_offsets.push(0);

        for i in 0..new_count {
            new_skeleton_offsets.push(new_skeleton_offsets[i] + edge_counts[i]);
            new_children_offsets.push(new_children_offsets[i] + child_counts[i]);
            new_mapping_offsets.push(new_mapping_offsets[i] + new_local_maps[i].len() as u32);
        }

        let total_mapping = new_mapping_offsets[new_count] as usize;

        let mut new_node_types: Vec<SpqrNodeType> = vec![SpqrNodeType::R; new_count];
        let mut new_node_parents: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; new_count];
        let mut new_skeleton_num_nodes: Vec<u32> = vec![0; new_count];

        let mut new_skeleton_edges: Vec<SkeletonEdge> = vec![SkeletonEdge::default(); total_edges];
        let mut new_children: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; total_children];
        let mut new_node_mapping: Vec<NodeId> = vec![NodeId::INVALID; total_mapping];

        let mut edge_write_pos: Vec<u32> = new_skeleton_offsets[..new_count].to_vec();
        let mut child_write_pos: Vec<u32> = new_children_offsets[..new_count].to_vec();

        let mut old_to_new_edge_idx: std::collections::HashMap<(u32, u32), u32> =
            std::collections::HashMap::new();
        {
            let mut counters: Vec<u32> = vec![0u32; new_count];
            for i in 0..n {
                let new_i = old_to_new[i].idx();
                let edge_start = self.skeleton_offsets[i] as usize;
                let edge_end = self.skeleton_offsets[i + 1] as usize;
                for ei in edge_start..edge_end {
                    let e = &self.skeleton_edges[ei];
                    if e.twin_tree_node.is_valid() {
                        let twin_new = old_to_new[e.twin_tree_node.idx()];
                        if twin_new == TreeNodeId(new_i as u32) {
                            continue;
                        }
                    }
                    old_to_new_edge_idx
                        .insert((i as u32, (ei - edge_start) as u32), counters[new_i]);
                    counters[new_i] += 1;
                }
            }
        }

        for new_i in 0..new_count {
            let map = &new_local_maps[new_i];
            let map_start = new_mapping_offsets[new_i] as usize;

            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(_, &idx)| idx);

            for (orig_node, &new_local) in entries {
                new_node_mapping[map_start + new_local as usize] = NodeId(*orig_node);
            }

            new_skeleton_num_nodes[new_i] = map.len() as u32;
        }

        for i in 0..n {
            if absorbed_into[i].is_some() {
                continue;
            }

            let new_i = old_to_new[i].idx();

            new_node_types[new_i] = self.node_types[i];
            let parent = self.node_parents[i];
            new_node_parents[new_i] = if parent.is_valid() {
                old_to_new[parent.idx()]
            } else {
                TreeNodeId::INVALID
            };

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

        for i in 0..n {
            let new_i = old_to_new[i].idx();
            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;
            let remap = &local_remaps[i];

            for ei in edge_start..edge_end {
                let mut e = self.skeleton_edges[ei];

                if e.twin_tree_node.is_valid() {
                    let twin_new = old_to_new[e.twin_tree_node.idx()];
                    if twin_new == TreeNodeId(new_i as u32) {
                        continue;
                    }
                    e.twin_tree_node = twin_new;

                    let twin_old_tid = self.skeleton_edges[ei].twin_tree_node.0;
                    let twin_old_eidx = self.skeleton_edges[ei].twin_edge_idx;
                    if let Some(&new_eidx) = old_to_new_edge_idx.get(&(twin_old_tid, twin_old_eidx))
                    {
                        e.twin_edge_idx = new_eidx;
                    }
                }

                let old_src = e.src.0 as usize;
                let old_dst = e.dst.0 as usize;
                if old_src < remap.len() && old_dst < remap.len() {
                    e.src = NodeId(remap[old_src]);
                    e.dst = NodeId(remap[old_dst]);
                }

                let pos = edge_write_pos[new_i] as usize;
                new_skeleton_edges[pos] = e;
                edge_write_pos[new_i] += 1;
            }
        }

        for tn in &mut self.edge_to_tree_node {
            if tn.is_valid() {
                *tn = old_to_new[tn.idx()];
            }
        }

        if self.root.is_valid() {
            self.root = old_to_new[self.root.idx()];
        }

        self.node_types = new_node_types;
        self.node_parents = new_node_parents;
        self.skeleton_offsets = new_skeleton_offsets;
        self.skeleton_edges = new_skeleton_edges;
        self.node_mapping_offsets = new_mapping_offsets;
        self.node_mapping = new_node_mapping;
        self.skeleton_num_nodes = new_skeleton_num_nodes;
        self.children_offsets = new_children_offsets;
        self.children = new_children;

        self.recompute_min_real_per_node();
    }

    pub fn compact(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }

        let mut is_alive: Vec<bool> = vec![false; n];
        for i in 0..n {
            let num_edges = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
            is_alive[i] = num_edges > 0;
        }

        let mut old_to_new: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n];
        let mut new_idx = 0u32;
        for i in 0..n {
            if is_alive[i] {
                old_to_new[i] = TreeNodeId(new_idx);
                new_idx += 1;
            }
        }

        if new_idx as usize == n {
            return;
        }

        let new_count = new_idx as usize;

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

            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            new_node_mapping.extend_from_slice(&self.node_mapping[map_start..map_end]);
            new_node_mapping_offsets.push(new_node_mapping.len() as u32);

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

        for tn in &mut self.edge_to_tree_node {
            if tn.is_valid() && old_to_new[tn.idx()].is_valid() {
                *tn = old_to_new[tn.idx()];
            } else if tn.is_valid() {
                *tn = TreeNodeId::INVALID;
            }
        }

        if self.root.is_valid() {
            self.root = old_to_new[self.root.idx()];
        }

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
