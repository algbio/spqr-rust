//! # SPQR Tree, triconnectivity decomposition
//!
//! Computes SPQR trees of biconnected multigraphs using a DFS based
//! triconnected components algorithm (Hopcroft-Tarjan with corrections
//! by Gutwenger & Mutzel, 2001).
//!

#![forbid(unsafe_code)]
#![allow(clippy::needless_range_loop)]

pub mod verify;

use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreeNodeId(pub u32);

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

#[derive(Clone, Debug)]
pub struct SkeletonEdge {
    pub src: NodeId,
    pub dst: NodeId,
    pub real_edge: EdgeId,
    pub virtual_id: u32,
    pub twin_tree_node: TreeNodeId,
    pub twin_edge_idx: u32,
}

#[derive(Clone, Debug)]
pub struct Skeleton {
    pub num_nodes: u32,
    pub edges: Vec<SkeletonEdge>,
    pub node_to_original: Vec<NodeId>,
}
impl Skeleton {
    fn new() -> Self {
        Skeleton {
            num_nodes: 0,
            edges: Vec::new(),
            node_to_original: Vec::new(),
        }
    }
    pub fn poles(&self) -> (NodeId, NodeId) {
        (self.node_to_original[0], self.node_to_original[1])
    }
    pub fn num_edges(&self) -> usize {
        self.edges.len()
    }
    pub fn real_edges(&self) -> impl Iterator<Item = (usize, &SkeletonEdge)> {
        self.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.real_edge.is_valid())
    }
    pub fn virtual_edges(&self) -> impl Iterator<Item = (usize, &SkeletonEdge)> {
        self.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.twin_tree_node.is_valid())
    }
}

#[derive(Clone, Debug)]
pub struct SpqrTreeNode {
    pub node_type: SpqrNodeType,
    pub skeleton: Skeleton,
    pub parent: TreeNodeId,
    pub children: Vec<TreeNodeId>,
}

pub struct SpqrTree {
    pub nodes: Vec<SpqrTreeNode>,
    pub root: TreeNodeId,
    edge_to_tree_node: Vec<TreeNodeId>,
}
impl SpqrTree {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    #[inline]
    pub fn node(&self, id: TreeNodeId) -> &SpqrTreeNode {
        &self.nodes[id.idx()]
    }
    pub fn tree_node_of_edge(&self, eid: EdgeId) -> TreeNodeId {
        self.edge_to_tree_node[eid.idx()]
    }
    pub fn count_by_type(&self) -> (usize, usize, usize) {
        let (mut s, mut p, mut r) = (0, 0, 0);
        for n in &self.nodes {
            match n.node_type {
                SpqrNodeType::S => s += 1,
                SpqrNodeType::P => p += 1,
                SpqrNodeType::R => r += 1,
            }
        }
        (s, p, r)
    }
    pub fn iter(&self) -> impl Iterator<Item = (TreeNodeId, &SpqrTreeNode)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (TreeNodeId(i as u32), n))
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
        return SpqrTree {
            nodes: Vec::new(),
            root: TreeNodeId::INVALID,
            edge_to_tree_node: vec![TreeNodeId::INVALID; m],
        };
    }
    if n == 1 {
        return SpqrTree {
            nodes: Vec::new(),
            root: TreeNodeId::INVALID,
            edge_to_tree_node: vec![TreeNodeId::INVALID; m],
        };
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
        let mut sk = Skeleton::new();
        sk.num_nodes = 2;
        sk.node_to_original = vec![e.src, e.dst];
        sk.edges.push(SkeletonEdge {
            src: NodeId(0),
            dst: NodeId(1),
            real_edge: EdgeId(eid_real as u32),
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        });
        let mut ettn = vec![TreeNodeId::INVALID; m];
        ettn[eid_real] = TreeNodeId(0);
        return SpqrTree {
            nodes: vec![SpqrTreeNode {
                node_type: SpqrNodeType::P,
                skeleton: sk,
                parent: TreeNodeId::INVALID,
                children: Vec::new(),
            }],
            root: TreeNodeId(0),
            edge_to_tree_node: ettn,
        };
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
    let mut sk = Skeleton::new();
    sk.num_nodes = 2;
    sk.node_to_original = vec![NodeId(0), NodeId(1)];
    let mut ettn = vec![TreeNodeId::INVALID; m];
    for i in 0..m {
        if is_self_loop[i] {
            continue;
        }
        let e = graph.edge(EdgeId(i as u32));
        sk.edges.push(SkeletonEdge {
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
        ettn[i] = TreeNodeId(0);
    }
    SpqrTree {
        nodes: vec![SpqrTreeNode {
            node_type: SpqrNodeType::P,
            skeleton: sk,
            parent: TreeNodeId::INVALID,
            children: Vec::new(),
        }],
        root: TreeNodeId(0),
        edge_to_tree_node: ettn,
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
    let mut bkt: Vec<Vec<u32>> = vec![Vec::new(); (maxb + 1) as usize];
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
            bkt[phi as usize].push(i as u32);
        }
    }
    let mut oadj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for phi in 1..=(maxb as usize) {
        for &ei in &bkt[phi] {
            oadj[esrc[ei as usize] as usize].push(ei);
        }
    }

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
            let v = fr.v;
            if fr.idx >= oadj[v as usize].len() {
                pfs.pop();
                continue;
            }
            let ei = oadj[v as usize][fr.idx] as usize;
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
                hp_init[w as usize].push((newnum[v as usize], ei as u32));
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
        for &ei in &oadj[v] {
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
    let mut tree_nodes: Vec<SpqrTreeNode> = Vec::with_capacity(components.len());
    let mut edge_to_tree_node = vec![TreeNodeId::INVALID; m];
    for comp in components {
        let nt = classify_component(comp);
        let tid = TreeNodeId(tree_nodes.len() as u32);
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
            if is_real {
                edge_to_tree_node[edge.eid as usize] = tid;
            }
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
        tree_nodes.push(SpqrTreeNode {
            node_type: nt,
            skeleton: Skeleton {
                num_nodes: n2o.len() as u32,
                edges: se,
                node_to_original: n2o,
            },
            parent: TreeNodeId::INVALID,
            children: Vec::new(),
        });
    }
    let num_virtual = next_virtual.saturating_sub(base) as usize;
    let mut first: Vec<Option<(usize, u32)>> = vec![None; num_virtual];
    let mut pairs: Vec<(usize, u32, usize, u32)> = Vec::new();
    for ti in 0..tree_nodes.len() {
        for ei in 0..tree_nodes[ti].skeleton.edges.len() {
            let vid = tree_nodes[ti].skeleton.edges[ei].virtual_id;
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
            let edge = &mut tree_nodes[ti].skeleton.edges[ei as usize];
            edge.virtual_id = INVALID;
            edge.twin_tree_node = TreeNodeId::INVALID;
            edge.twin_edge_idx = INVALID;
        }
    }
    let mut tree_adj: Vec<Vec<TreeNodeId>> = vec![Vec::new(); tree_nodes.len()];
    for (a, ea, b, eb) in pairs {
        assert!(a != b);
        let ta = TreeNodeId(a as u32);
        let t_b = TreeNodeId(b as u32);
        if a < b {
            let (left, right) = tree_nodes.split_at_mut(b);
            left[a].skeleton.edges[ea as usize].twin_tree_node = t_b;
            left[a].skeleton.edges[ea as usize].twin_edge_idx = eb;
            right[0].skeleton.edges[eb as usize].twin_tree_node = ta;
            right[0].skeleton.edges[eb as usize].twin_edge_idx = ea;
        } else {
            let (left, right) = tree_nodes.split_at_mut(a);
            left[b].skeleton.edges[eb as usize].twin_tree_node = ta;
            left[b].skeleton.edges[eb as usize].twin_edge_idx = ea;
            right[0].skeleton.edges[ea as usize].twin_tree_node = t_b;
            right[0].skeleton.edges[ea as usize].twin_edge_idx = eb;
        }
        tree_adj[a].push(t_b);
        tree_adj[b].push(ta);
    }
    let root = if tree_nodes.is_empty() {
        TreeNodeId::INVALID
    } else {
        TreeNodeId(0)
    };
    if root.is_valid() {
        let mut par = vec![TreeNodeId::INVALID; tree_nodes.len()];
        let mut vis = vec![false; tree_nodes.len()];
        let mut st = vec![root];
        vis[root.idx()] = true;
        while let Some(v) = st.pop() {
            for &u in &tree_adj[v.idx()] {
                if vis[u.idx()] {
                    continue;
                }
                vis[u.idx()] = true;
                par[u.idx()] = v;
                st.push(u);
            }
        }
        if !vis.iter().all(|&b| b) {
            for (i, &v) in vis.iter().enumerate() {
                if !v {
                    par[i] = root;
                }
            }
        }
        for i in 0..tree_nodes.len() {
            tree_nodes[i].parent = par[i];
            tree_nodes[i].children.clear();
        }
        for i in 0..tree_nodes.len() {
            let p = par[i];
            if p.is_valid() {
                tree_nodes[p.idx()].children.push(TreeNodeId(i as u32));
            }
        }
    }
    SpqrTree {
        nodes: tree_nodes,
        root,
        edge_to_tree_node,
    }
}

impl SpqrTree {
    pub fn normalize(&mut self) {
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..self.nodes.len() {
                if self.nodes[i].skeleton.edges.is_empty() {
                    continue;
                }
                let t = self.nodes[i].node_type;
                if t != SpqrNodeType::S && t != SpqrNodeType::P {
                    continue;
                }
                let parent = TreeNodeId(i as u32);
                let children = self.nodes[i].children.clone();
                for child in children {
                    if !child.is_valid() || child.idx() >= self.nodes.len() {
                        continue;
                    }
                    if self.nodes[child.idx()].skeleton.edges.is_empty() {
                        continue;
                    }
                    if self.nodes[child.idx()].node_type == t {
                        self.merge_into_parent(parent, child);
                        changed = true;
                    }
                }
            }
        }
    }

    fn swap_remove_skeleton_edge(&mut self, tid: TreeNodeId, idx: usize) -> SkeletonEdge {
        let (swapped_twin, removed) = {
            let node = &mut self.nodes[tid.idx()];
            let last = node.skeleton.edges.len() - 1;
            let mut swapped_twin: Option<(TreeNodeId, u32)> = None;
            if idx != last {
                node.skeleton.edges.swap(idx, last);
                let e = &node.skeleton.edges[idx];
                if e.virtual_id != INVALID && e.twin_tree_node.is_valid() {
                    swapped_twin = Some((e.twin_tree_node, e.twin_edge_idx));
                }
            }
            let removed = node.skeleton.edges.pop().expect("swap_remove on empty");
            (swapped_twin, removed)
        };
        if let Some((tt, tei)) = swapped_twin {
            self.nodes[tt.idx()].skeleton.edges[tei as usize].twin_edge_idx = idx as u32;
        }
        removed
    }

    fn merge_into_parent(&mut self, parent: TreeNodeId, child: TreeNodeId) {
        let p_edge_idx = match self.nodes[parent.idx()]
            .skeleton
            .edges
            .iter()
            .position(|e| e.virtual_id != INVALID && e.twin_tree_node == child)
        {
            Some(x) => x,
            None => return,
        };
        let c_edge_idx = match self.nodes[child.idx()]
            .skeleton
            .edges
            .iter()
            .position(|e| e.virtual_id != INVALID && e.twin_tree_node == parent)
        {
            Some(x) => x,
            None => return,
        };
        let _ = self.swap_remove_skeleton_edge(parent, p_edge_idx);
        let _ = self.swap_remove_skeleton_edge(child, c_edge_idx);
        self.nodes[parent.idx()].children.retain(|&c| c != child);
        let child_edges = std::mem::take(&mut self.nodes[child.idx()].skeleton.edges);
        let child_n2o = std::mem::take(&mut self.nodes[child.idx()].skeleton.node_to_original);
        let child_children = std::mem::take(&mut self.nodes[child.idx()].children);
        let mut orig_to_parent: HashMap<u32, u32> = HashMap::new();
        for (i, &orig) in self.nodes[parent.idx()]
            .skeleton
            .node_to_original
            .iter()
            .enumerate()
        {
            orig_to_parent.insert(orig.0, i as u32);
        }
        let mut child_local_to_parent: Vec<u32> = Vec::with_capacity(child_n2o.len());
        {
            let p_n2o = &mut self.nodes[parent.idx()].skeleton.node_to_original;
            for &orig in &child_n2o {
                let pid = if let Some(&pid) = orig_to_parent.get(&orig.0) {
                    pid
                } else {
                    let pid = p_n2o.len() as u32;
                    p_n2o.push(orig);
                    orig_to_parent.insert(orig.0, pid);
                    pid
                };
                child_local_to_parent.push(pid);
            }
            self.nodes[parent.idx()].skeleton.num_nodes = p_n2o.len() as u32;
        }
        for mut e in child_edges {
            e.src = NodeId(child_local_to_parent[e.src.idx()]);
            e.dst = NodeId(child_local_to_parent[e.dst.idx()]);
            if e.real_edge.is_valid() {
                let eid = e.real_edge.idx();
                if eid < self.edge_to_tree_node.len() {
                    self.edge_to_tree_node[eid] = parent;
                }
            }
            let is_virtual = e.virtual_id != INVALID;
            let twin_tid = e.twin_tree_node;
            let twin_ei = e.twin_edge_idx;
            let new_idx = {
                let p = &mut self.nodes[parent.idx()].skeleton.edges;
                p.push(e);
                (p.len() - 1) as u32
            };
            if is_virtual && twin_tid.is_valid() {
                self.nodes[twin_tid.idx()].skeleton.edges[twin_ei as usize].twin_tree_node = parent;
                self.nodes[twin_tid.idx()].skeleton.edges[twin_ei as usize].twin_edge_idx = new_idx;
            }
        }
        for g in child_children {
            self.nodes[g.idx()].parent = parent;
            self.nodes[parent.idx()].children.push(g);
        }
        self.nodes[child.idx()].parent = TreeNodeId::INVALID;
        self.nodes[child.idx()].skeleton.num_nodes = 0;
    }

    pub fn compact(&mut self) {
        let mut id_map = vec![TreeNodeId::INVALID; self.nodes.len()];
        let mut new_n: Vec<SpqrTreeNode> = Vec::new();
        for (i, nd) in self.nodes.iter().enumerate() {
            if !nd.skeleton.edges.is_empty() {
                id_map[i] = TreeNodeId(new_n.len() as u32);
                new_n.push(nd.clone());
            }
        }
        for nd in &mut new_n {
            if nd.parent.is_valid() {
                nd.parent = id_map[nd.parent.idx()];
            }
            nd.children = nd
                .children
                .iter()
                .filter_map(|c| {
                    let m = id_map[c.idx()];
                    if m.is_valid() {
                        Some(m)
                    } else {
                        None
                    }
                })
                .collect();
            for e in &mut nd.skeleton.edges {
                if e.twin_tree_node.is_valid() {
                    e.twin_tree_node = id_map[e.twin_tree_node.idx()];
                }
            }
        }
        for tn in &mut self.edge_to_tree_node {
            if tn.is_valid() {
                *tn = id_map[tn.idx()];
            }
        }
        self.root = if self.root.is_valid() {
            id_map[self.root.idx()]
        } else {
            TreeNodeId::INVALID
        };
        self.nodes = new_n;
    }
}

impl fmt::Display for SpqrTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "SPQR Tree ({} nodes):", self.nodes.len())?;
        for (i, nd) in self.nodes.iter().enumerate() {
            let t = match nd.node_type {
                SpqrNodeType::S => "S",
                SpqrNodeType::P => "P",
                SpqrNodeType::R => "R",
            };
            writeln!(
                f,
                "  [{}] {}: {} edges, {} children, poles={:?}",
                i,
                t,
                nd.skeleton.num_edges(),
                nd.children.len(),
                if nd.skeleton.node_to_original.len() >= 2 {
                    nd.skeleton.poles()
                } else {
                    (NodeId::INVALID, NodeId::INVALID)
                }
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
        assert_eq!(t.nodes[0].node_type, SpqrNodeType::P);
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
        assert_eq!(res.tree.nodes[0].node_type, SpqrNodeType::P);
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
}
