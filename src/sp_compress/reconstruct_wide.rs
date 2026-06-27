use crate::sp_compress::wide::{
    child_as_edge, child_as_macro, child_is_macro, ChildRef, SpNode, SpTree, SP_KIND_PARALLEL,
    SP_KIND_SERIES,
};
use crate::wide::{EdgeId, NodeId, SkeletonEdge, SpqrNodeType, SpqrTree, TreeNodeId, INVALID};
use std::time::Instant;

#[derive(Default, Clone, Copy, Debug)]
pub struct ReconstructTimings {
    pub t_build_builder_us: u64,
    pub t_normalize_in_place_us: u64,
    pub t_finalize_us: u64,
    pub t_defensive_normalize_us: u64,
    pub t_canon_root_us: u64,
    pub t_canon_node_order_us: u64,
    pub t_canon_edge_orient_us: u64,
    pub t_canon_move_root_us: u64,
}

fn elapsed_us(t: Instant) -> u64 {
    t.elapsed().as_micros() as u64
}

fn canonicalize_timed(tree: &mut SpqrTree, timings: &mut ReconstructTimings) {
    let t = Instant::now();
    tree.canonicalize_root();
    timings.t_canon_root_us = elapsed_us(t);

    let t = Instant::now();
    tree.canonicalize_skeleton_node_order();
    timings.t_canon_node_order_us = elapsed_us(t);

    let t = Instant::now();
    tree.canonicalize_skeleton_edge_orientation();
    timings.t_canon_edge_orient_us = elapsed_us(t);

    let t = Instant::now();
    tree.move_root_to_zero();
    timings.t_canon_move_root_us = elapsed_us(t);
}

pub fn reconstruct(t_core: &SpqrTree, macro_tree: &SpTree, core_node_inv: &[NodeId]) -> SpqrTree {
    reconstruct_timed(t_core, macro_tree, core_node_inv).0
}

pub fn reconstruct_timed(
    t_core: &SpqrTree,
    macro_tree: &SpTree,
    core_node_inv: &[NodeId],
) -> (SpqrTree, ReconstructTimings) {
    let n_real_edges = macro_tree.stats.input_edges as usize;

    if macro_tree.stats.fully_sp_reducible != 0 {
        return reconstruct_fully_reducible_timed(macro_tree);
    }
    if t_core.is_empty() {
        return reconstruct_fully_reducible_timed(macro_tree);
    }

    let mut timings = ReconstructTimings::default();

    let t = Instant::now();
    let mut b = reconstruct_build_builder(t_core, macro_tree, core_node_inv);
    timings.t_build_builder_us = elapsed_us(t);

    let t = Instant::now();
    b.normalize_in_place();
    timings.t_normalize_in_place_us = elapsed_us(t);

    let t = Instant::now();
    let mut tree = b.finalize(n_real_edges);
    timings.t_finalize_us = elapsed_us(t);

    let t = Instant::now();
    tree.normalize();
    timings.t_defensive_normalize_us = elapsed_us(t);

    canonicalize_timed(&mut tree, &mut timings);

    (tree, timings)
}

fn reconstruct_build_builder(
    t_core: &SpqrTree,
    macro_tree: &SpTree,
    core_node_inv: &[NodeId],
) -> Builder {
    let n_real_edges = macro_tree.stats.input_edges as usize;
    let mut b = Builder::new(n_real_edges);

    {
        let mut max_existing_vid: u64 = 0;

        for tn in 0..t_core.len() {
            for e in t_core.skeleton_edges_slice(TreeNodeId(tn as u64)) {
                if e.virtual_id != INVALID && e.virtual_id > max_existing_vid {
                    max_existing_vid = e.virtual_id;
                }
            }
        }

        let initial = std::cmp::max(n_real_edges as u64, max_existing_vid.saturating_add(1));
        b.next_vid = initial;
    }

    let mut t_core_to_recon: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; t_core.len()];

    for old_idx in 0..t_core.len() {
        let old = TreeNodeId(old_idx as u64);
        let new_id = b.copy_t_core_node(t_core, old, core_node_inv);

        t_core_to_recon[old_idx] = new_id;
    }

    for old_idx in 0..t_core.len() {
        let p_old = t_core.node_parents[old_idx];

        if p_old.is_valid() && p_old.idx() != old_idx {
            b.parents[t_core_to_recon[old_idx].idx()] = t_core_to_recon[p_old.idx()];
        }
    }

    for old_idx in 0..t_core.len() {
        let old = TreeNodeId(old_idx as u64);
        let new = t_core_to_recon[old_idx];
        let n_edges = t_core.skeleton_edges_slice(old).len();

        for ei in 0..n_edges {
            let twin_old = t_core.skeleton_edges_slice(old)[ei].twin_tree_node;

            if twin_old.is_valid() {
                b.edges[new.idx()][ei].twin_tree_node = t_core_to_recon[twin_old.idx()];
            }
        }
    }

    let original_recon_count = b.num_nodes();

    for recon_idx in 0..original_recon_count {
        let recon = TreeNodeId(recon_idx as u64);
        let mut ei = 0;

        loop {
            if ei >= b.edges[recon.idx()].len() {
                break;
            }

            let e = b.edges[recon.idx()][ei];

            if !e.real_edge.is_valid() {
                ei += 1;
                continue;
            }

            let core_edge_id = e.real_edge.0 as usize;
            debug_assert!(core_edge_id < macro_tree.core_edges.len());

            let child_ref = macro_tree.core_edges[core_edge_id].child;

            if !child_is_macro(child_ref) {
                let g_edge = child_as_edge(child_ref);
                b.edges[recon.idx()][ei].real_edge = g_edge;
                ei += 1;
                continue;
            }

            let macro_id = child_as_macro(child_ref);
            let mapping = &b.node_mapping[recon.idx()];
            let src_orig = mapping[e.src.idx()];
            let dst_orig = mapping[e.dst.idx()];

            let (sub_root, virt_in_sub) =
                expand_macro_subtree(&mut b, macro_tree, macro_id, src_orig, dst_orig);

            let virt_id = b.next_virtual_id();

            let our_edge = &mut b.edges[recon.idx()][ei];
            our_edge.real_edge = EdgeId::INVALID;
            our_edge.virtual_id = virt_id;
            our_edge.twin_tree_node = sub_root;
            our_edge.twin_edge_idx = virt_in_sub;

            let theirs = &mut b.edges[sub_root.idx()][virt_in_sub as usize];
            theirs.twin_tree_node = recon;
            theirs.twin_edge_idx = ei as u64;
            theirs.virtual_id = virt_id;

            b.parents[sub_root.idx()] = recon;
            ei += 1;
        }
    }

    b
}

#[allow(dead_code)]
pub(crate) fn reconstruct_fully_reducible(macro_tree: &SpTree) -> SpqrTree {
    reconstruct_fully_reducible_timed(macro_tree).0
}

pub(crate) fn reconstruct_fully_reducible_timed(
    macro_tree: &SpTree,
) -> (SpqrTree, ReconstructTimings) {
    let mut timings = ReconstructTimings::default();
    let n_real_edges = macro_tree.stats.input_edges as usize;

    if macro_tree.core_edges.is_empty() {
        return (
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
                edge_to_tree_node: vec![TreeNodeId::INVALID; n_real_edges],
                min_real_per_node: Vec::new(),
                preassembly_scad_export: None,
                preassembly_minimizer_sidecar: None,
            },
            timings,
        );
    }

    debug_assert_eq!(macro_tree.core_edges.len(), 1);

    let ce = macro_tree.core_edges[0];

    if !child_is_macro(ce.child) {
        return (
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
                edge_to_tree_node: vec![TreeNodeId::INVALID; n_real_edges],
                min_real_per_node: Vec::new(),
                preassembly_scad_export: None,
                preassembly_minimizer_sidecar: None,
            },
            timings,
        );
    }

    let macro_id = child_as_macro(ce.child);
    let pole_a = NodeId(ce.u);
    let pole_b = NodeId(ce.v);

    let mut b = Builder::new(n_real_edges);

    let t = Instant::now();
    expand_macro_root(&mut b, macro_tree, macro_id, pole_a, pole_b);
    timings.t_build_builder_us = elapsed_us(t);

    let t = Instant::now();
    b.normalize_in_place();
    timings.t_normalize_in_place_us = elapsed_us(t);

    let t = Instant::now();
    let mut tree = b.finalize(n_real_edges);
    timings.t_finalize_us = elapsed_us(t);

    let t = Instant::now();
    tree.normalize();
    timings.t_defensive_normalize_us = elapsed_us(t);

    canonicalize_timed(&mut tree, &mut timings);

    (tree, timings)
}

fn expand_macro_subtree(
    b: &mut Builder,
    macro_tree: &SpTree,
    macro_id: u64,
    pole_a: NodeId,
    pole_b: NodeId,
) -> (TreeNodeId, u64) {
    let m = macro_tree.macros[macro_id as usize];

    debug_assert!(
        m.kind == SP_KIND_SERIES || m.kind == SP_KIND_PARALLEL,
        "atomic macro should not reach expand_macro_subtree"
    );

    if m.kind == SP_KIND_SERIES {
        expand_series(b, macro_tree, m, pole_a, pole_b, true)
    } else {
        expand_parallel(b, macro_tree, m, pole_a, pole_b, true)
    }
}

fn expand_macro_root(
    b: &mut Builder,
    macro_tree: &SpTree,
    macro_id: u64,
    pole_a: NodeId,
    pole_b: NodeId,
) -> TreeNodeId {
    let m = macro_tree.macros[macro_id as usize];

    if m.kind == SP_KIND_PARALLEL && m.children_count == 2 {
        return expand_root_two_branch_cycle(b, macro_tree, m, pole_a, pole_b);
    }

    if m.kind == SP_KIND_SERIES && pole_a == pole_b {
        return expand_root_series_cycle(b, macro_tree, m, pole_a);
    }

    let (root, _) = if m.kind == SP_KIND_SERIES {
        expand_series(b, macro_tree, m, pole_a, pole_b, false)
    } else {
        expand_parallel(b, macro_tree, m, pole_a, pole_b, false)
    };

    root
}

fn expand_root_series_cycle(
    b: &mut Builder,
    macro_tree: &SpTree,
    m: SpNode,
    pole: NodeId,
) -> TreeNodeId {
    debug_assert_eq!(m.kind, SP_KIND_SERIES);

    let k = m.children_count as usize;
    debug_assert!(k >= 2, "Series macro forming a cycle needs ≥2 edges");

    let mut cycle_vertices: Vec<NodeId> = Vec::with_capacity(k);
    cycle_vertices.push(pole);

    let mut current = pole;

    for ci in 0..k {
        let cref = macro_tree.children[(m.children_offset as usize) + ci];
        let (a, c) = child_endpoints(macro_tree, cref);

        let next = if a == current {
            c
        } else if c == current {
            a
        } else {
            panic!(
                "Series-cycle chain inconsistent at child {}: ({:?},{:?}) doesn't include {:?}, pole={:?}",
                ci, a, c, current, pole
            );
        };

        if ci + 1 == k {
            debug_assert_eq!(
                next, pole,
                "Series-cycle did not close: expected {:?}, got {:?}",
                pole, next
            );
        } else {
            cycle_vertices.push(next);
        }

        current = next;
    }

    let num_skel_nodes = k as u64;
    let mut edges: Vec<SkeletonEdge> = Vec::with_capacity(k);

    for i in 0..k {
        let local_src = i as u64;
        let local_dst = ((i + 1) % k) as u64;
        let cref = macro_tree.children[(m.children_offset as usize) + i];

        if child_is_macro(cref) {
            edges.push(SkeletonEdge {
                src: NodeId(local_src),
                dst: NodeId(local_dst),
                real_edge: EdgeId::INVALID,
                virtual_id: INVALID,
                twin_tree_node: TreeNodeId::INVALID,
                twin_edge_idx: INVALID,
            });
        } else {
            edges.push(SkeletonEdge {
                src: NodeId(local_src),
                dst: NodeId(local_dst),
                real_edge: child_as_edge(cref),
                virtual_id: INVALID,
                twin_tree_node: TreeNodeId::INVALID,
                twin_edge_idx: INVALID,
            });
        }
    }

    let s_node = b.add_node(
        SpqrNodeType::S,
        num_skel_nodes,
        edges,
        cycle_vertices.clone(),
    );

    for i in 0..k {
        let cref = macro_tree.children[(m.children_offset as usize) + i];

        if !child_is_macro(cref) {
            continue;
        }

        let macro_id = child_as_macro(cref);
        let v_a = cycle_vertices[i];
        let v_b = cycle_vertices[(i + 1) % k];

        attach_parallel_child(b, macro_tree, s_node, i as u64, macro_id, v_a, v_b);
    }

    s_node
}

fn expand_root_two_branch_cycle(
    b: &mut Builder,
    macro_tree: &SpTree,
    m: SpNode,
    pole_a: NodeId,
    pole_b: NodeId,
) -> TreeNodeId {
    debug_assert_eq!(m.kind, SP_KIND_PARALLEL);
    debug_assert_eq!(m.children_count, 2);

    let c0 = macro_tree.children[m.children_offset as usize];
    let c1 = macro_tree.children[m.children_offset as usize + 1];

    let branch0 = branch_to_segments(macro_tree, c0, pole_a, pole_b);
    let branch1 = branch_to_segments(macro_tree, c1, pole_a, pole_b);

    let total_len = branch0.len() + branch1.len();

    let mut cycle_vertices: Vec<NodeId> = Vec::with_capacity(total_len);
    cycle_vertices.push(pole_a);

    for seg in &branch0 {
        cycle_vertices.push(seg.dst);
    }

    debug_assert_eq!(*cycle_vertices.last().unwrap(), pole_b);

    for seg in branch1.iter().rev() {
        cycle_vertices.push(seg.src);
    }

    let last = cycle_vertices.pop().expect("non-empty cycle");
    debug_assert_eq!(last, pole_a, "branch1 reversal didn't close the cycle");

    let num_skel_nodes = cycle_vertices.len() as u64;

    let local_of = |v: NodeId, expected_at: usize| -> u64 {
        debug_assert_eq!(
            cycle_vertices[expected_at], v,
            "local_of mismatch at idx {}: expected {:?}, found {:?}",
            expected_at, v, cycle_vertices[expected_at]
        );
        expected_at as u64
    };
    let _ = local_of;

    let mut edges: Vec<SkeletonEdge> = Vec::with_capacity(total_len);

    for (i, seg) in branch0.iter().enumerate() {
        edges.push(skel_edge_from_segment(seg, i as u64, (i + 1) as u64));
    }

    let n0 = branch0.len();

    for (j, seg) in branch1.iter().rev().enumerate() {
        let local_src = (n0 + j) as u64;
        let local_dst = if j + 1 == branch1.len() {
            0u64
        } else {
            (n0 + j + 1) as u64
        };

        edges.push(SkeletonEdge {
            src: NodeId(local_src),
            dst: NodeId(local_dst),
            real_edge: seg.real_edge,
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        });
    }

    let s_node = b.add_node(
        SpqrNodeType::S,
        num_skel_nodes,
        edges,
        cycle_vertices.clone(),
    );

    for (i, seg) in branch0.iter().enumerate() {
        if let Some(macro_id) = seg.macro_id {
            attach_parallel_child(b, macro_tree, s_node, i as u64, macro_id, seg.src, seg.dst);
        }
    }

    for (j, seg) in branch1.iter().rev().enumerate() {
        if let Some(macro_id) = seg.macro_id {
            let edge_idx = (n0 + j) as u64;
            attach_parallel_child(b, macro_tree, s_node, edge_idx, macro_id, seg.dst, seg.src);
        }
    }

    s_node
}

#[derive(Clone, Copy, Debug)]
struct ChainSegment {
    src: NodeId,
    dst: NodeId,
    real_edge: EdgeId,
    macro_id: Option<u64>,
}

fn branch_to_segments(
    macro_tree: &SpTree,
    cref: ChildRef,
    from: NodeId,
    to: NodeId,
) -> Vec<ChainSegment> {
    if !child_is_macro(cref) {
        let e = child_as_edge(cref);

        return vec![ChainSegment {
            src: from,
            dst: to,
            real_edge: e,
            macro_id: None,
        }];
    }

    let macro_id = child_as_macro(cref);
    let m = macro_tree.macros[macro_id as usize];

    if m.kind == SP_KIND_SERIES {
        let macro_left = NodeId(m.left);
        let macro_right = NodeId(m.right);
        let forward = from == macro_left && to == macro_right;
        let backward = from == macro_right && to == macro_left;

        assert!(
            forward || backward,
            "Series branch orientation mismatch: macro=({:?},{:?}), branch=({:?},{:?})",
            macro_left,
            macro_right,
            from,
            to
        );

        let k = m.children_count as usize;
        let order: Vec<usize> = if forward {
            (0..k).collect()
        } else {
            (0..k).rev().collect()
        };

        let mut segments = Vec::with_capacity(k);
        let mut current = from;

        for &ci in &order {
            let inner = macro_tree.children[m.children_offset as usize + ci];
            let (a, c) = child_endpoints(macro_tree, inner);

            let next = if a == current {
                c
            } else if c == current {
                a
            } else {
                panic!(
                    "Series segment lookup mismatch at child idx {}: ({:?},{:?}) doesn't include {:?}",
                    ci, a, c, current
                );
            };

            if child_is_macro(inner) {
                segments.push(ChainSegment {
                    src: current,
                    dst: next,
                    real_edge: EdgeId::INVALID,
                    macro_id: Some(child_as_macro(inner)),
                });
            } else {
                segments.push(ChainSegment {
                    src: current,
                    dst: next,
                    real_edge: child_as_edge(inner),
                    macro_id: None,
                });
            }

            current = next;
        }

        debug_assert_eq!(current, to);
        segments
    } else {
        vec![ChainSegment {
            src: from,
            dst: to,
            real_edge: EdgeId::INVALID,
            macro_id: Some(macro_id),
        }]
    }
}

fn skel_edge_from_segment(seg: &ChainSegment, src_local: u64, dst_local: u64) -> SkeletonEdge {
    SkeletonEdge {
        src: NodeId(src_local),
        dst: NodeId(dst_local),
        real_edge: seg.real_edge,
        virtual_id: INVALID,
        twin_tree_node: TreeNodeId::INVALID,
        twin_edge_idx: INVALID,
    }
}

fn attach_parallel_child(
    b: &mut Builder,
    macro_tree: &SpTree,
    parent_node: TreeNodeId,
    parent_edge_idx: u64,
    macro_id: u64,
    pole_a: NodeId,
    pole_b: NodeId,
) {
    let (child_root, virt_in_child) = expand_macro_subtree(b, macro_tree, macro_id, pole_a, pole_b);

    debug_assert_eq!(macro_tree.macros[macro_id as usize].kind, SP_KIND_PARALLEL);

    let vid = b.next_virtual_id();

    let our = &mut b.edges[parent_node.idx()][parent_edge_idx as usize];
    our.virtual_id = vid;
    our.twin_tree_node = child_root;
    our.twin_edge_idx = virt_in_child;

    let theirs = &mut b.edges[child_root.idx()][virt_in_child as usize];
    theirs.virtual_id = vid;
    theirs.twin_tree_node = parent_node;
    theirs.twin_edge_idx = parent_edge_idx;

    b.parents[child_root.idx()] = parent_node;
}

fn expand_series(
    b: &mut Builder,
    macro_tree: &SpTree,
    m: SpNode,
    pole_a: NodeId,
    pole_b: NodeId,
    has_parent: bool,
) -> (TreeNodeId, u64) {
    let k = m.children_count as usize;

    let macro_left = NodeId(m.left);
    let macro_right = NodeId(m.right);
    let forward = pole_a == macro_left && pole_b == macro_right;
    let backward = pole_a == macro_right && pole_b == macro_left;

    assert!(
        forward || backward,
        "Series macro orientation mismatch: macro=({:?},{:?}), pole=({:?},{:?})",
        macro_left,
        macro_right,
        pole_a,
        pole_b
    );

    let child_order: Vec<usize> = if forward {
        (0..k).collect()
    } else {
        (0..k).rev().collect()
    };

    let mut chain_vertices: Vec<NodeId> = Vec::with_capacity(k + 1);
    chain_vertices.push(pole_a);

    let mut current = pole_a;

    for &ci in &child_order {
        let cref = macro_tree.children[(m.children_offset as usize) + ci];
        let (a, c) = child_endpoints(macro_tree, cref);

        let next = if a == current {
            c
        } else if c == current {
            a
        } else {
            panic!(
                "Series chain inconsistent at child idx {}: child endpoints ({:?}, {:?}) don't include current ({:?}). Pole_a={:?}, pole_b={:?}, k={}, forward={}",
                ci, a, c, current, pole_a, pole_b, k, forward
            );
        };

        chain_vertices.push(next);
        current = next;
    }

    debug_assert_eq!(
        current, pole_b,
        "Series chain ended at {:?}, expected {:?}",
        current, pole_b
    );

    if !has_parent {
        unimplemented!(
            "Root-level Series macro (fully-reducible block reduces to a chain) \
             not yet supported in expand_series"
        );
    }

    let num_skel_nodes = (k + 1) as u64;
    let mut edges: Vec<SkeletonEdge> = Vec::with_capacity(k + 1);

    for (i, &ci) in child_order.iter().enumerate() {
        let cref = macro_tree.children[(m.children_offset as usize) + ci];

        if child_is_macro(cref) {
            edges.push(SkeletonEdge {
                src: NodeId(i as u64),
                dst: NodeId((i + 1) as u64),
                real_edge: EdgeId::INVALID,
                virtual_id: INVALID,
                twin_tree_node: TreeNodeId::INVALID,
                twin_edge_idx: INVALID,
            });
        } else {
            edges.push(SkeletonEdge {
                src: NodeId(i as u64),
                dst: NodeId((i + 1) as u64),
                real_edge: child_as_edge(cref),
                virtual_id: INVALID,
                twin_tree_node: TreeNodeId::INVALID,
                twin_edge_idx: INVALID,
            });
        }
    }

    let virt_up_idx = edges.len() as u64;

    edges.push(SkeletonEdge {
        src: NodeId(k as u64),
        dst: NodeId(0u64),
        real_edge: EdgeId::INVALID,
        virtual_id: INVALID,
        twin_tree_node: TreeNodeId::INVALID,
        twin_edge_idx: INVALID,
    });

    let s_node = b.add_node(
        SpqrNodeType::S,
        num_skel_nodes,
        edges,
        chain_vertices.clone(),
    );

    for (i, &ci) in child_order.iter().enumerate() {
        let cref = macro_tree.children[(m.children_offset as usize) + ci];

        if !child_is_macro(cref) {
            continue;
        }

        let sub_macro_id = child_as_macro(cref);
        let chain_a = chain_vertices[i];
        let chain_b = chain_vertices[i + 1];

        let (child_root, virt_in_child) =
            expand_macro_subtree(b, macro_tree, sub_macro_id, chain_a, chain_b);

        debug_assert_eq!(
            macro_tree.macros[sub_macro_id as usize].kind, SP_KIND_PARALLEL,
            "Series macro should only have Atomic or Parallel children (alternation invariant)"
        );

        let vid = b.next_virtual_id();

        let our_edge = &mut b.edges[s_node.idx()][i];
        our_edge.virtual_id = vid;
        our_edge.twin_tree_node = child_root;
        our_edge.twin_edge_idx = virt_in_child;

        let theirs = &mut b.edges[child_root.idx()][virt_in_child as usize];
        theirs.virtual_id = vid;
        theirs.twin_tree_node = s_node;
        theirs.twin_edge_idx = i as u64;

        b.parents[child_root.idx()] = s_node;
    }

    (s_node, virt_up_idx)
}

fn expand_parallel(
    b: &mut Builder,
    macro_tree: &SpTree,
    m: SpNode,
    pole_a: NodeId,
    pole_b: NodeId,
    has_parent: bool,
) -> (TreeNodeId, u64) {
    let k = m.children_count as usize;

    let num_skel_nodes = 2u64;
    let n_edges = k + if has_parent { 1 } else { 0 };
    let mut edges: Vec<SkeletonEdge> = Vec::with_capacity(n_edges);

    for i in 0..k {
        let cref = macro_tree.children[(m.children_offset as usize) + i];

        if child_is_macro(cref) {
            edges.push(SkeletonEdge {
                src: NodeId(0),
                dst: NodeId(1),
                real_edge: EdgeId::INVALID,
                virtual_id: INVALID,
                twin_tree_node: TreeNodeId::INVALID,
                twin_edge_idx: INVALID,
            });
        } else {
            edges.push(SkeletonEdge {
                src: NodeId(0),
                dst: NodeId(1),
                real_edge: child_as_edge(cref),
                virtual_id: INVALID,
                twin_tree_node: TreeNodeId::INVALID,
                twin_edge_idx: INVALID,
            });
        }
    }

    let virt_up_idx = if has_parent {
        let idx = edges.len() as u64;

        edges.push(SkeletonEdge {
            src: NodeId(0),
            dst: NodeId(1),
            real_edge: EdgeId::INVALID,
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        });

        idx
    } else {
        INVALID
    };

    let p_node = b.add_node(SpqrNodeType::P, num_skel_nodes, edges, vec![pole_a, pole_b]);

    for i in 0..k {
        let cref = macro_tree.children[(m.children_offset as usize) + i];

        if !child_is_macro(cref) {
            continue;
        }

        let sub_macro_id = child_as_macro(cref);
        let (child_root, virt_in_child) =
            expand_macro_subtree(b, macro_tree, sub_macro_id, pole_a, pole_b);

        debug_assert_eq!(
            macro_tree.macros[sub_macro_id as usize].kind, SP_KIND_SERIES,
            "Parallel macro should only have Atomic or Series children (alternation invariant)"
        );

        let vid = b.next_virtual_id();

        let our_edge = &mut b.edges[p_node.idx()][i];
        our_edge.virtual_id = vid;
        our_edge.twin_tree_node = child_root;
        our_edge.twin_edge_idx = virt_in_child;

        let theirs = &mut b.edges[child_root.idx()][virt_in_child as usize];
        theirs.virtual_id = vid;
        theirs.twin_tree_node = p_node;
        theirs.twin_edge_idx = i as u64;

        b.parents[child_root.idx()] = p_node;
    }

    (p_node, virt_up_idx)
}

fn child_endpoints(macro_tree: &SpTree, c: ChildRef) -> (NodeId, NodeId) {
    if child_is_macro(c) {
        let m = macro_tree.macros[child_as_macro(c) as usize];
        (NodeId(m.left), NodeId(m.right))
    } else {
        let e = child_as_edge(c);
        let endpoints = macro_tree.input_endpoints[e.idx()];
        (NodeId(endpoints[0]), NodeId(endpoints[1]))
    }
}

struct Builder {
    types: Vec<SpqrNodeType>,
    parents: Vec<TreeNodeId>,
    skel_num_nodes: Vec<u64>,
    edges: Vec<Vec<SkeletonEdge>>,
    node_mapping: Vec<Vec<NodeId>>,
    next_vid: u64,
    absorbed: Vec<Option<TreeNodeId>>,
}

impl Builder {
    fn new(n_real_edges: usize) -> Self {
        Builder {
            types: Vec::new(),
            parents: Vec::new(),
            skel_num_nodes: Vec::new(),
            edges: Vec::new(),
            node_mapping: Vec::new(),
            next_vid: n_real_edges as u64,
            absorbed: Vec::new(),
        }
    }

    fn num_nodes(&self) -> usize {
        self.types.len()
    }

    fn next_virtual_id(&mut self) -> u64 {
        let v = self.next_vid;
        self.next_vid = self.next_vid.checked_add(1).expect("virtual_id overflow");
        v
    }

    fn add_node(
        &mut self,
        ty: SpqrNodeType,
        skel_num_nodes: u64,
        edges: Vec<SkeletonEdge>,
        node_mapping: Vec<NodeId>,
    ) -> TreeNodeId {
        let id = TreeNodeId(self.types.len() as u64);

        self.types.push(ty);
        self.parents.push(TreeNodeId::INVALID);
        self.skel_num_nodes.push(skel_num_nodes);
        self.edges.push(edges);
        self.node_mapping.push(node_mapping);

        id
    }

    fn compute_absorbed_into(&self) -> Vec<Option<TreeNodeId>> {
        let n = self.types.len();

        if n == 0 {
            return Vec::new();
        }

        let mut uf: Vec<u64> = (0..n as u64).collect();

        fn find(p: &mut [u64], x: u64) -> u64 {
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
            let p_id = self.parents[i];

            if !p_id.is_valid() || p_id.idx() == i {
                continue;
            }

            let pi = p_id.idx();

            let t = self.types[i];

            if t != SpqrNodeType::S && t != SpqrNodeType::P {
                continue;
            }
            if self.types[pi] != t {
                continue;
            }
            if self.edges[i].is_empty() || self.edges[pi].is_empty() {
                continue;
            }

            let r_x = find(&mut uf, i as u64);
            let r_p = find(&mut uf, pi as u64);

            if r_x != r_p {
                uf[r_x as usize] = r_p;
            }
        }

        let mut absorbed: Vec<Option<TreeNodeId>> = vec![None; n];

        for i in 0..n {
            let r = find(&mut uf, i as u64) as usize;

            if r != i {
                absorbed[i] = Some(TreeNodeId(r as u64));
            }
        }

        absorbed
    }

    fn normalize_in_place(&mut self) -> Vec<Option<TreeNodeId>> {
        let n = self.types.len();
        let absorbed_into = self.compute_absorbed_into();
        let any_absorbed = absorbed_into.iter().any(|a| a.is_some());

        if !any_absorbed {
            self.absorbed = absorbed_into.clone();
            return absorbed_into;
        }

        let mut group_by_rep: Vec<Vec<u64>> = vec![Vec::new(); n];

        for i in 0..n {
            if let Some(rep) = absorbed_into[i] {
                group_by_rep[rep.idx()].push(i as u64);
            }
        }

        for rep_idx in 0..n {
            if group_by_rep[rep_idx].is_empty() {
                continue;
            }

            let children = std::mem::take(&mut group_by_rep[rep_idx]);
            self.merge_chain_into(rep_idx, &children, &absorbed_into);
        }

        for i in 0..n {
            if absorbed_into[i].is_some() {
                continue;
            }

            let p = self.parents[i];

            if p.is_valid() {
                if let Some(rep) = absorbed_into[p.idx()] {
                    self.parents[i] = rep;
                }
            }
        }

        for tid in 0..n {
            if absorbed_into[tid].is_some() {
                continue;
            }

            for e in self.edges[tid].iter_mut() {
                if e.twin_tree_node.is_valid() {
                    let twin = e.twin_tree_node.idx();

                    if let Some(rep) = absorbed_into[twin] {
                        e.twin_tree_node = rep;
                    }
                }
            }
        }

        for i in 0..n {
            if absorbed_into[i].is_none() {
                self.skel_num_nodes[i] = self.node_mapping[i].len() as u64;
            }
        }

        self.absorbed = absorbed_into.clone();
        absorbed_into
    }

    fn merge_chain_into(
        &mut self,
        target: usize,
        absorbed_children: &[u64],
        absorbed_into: &[Option<TreeNodeId>],
    ) {
        let is_in_chain = |idx: usize| -> bool {
            idx == target || matches!(absorbed_into[idx], Some(rep) if rep.idx() == target)
        };

        let mut target_mapping = std::mem::take(&mut self.node_mapping[target]);
        let mut target_edges = std::mem::take(&mut self.edges[target]);

        target_edges
            .retain(|e| !(e.twin_tree_node.is_valid() && is_in_chain(e.twin_tree_node.idx())));

        let mut orig_to_local: std::collections::HashMap<u64, u64> = target_mapping
            .iter()
            .enumerate()
            .map(|(i, n)| (n.0, i as u64))
            .collect();

        for &child_u32 in absorbed_children {
            let child = child_u32 as usize;
            let child_mapping = std::mem::take(&mut self.node_mapping[child]);
            let child_edges = std::mem::take(&mut self.edges[child]);

            let mut remap: Vec<u64> = Vec::with_capacity(child_mapping.len());

            for &orig in &child_mapping {
                let new_local = *orig_to_local.entry(orig.0).or_insert_with(|| {
                    let idx = target_mapping.len() as u64;
                    target_mapping.push(orig);
                    idx
                });

                remap.push(new_local);
            }

            for mut e in child_edges {
                if e.twin_tree_node.is_valid() && is_in_chain(e.twin_tree_node.idx()) {
                    continue;
                }

                e.src = NodeId(remap[e.src.0 as usize]);
                e.dst = NodeId(remap[e.dst.0 as usize]);

                target_edges.push(e);
            }
        }

        self.node_mapping[target] = target_mapping;
        self.edges[target] = target_edges;
    }

    #[allow(dead_code)]
    fn merge_one_into(&mut self, child: usize, target: usize) {
        debug_assert_ne!(child, target);

        let child_mapping = std::mem::take(&mut self.node_mapping[child]);
        let mut target_mapping = std::mem::take(&mut self.node_mapping[target]);

        let mut orig_to_target_local: std::collections::HashMap<u64, u64> = target_mapping
            .iter()
            .enumerate()
            .map(|(i, n)| (n.0, i as u64))
            .collect();

        let mut remap: Vec<u64> = Vec::with_capacity(child_mapping.len());

        for &orig in &child_mapping {
            let new_local = *orig_to_target_local.entry(orig.0).or_insert_with(|| {
                let idx = target_mapping.len() as u64;
                target_mapping.push(orig);
                idx
            });

            remap.push(new_local);
        }

        self.node_mapping[target] = target_mapping;

        let child_edges = std::mem::take(&mut self.edges[child]);
        let mut target_edges = std::mem::take(&mut self.edges[target]);

        target_edges.retain(|e| !(e.twin_tree_node.is_valid() && e.twin_tree_node.idx() == child));

        for mut e in child_edges {
            if e.twin_tree_node.is_valid() && e.twin_tree_node.idx() == target {
                continue;
            }

            e.src = NodeId(remap[e.src.0 as usize]);
            e.dst = NodeId(remap[e.dst.0 as usize]);

            target_edges.push(e);
        }

        self.edges[target] = target_edges;
    }

    fn copy_t_core_node(
        &mut self,
        t_core: &SpqrTree,
        old: TreeNodeId,
        core_node_inv: &[NodeId],
    ) -> TreeNodeId {
        let ty = t_core.node_type(old);
        let skel_num = t_core.skeleton_num_nodes(old);
        let edges_src = t_core.skeleton_edges_slice(old);
        let mapping_src = t_core.node_mapping_slice(old);

        let mapping: Vec<NodeId> = mapping_src
            .iter()
            .map(|&n| {
                if n.is_valid() {
                    core_node_inv[n.idx()]
                } else {
                    NodeId::INVALID
                }
            })
            .collect();

        let edges: Vec<SkeletonEdge> = edges_src.to_vec();

        self.add_node(ty, skel_num, edges, mapping)
    }

    fn finalize(self, n_real_edges: usize) -> SpqrTree {
        let n_old = self.types.len();

        let absorbed_present =
            self.absorbed.len() == n_old && self.absorbed.iter().any(|a| a.is_some());

        let (old_to_new, n_new) = if absorbed_present {
            let mut otn: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n_old];
            let mut next_id: u64 = 0;

            for i in 0..n_old {
                if self.absorbed[i].is_none() {
                    otn[i] = TreeNodeId(next_id);
                    next_id += 1;
                }
            }

            for i in 0..n_old {
                if let Some(target) = self.absorbed[i] {
                    otn[i] = otn[target.idx()];
                }
            }

            (otn, next_id as usize)
        } else {
            let otn: Vec<TreeNodeId> = (0..n_old as u64).map(TreeNodeId).collect();
            (otn, n_old)
        };

        let is_survivor = |i: usize| -> bool {
            if !absorbed_present {
                true
            } else {
                self.absorbed[i].is_none()
            }
        };

        let mut child_count = vec![0u64; n_new];

        for i in 0..n_old {
            if !is_survivor(i) {
                continue;
            }

            let p = self.parents[i];

            if p.is_valid() {
                let p_new = old_to_new[p.idx()].idx();
                child_count[p_new] += 1;
            }
        }

        let mut children_offsets = Vec::with_capacity(n_new + 1);
        children_offsets.push(0u64);

        let mut acc = 0u64;

        for &c in &child_count {
            acc += c;
            children_offsets.push(acc);
        }

        let mut children = vec![TreeNodeId::INVALID; acc as usize];
        let mut next_slot = vec![0u64; n_new];

        for i in 0..n_old {
            if !is_survivor(i) {
                continue;
            }

            let p = self.parents[i];

            if p.is_valid() {
                let p_new = old_to_new[p.idx()].idx();
                let slot = children_offsets[p_new] + next_slot[p_new];

                children[slot as usize] = old_to_new[i];
                next_slot[p_new] += 1;
            }
        }

        let mut skeleton_offsets = Vec::with_capacity(n_new + 1);
        skeleton_offsets.push(0u64);

        let total_edges: usize = (0..n_old)
            .filter(|&i| is_survivor(i))
            .map(|i| self.edges[i].len())
            .sum();

        let mut skeleton_edges: Vec<SkeletonEdge> = Vec::with_capacity(total_edges);
        let mut min_real_per_node: Vec<u64> = Vec::with_capacity(n_new);

        for i in 0..n_old {
            if !is_survivor(i) {
                continue;
            }

            let mut local_min = u64::MAX;

            for e in &self.edges[i] {
                if e.real_edge.is_valid() && e.real_edge.0 < local_min {
                    local_min = e.real_edge.0;
                }
            }

            min_real_per_node.push(local_min);

            for e in &self.edges[i] {
                let mut e2 = *e;

                if e2.twin_tree_node.is_valid() {
                    e2.twin_tree_node = old_to_new[e2.twin_tree_node.idx()];
                }

                skeleton_edges.push(e2);
            }

            skeleton_offsets.push(skeleton_edges.len() as u64);
        }

        if absorbed_present {
            let vid_count = self.next_vid as usize;
            let empty = (u64::MAX, u64::MAX);
            let mut vid_to_edge: Vec<[(u64, u64); 2]> = vec![[empty; 2]; vid_count];

            for tn_new in 0..n_new {
                let s = skeleton_offsets[tn_new] as usize;
                let e = skeleton_offsets[tn_new + 1] as usize;

                for (local_idx, edge) in skeleton_edges[s..e].iter().enumerate() {
                    if edge.virtual_id != INVALID && edge.twin_tree_node.is_valid() {
                        let idx = edge.virtual_id as usize;

                        if idx < vid_to_edge.len() {
                            let pair = &mut vid_to_edge[idx];
                            let here = (tn_new as u64, local_idx as u64);
                            if pair[0] == empty {
                                pair[0] = here;
                            } else if pair[0] != here && pair[1] == empty {
                                pair[1] = here;
                            }
                        }
                    }
                }
            }

            for tn_new in 0..n_new {
                let s = skeleton_offsets[tn_new] as usize;
                let e = skeleton_offsets[tn_new + 1] as usize;

                for edge in skeleton_edges[s..e].iter_mut() {
                    if edge.virtual_id == INVALID || !edge.twin_tree_node.is_valid() {
                        continue;
                    }

                    let idx = edge.virtual_id as usize;

                    if idx < vid_to_edge.len() {
                        let pair = vid_to_edge[idx];
                        let twin = edge.twin_tree_node.0;
                        if pair[0].0 == twin {
                            edge.twin_edge_idx = pair[0].1;
                        } else if pair[1].0 == twin {
                            edge.twin_edge_idx = pair[1].1;
                        }
                    }
                }
            }
        }

        let mut node_mapping_offsets = Vec::with_capacity(n_new + 1);
        node_mapping_offsets.push(0u64);

        let total_nodes: usize = (0..n_old)
            .filter(|&i| is_survivor(i))
            .map(|i| self.node_mapping[i].len())
            .sum();

        let mut node_mapping: Vec<NodeId> = Vec::with_capacity(total_nodes);

        for i in 0..n_old {
            if !is_survivor(i) {
                continue;
            }

            node_mapping.extend_from_slice(&self.node_mapping[i]);
            node_mapping_offsets.push(node_mapping.len() as u64);
        }

        let mut edge_to_tree_node = vec![TreeNodeId::INVALID; n_real_edges];

        for i in 0..n_old {
            if !is_survivor(i) {
                continue;
            }

            let new_id = old_to_new[i];

            for e in &self.edges[i] {
                if e.real_edge.is_valid() {
                    let idx = e.real_edge.idx();

                    if idx < edge_to_tree_node.len() {
                        edge_to_tree_node[idx] = new_id;
                    }
                }
            }
        }

        let mut root = TreeNodeId::INVALID;

        for i in 0..n_old {
            if !is_survivor(i) {
                continue;
            }

            if !self.parents[i].is_valid() {
                let nid = old_to_new[i];

                if !root.is_valid() || nid.0 < root.0 {
                    root = nid;
                }
            }
        }

        let mut node_types: Vec<SpqrNodeType> = Vec::with_capacity(n_new);
        let mut node_parents: Vec<TreeNodeId> = Vec::with_capacity(n_new);
        let mut skeleton_num_nodes: Vec<u64> = Vec::with_capacity(n_new);

        for i in 0..n_old {
            if !is_survivor(i) {
                continue;
            }

            node_types.push(self.types[i]);

            let p = self.parents[i];

            node_parents.push(if p.is_valid() {
                old_to_new[p.idx()]
            } else {
                TreeNodeId::INVALID
            });

            skeleton_num_nodes.push(self.skel_num_nodes[i]);
        }

        SpqrTree {
            root,
            node_types,
            node_parents,
            children_offsets,
            children,
            skeleton_offsets,
            skeleton_edges,
            node_mapping_offsets,
            node_mapping,
            skeleton_num_nodes,
            edge_to_tree_node,
            min_real_per_node,
            preassembly_scad_export: None,
            preassembly_minimizer_sidecar: None,
        }
    }
}
