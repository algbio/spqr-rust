use crate::sp_compress::types::{
    child_as_edge, make_child_edge, make_child_macro, ChildRef, CompressionResult, CoreEdge,
    SpNode, SpTree, SP_KIND_PARALLEL, SP_KIND_SERIES,
};
use crate::{EdgeId, NodeId};

#[derive(Clone, Copy, Default)]
struct CoreItem {
    u: u32,
    v: u32,
    child: ChildRef,
    first_edge: u32,
    child_sort_kind: u8,
}

fn bucket_sort_core_items_by_pair(items: &mut [CoreItem]) {
    items.sort_unstable_by_key(|item| ((item.u as u64) << 32) | item.v as u64);
}

fn bucket_sort_core_edges_by_pair(edges: &mut [CoreEdge]) {
    edges.sort_unstable_by_key(|e| ((e.u as u64) << 32) | e.v as u64);
}

enum IncidentMap {
    Dense {
        inc0: Vec<u32>,
        inc1: Vec<u32>,
        touched: Vec<u32>,
    },
    Sparse(SparseIncidentMap),
}

impl IncidentMap {
    fn new(n_nodes: usize, n_edges: usize) -> Self {
        const MIN_DENSE_NODES: usize = 4096;
        const MAX_DENSE_EDGE_OVERHEAD: usize = 3;

        let dense_limit = n_edges.saturating_mul(MAX_DENSE_EDGE_OVERHEAD);
        if n_nodes <= MIN_DENSE_NODES || n_nodes <= dense_limit {
            return Self::Dense {
                inc0: vec![u32::MAX; n_nodes],
                inc1: vec![u32::MAX; n_nodes],
                touched: Vec::new(),
            };
        }

        Self::Sparse(SparseIncidentMap::new(n_edges))
    }

    #[inline]
    fn add(&mut self, v: u32, edge: u32) -> Option<()> {
        match self {
            Self::Dense {
                inc0,
                inc1,
                touched,
            } => {
                let idx = v as usize;
                if inc0[idx] == u32::MAX {
                    inc0[idx] = edge;
                    touched.push(v);
                    Some(())
                } else if inc1[idx] == u32::MAX {
                    inc1[idx] = edge;
                    Some(())
                } else {
                    None
                }
            }
            Self::Sparse(map) => map.add(v, edge),
        }
    }

    #[inline]
    fn other(&self, v: u32, edge: u32) -> Option<u32> {
        match self {
            Self::Dense { inc0, inc1, .. } => {
                other_contractible_edge_dense(v as usize, edge, inc0, inc1)
            }
            Self::Sparse(map) => map.other(v, edge),
        }
    }

    fn all_touched_have_two_incidents(&self) -> bool {
        match self {
            Self::Dense {
                inc0,
                inc1,
                touched,
            } => touched
                .iter()
                .all(|&v| inc0[v as usize] != u32::MAX && inc1[v as usize] != u32::MAX),
            Self::Sparse(map) => map.all_touched_have_two_incidents(),
        }
    }
}

struct SparseIncidentMap {
    keys: Vec<u32>,
    inc0: Vec<u32>,
    inc1: Vec<u32>,
    touched: Vec<u32>,
    len: usize,
    mask: usize,
}

impl SparseIncidentMap {
    fn new(n_edges: usize) -> Self {
        let mut cap = 16usize;
        let target = n_edges.saturating_mul(2).clamp(1, 1024);
        while cap < target {
            cap <<= 1;
        }
        Self {
            keys: vec![u32::MAX; cap],
            inc0: vec![u32::MAX; cap],
            inc1: vec![u32::MAX; cap],
            touched: Vec::new(),
            len: 0,
            mask: cap - 1,
        }
    }

    #[inline(always)]
    fn hash_u32(mut x: u32) -> usize {
        x ^= x >> 16;
        x = x.wrapping_mul(0x7feb_352d);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846c_a68b);
        x ^= x >> 16;
        x as usize
    }

    fn grow(&mut self) {
        let new_cap = self.keys.len() * 2;
        let old_keys = std::mem::replace(&mut self.keys, vec![u32::MAX; new_cap]);
        let old_inc0 = std::mem::replace(&mut self.inc0, vec![u32::MAX; new_cap]);
        let old_inc1 = std::mem::replace(&mut self.inc1, vec![u32::MAX; new_cap]);
        self.mask = new_cap - 1;

        for (i, &key) in old_keys.iter().enumerate() {
            if key == u32::MAX {
                continue;
            }
            let slot = self.find_slot_for_insert(key);
            self.keys[slot] = key;
            self.inc0[slot] = old_inc0[i];
            self.inc1[slot] = old_inc1[i];
        }
    }

    #[inline]
    fn find_slot_for_insert(&self, key: u32) -> usize {
        let mut slot = Self::hash_u32(key) & self.mask;
        while self.keys[slot] != u32::MAX {
            slot = (slot + 1) & self.mask;
        }
        slot
    }

    #[inline]
    fn find_existing_slot(&self, key: u32) -> Option<usize> {
        let mut slot = Self::hash_u32(key) & self.mask;
        loop {
            let stored = self.keys[slot];
            if stored == key {
                return Some(slot);
            }
            if stored == u32::MAX {
                return None;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    fn slot_or_insert(&mut self, key: u32) -> usize {
        if let Some(slot) = self.find_existing_slot(key) {
            return slot;
        }
        if (self.len + 1) * 2 > self.keys.len() {
            self.grow();
        }
        let slot = self.find_slot_for_insert(key);
        self.keys[slot] = key;
        self.len += 1;
        self.touched.push(key);
        slot
    }

    fn add(&mut self, v: u32, edge: u32) -> Option<()> {
        let slot = self.slot_or_insert(v);
        if self.inc0[slot] == u32::MAX {
            self.inc0[slot] = edge;
            Some(())
        } else if self.inc1[slot] == u32::MAX {
            self.inc1[slot] = edge;
            Some(())
        } else {
            None
        }
    }

    fn other(&self, v: u32, edge: u32) -> Option<u32> {
        let slot = self.find_existing_slot(v)?;
        let a = self.inc0[slot];
        let b = self.inc1[slot];
        if a == edge && b != u32::MAX {
            Some(b)
        } else if b == edge && a != u32::MAX {
            Some(a)
        } else {
            None
        }
    }

    fn all_touched_have_two_incidents(&self) -> bool {
        self.touched.iter().all(|&v| {
            let slot = self
                .find_existing_slot(v)
                .expect("touched sparse incident node missing");
            self.inc0[slot] != u32::MAX && self.inc1[slot] != u32::MAX
        })
    }
}

trait DirectEdges {
    fn len(&self) -> usize;
    fn endpoint(&self, i: usize) -> (u32, u32);
    fn original_edge_id(&self, i: usize) -> EdgeId;
}

struct IndexedEdges<'a> {
    src: &'a [u32],
    dst: &'a [u32],
}

impl DirectEdges for IndexedEdges<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.src.len()
    }

    #[inline]
    fn endpoint(&self, i: usize) -> (u32, u32) {
        (self.src[i], self.dst[i])
    }

    #[inline]
    fn original_edge_id(&self, i: usize) -> EdgeId {
        EdgeId(i as u32)
    }
}

pub(crate) fn try_compress_degree2_direct_indexed(
    n_nodes: u32,
    src: &[u32],
    dst: &[u32],
    contractible: &[u8],
) -> Option<CompressionResult> {
    if src.len() != dst.len() {
        return None;
    }
    try_compress_degree2_direct_impl(n_nodes, IndexedEdges { src, dst }, contractible)
}

fn try_compress_degree2_direct_impl<E: DirectEdges>(
    n_nodes: u32,
    input_edges: E,
    contractible: &[u8],
) -> Option<CompressionResult> {
    let n = n_nodes as usize;
    if contractible.len() < n {
        return None;
    }
    if input_edges.len() > u32::MAX as usize {
        return None;
    }

    let mut tree = SpTree::default();
    tree.stats.input_nodes = n_nodes;
    tree.stats.input_edges = input_edges.len() as u32;

    let mut incidents = IncidentMap::new(n, input_edges.len());
    for i in 0..input_edges.len() {
        let (u_raw, v_raw) = input_edges.endpoint(i);
        let u = u_raw as usize;
        let v = v_raw as usize;
        if u >= n || v >= n {
            return None;
        }
        if u == v {
            if contractible[u] != 0 {
                return None;
            }
        } else {
            if contractible[u] != 0 {
                incidents.add(u_raw, i as u32)?;
            }
            if contractible[v] != 0 {
                incidents.add(v_raw, i as u32)?;
            }
        }
    }
    if !incidents.all_touched_have_two_incidents() {
        return None;
    }
    let mut visited_edges = vec![0u64; input_edges.len().div_ceil(64)];
    let mut visited_count = 0usize;
    let mut core_items: Vec<CoreItem> = Vec::new();
    let mut path_children: Vec<ChildRef> = Vec::with_capacity(64);

    for edge_idx in 0..input_edges.len() {
        if edge_marked(edge_idx, &visited_edges) {
            continue;
        }
        let (u, v) = input_edges.endpoint(edge_idx);
        if u == v {
            mark_edge(edge_idx, &mut visited_edges);
            visited_count += 1;
            let original_edge_id = input_edges.original_edge_id(edge_idx);
            core_items.push(CoreItem {
                u,
                v,
                child: make_child_edge(original_edge_id),
                first_edge: original_edge_id.0,
                child_sort_kind: 0,
            });
            continue;
        }

        let cu = contractible[u as usize] != 0;
        let cv = contractible[v as usize] != 0;
        if cu && cv {
            continue;
        }

        let start = if !cu { u } else { v };
        let mut cur = start;
        let mut next_edge = edge_idx as u32;
        path_children.clear();
        loop {
            let ei = next_edge as usize;
            if edge_marked(ei, &visited_edges) {
                return None;
            }
            mark_edge(ei, &mut visited_edges);
            visited_count += 1;
            path_children.push(make_child_edge(input_edges.original_edge_id(ei)));

            let (ie_u, ie_v) = input_edges.endpoint(ei);
            let next = if ie_u == cur {
                ie_v
            } else if ie_v == cur {
                ie_u
            } else {
                return None;
            };

            if contractible[next as usize] == 0 {
                let (child, first_edge, child_sort_kind) =
                    make_path_child(&mut tree, start, next, &mut path_children);
                let (a, b) = ordered_pair(start, next);
                core_items.push(CoreItem {
                    u: a,
                    v: b,
                    child,
                    first_edge,
                    child_sort_kind,
                });
                break;
            }

            let found = incidents.other(next, next_edge)?;
            cur = next;
            next_edge = found;
        }
    }

    if visited_count != input_edges.len() {
        return None;
    }
    bucket_sort_core_items_by_pair(&mut core_items);

    let mut grouped: Vec<CoreEdge> = Vec::with_capacity(core_items.len());
    let mut i = 0;
    while i < core_items.len() {
        let u = core_items[i].u;
        let v = core_items[i].v;
        let mut j = i + 1;
        while j < core_items.len() && core_items[j].u == u && core_items[j].v == v {
            j += 1;
        }
        if j - i >= 2 {
            core_items[i..j]
                .sort_unstable_by_key(|item| (item.child_sort_kind, item.first_edge, item.child));
        }
        if u != v && j - i >= 2 {
            let child = push_parallel_macro(&mut tree, u, v, &core_items[i..j]);
            grouped.push(CoreEdge { u, v, child });
            tree.stats.parallel_reductions += (j - i - 1) as u32;
        } else {
            for item in &core_items[i..j] {
                grouped.push(CoreEdge {
                    u: item.u,
                    v: item.v,
                    child: item.child,
                });
            }
        }
        i = j;
    }
    grouped = fbrq_contract_residual_degree2(&mut tree, grouped, n, false);

    let mut core_node_bits = vec![0u64; n.div_ceil(64)];
    for edge in &grouped {
        let u_idx = edge.u as usize;
        core_node_bits[u_idx >> 6] |= 1u64 << (u_idx & 63);
        if edge.u != edge.v {
            let v_idx = edge.v as usize;
            core_node_bits[v_idx >> 6] |= 1u64 << (v_idx & 63);
        }
    }
    tree.core_nodes
        .reserve(grouped.len().saturating_mul(2).min(n));
    for (word_idx, mut word) in core_node_bits.into_iter().enumerate() {
        while word != 0 {
            let bit = word.trailing_zeros() as usize;
            let node = word_idx * 64 + bit;
            if node < n {
                tree.core_nodes.push(NodeId(node as u32));
            }
            word &= word - 1;
        }
    }
    tree.core_edges = grouped;
    tree.stats.iterations = 1;
    tree.stats.fully_sp_reducible =
        if tree.core_edges.len() == 1 && tree.core_edges[0].u != tree.core_edges[0].v {
            1
        } else {
            0
        };
    tree.update_stats();

    Some(CompressionResult {
        tree,
        success: true,
        error_message: None,
    })
}

fn fbrq_contract_residual_degree2(
    tree: &mut SpTree,
    edges: Vec<CoreEdge>,
    n: usize,
    group_parallel: bool,
) -> Vec<CoreEdge> {
    if edges.len() <= 1 || n == 0 {
        return edges;
    }

    let mut degree = vec![0u8; n];
    for e in &edges {
        if e.u == e.v {
            continue;
        }
        let u = e.u as usize;
        let v = e.v as usize;
        if u >= n || v >= n {
            return edges;
        }
        degree[u] = degree[u].saturating_add(1).min(3);
        degree[v] = degree[v].saturating_add(1).min(3);
    }
    if !degree.contains(&2) {
        return maybe_fbrq_group_parallel(tree, edges, group_parallel);
    }

    let mut incidents = IncidentMap::new(n, edges.len());
    for (eid, e) in edges.iter().enumerate() {
        if e.u == e.v {
            continue;
        }
        if degree[e.u as usize] == 2 && incidents.add(e.u, eid as u32).is_none() {
            return edges;
        }
        if degree[e.v as usize] == 2 && incidents.add(e.v, eid as u32).is_none() {
            return edges;
        }
    }
    if !incidents.all_touched_have_two_incidents() {
        return edges;
    }

    let mut visited = vec![0u64; edges.len().div_ceil(64)];
    let mut quotient: Vec<CoreEdge> = Vec::with_capacity(edges.len());
    let mut path: Vec<usize> = Vec::with_capacity(64);
    let mut chain_children: Vec<ChildRef> = Vec::with_capacity(64);

    for start_eid in 0..edges.len() {
        if edge_marked(start_eid, &visited) {
            continue;
        }
        let e = edges[start_eid];
        if e.u == e.v {
            mark_edge(start_eid, &mut visited);
            quotient.push(e);
            continue;
        }

        let du = degree[e.u as usize];
        let dv = degree[e.v as usize];
        if du != 2 && dv != 2 {
            mark_edge(start_eid, &mut visited);
            quotient.push(e);
            continue;
        }

        if du == 2 && dv == 2 {
            continue;
        }

        let (start, mut curr) = if du != 2 && dv == 2 {
            (e.u, e.v)
        } else {
            (e.v, e.u)
        };
        let mut curr_eid = start_eid as u32;
        path.clear();
        let mut valid = true;
        mark_edge(start_eid, &mut visited);
        path.push(start_eid);

        while degree[curr as usize] == 2 {
            let Some(next_eid) = incidents.other(curr, curr_eid) else {
                valid = false;
                break;
            };
            let next_idx = next_eid as usize;
            if next_idx >= edges.len() || edge_marked(next_idx, &visited) {
                valid = false;
                break;
            }
            mark_edge(next_idx, &mut visited);
            path.push(next_idx);
            let ne = edges[next_idx];
            curr = if ne.u == curr {
                ne.v
            } else if ne.v == curr {
                ne.u
            } else {
                valid = false;
                break;
            };
            curr_eid = next_eid;
            if curr == start {
                valid = false;
                break;
            }
        }

        if valid && path.len() >= 2 && degree[curr as usize] != 2 && curr != start {
            let (mut left, mut right) = (start, curr);
            chain_children.clear();
            chain_children.extend(path.iter().map(|&eid| edges[eid].child));
            if left > right {
                chain_children.reverse();
                std::mem::swap(&mut left, &mut right);
            }
            let child = push_macro(tree, SP_KIND_SERIES, left, right, &chain_children);
            quotient.push(CoreEdge {
                u: left,
                v: right,
                child,
            });
            tree.stats.series_reductions = tree
                .stats
                .series_reductions
                .saturating_add((chain_children.len() - 1) as u32);
        } else {
            for &eid in &path {
                quotient.push(edges[eid]);
            }
        }
    }

    for eid in 0..edges.len() {
        if edge_marked(eid, &visited) {
            continue;
        }
        mark_edge(eid, &mut visited);
        let e = edges[eid];
        quotient.push(e);
    }

    maybe_fbrq_group_parallel(tree, quotient, group_parallel)
}

fn maybe_fbrq_group_parallel(
    tree: &mut SpTree,
    mut edges: Vec<CoreEdge>,
    group_parallel: bool,
) -> Vec<CoreEdge> {
    if !group_parallel || edges.len() <= 1 {
        return edges;
    }
    bucket_sort_core_edges_by_pair(&mut edges);
    let mut out = Vec::with_capacity(edges.len());
    let mut i = 0usize;
    while i < edges.len() {
        let u = edges[i].u;
        let v = edges[i].v;
        let mut j = i + 1;
        while j < edges.len() && edges[j].u == u && edges[j].v == v {
            j += 1;
        }
        if u != v && j - i >= 2 {
            let mut children: Vec<ChildRef> = edges[i..j].iter().map(|e| e.child).collect();
            children.sort_unstable();
            let child = push_macro(tree, SP_KIND_PARALLEL, u, v, &children);
            out.push(CoreEdge { u, v, child });
            tree.stats.parallel_reductions = tree
                .stats
                .parallel_reductions
                .saturating_add((j - i - 1) as u32);
        } else {
            out.extend_from_slice(&edges[i..j]);
        }
        i = j;
    }
    out
}

#[inline]
fn other_contractible_edge_dense(v: usize, edge: u32, inc0: &[u32], inc1: &[u32]) -> Option<u32> {
    let a = inc0[v];
    let b = inc1[v];
    if a == edge && b != u32::MAX {
        Some(b)
    } else if b == edge && a != u32::MAX {
        Some(a)
    } else {
        None
    }
}

#[inline]
fn ordered_pair(a: u32, b: u32) -> (u32, u32) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

#[inline]
fn edge_marked(edge: usize, visited: &[u64]) -> bool {
    (visited[edge >> 6] & (1u64 << (edge & 63))) != 0
}

#[inline]
fn mark_edge(edge: usize, visited: &mut [u64]) {
    visited[edge >> 6] |= 1u64 << (edge & 63);
}

fn make_path_child(
    tree: &mut SpTree,
    mut left: u32,
    mut right: u32,
    children: &mut [ChildRef],
) -> (ChildRef, u32, u8) {
    if children.len() == 1 {
        let child = children[0];
        return (child, child_as_edge(child).0, 0);
    }
    if left > right
        || (left == right
            && children.last().copied().unwrap_or(0) < children.first().copied().unwrap_or(0))
    {
        children.reverse();
        std::mem::swap(&mut left, &mut right);
    }
    let first_edge = child_as_edge(children[0]).0;
    let child = push_macro(tree, SP_KIND_SERIES, left, right, children);
    tree.stats.series_reductions += (children.len() - 1) as u32;
    (child, first_edge, SP_KIND_SERIES)
}

fn push_macro(
    tree: &mut SpTree,
    kind: u8,
    left: u32,
    right: u32,
    children: &[ChildRef],
) -> ChildRef {
    let off = tree.children.len() as u64;
    tree.children.extend_from_slice(children);
    let mid = tree.macros.len() as u64;
    tree.macros.push(SpNode {
        kind,
        _pad: [0; 3],
        left,
        right,
        children_offset: off,
        children_count: children.len() as u64,
    });
    make_child_macro(mid)
}

fn push_parallel_macro(
    tree: &mut SpTree,
    left: u32,
    right: u32,
    children: &[CoreItem],
) -> ChildRef {
    let off = tree.children.len() as u64;
    tree.children.extend(children.iter().map(|item| item.child));
    let mid = tree.macros.len() as u64;
    tree.macros.push(SpNode {
        kind: SP_KIND_PARALLEL,
        _pad: [0; 3],
        left,
        right,
        children_offset: off,
        children_count: children.len() as u64,
    });
    make_child_macro(mid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp_compress::types::{child_as_macro, child_is_macro};

    fn indexed_edges(edges: &[(u32, u32)]) -> (Vec<u32>, Vec<u32>) {
        edges.iter().copied().unzip()
    }

    #[test]
    fn isolated_contractible_nodes_do_not_force_fallback() {
        let (src, dst) = indexed_edges(&[(0, 1), (1, 2), (2, 3)]);
        let mut contractible = vec![0u8; 10_000];
        contractible[1] = 1;
        contractible[2] = 1;
        contractible[9_999] = 1;

        let result = try_compress_degree2_direct_indexed(10_000, &src, &dst, &contractible)
            .expect("isolated contractible node should not affect direct compression");

        assert_eq!(result.tree.core_edges.len(), 1);
        assert_eq!(result.tree.core_edges[0].u, 0);
        assert_eq!(result.tree.core_edges[0].v, 3);
        assert_eq!(result.tree.core_nodes, vec![NodeId(0), NodeId(3)]);
        assert_eq!(result.tree.stats.series_reductions, 2);
        assert_eq!(result.tree.stats.fully_sp_reducible, 1);
    }

    #[test]
    fn touched_degree_one_contractible_node_still_rejects_direct_path() {
        let (src, dst) = indexed_edges(&[(0, 1)]);
        let contractible = [0u8, 1];

        assert!(try_compress_degree2_direct_indexed(2, &src, &dst, &contractible).is_none());
    }

    #[test]
    fn fbrq_contracts_residual_degree_two_path_into_series_macro() {
        let mut tree = SpTree::default();
        let edges = vec![
            CoreEdge {
                u: 0,
                v: 1,
                child: make_child_edge(EdgeId(0)),
            },
            CoreEdge {
                u: 1,
                v: 2,
                child: make_child_edge(EdgeId(1)),
            },
            CoreEdge {
                u: 2,
                v: 3,
                child: make_child_edge(EdgeId(2)),
            },
        ];

        let out = fbrq_contract_residual_degree2(&mut tree, edges, 4, false);

        assert_eq!(out.len(), 1);
        assert_eq!((out[0].u, out[0].v), (0, 3));
        assert!(child_is_macro(out[0].child));

        let mid = child_as_macro(out[0].child) as usize;
        assert_eq!(tree.macros[mid].kind, SP_KIND_SERIES);
        assert_eq!(tree.macros[mid].left, 0);
        assert_eq!(tree.macros[mid].right, 3);
        assert_eq!(tree.macros[mid].children_count, 3);
    }

    #[test]
    fn fbrq_keeps_all_degree_two_cycle_uncontracted() {
        let mut tree = SpTree::default();
        let edges = vec![
            CoreEdge {
                u: 0,
                v: 1,
                child: make_child_edge(EdgeId(0)),
            },
            CoreEdge {
                u: 1,
                v: 2,
                child: make_child_edge(EdgeId(1)),
            },
            CoreEdge {
                u: 2,
                v: 0,
                child: make_child_edge(EdgeId(2)),
            },
        ];

        let out = fbrq_contract_residual_degree2(&mut tree, edges, 3, false);

        assert_eq!(out.len(), 3);
        assert_eq!(tree.macros.len(), 0);
    }

    #[test]
    fn fbrq_reverses_series_children_when_endpoint_order_is_normalized() {
        let mut tree = SpTree::default();
        let edges = vec![
            CoreEdge {
                u: 3,
                v: 2,
                child: make_child_edge(EdgeId(0)),
            },
            CoreEdge {
                u: 2,
                v: 1,
                child: make_child_edge(EdgeId(1)),
            },
            CoreEdge {
                u: 1,
                v: 0,
                child: make_child_edge(EdgeId(2)),
            },
        ];

        let out = fbrq_contract_residual_degree2(&mut tree, edges, 4, false);

        assert_eq!(out.len(), 1);
        assert_eq!((out[0].u, out[0].v), (0, 3));
        assert!(child_is_macro(out[0].child));

        let mid = child_as_macro(out[0].child) as usize;
        let m = tree.macros[mid];
        assert_eq!(m.kind, SP_KIND_SERIES);
        assert_eq!(m.left, 0);
        assert_eq!(m.right, 3);
        assert_eq!(m.children_count, 3);
        let off = m.children_offset as usize;
        assert_eq!(
            &tree.children[off..off + 3],
            &[
                make_child_edge(EdgeId(2)),
                make_child_edge(EdgeId(1)),
                make_child_edge(EdgeId(0)),
            ]
        );
    }

    #[test]
    fn fbrq_can_group_parallel_quotient_edges_as_macro_p() {
        let mut tree = SpTree::default();
        let edges = vec![
            CoreEdge {
                u: 0,
                v: 1,
                child: make_child_edge(EdgeId(0)),
            },
            CoreEdge {
                u: 0,
                v: 1,
                child: make_child_edge(EdgeId(1)),
            },
        ];

        let out = fbrq_contract_residual_degree2(&mut tree, edges, 2, true);

        assert_eq!(out.len(), 1);
        assert!(child_is_macro(out[0].child));
        let mid = child_as_macro(out[0].child) as usize;
        assert_eq!(tree.macros[mid].kind, SP_KIND_PARALLEL);
        assert_eq!(tree.macros[mid].children_count, 2);
    }
}
