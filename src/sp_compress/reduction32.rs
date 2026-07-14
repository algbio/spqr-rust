use crate::sp_compress::adj::{AdjStore, INVALID_ADJ};
use crate::sp_compress::arena::{PNodeArena, INVALID_PNODE, PK_ATOMIC, PK_PARALLEL, PK_SERIES};
use crate::sp_compress::pmap32::{make_pair_key, pair_first, pair_second, FlatPairMap, PairKey};
use crate::sp_compress::types32::{
    child_as_edge, child_as_macro, child_is_edge, child_is_macro, make_child_edge,
    make_child_macro, ChildRef, CompressionResult, CoreEdge, SpNode, SpNodeId, SpTree,
    SP_KIND_PARALLEL, SP_KIND_SERIES,
};
use crate::{EdgeId, NodeId};

#[derive(Clone, Copy, Debug)]
struct WorkEdge {
    u: NodeId,
    v: NodeId,
    pnode: u32,
    adj_node_u: u32,
    adj_node_v: u32,
    bucket_next: u32,
}

const _: () = assert!(std::mem::size_of::<WorkEdge>() == 24);

#[inline(always)]
fn work_deactivate(w: &mut WorkEdge) {
    w.pnode = INVALID_PNODE;
}

pub fn compress_borrowed(
    n_nodes: u32,
    input_edges: &[crate::sp_compress::types32::InputEdge],
    contractible: &[u8],
) -> CompressionResult {
    let mut tree = SpTree::default();
    tree.stats.input_nodes = n_nodes;
    tree.stats.input_edges = input_edges.len() as u32;

    tree.set_input_edges(input_edges);

    if contractible.len() < n_nodes as usize {
        return CompressionResult {
            tree,
            success: false,
            error_message: Some("contractible mask shorter than n_nodes"),
        };
    }

    let mut arena = PNodeArena::new();
    arena.reserve(input_edges.len() * 5 / 4 + 16);

    let mut adj = AdjStore::new();

    let mut pmap = FlatPairMap::default();
    pmap.init(input_edges.len() + 16);

    let pnode_start = arena.bulk_init_atomic(input_edges);

    let mut edges: Vec<WorkEdge> = Vec::with_capacity(input_edges.len() * 5 / 4 + 16);
    adj.init(n_nodes, input_edges.len());

    for (k, ie) in input_edges.iter().enumerate() {
        let pnode_id = pnode_start + k as u32;
        let edge_idx = edges.len() as u32;
        let adj_u = adj.insert(ie.u, edge_idx);
        let adj_v = if ie.u != ie.v {
            adj.insert(ie.v, edge_idx)
        } else {
            INVALID_ADJ
        };
        edges.push(WorkEdge {
            u: ie.u,
            v: ie.v,
            pnode: pnode_id,
            adj_node_u: adj_u,
            adj_node_v: adj_v,
            bucket_next: u32::MAX,
        });

        if ie.u != ie.v {
            let k = make_pair_key(ie.u, ie.v);
            let r = pmap.on_seen(k, edge_idx);
            apply_on_seen(r, edge_idx, &mut edges);
        }
    }
    let mut node_dirty: Vec<NodeId> = Vec::new();
    let mut node_in_dirty: Vec<u64> = vec![0; (n_nodes as usize).div_ceil(64)];

    let n_nodes = n_nodes as usize;

    for v_idx in 0..n_nodes {
        let v = NodeId(v_idx as u32);
        if contractible[v_idx] != 0 && adj.deg[v_idx] == 2 {
            let bit = 1u64 << (v_idx & 63);
            let w = &mut node_in_dirty[v_idx >> 6];
            if (*w & bit) == 0 {
                *w |= bit;
                node_dirty.push(v);
            }
        }
    }

    let mut pair_dirty: Vec<PairKey> = Vec::new();

    if !pmap.buckets.is_empty() {
        for slot in pmap.slots.iter() {
            let v = slot.value;
            if FlatPairMap::is_indirect(v) {
                let bid = FlatPairMap::bucket_index(v) as usize;
                if pmap.buckets[bid].count >= 2 {
                    pair_dirty.push(slot.key);
                }
            }
        }
    }
    let mut series_reductions: u32 = 0;
    let mut parallel_reductions: u32 = 0;

    let mut bucket_edges_buf: Vec<u32> = Vec::with_capacity(64);
    let mut kid_pnodes_buf: Vec<u32> = Vec::with_capacity(64);

    while !node_dirty.is_empty() || !pair_dirty.is_empty() {
        while let Some(v) = node_dirty.pop() {
            let v_idx = v.idx();
            node_in_dirty[v_idx >> 6] &= !(1u64 << (v_idx & 63));

            if contractible[v_idx] == 0 {
                continue;
            }
            if adj.deg[v_idx] != 2 {
                continue;
            }

            let (e1_idx, e2_idx) = adj.take_two(v);
            if e1_idx == e2_idx {
                continue;
            }

            let (e1_p, e1_u, e1_v, e1_au, e1_av) = {
                let e = &edges[e1_idx as usize];
                (e.pnode, e.u, e.v, e.adj_node_u, e.adj_node_v)
            };
            if e1_p == INVALID_PNODE || e1_u == e1_v {
                continue;
            }

            let (e2_p, e2_u, e2_v, e2_au, e2_av) = {
                let e = &edges[e2_idx as usize];
                (e.pnode, e.u, e.v, e.adj_node_u, e.adj_node_v)
            };
            if e2_p == INVALID_PNODE || e2_u == e2_v {
                continue;
            }

            let a = if e1_u == v { e1_v } else { e1_u };
            let b = if e2_u == v { e2_v } else { e2_u };
            if a == v || b == v {
                continue;
            }

            let merged = arena.combine_series(v, a, b, e1_p, e2_p);

            adj.remove(e1_u, e1_au);
            if e1_u != e1_v {
                adj.remove(e1_v, e1_av);
            }
            adj.remove(e2_u, e2_au);
            if e2_u != e2_v {
                adj.remove(e2_v, e2_av);
            }
            work_deactivate(&mut edges[e1_idx as usize]);
            work_deactivate(&mut edges[e2_idx as usize]);

            add_new_edge(
                a,
                b,
                merged,
                &mut edges,
                &mut adj,
                &mut pmap,
                &mut node_dirty,
                &mut node_in_dirty,
                &mut pair_dirty,
                contractible,
                n_nodes,
            );
            series_reductions += 1;
        }

        while let Some(k) = pair_dirty.pop() {
            bucket_compact(&mut pmap, &mut edges, k);

            let bid_opt = pmap.find_bucket(k);
            let bid = match bid_opt {
                Some(b) => b,
                None => continue,
            };
            if pmap.buckets[bid as usize].count < 2 {
                continue;
            }

            bucket_edges_buf.clear();
            let mut cur = pmap.buckets[bid as usize].head;
            while cur != u32::MAX {
                let e = &edges[cur as usize];
                let nxt = e.bucket_next;
                if e.pnode != INVALID_PNODE {
                    bucket_edges_buf.push(cur);
                }
                cur = nxt;
            }
            if bucket_edges_buf.len() < 2 {
                continue;
            }

            let a = pair_first(k);
            let c = pair_second(k);

            kid_pnodes_buf.clear();
            kid_pnodes_buf.reserve(bucket_edges_buf.len());
            for &idx in &bucket_edges_buf {
                kid_pnodes_buf.push(edges[idx as usize].pnode);
            }

            let merged = arena.make_parallel(a, c, &kid_pnodes_buf);

            for &idx in &bucket_edges_buf {
                let (eu, ev, eau, eav) = {
                    let e = &edges[idx as usize];
                    (e.u, e.v, e.adj_node_u, e.adj_node_v)
                };
                adj.remove(eu, eau);
                if eu != ev {
                    adj.remove(ev, eav);
                }
                work_deactivate(&mut edges[idx as usize]);
            }
            pmap.erase_pair(k);

            add_new_edge(
                a,
                c,
                merged,
                &mut edges,
                &mut adj,
                &mut pmap,
                &mut node_dirty,
                &mut node_in_dirty,
                &mut pair_dirty,
                contractible,
                n_nodes,
            );
            parallel_reductions += 1;
        }
    }

    let mut node_used: Vec<u64> = vec![0u64; n_nodes.div_ceil(64)];

    tree.children.reserve(input_edges.len());

    let mut mat_stack: Vec<(u32, u8)> = Vec::with_capacity(64);
    let mut mat_resolved: Vec<ChildRef> = Vec::with_capacity(64);

    for i in 0..edges.len() {
        let (epn, mut ce_u, mut ce_v) = {
            let e = &edges[i];
            if e.pnode == INVALID_PNODE {
                continue;
            }
            (e.pnode, e.u, e.v)
        };
        node_used[ce_u.idx() >> 6] |= 1u64 << (ce_u.idx() & 63);
        if ce_u != ce_v {
            node_used[ce_v.idx() >> 6] |= 1u64 << (ce_v.idx() & 63);
        }

        let root_ref = materialize(
            epn,
            &mut arena,
            &mut tree,
            &mut mat_stack,
            &mut mat_resolved,
        );

        if ce_u.0 > ce_v.0 {
            std::mem::swap(&mut ce_u, &mut ce_v);
        }
        tree.core_edges.push(CoreEdge {
            u: ce_u.0,
            v: ce_v.0,
            child: root_ref,
        });
    }
    arena.drop_storage();
    let _ = std::mem::take(&mut edges);
    adj.drop_storage();
    pmap.drop_storage();
    let _ = std::mem::take(&mut node_dirty);
    let _ = std::mem::take(&mut node_in_dirty);
    let _ = std::mem::take(&mut pair_dirty);

    canonize_series_orientation(&mut tree);

    tree.core_edges.sort_unstable_by(|a, b| {
        a.u.cmp(&b.u)
            .then(a.v.cmp(&b.v))
            .then(a.child.cmp(&b.child))
    });

    for v_idx in 0..n_nodes {
        if (node_used[v_idx >> 6] & (1u64 << (v_idx & 63))) != 0 {
            tree.core_nodes.push(NodeId(v_idx as u32));
        }
    }

    tree.stats.iterations = 1;
    tree.stats.series_reductions = series_reductions;
    tree.stats.parallel_reductions = parallel_reductions;
    tree.stats.fully_sp_reducible =
        if tree.core_edges.len() == 1 && tree.core_edges[0].u != tree.core_edges[0].v {
            1
        } else {
            0
        };

    tree.update_stats();

    tree.macros.shrink_to_fit();
    tree.children.shrink_to_fit();
    tree.core_edges.shrink_to_fit();
    tree.core_nodes.shrink_to_fit();
    tree.input_endpoints.shrink_to_fit();

    CompressionResult {
        tree,
        success: true,
        error_message: None,
    }
}

#[inline(always)]
fn apply_on_seen(
    result: crate::sp_compress::pmap32::OnSeenResult,
    edge_idx: u32,
    edges: &mut [WorkEdge],
) {
    use crate::sp_compress::pmap32::OnSeenResult;
    match result {
        OnSeenResult::SingleStored => {}
        OnSeenResult::InsertedFirst { bucket_next } => {
            edges[edge_idx as usize].bucket_next = bucket_next;
        }
        OnSeenResult::PromotedAndInserted {
            promoted_edge,
            bucket_next,
        } => {
            edges[promoted_edge as usize].bucket_next = u32::MAX;

            edges[edge_idx as usize].bucket_next = bucket_next;
        }
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn add_new_edge(
    u: NodeId,
    v: NodeId,
    pnode_id: u32,
    edges: &mut Vec<WorkEdge>,
    adj: &mut AdjStore,
    pmap: &mut FlatPairMap,
    node_dirty: &mut Vec<NodeId>,
    node_in_dirty: &mut [u64],
    pair_dirty: &mut Vec<PairKey>,
    contractible: &[u8],
    n_nodes: usize,
) -> u32 {
    let idx = edges.len() as u32;
    let adj_u = adj.insert(u, idx);
    let adj_v = if u != v {
        adj.insert(v, idx)
    } else {
        INVALID_ADJ
    };
    edges.push(WorkEdge {
        u,
        v,
        pnode: pnode_id,
        adj_node_u: adj_u,
        adj_node_v: adj_v,
        bucket_next: u32::MAX,
    });

    if u != v {
        let k = make_pair_key(u, v);
        let r = pmap.on_seen(k, idx);

        let needs_pair_dirty = matches!(
            r,
            crate::sp_compress::pmap32::OnSeenResult::PromotedAndInserted { .. }
                | crate::sp_compress::pmap32::OnSeenResult::InsertedFirst { .. }
        );
        apply_on_seen(r, idx, edges);
        if needs_pair_dirty {
            pair_dirty.push(k);
        }
    }

    let try_enq = |w: NodeId, node_dirty: &mut Vec<NodeId>, node_in_dirty: &mut [u64]| {
        let wi = w.idx();
        if wi >= n_nodes {
            return;
        }
        if contractible[wi] == 0 {
            return;
        }
        if adj.deg[wi] != 2 {
            return;
        }
        let bit = 1u64 << (wi & 63);
        if (node_in_dirty[wi >> 6] & bit) != 0 {
            return;
        }
        node_in_dirty[wi >> 6] |= bit;
        node_dirty.push(w);
    };
    try_enq(u, node_dirty, node_in_dirty);
    if u != v {
        try_enq(v, node_dirty, node_in_dirty);
    }

    idx
}

#[inline]
fn bucket_compact(pmap: &mut FlatPairMap, edges: &mut [WorkEdge], k: PairKey) {
    let bid = match pmap.find_bucket(k) {
        Some(b) => b,
        None => return,
    };
    let bid_us = bid as usize;
    let mut cur = pmap.buckets[bid_us].head;
    let mut new_head: u32 = u32::MAX;
    let mut kept: u32 = 0;
    while cur != u32::MAX {
        let e = &mut edges[cur as usize];
        let nxt = e.bucket_next;
        if e.pnode != INVALID_PNODE {
            e.bucket_next = new_head;
            new_head = cur;
            kept += 1;
        }
        cur = nxt;
    }
    pmap.buckets[bid_us].head = new_head;
    pmap.buckets[bid_us].count = kept;
    if kept == 0 {
        pmap.erase_pair(k);
    }
}

fn materialize(
    root_pnode: u32,
    arena: &mut PNodeArena,
    tree: &mut SpTree,
    mat_stack: &mut Vec<(u32, u8)>,
    mat_resolved: &mut Vec<ChildRef>,
) -> ChildRef {
    mat_stack.clear();
    mat_stack.push((root_pnode, 0));

    while let Some(&top) = mat_stack.last() {
        let (p, phase) = top;

        let (kind, left_kid, left, right) = {
            let pn = &arena.pool[p as usize];
            (pn.kind, pn.left_kid, pn.left, pn.right)
        };

        if kind == PK_ATOMIC {
            mat_stack.pop();
            continue;
        }

        if phase == 0 {
            mat_stack.last_mut().unwrap().1 = 1;
            let mut c = left_kid;
            while c != INVALID_PNODE {
                mat_stack.push((c, 0));
                c = arena.pool[c as usize].next;
            }
            continue;
        }

        mat_resolved.clear();
        let mut c = left_kid;
        while c != INVALID_PNODE {
            let cn = &arena.pool[c as usize];
            let next = cn.next;
            if cn.kind == PK_ATOMIC {
                mat_resolved.push(make_child_edge(cn.edge_id));
            } else {
                let mm: SpNodeId = cn.edge_id.0;
                mat_resolved.push(make_child_macro(mm));
            }
            c = next;
        }

        if kind == PK_PARALLEL {
            let macros_snapshot: &[SpNode] = &tree.macros;
            let children_snapshot: &[ChildRef] = &tree.children;
            mat_resolved.sort_unstable_by(|&ra, &rb| {
                let ka: u32 = if child_is_edge(ra) {
                    0
                } else {
                    macros_snapshot[child_as_macro(ra) as usize].kind as u32
                };
                let kb: u32 = if child_is_edge(rb) {
                    0
                } else {
                    macros_snapshot[child_as_macro(rb) as usize].kind as u32
                };
                if ka != kb {
                    return ka.cmp(&kb);
                }

                let first_edge_of = |r: ChildRef| -> EdgeId {
                    if child_is_edge(r) {
                        return child_as_edge(r);
                    }
                    let mut cur = r;
                    while child_is_macro(cur) {
                        let m = &macros_snapshot[child_as_macro(cur) as usize];
                        if m.children_count == 0 {
                            return EdgeId::INVALID;
                        }
                        cur = children_snapshot[m.children_offset as usize];
                    }
                    if child_is_edge(cur) {
                        child_as_edge(cur)
                    } else {
                        EdgeId::INVALID
                    }
                };
                let ea = first_edge_of(ra);
                let eb = first_edge_of(rb);
                if ea != eb {
                    return ea.cmp(&eb);
                }
                ra.cmp(&rb)
            });
        }

        let children_offset = tree.children.len() as u32;
        for &cr in mat_resolved.iter() {
            tree.children.push(cr);
        }

        let m = SpNode {
            kind: if kind == PK_SERIES {
                SP_KIND_SERIES
            } else {
                SP_KIND_PARALLEL
            },
            _pad: [0; 3],
            left: left.0,
            right: right.0,
            children_offset,
            children_count: mat_resolved.len() as u32,
        };

        let new_mid = tree.macros.len() as SpNodeId;
        tree.macros.push(m);

        arena.pool[p as usize].edge_id = EdgeId(new_mid);

        mat_stack.pop();
    }

    let root = &arena.pool[root_pnode as usize];
    if root.kind == PK_ATOMIC {
        return make_child_edge(root.edge_id);
    }
    make_child_macro(root.edge_id.0)
}

fn canonize_series_orientation(tree: &mut SpTree) {
    fn child_first_edge_id(tree: &SpTree, c: ChildRef) -> EdgeId {
        if child_is_edge(c) {
            return child_as_edge(c);
        }
        let mut cur_macro = child_as_macro(c);
        loop {
            let m = tree.macros[cur_macro as usize];
            if m.children_count == 0 {
                return EdgeId::INVALID;
            }
            let first = tree.children[m.children_offset as usize];
            if child_is_edge(first) {
                return child_as_edge(first);
            }
            cur_macro = child_as_macro(first);
        }
    }

    for mid in 0..tree.macros.len() {
        let m = tree.macros[mid];
        if m.kind != SP_KIND_SERIES {
            continue;
        }

        let reverse_it = match m.left.cmp(&m.right) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => {
                if m.children_count >= 2 {
                    let first_child = tree.children[m.children_offset as usize];
                    let last_child =
                        tree.children[(m.children_offset + m.children_count - 1) as usize];
                    let ef = child_first_edge_id(tree, first_child);
                    let el = child_first_edge_id(tree, last_child);
                    el < ef
                } else {
                    false
                }
            }
            std::cmp::Ordering::Less => false,
        };

        if reverse_it {
            let off = m.children_offset as usize;
            let cnt = m.children_count as usize;
            let mut a = 0;
            let mut b = cnt - 1;
            while a < b {
                tree.children.swap(off + a, off + b);
                a += 1;
                b -= 1;
            }
            let mref = &mut tree.macros[mid];
            std::mem::swap(&mut mref.left, &mut mref.right);
        }
    }
}
