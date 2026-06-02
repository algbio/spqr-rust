#![allow(clippy::missing_safety_doc)]

use crate::sp_compress::direct::try_compress_degree2_direct_indexed;
use crate::sp_compress::integration::{
    build_core_spqr_parts_fast, core_edges_have_no_non_loop_parallel, CompressAndSpqrResult,
    CoreNodeMapper, FfiScadComponent, FfiScadEdge, FfiScadIncidence, FfiSpqraBehaviorAtom,
    FfiSpqraBehaviorAtomItem, FfiSpqraMinimizerComponent, FfiSpqraMinimizerEdge,
    FfiSpqraMinimizerSummary,
};
use crate::sp_compress::reduction::{
    compress_borrowed, compress_borrowed_timed, CompressionTimings,
};
use crate::sp_compress::types::{ChildRef, CompressionStats, CoreEdge, InputEdge, SpNode, SpTree};
use crate::{EdgeId, NodeId};
use std::ptr;
use std::slice;
use std::time::Instant;

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
    pub macros_len: u32,
    pub children_ptr: *const ChildRef,
    pub children_len: u32,
    pub core_edges_ptr: *const CoreEdge,
    pub core_edges_len: u32,

    pub core_nodes_ptr: *const u32,
    pub core_nodes_len: u32,

    pub input_endpoints_ptr: *const u32,
    pub input_endpoints_len: u32,
    pub stats: CompressionStats,
}

#[repr(C)]
pub struct CoreScadFfi {
    pub components_ptr: *const FfiScadComponent,
    pub components_len: u32,
    pub edges_ptr: *const FfiScadEdge,
    pub edges_len: u32,
    pub incidences_ptr: *const FfiScadIncidence,
    pub incidences_len: u32,
    pub node_mapping_ptr: *const u32,
    pub node_mapping_len: u32,
}

#[repr(C)]
pub struct SpqraMinimizerView {
    pub components_ptr: *const FfiSpqraMinimizerComponent,
    pub components_len: u32,
    pub edges_ptr: *const FfiSpqraMinimizerEdge,
    pub edges_len: u32,
    pub node_mapping_ptr: *const u32,
    pub node_mapping_len: u32,
    pub children_ptr: *const u32,
    pub children_len: u32,
    pub postorder_ptr: *const u32,
    pub postorder_len: u32,
    pub summary: FfiSpqraMinimizerSummary,
}

#[repr(C)]
pub struct SpqraBehaviorAtomView {
    pub atoms_ptr: *const FfiSpqraBehaviorAtom,
    pub atoms_len: u32,
    pub items_ptr: *const FfiSpqraBehaviorAtomItem,
    pub items_len: u32,
}

impl SpqraBehaviorAtomView {
    fn empty() -> Self {
        Self {
            atoms_ptr: ptr::null(),
            atoms_len: 0,
            items_ptr: ptr::null(),
            items_len: 0,
        }
    }
}

impl SpqraMinimizerView {
    fn empty() -> Self {
        Self {
            components_ptr: ptr::null(),
            components_len: 0,
            edges_ptr: ptr::null(),
            edges_len: 0,
            node_mapping_ptr: ptr::null(),
            node_mapping_len: 0,
            children_ptr: ptr::null(),
            children_len: 0,
            postorder_ptr: ptr::null(),
            postorder_len: 0,
            summary: FfiSpqraMinimizerSummary {
                root: crate::INVALID,
                ..FfiSpqraMinimizerSummary::default()
            },
        }
    }
}

impl CoreScadFfi {
    fn empty() -> Self {
        Self {
            components_ptr: ptr::null(),
            components_len: 0,
            edges_ptr: ptr::null(),
            edges_len: 0,
            incidences_ptr: ptr::null(),
            incidences_len: 0,
            node_mapping_ptr: ptr::null(),
            node_mapping_len: 0,
        }
    }
}

pub enum SpCompressHandle {
    PlainTree { tree: SpTree, success: bool },
    WithSpqr(Box<CompressAndSpqrResult>),
}

impl SpCompressHandle {
    fn tree(&self) -> &SpTree {
        match self {
            SpCompressHandle::PlainTree { tree, .. } => tree,
            SpCompressHandle::WithSpqr(r) => &r.macro_tree,
        }
    }

    fn success(&self) -> bool {
        match self {
            SpCompressHandle::PlainTree { success, .. } => *success,
            SpCompressHandle::WithSpqr(_) => true,
        }
    }
}

#[inline(always)]
fn build_core_spqr_timed(
    n_nodes: u32,
    macro_tree: &SpTree,
    timings: &mut SpCompressTimings,
    fill_spqr_timings: bool,
    skip_assemble: bool,
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
    let core_child_refs = if crate::sp_compress::integration::wants_spqra_minimizer_sidecar() {
        Some(
            macro_tree
                .core_edges
                .iter()
                .map(|ce| ce.child)
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let spqr = crate::with_spqra_core_child_refs(core_child_refs, || {
        crate::with_skip_assemble_spqr(skip_assemble, || {
            if fill_spqr_timings {
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
            }
        })
    });
    timings.t_core_spqr_raw_us = t_spqr.elapsed().as_micros() as u64;

    (Some(spqr), Vec::new(), Vec::new())
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_ffi(
    n_nodes: u32,
    edges_ptr: *const InputEdge,
    edges_len: u32,
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
        let cr = compress_borrowed(n_nodes, edges_slice, contr_slice);
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
        let r = compress_borrowed(n_nodes, edges_slice, contr_slice);
        SpCompressHandle::PlainTree {
            tree: r.tree,
            success: r.success,
        }
    };

    Box::into_raw(Box::new(handle))
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_timed_ffi(
    n_nodes: u32,
    edges_ptr: *const InputEdge,
    edges_len: u32,
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
        let (core_spqr, core_node_remap, core_node_inv) = build_core_spqr_timed(
            n_nodes,
            &macro_tree,
            &mut timings,
            true,
            build_core_spqr > 1,
        );

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

    let t = (*handle).tree();

    MacroTreeFfi {
        macros_ptr: t.macros.as_ptr(),
        macros_len: t.macros.len() as u32,
        children_ptr: t.children.as_ptr(),
        children_len: t.children.len() as u32,
        core_edges_ptr: t.core_edges.as_ptr(),
        core_edges_len: t.core_edges.len() as u32,
        core_nodes_ptr: t.core_nodes.as_ptr() as *const u32,
        core_nodes_len: t.core_nodes.len() as u32,
        input_endpoints_ptr: t.input_endpoints.as_ptr() as *const u32,
        input_endpoints_len: (t.input_endpoints.len() * 2) as u32,
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
pub unsafe extern "C" fn sp_compress_get_core_scad_export(
    handle: *const SpCompressHandle,
) -> CoreScadFfi {
    if handle.is_null() {
        return CoreScadFfi::empty();
    }

    if let SpCompressHandle::WithSpqr(r) = &*handle {
        if let Some(scad) = &r.core_scad_export {
            return CoreScadFfi {
                components_ptr: scad.components.as_ptr(),
                components_len: scad.components.len() as u32,
                edges_ptr: scad.edges.as_ptr(),
                edges_len: scad.edges.len() as u32,
                incidences_ptr: scad.incidences.as_ptr(),
                incidences_len: scad.incidences.len() as u32,
                node_mapping_ptr: scad.node_mapping.as_ptr(),
                node_mapping_len: scad.node_mapping.len() as u32,
            };
        }
    }

    CoreScadFfi::empty()
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_get_spqra_minimizer_view(
    handle: *const SpCompressHandle,
) -> SpqraMinimizerView {
    if handle.is_null() {
        return SpqraMinimizerView::empty();
    }

    if let SpCompressHandle::WithSpqr(r) = &*handle {
        if let Some(sidecar) = &r.spqra_minimizer_sidecar {
            return SpqraMinimizerView {
                components_ptr: sidecar.components.as_ptr(),
                components_len: sidecar.components.len() as u32,
                edges_ptr: sidecar.edges.as_ptr(),
                edges_len: sidecar.edges.len() as u32,
                node_mapping_ptr: sidecar.node_mapping.as_ptr(),
                node_mapping_len: sidecar.node_mapping.len() as u32,
                children_ptr: sidecar.children.as_ptr(),
                children_len: sidecar.children.len() as u32,
                postorder_ptr: sidecar.postorder.as_ptr(),
                postorder_len: sidecar.postorder.len() as u32,
                summary: sidecar.summary,
            };
        }
    }

    SpqraMinimizerView::empty()
}

#[no_mangle]
pub unsafe extern "C" fn sp_compress_get_spqra_behavior_atom_view(
    handle: *const SpCompressHandle,
) -> SpqraBehaviorAtomView {
    if handle.is_null() {
        return SpqraBehaviorAtomView::empty();
    }

    if let SpCompressHandle::WithSpqr(r) = &*handle {
        if let Some(sidecar) = &r.spqra_minimizer_sidecar {
            return SpqraBehaviorAtomView {
                atoms_ptr: sidecar.behavior_atoms.as_ptr(),
                atoms_len: sidecar.behavior_atoms.len() as u32,
                items_ptr: sidecar.behavior_atom_items.as_ptr(),
                items_len: sidecar.behavior_atom_items.len() as u32,
            };
        }
    }

    SpqraBehaviorAtomView::empty()
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
pub unsafe extern "C" fn sp_compress_reconstruct_ffi(
    n_nodes: u32,
    edges_ptr: *const InputEdge,
    edges_len: u32,
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
        build_core_spqr_timed(n_nodes, &macro_tree, &mut timings, false, false);

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
