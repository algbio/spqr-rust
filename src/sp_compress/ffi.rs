#![allow(clippy::missing_safety_doc)]

use crate::sp_compress::direct::try_compress_degree2_direct_indexed;
use crate::sp_compress::direct_wide::try_compress_degree2_direct_indexed_u64;
use crate::sp_compress::integration::{
    build_core_spqr_parts_fast, core_edges_have_no_non_loop_parallel, CompressAndSpqrResult,
    CoreNodeMapper,
};
use crate::sp_compress::reduction::{
    compress_borrowed, compress_borrowed_timed, compress_borrowed_with_max_original_edge_id,
    CompressionTimings,
};
use crate::sp_compress::types::{ChildRef, CompressionStats, CoreEdge, InputEdge, SpNode, SpTree};
use crate::{EdgeId, NodeId, SkeletonEdge, SpqrNodeType, SpqrResult, SpqrTree, TreeNodeId};
use std::ptr;
use std::slice;
use std::time::Instant;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEdge64 {
    pub u: u64,
    pub v: u64,
    pub original_edge_id: u64,
}

#[inline]
fn checked_u64_to_u32(value: u64) -> Option<u32> {
    if value <= u32::MAX as u64 {
        Some(value as u32)
    } else {
        None
    }
}

#[inline]
fn checked_u64_to_usize(value: u64) -> Option<usize> {
    if value <= usize::MAX as u64 {
        Some(value as usize)
    } else {
        None
    }
}

unsafe fn ffi_slice<'a, T>(ptr: *const T, len: u64) -> Option<&'a [T]> {
    let len = checked_u64_to_usize(len)?;
    if ptr.is_null() && len != 0 {
        return None;
    }
    Some(if len == 0 {
        &[]
    } else {
        slice::from_raw_parts(ptr, len)
    })
}

unsafe fn wide_ffi_slices<'a>(
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
) -> Option<(&'a [InputEdge64], &'a [u8])> {
    Some((
        ffi_slice(edges_ptr, edges_len)?,
        ffi_slice(contractible_ptr, contractible_len)?,
    ))
}

unsafe fn wide_indexed_slices<'a>(
    src_ptr: *const u64,
    dst_ptr: *const u64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
) -> Option<(&'a [u64], &'a [u64], &'a [u8])> {
    Some((
        ffi_slice(src_ptr, edges_len)?,
        ffi_slice(dst_ptr, edges_len)?,
        ffi_slice(contractible_ptr, contractible_len)?,
    ))
}

fn downcast_u64_edges(n_nodes: u64, edges: &[InputEdge64]) -> Option<(u32, Vec<InputEdge>)> {
    let n32 = checked_u64_to_u32(n_nodes)?;
    let mut out = Vec::with_capacity(edges.len());
    for e in edges {
        out.push(InputEdge {
            u: NodeId(checked_u64_to_u32(e.u)?),
            v: NodeId(checked_u64_to_u32(e.v)?),
            original_edge_id: EdgeId(checked_u64_to_u32(e.original_edge_id)?),
        });
    }
    Some((n32, out))
}

fn wide_edges_from_ffi(edges: &[InputEdge64]) -> Option<Vec<crate::sp_compress::wide::InputEdge>> {
    let mut out = Vec::with_capacity(edges.len());
    for e in edges {
        if e.original_edge_id >= crate::sp_compress::wide::TAG_BIT {
            return None;
        }
        out.push(crate::sp_compress::wide::InputEdge {
            u: crate::wide::NodeId(e.u),
            v: crate::wide::NodeId(e.v),
            original_edge_id: crate::wide::EdgeId(e.original_edge_id),
        });
    }
    Some(out)
}

fn wide_edges_from_ffi_dense(
    edges: &[InputEdge64],
) -> Option<Vec<crate::sp_compress::wide::InputEdge>> {
    let mut out = Vec::with_capacity(edges.len());
    for (i, e) in edges.iter().enumerate() {
        if e.original_edge_id != i as u64 || e.original_edge_id >= crate::sp_compress::wide::TAG_BIT
        {
            return None;
        }
        out.push(crate::sp_compress::wide::InputEdge {
            u: crate::wide::NodeId(e.u),
            v: crate::wide::NodeId(e.v),
            original_edge_id: crate::wide::EdgeId(e.original_edge_id),
        });
    }
    Some(out)
}

fn wide_edges_from_arrays(
    src: &[u64],
    dst: &[u64],
) -> Option<Vec<crate::sp_compress::wide::InputEdge>> {
    if src.len() != dst.len() {
        return None;
    }
    let mut out = Vec::with_capacity(src.len());
    for i in 0..src.len() {
        if (i as u64) >= crate::sp_compress::wide::TAG_BIT {
            return None;
        }
        out.push(crate::sp_compress::wide::InputEdge {
            u: crate::wide::NodeId(src[i]),
            v: crate::wide::NodeId(dst[i]),
            original_edge_id: crate::wide::EdgeId(i as u64),
        });
    }
    Some(out)
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SpCompressTimings {
    pub t_compress_us: u64,
    pub t_build_spqr_core_us: u64,
    pub t_reconstruct_us: u64,
    pub t_normalize_us: u64,

    pub t_canonicalize_us: u64,
    pub t_canon_root_us: u64,
    pub t_canon_node_order_us: u64,
    pub t_canon_edge_orient_us: u64,
    pub t_canon_move_root_us: u64,

    pub t_reconstruct_build_builder_us: u64,
    pub t_reconstruct_normalize_in_place_us: u64,
    pub t_reconstruct_finalize_us: u64,
    pub t_reconstruct_defensive_normalize_us: u64,

    pub t_core_remap_us: u64,
    pub t_core_graph_build_us: u64,
    pub t_core_spqr_raw_us: u64,
    pub t_handle_wrap_us: u64,
    pub t_total_us: u64,

    pub t_compress_input_edges_us: u64,
    pub t_compress_init_work_us: u64,
    pub t_compress_init_dirty_us: u64,
    pub t_compress_reduce_series_us: u64,
    pub t_compress_reduce_parallel_us: u64,
    pub t_compress_materialize_us: u64,
    pub t_compress_cleanup_us: u64,
    pub t_compress_canon_series_us: u64,
    pub t_compress_sort_core_edges_us: u64,
    pub t_compress_collect_core_nodes_us: u64,
    pub t_compress_stats_shrink_us: u64,

    pub t_spqr_self_loop_scan_us: u64,
    pub t_spqr_precheck_us: u64,
    pub t_spqr_split_multi_edges_us: u64,
    pub t_spqr_work_graph_us: u64,
    pub t_spqr_triconn_us: u64,
    pub t_spqr_relabel_us: u64,
    pub t_spqr_combine_us: u64,
    pub t_spqr_merge_us: u64,
    pub t_spqr_assemble_us: u64,
    pub t_spqr_tree_total_us: u64,

    pub c_spqr_multi_components: u64,
    pub c_spqr_triconn_components: u64,
    pub c_spqr_precombine_components: u64,
    pub c_spqr_combined_components: u64,
    pub c_spqr_merged_components: u64,
    pub c_spqr_merged_real_edges: u64,
    pub c_spqr_merged_virtual_incidences: u64,
    pub c_spqr_virtual_id_span: u64,
    pub c_spqr_tree_nodes: u64,
    pub c_spqr_tree_edges: u64,
    pub c_spqr_tree_skeleton_edges: u64,
    pub c_spqr_tree_virtual_incidences: u64,
}

fn fill_production_reconstruct_timings(
    timings: &mut SpCompressTimings,
    rt: crate::sp_compress::reconstruct::ReconstructTimings,
) {
    timings.t_reconstruct_build_builder_us = rt.t_build_builder_us;
    timings.t_reconstruct_normalize_in_place_us = rt.t_normalize_in_place_us;
    timings.t_reconstruct_finalize_us = rt.t_finalize_us;
    timings.t_reconstruct_defensive_normalize_us = rt.t_defensive_normalize_us;

    timings.t_reconstruct_us =
        rt.t_build_builder_us + rt.t_finalize_us + rt.t_defensive_normalize_us;
    timings.t_normalize_us = rt.t_normalize_in_place_us;

    timings.t_canon_root_us = rt.t_canon_root_us;
    timings.t_canon_node_order_us = rt.t_canon_node_order_us;
    timings.t_canon_edge_orient_us = rt.t_canon_edge_orient_us;
    timings.t_canon_move_root_us = rt.t_canon_move_root_us;

    timings.t_canonicalize_us = rt.t_canon_root_us
        + rt.t_canon_node_order_us
        + rt.t_canon_edge_orient_us
        + rt.t_canon_move_root_us;
}

fn fill_wide_production_reconstruct_timings(
    timings: &mut SpCompressTimings,
    rt: crate::sp_compress::reconstruct_wide::ReconstructTimings,
) {
    timings.t_reconstruct_build_builder_us = rt.t_build_builder_us;
    timings.t_reconstruct_normalize_in_place_us = rt.t_normalize_in_place_us;
    timings.t_reconstruct_finalize_us = rt.t_finalize_us;
    timings.t_reconstruct_defensive_normalize_us = rt.t_defensive_normalize_us;

    timings.t_reconstruct_us =
        rt.t_build_builder_us + rt.t_finalize_us + rt.t_defensive_normalize_us;
    timings.t_normalize_us = rt.t_normalize_in_place_us;

    timings.t_canon_root_us = rt.t_canon_root_us;
    timings.t_canon_node_order_us = rt.t_canon_node_order_us;
    timings.t_canon_edge_orient_us = rt.t_canon_edge_orient_us;
    timings.t_canon_move_root_us = rt.t_canon_move_root_us;

    timings.t_canonicalize_us = rt.t_canon_root_us
        + rt.t_canon_node_order_us
        + rt.t_canon_edge_orient_us
        + rt.t_canon_move_root_us;
}

fn fill_compression_timings(timings: &mut SpCompressTimings, ct: CompressionTimings) {
    timings.t_compress_input_edges_us = ct.t_input_edges_us;
    timings.t_compress_init_work_us = ct.t_init_work_us;
    timings.t_compress_init_dirty_us = ct.t_init_dirty_us;
    timings.t_compress_reduce_series_us = ct.t_reduce_series_us;
    timings.t_compress_reduce_parallel_us = ct.t_reduce_parallel_us;
    timings.t_compress_materialize_us = ct.t_materialize_us;
    timings.t_compress_cleanup_us = ct.t_cleanup_us;
    timings.t_compress_canon_series_us = ct.t_canon_series_us;
    timings.t_compress_sort_core_edges_us = ct.t_sort_core_edges_us;
    timings.t_compress_collect_core_nodes_us = ct.t_collect_core_nodes_us;
    timings.t_compress_stats_shrink_us = ct.t_stats_shrink_us;
}

#[repr(C)]
pub struct MacroTreeFfi {
    pub macros_ptr: *const SpNode,
    pub macros_len: u64,
    pub children_ptr: *const ChildRef,
    pub children_len: u64,
    pub core_edges_ptr: *const CoreEdge,
    pub core_edges_len: u64,

    pub core_nodes_ptr: *const u32,
    pub core_nodes_len: u64,

    pub input_endpoints_ptr: *const u32,
    pub input_endpoints_len: u64,
    pub stats: CompressionStats,
}

#[repr(C)]
pub struct CoreSpqrSnapshotFfi {
    pub root: u32,
    pub node_count: u32,

    pub node_types_ptr: *const u8,
    pub node_parents_ptr: *const u32,

    pub children_offsets_ptr: *const u32,
    pub children_offsets_len: u32,
    pub children_ptr: *const u32,
    pub children_len: u32,

    pub skeleton_offsets_ptr: *const u32,
    pub skeleton_offsets_len: u32,
    pub skeleton_edges_ptr: *const SkeletonEdge,
    pub skeleton_edges_len: u32,

    pub node_mapping_offsets_ptr: *const u32,
    pub node_mapping_offsets_len: u32,
    pub node_mapping_ptr: *const u32,
    pub node_mapping_len: u32,

    pub skeleton_num_nodes_ptr: *const u32,
    pub skeleton_num_nodes_len: u32,
}

#[repr(C)]
pub struct SpCompressSnapshotFfi {
    pub macros_ptr: *const SpNode,
    pub macros_len: u64,
    pub children_ptr: *const ChildRef,
    pub children_len: u64,
    pub core_edges_ptr: *const CoreEdge,
    pub core_edges_len: u64,
    pub core_nodes_ptr: *const u32,
    pub core_nodes_len: u64,
    pub input_endpoints_ptr: *const u32,
    pub input_endpoints_len: u64,
    pub stats: CompressionStats,
    pub core_spqr: *const CoreSpqrSnapshotFfi,
    pub core_node_inv_ptr: *const u32,
    pub core_node_inv_len: u32,
}

#[repr(C)]
pub struct MacroTreeFfi64 {
    pub macros_ptr: *const crate::sp_compress::wide::SpNode,
    pub macros_len: u64,
    pub children_ptr: *const crate::sp_compress::wide::ChildRef,
    pub children_len: u64,
    pub core_edges_ptr: *const crate::sp_compress::wide::CoreEdge,
    pub core_edges_len: u64,

    pub core_nodes_ptr: *const u64,
    pub core_nodes_len: u64,

    pub input_endpoints_ptr: *const u64,
    pub input_endpoints_len: u64,
    pub stats: crate::sp_compress::wide::CompressionStats,
}

impl MacroTreeFfi64 {
    fn empty() -> Self {
        Self {
            macros_ptr: ptr::null(),
            macros_len: 0,
            children_ptr: ptr::null(),
            children_len: 0,
            core_edges_ptr: ptr::null(),
            core_edges_len: 0,
            core_nodes_ptr: ptr::null(),
            core_nodes_len: 0,
            input_endpoints_ptr: ptr::null(),
            input_endpoints_len: 0,
            stats: crate::sp_compress::wide::CompressionStats::default(),
        }
    }
}

fn make_wide_spqr_result(
    macro_tree: crate::sp_compress::wide::SpTree,
    core_spqr: Option<crate::wide::SpqrResult>,
    core_node_inv: Vec<crate::wide::NodeId>,
) -> CompressAndWideSpqrResult {
    CompressAndWideSpqrResult {
        macro_tree,
        core_spqr,
        core_node_inv,
    }
}

pub struct CompressAndWideSpqrResult {
    pub macro_tree: crate::sp_compress::wide::SpTree,
    pub core_spqr: Option<crate::wide::SpqrResult>,
    pub core_node_inv: Vec<crate::wide::NodeId>,
}

#[inline]
fn core_edges_have_no_non_loop_parallel_wide(
    macro_tree: &crate::sp_compress::wide::SpTree,
) -> bool {
    let mut prev: Option<(u64, u64)> = None;
    for ce in &macro_tree.core_edges {
        if ce.u == ce.v {
            continue;
        }
        let key = if ce.u <= ce.v {
            (ce.u, ce.v)
        } else {
            (ce.v, ce.u)
        };
        if let Some(prev_key) = prev {
            if key <= prev_key {
                return false;
            }
        }
        prev = Some(key);
    }
    true
}

fn build_core_spqr_wide_timed(
    macro_tree: &crate::sp_compress::wide::SpTree,
    timings: Option<&mut SpCompressTimings>,
) -> (Option<crate::wide::SpqrResult>, Vec<crate::wide::NodeId>) {
    if macro_tree.stats.fully_sp_reducible != 0 || macro_tree.core_edges.is_empty() {
        return (None, Vec::new());
    }

    let mut timings = timings;
    let t_graph = Instant::now();
    let inv = macro_tree.core_nodes.clone();
    let mut remap = std::collections::HashMap::with_capacity(inv.len().saturating_mul(2));
    for (idx, node) in inv.iter().enumerate() {
        remap.insert(node.0, idx as u64);
    }

    let mut graph = crate::wide::Graph::with_capacity(inv.len(), macro_tree.core_edges.len());
    graph.add_nodes_fast(inv.len());
    let mut has_self_loop = false;
    for ce in &macro_tree.core_edges {
        let Some(&u_remap) = remap.get(&ce.u) else {
            return (None, inv);
        };
        let Some(&v_remap) = remap.get(&ce.v) else {
            return (None, inv);
        };
        has_self_loop |= u_remap == v_remap;
        graph.add_edge(crate::wide::NodeId(u_remap), crate::wide::NodeId(v_remap));
    }
    if let Some(t) = timings.as_mut() {
        t.t_core_graph_build_us = t_graph.elapsed().as_micros() as u64;
    }

    let t_spqr = Instant::now();
    let no_non_loop_parallel = core_edges_have_no_non_loop_parallel_wide(macro_tree);
    let (spqr, st) = if no_non_loop_parallel {
        crate::wide::build_spqr_raw_no_multi_edges_timed(&graph)
    } else if has_self_loop {
        crate::wide::build_spqr_raw_timed(&graph)
    } else {
        crate::wide::build_spqr_raw_no_self_loops_timed(&graph)
    };
    if let Some(t) = timings.as_mut() {
        t.t_core_spqr_raw_us = t_spqr.elapsed().as_micros() as u64;
        t.t_spqr_self_loop_scan_us = st.t_self_loop_scan_us;
        t.t_spqr_precheck_us = st.t_precheck_us;
        t.t_spqr_split_multi_edges_us = st.t_split_multi_edges_us;
        t.t_spqr_work_graph_us = st.t_work_graph_us;
        t.t_spqr_triconn_us = st.t_triconn_us;
        t.t_spqr_relabel_us = st.t_relabel_us;
        t.t_spqr_combine_us = st.t_combine_us;
        t.t_spqr_merge_us = st.t_merge_us;
        t.t_spqr_assemble_us = st.t_assemble_us;
        t.t_spqr_tree_total_us = st.t_tree_total_us;
        t.c_spqr_multi_components = st.c_multi_components;
        t.c_spqr_triconn_components = st.c_triconn_components;
        t.c_spqr_precombine_components = st.c_precombine_components;
        t.c_spqr_combined_components = st.c_combined_components;
        t.c_spqr_merged_components = st.c_merged_components;
        t.c_spqr_merged_real_edges = st.c_merged_real_edges;
        t.c_spqr_merged_virtual_incidences = st.c_merged_virtual_incidences;
        t.c_spqr_virtual_id_span = st.c_virtual_id_span;
        t.c_spqr_tree_nodes = st.c_tree_nodes;
        t.c_spqr_tree_edges = st.c_tree_edges;
        t.c_spqr_tree_skeleton_edges = st.c_tree_skeleton_edges;
        t.c_spqr_tree_virtual_incidences = st.c_tree_virtual_incidences;
    }

    (Some(spqr), inv)
}

fn box_wide_tree(
    macro_tree: crate::sp_compress::wide::SpTree,
    success: bool,
    build_core_spqr: u8,
    mut timings: Option<&mut SpCompressTimings>,
) -> *mut SpCompressHandle {
    let handle = if build_core_spqr != 0 {
        let t0 = Instant::now();
        let (core_spqr, core_node_inv) =
            build_core_spqr_wide_timed(&macro_tree, timings.as_deref_mut());
        if let Some(t) = timings.as_mut() {
            t.t_build_spqr_core_us = t0.elapsed().as_micros() as u64;
        }
        SpCompressHandle::WideWithSpqr(Box::new(make_wide_spqr_result(
            macro_tree,
            core_spqr,
            core_node_inv,
        )))
    } else {
        SpCompressHandle::WidePlainTree {
            tree: macro_tree,
            success,
        }
    };
    Box::into_raw(Box::new(handle))
}

fn make_wide_handle(
    n_nodes: u64,
    edges_slice: &[InputEdge64],
    contr_slice: &[u8],
    build_core_spqr: u8,
    mut timings: Option<&mut SpCompressTimings>,
) -> *mut SpCompressHandle {
    let Some(wide_edges) = wide_edges_from_ffi(edges_slice) else {
        return ptr::null_mut();
    };
    let t0 = Instant::now();
    let cr =
        crate::sp_compress::wide::compress_borrowed_remapped(n_nodes, &wide_edges, contr_slice);
    if let Some(t) = timings.as_mut() {
        t.t_compress_us = t0.elapsed().as_micros() as u64;
    }
    box_wide_tree(cr.tree, cr.success, build_core_spqr, timings)
}

fn make_wide_indexed_handle(
    n_nodes: u64,
    src_slice: &[u64],
    dst_slice: &[u64],
    contr_slice: &[u8],
    build_core_spqr: u8,
) -> *mut SpCompressHandle {
    let Some(wide_edges) = wide_edges_from_arrays(src_slice, dst_slice) else {
        return ptr::null_mut();
    };
    let cr =
        crate::sp_compress::wide::compress_borrowed_remapped(n_nodes, &wide_edges, contr_slice);
    box_wide_tree(cr.tree, cr.success, build_core_spqr, None)
}

pub enum SpCompressHandle {
    PlainTree {
        tree: SpTree,
        success: bool,
    },
    WithSpqr(Box<CompressAndSpqrResult>),
    WidePlainTree {
        tree: crate::sp_compress::wide::SpTree,
        success: bool,
    },
    WideWithSpqr(Box<CompressAndWideSpqrResult>),
}

impl SpCompressHandle {
    fn tree(&self) -> Option<&SpTree> {
        match self {
            SpCompressHandle::PlainTree { tree, .. } => Some(tree),
            SpCompressHandle::WithSpqr(r) => Some(&r.macro_tree),
            _ => None,
        }
    }

    fn wide_tree(&self) -> Option<&crate::sp_compress::wide::SpTree> {
        match self {
            SpCompressHandle::WidePlainTree { tree, .. } => Some(tree),
            SpCompressHandle::WideWithSpqr(r) => Some(&r.macro_tree),
            _ => None,
        }
    }

    fn success(&self) -> bool {
        match self {
            SpCompressHandle::PlainTree { success, .. } => *success,
            SpCompressHandle::WithSpqr(_) => true,
            SpCompressHandle::WidePlainTree { success, .. } => *success,
            SpCompressHandle::WideWithSpqr(_) => true,
        }
    }
}

#[inline(always)]
fn build_core_spqr_timed(
    n_nodes: u32,
    macro_tree: &SpTree,
    timings: &mut SpCompressTimings,
    fill_spqr_timings: bool,
) -> (Option<crate::SpqrResult>, Vec<u32>, Vec<NodeId>) {
    if macro_tree.stats.fully_sp_reducible != 0 || macro_tree.core_edges.is_empty() {
        return (None, Vec::new(), Vec::new());
    }

    let t_remap = Instant::now();
    let inv: &[NodeId] = &macro_tree.core_nodes;
    let mapper = CoreNodeMapper::new(n_nodes as usize, inv);

    timings.t_core_remap_us = t_remap.elapsed().as_micros() as u64;

    let t_graph = Instant::now();
    let n_core = inv.len();
    let m_core = macro_tree.core_edges.len();

    let mut graph = crate::Graph::with_capacity(n_core, m_core);
    graph.add_nodes_fast(n_core);

    let mut has_self_loop = false;
    for ce in &macro_tree.core_edges {
        let u_remap = mapper.lookup(inv, ce.u);
        let v_remap = mapper.lookup(inv, ce.v);
        has_self_loop |= u_remap == v_remap;
        graph.add_edge(NodeId(u_remap), NodeId(v_remap));
    }

    timings.t_core_graph_build_us = t_graph.elapsed().as_micros() as u64;

    let t_spqr = Instant::now();
    let no_non_loop_parallel = core_edges_have_no_non_loop_parallel(macro_tree);
    let spqr = if fill_spqr_timings {
        let (spqr, st) = if no_non_loop_parallel {
            crate::build_spqr_raw_no_multi_edges_timed(&graph)
        } else if has_self_loop {
            crate::build_spqr_raw_timed(&graph)
        } else {
            crate::build_spqr_raw_no_self_loops_timed(&graph)
        };
        timings.t_spqr_self_loop_scan_us = st.t_self_loop_scan_us;
        timings.t_spqr_precheck_us = st.t_precheck_us;
        timings.t_spqr_split_multi_edges_us = st.t_split_multi_edges_us;
        timings.t_spqr_work_graph_us = st.t_work_graph_us;
        timings.t_spqr_triconn_us = st.t_triconn_us;
        timings.t_spqr_relabel_us = st.t_relabel_us;
        timings.t_spqr_combine_us = st.t_combine_us;
        timings.t_spqr_merge_us = st.t_merge_us;
        timings.t_spqr_assemble_us = st.t_assemble_us;
        timings.t_spqr_tree_total_us = st.t_tree_total_us;
        timings.c_spqr_multi_components = st.c_multi_components;
        timings.c_spqr_triconn_components = st.c_triconn_components;
        timings.c_spqr_precombine_components = st.c_precombine_components;
        timings.c_spqr_combined_components = st.c_combined_components;
        timings.c_spqr_merged_components = st.c_merged_components;
        timings.c_spqr_merged_real_edges = st.c_merged_real_edges;
        timings.c_spqr_merged_virtual_incidences = st.c_merged_virtual_incidences;
        timings.c_spqr_virtual_id_span = st.c_virtual_id_span;
        timings.c_spqr_tree_nodes = st.c_tree_nodes;
        timings.c_spqr_tree_edges = st.c_tree_edges;
        timings.c_spqr_tree_skeleton_edges = st.c_tree_skeleton_edges;
        timings.c_spqr_tree_virtual_incidences = st.c_tree_virtual_incidences;
        spqr
    } else if no_non_loop_parallel {
        crate::build_spqr_raw_no_multi_edges(&graph)
    } else if has_self_loop {
        crate::build_spqr_raw(&graph)
    } else {
        crate::build_spqr_raw_no_self_loops(&graph)
    };
    timings.t_core_spqr_raw_us = t_spqr.elapsed().as_micros() as u64;

    (Some(spqr), Vec::new(), Vec::new())
}

fn copy_slice<T: Copy>(ptr: *const T, len: usize) -> Option<Vec<T>> {
    if len == 0 {
        return Some(Vec::new());
    }
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { slice::from_raw_parts(ptr, len) }.to_vec())
}

fn spqr_tree_from_snapshot(core: &CoreSpqrSnapshotFfi, num_core_edges: usize) -> Option<SpqrTree> {
    let n = core.node_count as usize;
    if n == 0 {
        return None;
    }
    if core.root as usize >= n {
        return None;
    }
    if core.children_offsets_len as usize != n + 1
        || core.skeleton_offsets_len as usize != n + 1
        || core.node_mapping_offsets_len as usize != n + 1
        || core.skeleton_num_nodes_len as usize != n
    {
        return None;
    }

    let type_bytes = copy_slice(core.node_types_ptr, n)?;
    let mut node_types = Vec::with_capacity(n);
    for value in type_bytes {
        node_types.push(match value {
            crate::ffi::SPQR_NODE_TYPE_S => SpqrNodeType::S,
            crate::ffi::SPQR_NODE_TYPE_P => SpqrNodeType::P,
            crate::ffi::SPQR_NODE_TYPE_R => SpqrNodeType::R,
            _ => return None,
        });
    }

    let node_parents_raw = copy_slice(core.node_parents_ptr, n)?;
    let node_parents: Vec<TreeNodeId> = node_parents_raw.into_iter().map(TreeNodeId).collect();
    let children_offsets = copy_slice(core.children_offsets_ptr, n + 1)?;
    let children_raw = copy_slice(core.children_ptr, core.children_len as usize)?;
    let children: Vec<TreeNodeId> = children_raw.into_iter().map(TreeNodeId).collect();
    let skeleton_offsets = copy_slice(core.skeleton_offsets_ptr, n + 1)?;
    let skeleton_edges = copy_slice(core.skeleton_edges_ptr, core.skeleton_edges_len as usize)?;
    let node_mapping_offsets = copy_slice(core.node_mapping_offsets_ptr, n + 1)?;
    let node_mapping_raw = copy_slice(core.node_mapping_ptr, core.node_mapping_len as usize)?;
    let node_mapping: Vec<NodeId> = node_mapping_raw.into_iter().map(NodeId).collect();
    let skeleton_num_nodes = copy_slice(core.skeleton_num_nodes_ptr, n)?;

    if children_offsets.last().copied().unwrap_or(0) as usize != children.len()
        || skeleton_offsets.last().copied().unwrap_or(0) as usize != skeleton_edges.len()
        || node_mapping_offsets.last().copied().unwrap_or(0) as usize != node_mapping.len()
    {
        return None;
    }

    let mut edge_to_tree_node = vec![TreeNodeId::INVALID; num_core_edges];
    let mut min_real_per_node = vec![u32::MAX; n];
    for tn in 0..n {
        let start = skeleton_offsets[tn] as usize;
        let end = skeleton_offsets[tn + 1] as usize;
        if start > end || end > skeleton_edges.len() {
            return None;
        }
        for edge in &skeleton_edges[start..end] {
            if edge.real_edge.is_valid() {
                let eidx = edge.real_edge.idx();
                if eidx < edge_to_tree_node.len() {
                    edge_to_tree_node[eidx] = TreeNodeId(tn as u32);
                }
                if edge.real_edge.0 < min_real_per_node[tn] {
                    min_real_per_node[tn] = edge.real_edge.0;
                }
            }
        }
    }

    Some(SpqrTree {
        root: TreeNodeId(core.root),
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
    })
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_from_snapshot_ffi(
    snapshot: *const SpCompressSnapshotFfi,
) -> *mut SpCompressHandle {
    if snapshot.is_null() {
        return ptr::null_mut();
    }
    let snap = unsafe { &*snapshot };

    let macros = match copy_slice(snap.macros_ptr, snap.macros_len as usize) {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    let children = match copy_slice(snap.children_ptr, snap.children_len as usize) {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    let core_edges = match copy_slice(snap.core_edges_ptr, snap.core_edges_len as usize) {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    let core_nodes_raw = match copy_slice(snap.core_nodes_ptr, snap.core_nodes_len as usize) {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    let input_endpoint_raw =
        match copy_slice(snap.input_endpoints_ptr, snap.input_endpoints_len as usize) {
            Some(v) => v,
            None => return ptr::null_mut(),
        };
    if input_endpoint_raw.len() % 2 != 0 {
        return ptr::null_mut();
    }
    let input_endpoints: Vec<[u32; 2]> =
        input_endpoint_raw.chunks(2).map(|p| [p[0], p[1]]).collect();

    let mut tree = SpTree {
        macros,
        children,
        core_edges,
        core_nodes: core_nodes_raw.into_iter().map(NodeId).collect(),
        input_endpoints,
        stats: snap.stats,
    };
    tree.update_stats();
    tree.stats.input_edges = tree.input_endpoints.len() as u32;
    tree.stats.input_nodes = snap.stats.input_nodes;
    tree.stats.series_reductions = snap.stats.series_reductions;
    tree.stats.parallel_reductions = snap.stats.parallel_reductions;
    tree.stats.iterations = snap.stats.iterations;
    tree.stats.fully_sp_reducible = snap.stats.fully_sp_reducible;

    let core_spqr = if snap.core_spqr.is_null() {
        None
    } else {
        match spqr_tree_from_snapshot(unsafe { &*snap.core_spqr }, tree.core_edges.len()) {
            Some(tree) => Some(SpqrResult {
                tree,
                self_loops: Vec::new(),
            }),
            None => return ptr::null_mut(),
        }
    };

    let core_node_inv_raw =
        match copy_slice(snap.core_node_inv_ptr, snap.core_node_inv_len as usize) {
            Some(v) => v,
            None => return ptr::null_mut(),
        };
    let core_node_inv: Vec<NodeId> = core_node_inv_raw.into_iter().map(NodeId).collect();
    let result = CompressAndSpqrResult::from_parts(tree, core_spqr, Vec::new(), core_node_inv);
    Box::into_raw(Box::new(SpCompressHandle::WithSpqr(Box::new(result))))
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_ffi(
    n_nodes: u32,
    edges_ptr: *const InputEdge,
    edges_len: u32,
    max_original_edge_id: u32,
    contractible_ptr: *const u8,
    contractible_len: u32,
    build_core_spqr: u8,
) -> *mut SpCompressHandle {
    if edges_ptr.is_null() && edges_len > 0 {
        return ptr::null_mut();
    }
    if contractible_ptr.is_null() && contractible_len > 0 {
        return ptr::null_mut();
    }
    if (contractible_len as u64) < (n_nodes as u64) {
        return ptr::null_mut();
    }

    let edges_slice: &[InputEdge] = if edges_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(edges_ptr, edges_len as usize)
    };
    let contr_slice: &[u8] = if contractible_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(contractible_ptr, contractible_len as usize)
    };

    let handle = if build_core_spqr != 0 {
        let cr = compress_borrowed_with_max_original_edge_id(
            n_nodes,
            edges_slice,
            contr_slice,
            max_original_edge_id,
        );
        let macro_tree = cr.tree;
        let (core_spqr, core_node_remap, core_node_inv) =
            build_core_spqr_parts_fast(n_nodes, &macro_tree);
        let r = CompressAndSpqrResult::from_parts(
            macro_tree,
            core_spqr,
            core_node_remap,
            core_node_inv,
        );
        SpCompressHandle::WithSpqr(Box::new(r))
    } else {
        let r = compress_borrowed_with_max_original_edge_id(
            n_nodes,
            edges_slice,
            contr_slice,
            max_original_edge_id,
        );
        SpCompressHandle::PlainTree {
            tree: r.tree,
            success: r.success,
        }
    };

    Box::into_raw(Box::new(handle))
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_ffi64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
    build_core_spqr: u8,
) -> *mut SpCompressHandle {
    let Some((edges_slice, contr_slice)) =
        wide_ffi_slices(edges_ptr, edges_len, contractible_ptr, contractible_len)
    else {
        return ptr::null_mut();
    };

    if contractible_len >= n_nodes {
        if let (Some((n32, downcast_edges)), Some(contractible_len32)) = (
            downcast_u64_edges(n_nodes, edges_slice),
            checked_u64_to_u32(contractible_len),
        ) {
            return sp_compress_ffi(
                n32,
                downcast_edges.as_ptr(),
                downcast_edges.len() as u32,
                downcast_edges
                    .iter()
                    .map(|edge| edge.original_edge_id.0)
                    .max()
                    .unwrap_or(0),
                contractible_ptr,
                contractible_len32,
                build_core_spqr,
            );
        }
    }

    make_wide_handle(n_nodes, edges_slice, contr_slice, build_core_spqr, None)
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_ffi_u64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
    build_core_spqr: u8,
) -> *mut SpCompressHandle {
    let Some((edges_slice, contr_slice)) =
        wide_ffi_slices(edges_ptr, edges_len, contractible_ptr, contractible_len)
    else {
        return ptr::null_mut();
    };
    make_wide_handle(n_nodes, edges_slice, contr_slice, build_core_spqr, None)
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_timed_ffi(
    n_nodes: u32,
    edges_ptr: *const InputEdge,
    edges_len: u32,
    _max_original_edge_id: u32,
    contractible_ptr: *const u8,
    contractible_len: u32,
    build_core_spqr: u8,
    out_timings: *mut SpCompressTimings,
) -> *mut SpCompressHandle {
    if edges_ptr.is_null() && edges_len > 0 {
        return ptr::null_mut();
    }
    if contractible_ptr.is_null() && contractible_len > 0 {
        return ptr::null_mut();
    }
    if (contractible_len as u64) < (n_nodes as u64) {
        return ptr::null_mut();
    }

    let edges_slice: &[InputEdge] = if edges_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(edges_ptr, edges_len as usize)
    };
    let contr_slice: &[u8] = if contractible_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(contractible_ptr, contractible_len as usize)
    };

    let total_t0 = Instant::now();
    let mut timings = SpCompressTimings::default();

    let handle = if build_core_spqr != 0 {
        let t0 = Instant::now();
        let (cr, ct) = compress_borrowed_timed(n_nodes, edges_slice, contr_slice);
        let macro_tree = cr.tree;
        timings.t_compress_us = t0.elapsed().as_micros() as u64;
        fill_compression_timings(&mut timings, ct);

        let core_total_t0 = Instant::now();
        let (core_spqr, core_node_remap, core_node_inv) =
            build_core_spqr_timed(n_nodes, &macro_tree, &mut timings, true);

        timings.t_build_spqr_core_us = core_total_t0.elapsed().as_micros() as u64;

        let t_wrap = Instant::now();
        let h = SpCompressHandle::WithSpqr(Box::new(CompressAndSpqrResult::from_parts(
            macro_tree,
            core_spqr,
            core_node_remap,
            core_node_inv,
        )));
        timings.t_handle_wrap_us = t_wrap.elapsed().as_micros() as u64;
        h
    } else {
        let t0 = Instant::now();
        let (r, ct) = compress_borrowed_timed(n_nodes, edges_slice, contr_slice);

        timings.t_compress_us = t0.elapsed().as_micros() as u64;
        fill_compression_timings(&mut timings, ct);

        let t_wrap = Instant::now();
        let h = SpCompressHandle::PlainTree {
            tree: r.tree,
            success: r.success,
        };
        timings.t_handle_wrap_us = t_wrap.elapsed().as_micros() as u64;
        h
    };

    timings.t_total_us = total_t0.elapsed().as_micros() as u64;

    if !out_timings.is_null() {
        *out_timings = timings;
    }

    Box::into_raw(Box::new(handle))
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_timed_ffi64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
    build_core_spqr: u8,
    out_timings: *mut SpCompressTimings,
) -> *mut SpCompressHandle {
    let Some((edges_slice, contr_slice)) =
        wide_ffi_slices(edges_ptr, edges_len, contractible_ptr, contractible_len)
    else {
        return ptr::null_mut();
    };

    if contractible_len >= n_nodes {
        if let (Some((n32, downcast_edges)), Some(contractible_len32)) = (
            downcast_u64_edges(n_nodes, edges_slice),
            checked_u64_to_u32(contractible_len),
        ) {
            return sp_compress_timed_ffi(
                n32,
                downcast_edges.as_ptr(),
                downcast_edges.len() as u32,
                downcast_edges
                    .iter()
                    .map(|edge| edge.original_edge_id.0)
                    .max()
                    .unwrap_or(0),
                contractible_ptr,
                contractible_len32,
                build_core_spqr,
                out_timings,
            );
        }
    }

    let total_t0 = Instant::now();
    let mut timings = SpCompressTimings::default();
    let handle = make_wide_handle(
        n_nodes,
        edges_slice,
        contr_slice,
        build_core_spqr,
        Some(&mut timings),
    );
    timings.t_total_us = total_t0.elapsed().as_micros() as u64;
    if !out_timings.is_null() {
        *out_timings = timings;
    }
    handle
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_timed_ffi_u64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
    build_core_spqr: u8,
    out_timings: *mut SpCompressTimings,
) -> *mut SpCompressHandle {
    let Some((edges_slice, contr_slice)) =
        wide_ffi_slices(edges_ptr, edges_len, contractible_ptr, contractible_len)
    else {
        return ptr::null_mut();
    };
    let total_t0 = Instant::now();
    let mut timings = SpCompressTimings::default();
    let handle = make_wide_handle(
        n_nodes,
        edges_slice,
        contr_slice,
        build_core_spqr,
        Some(&mut timings),
    );
    timings.t_total_us = total_t0.elapsed().as_micros() as u64;
    if !out_timings.is_null() {
        *out_timings = timings;
    }
    handle
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_indexed_ffi(
    n_nodes: u32,
    src_ptr: *const u32,
    dst_ptr: *const u32,
    edges_len: u32,
    contractible_ptr: *const u8,
    contractible_len: u32,
    build_core_spqr: u8,
) -> *mut SpCompressHandle {
    if (src_ptr.is_null() || dst_ptr.is_null()) && edges_len > 0 {
        return ptr::null_mut();
    }
    if contractible_ptr.is_null() && contractible_len > 0 {
        return ptr::null_mut();
    }
    if (contractible_len as u64) < (n_nodes as u64) {
        return ptr::null_mut();
    }

    let src_slice: &[u32] = if edges_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(src_ptr, edges_len as usize)
    };
    let dst_slice: &[u32] = if edges_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(dst_ptr, edges_len as usize)
    };
    let contr_slice: &[u8] = if contractible_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(contractible_ptr, contractible_len as usize)
    };

    let cr = match try_compress_degree2_direct_indexed(n_nodes, src_slice, dst_slice, contr_slice) {
        Some(r) => r,
        None => {
            let mut edges = Vec::with_capacity(edges_len as usize);
            for i in 0..edges_len as usize {
                edges.push(InputEdge {
                    u: NodeId(src_slice[i]),
                    v: NodeId(dst_slice[i]),
                    original_edge_id: EdgeId(i as u32),
                });
            }
            compress_borrowed(n_nodes, &edges, contr_slice)
        }
    };
    let macro_tree = cr.tree;

    let handle = if build_core_spqr != 0 {
        let (core_spqr, core_node_remap, core_node_inv) =
            build_core_spqr_parts_fast(n_nodes, &macro_tree);
        SpCompressHandle::WithSpqr(Box::new(CompressAndSpqrResult::from_parts(
            macro_tree,
            core_spqr,
            core_node_remap,
            core_node_inv,
        )))
    } else {
        SpCompressHandle::PlainTree {
            tree: macro_tree,
            success: cr.success,
        }
    };

    Box::into_raw(Box::new(handle))
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_indexed_ffi_u64(
    n_nodes: u64,
    src_ptr: *const u64,
    dst_ptr: *const u64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
    build_core_spqr: u8,
) -> *mut SpCompressHandle {
    let Some((src_slice, dst_slice, contr_slice)) = wide_indexed_slices(
        src_ptr,
        dst_ptr,
        edges_len,
        contractible_ptr,
        contractible_len,
    ) else {
        return ptr::null_mut();
    };

    if let Some(cr) =
        try_compress_degree2_direct_indexed_u64(n_nodes, src_slice, dst_slice, contr_slice)
    {
        return box_wide_tree(cr.tree, cr.success, build_core_spqr, None);
    }

    make_wide_indexed_handle(n_nodes, src_slice, dst_slice, contr_slice, build_core_spqr)
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_free(handle: *mut SpCompressHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_success(handle: *const SpCompressHandle) -> u8 {
    if handle.is_null() {
        return 0;
    }

    if (*handle).success() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_get_tree(handle: *const SpCompressHandle) -> MacroTreeFfi {
    if handle.is_null() {
        return MacroTreeFfi {
            macros_ptr: ptr::null(),
            macros_len: 0,
            children_ptr: ptr::null(),
            children_len: 0,
            core_edges_ptr: ptr::null(),
            core_edges_len: 0,
            core_nodes_ptr: ptr::null(),
            core_nodes_len: 0,
            input_endpoints_ptr: ptr::null(),
            input_endpoints_len: 0,
            stats: CompressionStats::default(),
        };
    }

    let Some(t) = (*handle).tree() else {
        return MacroTreeFfi {
            macros_ptr: ptr::null(),
            macros_len: 0,
            children_ptr: ptr::null(),
            children_len: 0,
            core_edges_ptr: ptr::null(),
            core_edges_len: 0,
            core_nodes_ptr: ptr::null(),
            core_nodes_len: 0,
            input_endpoints_ptr: ptr::null(),
            input_endpoints_len: 0,
            stats: CompressionStats::default(),
        };
    };

    MacroTreeFfi {
        macros_ptr: t.macros.as_ptr(),
        macros_len: t.macros.len() as u64,
        children_ptr: t.children.as_ptr(),
        children_len: t.children.len() as u64,
        core_edges_ptr: t.core_edges.as_ptr(),
        core_edges_len: t.core_edges.len() as u64,
        core_nodes_ptr: t.core_nodes.as_ptr() as *const u32,
        core_nodes_len: t.core_nodes.len() as u64,
        input_endpoints_ptr: t.input_endpoints.as_ptr() as *const u32,
        input_endpoints_len: (t.input_endpoints.len() * 2) as u64,
        stats: t.stats,
    }
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_get_tree_u64(
    handle: *const SpCompressHandle,
) -> MacroTreeFfi64 {
    if handle.is_null() {
        return MacroTreeFfi64::empty();
    }
    let Some(t) = (*handle).wide_tree() else {
        return MacroTreeFfi64::empty();
    };
    MacroTreeFfi64 {
        macros_ptr: t.macros.as_ptr(),
        macros_len: t.macros.len() as u64,
        children_ptr: t.children.as_ptr(),
        children_len: t.children.len() as u64,
        core_edges_ptr: t.core_edges.as_ptr(),
        core_edges_len: t.core_edges.len() as u64,
        core_nodes_ptr: t.core_nodes.as_ptr() as *const u64,
        core_nodes_len: t.core_nodes.len() as u64,
        input_endpoints_ptr: t.input_endpoints.as_ptr() as *const u64,
        input_endpoints_len: (t.input_endpoints.len() * 2) as u64,
        stats: t.stats,
    }
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_get_core_spqr(
    handle: *const SpCompressHandle,
) -> *const crate::SpqrTree {
    if handle.is_null() {
        return ptr::null();
    }

    if let SpCompressHandle::WithSpqr(r) = &*handle {
        if let Some(s) = &r.core_spqr {
            return &s.tree as *const _;
        }
    }

    ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_get_core_spqr_u64(
    handle: *const SpCompressHandle,
) -> *const crate::wide::SpqrTree {
    if handle.is_null() {
        return ptr::null();
    }

    if let SpCompressHandle::WideWithSpqr(r) = &*handle {
        if let Some(s) = &r.core_spqr {
            return &s.tree as *const _;
        }
    }

    ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_core_node_inv(
    handle: *const SpCompressHandle,
    out_len: *mut u32,
) -> *const NodeId {
    if handle.is_null() {
        if !out_len.is_null() {
            *out_len = 0;
        }
        return ptr::null();
    }

    if let SpCompressHandle::WithSpqr(r) = &*handle {
        if r.core_spqr.is_none() {
            if !out_len.is_null() {
                *out_len = 0;
            }
            return ptr::null();
        }

        let inv: &[NodeId] = if r.core_node_inv.is_empty() {
            &r.macro_tree.core_nodes
        } else {
            &r.core_node_inv
        };
        if !out_len.is_null() {
            *out_len = inv.len() as u32;
        }
        if inv.is_empty() {
            return ptr::null();
        }
        return inv.as_ptr();
    }

    if !out_len.is_null() {
        *out_len = 0;
    }

    ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_core_node_inv_u64(
    handle: *const SpCompressHandle,
    out_len: *mut u64,
) -> *const u64 {
    if handle.is_null() {
        if !out_len.is_null() {
            *out_len = 0;
        }
        return ptr::null();
    }

    if let SpCompressHandle::WideWithSpqr(r) = &*handle {
        if r.core_spqr.is_none() {
            if !out_len.is_null() {
                *out_len = 0;
            }
            return ptr::null();
        }
        let inv: &[crate::wide::NodeId] = if r.core_node_inv.is_empty() {
            &r.macro_tree.core_nodes
        } else {
            &r.core_node_inv
        };
        if !out_len.is_null() {
            *out_len = inv.len() as u64;
        }
        if inv.is_empty() {
            return ptr::null();
        }
        return inv.as_ptr() as *const u64;
    }

    if !out_len.is_null() {
        *out_len = 0;
    }
    ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_reconstruct_ffi(
    n_nodes: u32,
    edges_ptr: *const InputEdge,
    edges_len: u32,
    _max_original_edge_id: u32,
    contractible_ptr: *const u8,
    contractible_len: u32,
) -> *mut crate::SpqrResult {
    if edges_ptr.is_null() && edges_len > 0 {
        return ptr::null_mut();
    }
    if contractible_ptr.is_null() && contractible_len > 0 {
        return ptr::null_mut();
    }
    if (contractible_len as u64) < (n_nodes as u64) {
        return ptr::null_mut();
    }

    let edges_slice: &[InputEdge] = if edges_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(edges_ptr, edges_len as usize)
    };
    let contr_slice: &[u8] = if contractible_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(contractible_ptr, contractible_len as usize)
    };

    let result =
        crate::sp_compress::compress_and_build_spqr_borrowed(n_nodes, edges_slice, contr_slice);

    let tree = crate::sp_compress::reconstruct::reconstruct_from_compress_result(&result);

    let self_loops: Vec<crate::EdgeId> = edges_slice
        .iter()
        .filter(|e| e.u == e.v)
        .map(|e| e.original_edge_id)
        .collect();

    let spqr_result = crate::SpqrResult { tree, self_loops };
    Box::into_raw(Box::new(spqr_result))
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_reconstruct_with_timings_ffi(
    n_nodes: u32,
    edges_ptr: *const InputEdge,
    edges_len: u32,
    _max_original_edge_id: u32,
    contractible_ptr: *const u8,
    contractible_len: u32,
    out_stats: *mut CompressionStats,
    out_timings: *mut SpCompressTimings,
) -> *mut crate::SpqrResult {
    if edges_ptr.is_null() && edges_len > 0 {
        return ptr::null_mut();
    }
    if contractible_ptr.is_null() && contractible_len > 0 {
        return ptr::null_mut();
    }
    if (contractible_len as u64) < (n_nodes as u64) {
        return ptr::null_mut();
    }

    let edges_slice: &[InputEdge] = if edges_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(edges_ptr, edges_len as usize)
    };
    let contr_slice: &[u8] = if contractible_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(contractible_ptr, contractible_len as usize)
    };

    let mut timings = SpCompressTimings::default();

    let t0 = Instant::now();
    let cr = compress_borrowed(n_nodes, edges_slice, contr_slice);
    let macro_tree = cr.tree;
    timings.t_compress_us = t0.elapsed().as_micros() as u64;

    if !out_stats.is_null() {
        *out_stats = macro_tree.stats;
    }

    let t1 = Instant::now();
    let (core_spqr, core_node_remap, core_node_inv) =
        build_core_spqr_timed(n_nodes, &macro_tree, &mut timings, false);

    timings.t_build_spqr_core_us = t1.elapsed().as_micros() as u64;

    let result =
        CompressAndSpqrResult::from_parts(macro_tree, core_spqr, core_node_remap, core_node_inv);

    let (tree, rt) = match &result.core_spqr {
        Some(spqr) if !spqr.tree.is_empty() => {
            let core_node_inv = if result.core_node_inv.is_empty() {
                &result.macro_tree.core_nodes
            } else {
                &result.core_node_inv
            };
            crate::sp_compress::reconstruct::reconstruct_timed(
                &spqr.tree,
                &result.macro_tree,
                core_node_inv,
            )
        }
        _ => crate::sp_compress::reconstruct::reconstruct_fully_reducible_timed(&result.macro_tree),
    };

    fill_production_reconstruct_timings(&mut timings, rt);

    if !out_timings.is_null() {
        *out_timings = timings;
    }

    let self_loops: Vec<crate::EdgeId> = edges_slice
        .iter()
        .filter(|e| e.u == e.v)
        .map(|e| e.original_edge_id)
        .collect();

    let spqr_result = crate::SpqrResult { tree, self_loops };
    Box::into_raw(Box::new(spqr_result))
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_reconstruct_ffi64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
) -> *mut crate::ffi::SpqrResult64 {
    sp_compress_reconstruct_with_timings_ffi64(
        n_nodes,
        edges_ptr,
        edges_len,
        contractible_ptr,
        contractible_len,
        ptr::null_mut(),
        ptr::null_mut(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_reconstruct_ffi_u64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
) -> *mut crate::ffi::SpqrResult64 {
    sp_compress_reconstruct_ffi64(
        n_nodes,
        edges_ptr,
        edges_len,
        contractible_ptr,
        contractible_len,
    )
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_reconstruct_with_timings_ffi64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
    out_stats: *mut crate::sp_compress::wide::CompressionStats,
    out_timings: *mut SpCompressTimings,
) -> *mut crate::ffi::SpqrResult64 {
    let Some((edges_slice, contr_slice)) =
        wide_ffi_slices(edges_ptr, edges_len, contractible_ptr, contractible_len)
    else {
        return ptr::null_mut();
    };

    let Some(wide_edges) = wide_edges_from_ffi_dense(edges_slice) else {
        return ptr::null_mut();
    };

    let total_t0 = Instant::now();
    let mut timings = SpCompressTimings::default();

    let t0 = Instant::now();
    let cr =
        crate::sp_compress::wide::compress_borrowed_remapped(n_nodes, &wide_edges, contr_slice);
    let macro_tree = cr.tree;
    timings.t_compress_us = t0.elapsed().as_micros() as u64;

    if !cr.success {
        return ptr::null_mut();
    }

    if !out_stats.is_null() {
        *out_stats = macro_tree.stats;
    }

    let t1 = Instant::now();
    let (core_spqr, core_node_inv) = build_core_spqr_wide_timed(&macro_tree, Some(&mut timings));
    timings.t_build_spqr_core_us = t1.elapsed().as_micros() as u64;

    let (tree, rt) = match &core_spqr {
        Some(spqr) if !spqr.tree.is_empty() => {
            let inv: &[crate::wide::NodeId] = if core_node_inv.is_empty() {
                &macro_tree.core_nodes
            } else {
                &core_node_inv
            };
            crate::sp_compress::reconstruct_wide::reconstruct_timed(&spqr.tree, &macro_tree, inv)
        }
        _ => crate::sp_compress::reconstruct_wide::reconstruct_fully_reducible_timed(&macro_tree),
    };

    fill_wide_production_reconstruct_timings(&mut timings, rt);
    timings.t_total_us = total_t0.elapsed().as_micros() as u64;

    if !out_timings.is_null() {
        *out_timings = timings;
    }

    let self_loops: Vec<crate::wide::EdgeId> = wide_edges
        .iter()
        .filter(|e| e.u == e.v)
        .map(|e| e.original_edge_id)
        .collect();
    crate::ffi::make_spqr_result64(crate::wide::SpqrResult { tree, self_loops })
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_reconstruct_with_timings_ffi_u64(
    n_nodes: u64,
    edges_ptr: *const InputEdge64,
    edges_len: u64,
    contractible_ptr: *const u8,
    contractible_len: u64,
    out_stats: *mut crate::sp_compress::wide::CompressionStats,
    out_timings: *mut SpCompressTimings,
) -> *mut crate::ffi::SpqrResult64 {
    sp_compress_reconstruct_with_timings_ffi64(
        n_nodes,
        edges_ptr,
        edges_len,
        contractible_ptr,
        contractible_len,
        out_stats,
        out_timings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EdgeId;

    #[test]
    fn ffi_compress_basic() {
        let edges = [
            InputEdge {
                u: NodeId(0),
                v: NodeId(1),
                original_edge_id: EdgeId(0),
            },
            InputEdge {
                u: NodeId(1),
                v: NodeId(2),
                original_edge_id: EdgeId(1),
            },
            InputEdge {
                u: NodeId(2),
                v: NodeId(3),
                original_edge_id: EdgeId(2),
            },
        ];
        let contr = [0u8, 1, 1, 0];

        unsafe {
            let h = sp_compress_ffi(
                4,
                edges.as_ptr(),
                edges.len() as u32,
                edges.len() as u32 - 1,
                contr.as_ptr(),
                contr.len() as u32,
                0,
            );

            assert!(!h.is_null());

            let view = sp_compress_get_tree(h);
            assert_eq!(view.macros_len, 1);
            assert_eq!(view.core_edges_len, 1);
            assert_eq!(view.input_endpoints_len, 6);

            sp_compress_free(h);
        }
    }

    #[test]
    fn ffi64_downcasts_when_block_fits_u32() {
        let edges = [
            InputEdge64 {
                u: 0,
                v: 1,
                original_edge_id: 0,
            },
            InputEdge64 {
                u: 1,
                v: 2,
                original_edge_id: 1,
            },
            InputEdge64 {
                u: 2,
                v: 3,
                original_edge_id: 2,
            },
        ];
        let contr = [0u8, 1, 1, 0];

        unsafe {
            let h = sp_compress_ffi64(
                4,
                edges.as_ptr(),
                edges.len() as u64,
                contr.as_ptr(),
                contr.len() as u64,
                0,
            );

            assert!(!h.is_null());
            let view = sp_compress_get_tree(h);
            assert_eq!(view.macros_len, 1);
            assert_eq!(view.core_edges_len, 1);
            assert_eq!(view.input_endpoints_len, 6);
            sp_compress_free(h);
        }
    }

    #[test]
    fn ffi_u64_aliases_and_indexed_path_work() {
        let edges = [
            InputEdge64 {
                u: 0,
                v: 1,
                original_edge_id: 0,
            },
            InputEdge64 {
                u: 1,
                v: 2,
                original_edge_id: 1,
            },
        ];
        let src = [0u64, 1];
        let dst = [1u64, 2];
        let contr = [0u8, 1, 0];

        unsafe {
            let h = sp_compress_ffi_u64(
                3,
                edges.as_ptr(),
                edges.len() as u64,
                contr.as_ptr(),
                contr.len() as u64,
                0,
            );
            assert!(!h.is_null());
            sp_compress_free(h);

            let h = sp_compress_indexed_ffi_u64(
                3,
                src.as_ptr(),
                dst.as_ptr(),
                src.len() as u64,
                contr.as_ptr(),
                contr.len() as u64,
                0,
            );
            assert!(!h.is_null());
            sp_compress_free(h);
        }
    }

    #[test]
    fn ffi64_compresses_values_requiring_true_u64_backend() {
        let base = u32::MAX as u64 + 10;
        let edges = [
            InputEdge64 {
                u: base,
                v: base + 1,
                original_edge_id: 0,
            },
            InputEdge64 {
                u: base + 1,
                v: base + 2,
                original_edge_id: 1,
            },
        ];
        let contr_dense_over_touched_nodes = [0u8, 1, 0];

        unsafe {
            let h = sp_compress_ffi64(
                base + 3,
                edges.as_ptr(),
                edges.len() as u64,
                contr_dense_over_touched_nodes.as_ptr(),
                contr_dense_over_touched_nodes.len() as u64,
                0,
            );
            assert!(!h.is_null());
            assert_eq!(sp_compress_success(h), 1);

            let old_view = sp_compress_get_tree(h);
            assert_eq!(old_view.core_edges_len, 0);

            let view = sp_compress_get_tree_u64(h);
            assert_eq!(view.macros_len, 1);
            assert_eq!(view.core_edges_len, 1);
            assert_eq!(view.input_endpoints_len, 4);
            let core = *view.core_edges_ptr;
            assert_eq!(core.u, base);
            assert_eq!(core.v, base + 2);

            sp_compress_free(h);
        }
    }

    #[test]
    fn ffi64_builds_wide_core_spqr() {
        let base = u32::MAX as u64 + 100;
        let edges = [
            InputEdge64 {
                u: base,
                v: base + 1,
                original_edge_id: 0,
            },
            InputEdge64 {
                u: base,
                v: base + 2,
                original_edge_id: 1,
            },
            InputEdge64 {
                u: base,
                v: base + 3,
                original_edge_id: 2,
            },
            InputEdge64 {
                u: base + 1,
                v: base + 2,
                original_edge_id: 3,
            },
            InputEdge64 {
                u: base + 1,
                v: base + 3,
                original_edge_id: 4,
            },
            InputEdge64 {
                u: base + 2,
                v: base + 3,
                original_edge_id: 5,
            },
        ];
        let contr_dense_over_touched_nodes = [0u8, 0, 0, 0];

        unsafe {
            let h = sp_compress_ffi64(
                base + 4,
                edges.as_ptr(),
                edges.len() as u64,
                contr_dense_over_touched_nodes.as_ptr(),
                contr_dense_over_touched_nodes.len() as u64,
                1,
            );
            assert!(!h.is_null());
            assert_eq!(sp_compress_success(h), 1);

            let view = sp_compress_get_tree_u64(h);
            assert_eq!(view.core_edges_len, 6);
            assert_eq!(view.core_nodes_len, 4);
            assert!(!view.core_nodes_ptr.is_null());
            assert_eq!(*view.core_nodes_ptr, base);

            let tree = sp_compress_get_core_spqr_u64(h);
            assert!(!tree.is_null());
            assert!(crate::ffi::spqr_tree_len_u64(tree) > 0);

            let mut inv_len = 0u64;
            let inv = sp_compress_core_node_inv_u64(h, &mut inv_len);
            assert_eq!(inv_len, 4);
            assert!(!inv.is_null());
            assert_eq!(*inv, base);

            sp_compress_free(h);
        }
    }

    #[test]
    fn ffi_with_spqr_k4() {
        let edges = [
            InputEdge {
                u: NodeId(0),
                v: NodeId(1),
                original_edge_id: EdgeId(0),
            },
            InputEdge {
                u: NodeId(0),
                v: NodeId(2),
                original_edge_id: EdgeId(1),
            },
            InputEdge {
                u: NodeId(0),
                v: NodeId(3),
                original_edge_id: EdgeId(2),
            },
            InputEdge {
                u: NodeId(1),
                v: NodeId(2),
                original_edge_id: EdgeId(3),
            },
            InputEdge {
                u: NodeId(1),
                v: NodeId(3),
                original_edge_id: EdgeId(4),
            },
            InputEdge {
                u: NodeId(2),
                v: NodeId(3),
                original_edge_id: EdgeId(5),
            },
        ];
        let contr = [1u8, 1, 1, 1];

        unsafe {
            let h = sp_compress_ffi(
                4,
                edges.as_ptr(),
                edges.len() as u32,
                edges.len() as u32 - 1,
                contr.as_ptr(),
                contr.len() as u32,
                1,
            );

            assert!(!h.is_null());

            let spqr = sp_compress_get_core_spqr(h);
            assert!(!spqr.is_null());

            let mut len: u32 = 0;
            let inv = sp_compress_core_node_inv(h, &mut len as *mut u32);

            assert!(len > 0);
            assert!(!inv.is_null());

            sp_compress_free(h);
        }
    }
}
