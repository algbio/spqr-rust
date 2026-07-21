//! FFI (interface) for C/C++

#![allow(clippy::missing_safety_doc)]

use crate::biconnected::BCTree;
use crate::connected::{connected_components, ConnectedComponents};
use crate::spqr_format::write_spqr_format;
use crate::wide_biconnected::WideBCTree;
use crate::{
    build_spqr, EdgeId, Graph, NodeId, SkeletonEdge, SpqrNodeType, SpqrResult, SpqrTree,
    TreeNodeId, FAST_CYCLE_CALLS, FAST_CYCLE_HITS,
};
use std::ffi::CStr;
use std::io::Cursor;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

pub struct Graph64 {
    inner: crate::wide::Graph,
}

pub struct WideBCTreeFFI {
    inner: WideBCTree,
}

pub struct SpqrResult64 {
    inner: crate::wide::SpqrResult,
    self_loops_u64: Vec<u64>,
}

pub struct SpqrPayloadResult64 {
    inner: crate::wide::SpqrPayloadResult,
}

#[repr(C)]
pub struct SpqrPayloadTreeColumns64 {
    pub root: u64,
    pub tree_nodes: u64,
    pub children: u64,
    pub skeleton_edges: u64,
    pub node_mapping: u64,
    pub node_types: *const u8,
    pub node_parents: *const u64,
    pub children_offsets: *const u64,
    pub child_nodes: *const u64,
    pub skeleton_offsets: *const u64,
    pub node_mapping_offsets: *const u64,
    pub node_mapping_values: *const u64,
    pub skeleton_num_nodes: *const u64,
}

#[repr(C)]
pub struct SpqrPackedU40Column64 {
    pub low: *const u32,
    pub high: *const u8,
    pub count: u64,
}

#[repr(C)]
pub struct SpqrPackedSkeleton64 {
    pub edge_words: *const u64,
    pub edge_count: u64,
    pub first_virtual: u64,
    pub first_tree: SpqrPackedU40Column64,
    pub first_edge: SpqrPackedU40Column64,
    pub second_tree: SpqrPackedU40Column64,
    pub second_edge: SpqrPackedU40Column64,
}

#[inline(always)]
pub(crate) fn make_spqr_result64(inner: crate::wide::SpqrResult) -> *mut SpqrResult64 {
    let self_loops_u64 = inner.self_loops.iter().map(|edge| edge.0).collect();
    Box::into_raw(Box::new(SpqrResult64 {
        inner,
        self_loops_u64,
    }))
}

#[repr(C)]
pub struct SkeletonEdgeInfo64 {
    pub src: u64,
    pub dst: u64,
    pub real_edge: u64,
    pub twin_tree_node: u64,
    pub is_virtual: bool,
}

#[inline]
fn ffi_u64_to_usize(value: u64) -> Option<usize> {
    if value <= usize::MAX as u64 {
        Some(value as usize)
    } else {
        None
    }
}

#[inline]
fn ffi_u64_invalid() -> u64 {
    u64::MAX
}

#[inline]
fn ffi_u64_or_invalid(value: u64) -> u64 {
    if value == u64::MAX {
        ffi_u64_invalid()
    } else {
        value
    }
}

#[inline]
fn ffi_capacity_hint(value: u64) -> usize {
    const MAX_EAGER_RESERVE: usize = 1 << 30;
    ffi_u64_to_usize(value)
        .filter(|&v| v <= MAX_EAGER_RESERVE)
        .unwrap_or(0)
}

#[inline]
unsafe fn graph64<'a>(graph: *const Graph64) -> Option<&'a crate::wide::Graph> {
    graph.as_ref().map(|graph| &graph.inner)
}

#[inline]
unsafe fn graph64_mut<'a>(graph: *mut Graph64) -> Option<&'a mut crate::wide::Graph> {
    graph.as_mut().map(|graph| &mut graph.inner)
}

#[inline(always)]
unsafe fn graph64_storage_mut<T, F>(graph: *mut Graph64, storage: F) -> *mut T
where
    F: for<'a> FnOnce(&'a mut crate::wide::Graph) -> Option<&'a mut [T]>,
{
    graph64_mut(graph)
        .and_then(storage)
        .map_or(ptr::null_mut(), |values| values.as_mut_ptr())
}

#[inline]
unsafe fn graph64_node<'a>(graph: *const Graph64, node: u64) -> Option<&'a crate::wide::Graph> {
    let graph = graph64(graph)?;
    let node_idx = ffi_u64_to_usize(node)?;
    (node_idx < graph.num_nodes()).then_some(graph)
}

#[inline]
unsafe fn graph64_edge<'a>(
    graph: *const Graph64,
    edge_id: u64,
) -> Option<(&'a crate::wide::Graph, crate::wide::EdgeId)> {
    let graph = graph64(graph)?;
    let edge_idx = ffi_u64_to_usize(edge_id)?;
    (edge_idx < graph.num_edges()).then_some((graph, crate::wide::EdgeId(edge_id)))
}

#[inline]
unsafe fn tree64<'a>(tree: *const crate::wide::SpqrTree) -> Option<&'a crate::wide::SpqrTree> {
    tree.as_ref()
}

#[inline]
unsafe fn tree64_mut<'a>(
    tree: *mut crate::wide::SpqrTree,
) -> Option<&'a mut crate::wide::SpqrTree> {
    tree.as_mut()
}

#[inline]
unsafe fn tree64_node<'a>(
    tree: *const crate::wide::SpqrTree,
    node_id: u64,
) -> Option<crate::wide::SpqrTreeNodeView<'a>> {
    let tree = tree64(tree)?;
    let node_idx = ffi_u64_to_usize(node_id)?;
    (node_idx < tree.len()).then(|| tree.node(crate::wide::TreeNodeId(node_id)))
}

#[inline(always)]
unsafe fn ffi_slice_ptr<T>(values: Option<&[T]>, out_len: *mut u64) -> *const T {
    if !out_len.is_null() {
        *out_len = values.map_or(0, |slice| slice.len() as u64);
    }
    values.map_or(ptr::null(), |slice| slice.as_ptr())
}

#[inline(always)]
unsafe fn ffi_packed_u40_ptrs(
    values: Option<(&[u32], &[u8])>,
    out_low: *mut *const u32,
    out_high: *mut *const u8,
    out_len: *mut u64,
) -> bool {
    if let Some((low, high)) = values {
        if !out_low.is_null() {
            *out_low = low.as_ptr();
        }
        if !out_high.is_null() {
            *out_high = high.as_ptr();
        }
        if !out_len.is_null() {
            *out_len = low.len() as u64;
        }
        true
    } else {
        if !out_low.is_null() {
            *out_low = ptr::null();
        }
        if !out_high.is_null() {
            *out_high = ptr::null();
        }
        if !out_len.is_null() {
            *out_len = 0;
        }
        false
    }
}

#[inline(always)]
fn packed_u40_column(low: &[u32], high: &[u8]) -> SpqrPackedU40Column64 {
    SpqrPackedU40Column64 {
        low: low.as_ptr(),
        high: high.as_ptr(),
        count: low.len() as u64,
    }
}

#[inline(always)]
fn wide_graph_is_buildable(graph: &crate::wide::Graph) -> bool {
    graph.num_nodes() <= ((i64::MAX - 2) / 3) as usize
        && graph.num_edges().checked_mul(2).is_some()
        && graph
            .num_edges()
            .checked_add(graph.num_nodes())
            .and_then(|size| size.checked_mul(2))
            .and_then(|size| size.checked_add(2))
            .is_some()
}

#[inline]
fn spqr_node_type_byte_u64(t: crate::wide::SpqrNodeType) -> u8 {
    match t {
        crate::wide::SpqrNodeType::S => SPQR_NODE_TYPE_S,
        crate::wide::SpqrNodeType::P => SPQR_NODE_TYPE_P,
        crate::wide::SpqrNodeType::R => SPQR_NODE_TYPE_R,
    }
}

#[no_mangle]
pub extern "C" fn spqr_get_fast_cycle_hits() -> u64 {
    FAST_CYCLE_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn spqr_get_fast_cycle_calls() -> u64 {
    FAST_CYCLE_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn spqr_set_canonicalize_root_enabled(enabled: u8) {
    crate::CANONICALIZE_ROOT_ENABLED.store(enabled != 0, std::sync::atomic::Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn spqr_set_thread_count(threads: u32) -> bool {
    crate::set_spqr_thread_count(threads as usize)
}

#[no_mangle]
pub extern "C" fn spqr_get_canonicalize_root_enabled() -> u8 {
    if crate::CANONICALIZE_ROOT_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn spqr_graph_new_u64(node_capacity: u64, edge_capacity: u64) -> *mut Graph64 {
    Box::into_raw(Box::new(Graph64 {
        inner: crate::wide::Graph::with_capacity(
            ffi_capacity_hint(node_capacity),
            ffi_capacity_hint(edge_capacity),
        ),
    }))
}

#[no_mangle]
pub extern "C" fn spqr_graph_new_edge_storage_u64(num_nodes: u64, num_edges: u64) -> *mut Graph64 {
    let Some(n_usize) = ffi_u64_to_usize(num_nodes) else {
        return ptr::null_mut();
    };
    let Some(m_usize) = ffi_u64_to_usize(num_edges) else {
        return ptr::null_mut();
    };
    let Some(inner) = crate::wide::Graph::with_edge_storage(n_usize, m_usize) else {
        return ptr::null_mut();
    };
    match catch_unwind(AssertUnwindSafe(|| Box::new(Graph64 { inner }))) {
        Ok(graph) => Box::into_raw(graph),
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn spqr_graph_new_packed_edge_storage_u40(
    num_nodes: u64,
    num_edges: u64,
) -> *mut Graph64 {
    let Some(n_usize) = ffi_u64_to_usize(num_nodes) else {
        return ptr::null_mut();
    };
    let Some(m_usize) = ffi_u64_to_usize(num_edges) else {
        return ptr::null_mut();
    };
    let Some(inner) = crate::wide::Graph::with_packed_edge_storage(n_usize, m_usize) else {
        return ptr::null_mut();
    };
    match catch_unwind(AssertUnwindSafe(|| Box::new(Graph64 { inner }))) {
        Ok(graph) => Box::into_raw(graph),
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn spqr_graph_new_split_packed_edge_storage_u40(
    num_nodes: u64,
    num_edges: u64,
) -> *mut Graph64 {
    let Some(n_usize) = ffi_u64_to_usize(num_nodes) else {
        return ptr::null_mut();
    };
    let Some(m_usize) = ffi_u64_to_usize(num_edges) else {
        return ptr::null_mut();
    };
    match catch_unwind(AssertUnwindSafe(|| {
        crate::wide::Graph::with_split_packed_edge_storage(n_usize, m_usize)
            .map(|inner| Box::new(Graph64 { inner }))
    })) {
        Ok(Some(graph)) => Box::into_raw(graph),
        Err(_) => ptr::null_mut(),
        Ok(None) => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_edge_storage_mut_u64(graph: *mut Graph64) -> *mut u64 {
    graph64_storage_mut(graph, crate::wide::Graph::edge_storage_mut)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_packed_edge_storage_low_mut_u40(
    graph: *mut Graph64,
) -> *mut u32 {
    graph64_storage_mut(graph, crate::wide::Graph::packed_edge_storage_low_mut)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_packed_edge_storage_high_mut_u40(
    graph: *mut Graph64,
) -> *mut u8 {
    graph64_storage_mut(graph, crate::wide::Graph::packed_edge_storage_high_mut)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_split_packed_edge_storage_source_low_mut_u40(
    graph: *mut Graph64,
) -> *mut u32 {
    let storage = crate::wide::Graph::split_packed_edge_storage_source_low_mut;
    graph64_storage_mut(graph, storage)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_split_packed_edge_storage_source_high_mut_u40(
    graph: *mut Graph64,
) -> *mut u8 {
    let storage = crate::wide::Graph::split_packed_edge_storage_source_high_mut;
    graph64_storage_mut(graph, storage)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_allocate_split_packed_edge_storage_target_u40(
    graph: *mut Graph64,
) -> bool {
    let Some(graph) = graph64_mut(graph) else {
        return false;
    };
    catch_unwind(AssertUnwindSafe(|| {
        graph.allocate_split_packed_edge_storage_target()
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_split_packed_edge_storage_target_low_mut_u40(
    graph: *mut Graph64,
) -> *mut u32 {
    let storage = crate::wide::Graph::split_packed_edge_storage_target_low_mut;
    graph64_storage_mut(graph, storage)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_split_packed_edge_storage_target_high_mut_u40(
    graph: *mut Graph64,
) -> *mut u8 {
    let storage = crate::wide::Graph::split_packed_edge_storage_target_high_mut;
    graph64_storage_mut(graph, storage)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_finalize_edge_storage_u64(graph: *mut Graph64) -> bool {
    let Some(graph) = graph64_mut(graph) else {
        return false;
    };
    catch_unwind(AssertUnwindSafe(|| graph.finalize_edge_storage())).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_free_u64(graph: *mut Graph64) {
    if !graph.is_null() {
        drop(Box::from_raw(graph));
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_add_nodes_u64(graph: *mut Graph64, count: u64) -> u64 {
    let Some(graph) = graph64_mut(graph) else {
        return ffi_u64_invalid();
    };
    let Some(count_usize) = ffi_u64_to_usize(count) else {
        return ffi_u64_invalid();
    };
    let first = graph.num_nodes();
    if first.checked_add(count_usize).is_none() {
        return ffi_u64_invalid();
    }
    graph.add_nodes_fast(count_usize);
    first as u64
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_add_edge_u64(graph: *mut Graph64, u: u64, v: u64) -> u64 {
    let Some(graph) = graph64_mut(graph) else {
        return ffi_u64_invalid();
    };
    let Some(ui) = ffi_u64_to_usize(u) else {
        return ffi_u64_invalid();
    };
    let Some(vi) = ffi_u64_to_usize(v) else {
        return ffi_u64_invalid();
    };
    if ui >= graph.num_nodes() || vi >= graph.num_nodes() || graph.num_edges() as u64 == u64::MAX {
        return ffi_u64_invalid();
    }
    graph
        .add_edge(crate::wide::NodeId(u), crate::wide::NodeId(v))
        .0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_from_arrays_u64(
    num_nodes: u64,
    src: *const u64,
    dst: *const u64,
    num_edges: u64,
) -> *mut Graph64 {
    let Some(n_usize) = ffi_u64_to_usize(num_nodes) else {
        return ptr::null_mut();
    };
    let Some(m_usize) = ffi_u64_to_usize(num_edges) else {
        return ptr::null_mut();
    };
    if m_usize.checked_mul(2).is_none() {
        return ptr::null_mut();
    }
    if m_usize > 0 && (src.is_null() || dst.is_null()) {
        return ptr::null_mut();
    }
    let empty: &[u64] = &[];
    let (src_slice, dst_slice) = if m_usize == 0 {
        (empty, empty)
    } else {
        (
            slice::from_raw_parts(src, m_usize),
            slice::from_raw_parts(dst, m_usize),
        )
    };
    let valid = if m_usize >= 4_000_000 && crate::spqr_thread_count() > 1 {
        let invalid = AtomicBool::new(false);
        let workers = crate::spqr_thread_count().min(m_usize).max(1);
        let chunk_len = m_usize.div_ceil(workers);
        thread::scope(|scope| {
            for (src_chunk, dst_chunk) in
                src_slice.chunks(chunk_len).zip(dst_slice.chunks(chunk_len))
            {
                let invalid = &invalid;
                scope.spawn(move || {
                    if src_chunk
                        .iter()
                        .zip(dst_chunk)
                        .any(|(&u, &v)| u >= n_usize as u64 || v >= n_usize as u64)
                    {
                        invalid.store(true, Ordering::Relaxed);
                    }
                });
            }
        });
        !invalid.load(Ordering::Relaxed)
    } else {
        src_slice.iter().zip(dst_slice).all(|(&u, &v)| {
            ffi_u64_to_usize(u).is_some_and(|ui| ui < n_usize)
                && ffi_u64_to_usize(v).is_some_and(|vi| vi < n_usize)
        })
    };
    if !valid {
        return ptr::null_mut();
    }
    match catch_unwind(AssertUnwindSafe(|| {
        Box::new(Graph64 {
            inner: crate::wide::Graph::from_edge_arrays(n_usize, src_slice, dst_slice),
        })
    })) {
        Ok(graph) => Box::into_raw(graph),
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_release_adjacency_u64(graph: *mut Graph64) {
    if let Some(graph) = graph64_mut(graph) {
        graph.release_adjacency();
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_num_nodes_u64(graph: *const Graph64) -> u64 {
    graph64(graph).map_or(0, |graph| graph.num_nodes() as u64)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_num_edges_u64(graph: *const Graph64) -> u64 {
    graph64(graph).map_or(0, |graph| graph.num_edges() as u64)
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_copy_edges_u64(
    graph: *const Graph64,
    first_edge: u64,
    sources: *mut u64,
    targets: *mut u64,
    count: u64,
) -> bool {
    let Some(graph) = graph64(graph) else {
        return false;
    };
    let Some(first_edge) = ffi_u64_to_usize(first_edge) else {
        return false;
    };
    let Some(count) = ffi_u64_to_usize(count) else {
        return false;
    };
    if first_edge.checked_add(count).is_none()
        || first_edge + count > graph.num_edges()
        || (count != 0 && sources.is_null() && targets.is_null())
    {
        return false;
    }
    for offset in 0..count {
        let edge = graph.edge(crate::wide::EdgeId((first_edge + offset) as u64));
        if !sources.is_null() {
            *sources.add(offset) = edge.src.0;
        }
        if !targets.is_null() {
            *targets.add(offset) = edge.dst.0;
        }
    }
    true
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_packed_half_edges_u40(
    graph: *const Graph64,
    out: *mut SpqrPackedU40Column64,
) -> bool {
    let (Some(graph), Some(out)) = (graph64(graph), out.as_mut()) else {
        return false;
    };
    let Some((low, high)) = graph.packed_edge_storage() else {
        return false;
    };
    *out = packed_u40_column(low, high);
    true
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_split_packed_edges_u40(
    graph: *const Graph64,
    source: *mut SpqrPackedU40Column64,
    target: *mut SpqrPackedU40Column64,
) -> bool {
    let (Some(graph), Some(source), Some(target)) =
        (graph64(graph), source.as_mut(), target.as_mut())
    else {
        return false;
    };
    let Some((source_low, source_high, target_low, target_high)) =
        graph.split_packed_edge_storage()
    else {
        return false;
    };
    *source = packed_u40_column(source_low, source_high);
    *target = packed_u40_column(target_low, target_high);
    true
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_edge_src_u64(graph: *const Graph64, edge_id: u64) -> u64 {
    let Some((graph, edge_id)) = graph64_edge(graph, edge_id) else {
        return ffi_u64_invalid();
    };
    graph.edge(edge_id).src.0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_edge_dst_u64(graph: *const Graph64, edge_id: u64) -> u64 {
    let Some((graph, edge_id)) = graph64_edge(graph, edge_id) else {
        return ffi_u64_invalid();
    };
    graph.edge(edge_id).dst.0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_degree_u64(graph: *const Graph64, node: u64) -> u64 {
    let Some(graph) = graph64_node(graph, node) else {
        return ffi_u64_invalid();
    };
    graph.degree(crate::wide::NodeId(node)) as u64
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_adj_cursor_u64(graph: *const Graph64, node: u64) -> u64 {
    let Some(graph) = graph64_node(graph, node) else {
        return ffi_u64_invalid();
    };
    graph.adj_cursor(crate::wide::NodeId(node))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_adj_next_u64(
    graph: *const Graph64,
    cursor: u64,
    out_neighbor: *mut u64,
    out_edge: *mut u64,
    out_next_cursor: *mut u64,
) -> bool {
    if graph.is_null() {
        return false;
    }
    let graph = &*graph;
    let Some((neighbor, edge, next)) = graph.inner.adj_next(cursor) else {
        return false;
    };
    if !out_neighbor.is_null() {
        *out_neighbor = neighbor.0;
    }
    if !out_edge.is_null() {
        *out_edge = edge.0;
    }
    if !out_next_cursor.is_null() {
        *out_next_cursor = next;
    }
    true
}

type NeighborCallback64 = unsafe extern "C" fn(u64, u64, *mut std::ffi::c_void) -> bool;

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_for_each_neighbor_u64(
    graph: *const Graph64,
    node: u64,
    callback: Option<NeighborCallback64>,
    user_data: *mut std::ffi::c_void,
) {
    if graph.is_null() || callback.is_none() {
        return;
    }
    let Some(node_idx) = ffi_u64_to_usize(node) else {
        return;
    };
    let graph = &*graph;
    if node_idx >= graph.inner.num_nodes() {
        return;
    }
    let callback = callback.unwrap();
    for (neighbor, edge) in graph.inner.neighbors(crate::wide::NodeId(node)) {
        if !callback(neighbor.0, edge.0, user_data) {
            break;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_neighbors_to_buffer_u64(
    graph: *const Graph64,
    node: u64,
    nodes_out: *mut u64,
    edges_out: *mut u64,
    buffer_size: u64,
) -> u64 {
    if graph.is_null() {
        return 0;
    }
    let Some(node_idx) = ffi_u64_to_usize(node) else {
        return 0;
    };
    let Some(capacity) = ffi_u64_to_usize(buffer_size) else {
        return 0;
    };
    let graph = &*graph;
    if node_idx >= graph.inner.num_nodes() {
        return 0;
    }
    let mut count = 0usize;
    for (neighbor, edge) in graph.inner.neighbors(crate::wide::NodeId(node)) {
        if count >= capacity {
            break;
        }
        if !nodes_out.is_null() {
            *nodes_out.add(count) = neighbor.0;
        }
        if !edges_out.is_null() {
            *edges_out.add(count) = edge.0;
        }
        count += 1;
    }
    count as u64
}

pub extern "C" fn spqr_graph_new(node_capacity: u32, edge_capacity: u32) -> *mut Graph {
    Box::into_raw(Box::new(Graph::with_capacity(
        node_capacity as usize,
        edge_capacity as usize,
    )))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_free(graph: *mut Graph) {
    if !graph.is_null() {
        drop(Box::from_raw(graph));
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_add_nodes(graph: *mut Graph, count: u32) -> u32 {
    let graph = &mut *graph;
    let first = graph.num_nodes() as u32;
    graph.add_nodes_fast(count as usize);
    first
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_add_edge(graph: *mut Graph, u: u32, v: u32) -> u32 {
    (*graph).add_edge(NodeId(u), NodeId(v)).0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_add_edges_batch(
    graph: *mut Graph,
    edges: *const u32,
    count: u32,
) {
    let graph = &mut *graph;
    let pairs = slice::from_raw_parts(edges, (count * 2) as usize);
    graph.add_edges_flat(pairs);
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_from_edges(
    num_nodes: u32,
    edges: *const u32,
    num_edges: u32,
) -> *mut Graph {
    let pairs = slice::from_raw_parts(edges, (num_edges * 2) as usize);
    let graph = Graph::from_edge_pairs(num_nodes as usize, pairs);
    Box::into_raw(Box::new(graph))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_from_arrays(
    num_nodes: u32,
    src: *const u32,
    dst: *const u32,
    num_edges: u32,
) -> *mut Graph {
    let src_slice = slice::from_raw_parts(src, num_edges as usize);
    let dst_slice = slice::from_raw_parts(dst, num_edges as usize);
    let graph = Graph::from_edge_arrays(num_nodes as usize, src_slice, dst_slice);
    Box::into_raw(Box::new(graph))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_num_nodes(graph: *const Graph) -> u32 {
    (*graph).num_nodes() as u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_num_edges(graph: *const Graph) -> u32 {
    (*graph).num_edges() as u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_edge_src(graph: *const Graph, edge_id: u32) -> u32 {
    (*graph).edge(EdgeId(edge_id)).src.0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_edge_dst(graph: *const Graph, edge_id: u32) -> u32 {
    (*graph).edge(EdgeId(edge_id)).dst.0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_degree(graph: *const Graph, node: u32) -> u32 {
    (*graph).degree(NodeId(node)) as u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_adj_cursor(graph: *const Graph, node: u32) -> u32 {
    (*graph).adj_cursor(NodeId(node))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_adj_next(
    graph: *const Graph,
    cursor: u32,
    out_neighbor: *mut u32,
    out_edge: *mut u32,
    out_next_cursor: *mut u32,
) -> bool {
    match (*graph).adj_next(cursor) {
        Some((neighbor, edge, next)) => {
            *out_neighbor = neighbor.0;
            *out_edge = edge.0;
            *out_next_cursor = next;
            true
        }
        None => false,
    }
}

pub type NeighborCallback = unsafe extern "C" fn(u32, u32, *mut std::ffi::c_void) -> bool;

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_for_each_neighbor(
    graph: *const Graph,
    node: u32,
    callback: NeighborCallback,
    user_data: *mut std::ffi::c_void,
) {
    for (v, eid) in (*graph).neighbors(NodeId(node)) {
        if !callback(v.0, eid.0, user_data) {
            break;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_graph_neighbors_to_buffer(
    graph: *const Graph,
    node: u32,
    nodes_out: *mut u32,
    edges_out: *mut u32,
    buffer_size: u32,
) -> u32 {
    let mut count = 0u32;
    for (v, eid) in (*graph).neighbors(NodeId(node)) {
        if count >= buffer_size {
            break;
        }
        *nodes_out.add(count as usize) = v.0;
        *edges_out.add(count as usize) = eid.0;
        count += 1;
    }
    count
}

pub struct CCResult {
    inner: ConnectedComponents,
}

#[no_mangle]
pub unsafe extern "C" fn spqr_connected_components(graph: *const Graph) -> *mut CCResult {
    Box::into_raw(Box::new(CCResult {
        inner: connected_components(&*graph),
    }))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_cc_free(cc: *mut CCResult) {
    if !cc.is_null() {
        drop(Box::from_raw(cc));
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_cc_count(cc: *const CCResult) -> u32 {
    (*cc).inner.num_components
}

#[no_mangle]
pub unsafe extern "C" fn spqr_cc_component_of(cc: *const CCResult, node: u32) -> u32 {
    (*cc).inner.component_of(NodeId(node))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_cc_components_raw(
    cc: *const CCResult,
    out_len: *mut u32,
) -> *const u32 {
    let comp = &(*cc).inner.component;
    *out_len = comp.len() as u32;
    comp.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn spqr_cc_count_in(cc: *const CCResult, component_id: u32) -> u32 {
    (*cc).inner.count_in(component_id) as u32
}

pub type NodeCallback = unsafe extern "C" fn(u32, *mut std::ffi::c_void) -> bool;

#[no_mangle]
pub unsafe extern "C" fn spqr_cc_for_each_in(
    cc: *const CCResult,
    component_id: u32,
    callback: NodeCallback,
    user_data: *mut std::ffi::c_void,
) {
    for node in (*cc).inner.nodes_in_iter(component_id) {
        if !callback(node.0, user_data) {
            break;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_tree_build(graph: *const Graph) -> *mut BCTree {
    Box::into_raw(Box::new(BCTree::build(&*graph)))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_tree_free(bc: *mut BCTree) {
    if !bc.is_null() {
        drop(Box::from_raw(bc));
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_num_blocks(bc: *const BCTree) -> u32 {
    (*bc).num_blocks() as u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_num_cut_vertices(bc: *const BCTree) -> u32 {
    (*bc).num_cut_vertices() as u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_is_biconnected(bc: *const BCTree) -> bool {
    (*bc).is_biconnected()
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_is_cut_vertex(bc: *const BCTree, node: u32) -> bool {
    (*bc).is_cut_vertex(NodeId(node))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_block_nodes(
    bc: *const BCTree,
    block_idx: u32,
    out_len: *mut u32,
) -> *const u32 {
    let nodes = (*bc).block_nodes(block_idx as usize);
    *out_len = nodes.len() as u32;
    nodes.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_block_edges(
    bc: *const BCTree,
    block_idx: u32,
    out_len: *mut u32,
) -> *const u32 {
    let edges = (*bc).block_edges(block_idx as usize);
    *out_len = edges.len() as u32;
    edges.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_cut_vertices(bc: *const BCTree, out_len: *mut u32) -> *const u32 {
    let cvs = (*bc).cut_vertices();
    *out_len = cvs.len() as u32;
    cvs.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_blocks_raw(
    bc: *const BCTree,
    out_num_blocks: *mut u32,
) -> *const u32 {
    let blocks = (*bc).blocks_raw();
    *out_num_blocks = blocks.len() as u32;
    blocks.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_nodes_flat_raw(
    bc: *const BCTree,
    out_len: *mut u32,
) -> *const u32 {
    let nodes = (*bc).nodes_flat_raw();
    *out_len = nodes.len() as u32;
    nodes.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_edges_flat_raw(
    bc: *const BCTree,
    out_len: *mut u32,
) -> *const u32 {
    let edges = (*bc).edges_flat_raw();
    *out_len = edges.len() as u32;
    edges.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_get_sizes(
    bc: *const BCTree,
    out_num_blocks: *mut u32,
    out_total_nodes: *mut u32,
    out_total_edges: *mut u32,
) {
    let bc = &*bc;
    let mut total_nodes = 0u32;
    let mut total_edges = 0u32;
    for i in 0..bc.num_blocks() {
        total_nodes += bc.block_nodes(i).len() as u32;
        total_edges += bc.block_edges(i).len() as u32;
    }
    *out_num_blocks = bc.num_blocks() as u32;
    *out_total_nodes = total_nodes;
    *out_total_edges = total_edges;
}

#[no_mangle]
pub unsafe extern "C" fn spqr_bc_bulk_export(
    bc: *const BCTree,
    block_node_offsets: *mut u32,
    block_nodes: *mut u32,
    block_edge_offsets: *mut u32,
    block_edges: *mut u32,
) {
    let bc = &*bc;
    let mut node_idx = 0u32;
    let mut edge_idx = 0u32;

    for i in 0..bc.num_blocks() {
        *block_node_offsets.add(i) = node_idx;
        for &node in bc.block_nodes(i) {
            *block_nodes.add(node_idx as usize) = node.0;
            node_idx += 1;
        }

        *block_edge_offsets.add(i) = edge_idx;
        for &edge in bc.block_edges(i) {
            *block_edges.add(edge_idx as usize) = edge.0;
            edge_idx += 1;
        }
    }

    let n = bc.num_blocks();
    *block_node_offsets.add(n) = node_idx;
    *block_edge_offsets.add(n) = edge_idx;
}

pub const SPQR_NODE_TYPE_S: u8 = 0;
pub const SPQR_NODE_TYPE_P: u8 = 1;
pub const SPQR_NODE_TYPE_R: u8 = 2;

#[no_mangle]
pub unsafe extern "C" fn spqr_build(graph: *const Graph) -> *mut SpqrResult {
    Box::into_raw(Box::new(build_spqr(&*graph)))
}

#[no_mangle]
pub unsafe extern "C" fn spqr_result_free(result: *mut SpqrResult) {
    if !result.is_null() {
        drop(Box::from_raw(result));
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_result_tree(result: *const SpqrResult) -> *const SpqrTree {
    &(*result).tree
}

#[no_mangle]
pub unsafe extern "C" fn spqr_result_self_loops(
    result: *const SpqrResult,
    out_len: *mut u32,
) -> *const u32 {
    let loops = &(*result).self_loops;
    *out_len = loops.len() as u32;
    loops.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_len(tree: *const SpqrTree) -> u32 {
    (*tree).len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_root(tree: *const SpqrTree) -> u32 {
    (*tree).root.0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_get_sizes(
    tree: *const SpqrTree,
    out_num_nodes: *mut u32,
    out_total_children: *mut u32,
    out_total_skeleton_edges: *mut u32,
) {
    let tree = &*tree;
    let num_nodes = tree.len();
    let total_children = tree.children.len();
    let total_skeleton_edges = tree.skeleton_edges.len();

    *out_num_nodes = num_nodes as u32;
    *out_total_children = total_children as u32;
    *out_total_skeleton_edges = total_skeleton_edges as u32;
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_bulk_export(
    tree: *const SpqrTree,
    node_types: *mut u8,
    node_parents: *mut u32,
    children_offsets: *mut u32,
    children: *mut u32,
    skeleton_offsets: *mut u32,
    skeleton_src: *mut u32,
    skeleton_dst: *mut u32,
    skeleton_real_edge: *mut u32,
    skeleton_is_virtual: *mut u8,
) {
    let tree = &*tree;
    let n = tree.len();

    // Copy node types
    for i in 0..n {
        *node_types.add(i) = match tree.node_types[i] {
            SpqrNodeType::S => SPQR_NODE_TYPE_S,
            SpqrNodeType::P => SPQR_NODE_TYPE_P,
            SpqrNodeType::R => SPQR_NODE_TYPE_R,
        };
    }

    // Copy node parents
    for i in 0..n {
        *node_parents.add(i) = tree.node_parents[i].0;
    }

    // Copy children offsets and children
    for i in 0..=n {
        *children_offsets.add(i) = tree.children_offsets[i];
    }
    for (i, child) in tree.children.iter().enumerate() {
        *children.add(i) = child.0;
    }

    // Copy skeleton offsets
    for i in 0..=n {
        *skeleton_offsets.add(i) = tree.skeleton_offsets[i];
    }

    // Copy skeleton edges
    for (i, edge) in tree.skeleton_edges.iter().enumerate() {
        *skeleton_src.add(i) = edge.src.0;
        *skeleton_dst.add(i) = edge.dst.0;
        *skeleton_real_edge.add(i) = edge.real_edge.0;
        *skeleton_is_virtual.add(i) = if edge.twin_tree_node.is_valid() { 1 } else { 0 };
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_bulk_export_node_mapping(
    tree: *const SpqrTree,
    node_mapping_offsets: *mut u32,
    node_mapping: *mut u32,
) {
    let tree = &*tree;
    let n = tree.len();

    // Copy offsets
    for i in 0..=n {
        *node_mapping_offsets.add(i) = tree.node_mapping_offsets[i];
    }

    // Copy node mappings
    for (i, &orig) in tree.node_mapping.iter().enumerate() {
        *node_mapping.add(i) = orig.0;
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_node_type(tree: *const SpqrTree, node_id: u32) -> u8 {
    match (*tree).node(TreeNodeId(node_id)).node_type {
        SpqrNodeType::S => SPQR_NODE_TYPE_S,
        SpqrNodeType::P => SPQR_NODE_TYPE_P,
        SpqrNodeType::R => SPQR_NODE_TYPE_R,
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_node_parent(tree: *const SpqrTree, node_id: u32) -> u32 {
    (*tree).node(TreeNodeId(node_id)).parent.0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_node_children(
    tree: *const SpqrTree,
    node_id: u32,
    out_len: *mut u32,
) -> *const u32 {
    let children = &(*tree).node(TreeNodeId(node_id)).children;
    *out_len = children.len() as u32;
    children.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_num_edges(tree: *const SpqrTree, node_id: u32) -> u32 {
    (*tree).node(TreeNodeId(node_id)).skeleton.num_edges() as u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_num_nodes(tree: *const SpqrTree, node_id: u32) -> u32 {
    (*tree).node(TreeNodeId(node_id)).skeleton.num_nodes
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_poles(
    tree: *const SpqrTree,
    node_id: u32,
    pole1: *mut u32,
    pole2: *mut u32,
) {
    let (p1, p2) = (*tree).node(TreeNodeId(node_id)).skeleton.poles();
    *pole1 = p1.0;
    *pole2 = p2.0;
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_node_of_edge(tree: *const SpqrTree, edge_id: u32) -> u32 {
    (*tree).tree_node_of_edge(EdgeId(edge_id)).0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_edge_mapping_raw(
    tree: *const SpqrTree,
    out_len: *mut u32,
) -> *const u32 {
    let mapping = &(*tree).edge_to_tree_node;
    *out_len = mapping.len() as u32;
    mapping.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_edge_mapping_bulk(
    tree: *const SpqrTree,
    num_edges: u32,
    out_tree_nodes: *mut u32,
) {
    let tree = &*tree;
    for i in 0..num_edges as usize {
        *out_tree_nodes.add(i) = tree.tree_node_of_edge(EdgeId(i as u32)).0;
    }
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_normalize(tree: *mut SpqrTree) {
    (*tree).normalize();
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_compact(tree: *mut SpqrTree) {
    (*tree).compact();
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_count_by_type(
    tree: *const SpqrTree,
    s_count: *mut u32,
    p_count: *mut u32,
    r_count: *mut u32,
) {
    let (s, p, r) = (*tree).count_by_type();
    *s_count = s as u32;
    *p_count = p as u32;
    *r_count = r as u32;
}

#[repr(C)]
pub struct SkeletonEdgeInfo {
    pub src: u32,
    pub dst: u32,
    pub real_edge: u32,
    pub twin_tree_node: u32,
    pub is_virtual: bool,
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_edge(
    tree: *const SpqrTree,
    node_id: u32,
    edge_idx: u32,
    out: *mut SkeletonEdgeInfo,
) {
    let skeleton = &(*tree).node(TreeNodeId(node_id)).skeleton;
    let edge = &skeleton.edges[edge_idx as usize];
    (*out).src = edge.src.0;
    (*out).dst = edge.dst.0;
    (*out).real_edge = edge.real_edge.0;
    (*out).twin_tree_node = edge.twin_tree_node.0;
    (*out).is_virtual = edge.twin_tree_node.is_valid();
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_original_node(
    tree: *const SpqrTree,
    tree_node_id: u32,
    local_node: u32,
) -> u32 {
    let skeleton = &(*tree).node(TreeNodeId(tree_node_id)).skeleton;
    skeleton.node_to_original[local_node as usize].0
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_info(
    tree: *const SpqrTree,
    out_num_nodes: *mut u32,
    out_root: *mut u32,
) {
    let t = &*tree;
    *out_num_nodes = t.len() as u32;
    *out_root = t.root.0;
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_node_types_raw(tree: *const SpqrTree) -> *const u8 {
    (*tree).node_types.as_ptr() as *const u8
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_node_parents_raw(tree: *const SpqrTree) -> *const u32 {
    (*tree).node_parents.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_children_offsets_raw(tree: *const SpqrTree) -> *const u32 {
    (*tree).children_offsets.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_children_raw(
    tree: *const SpqrTree,
    out_len: *mut u32,
) -> *const u32 {
    let c = &(*tree).children;
    *out_len = c.len() as u32;
    c.as_ptr() as *const u32
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_offsets_raw(tree: *const SpqrTree) -> *const u32 {
    (*tree).skeleton_offsets.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_edges_raw(
    tree: *const SpqrTree,
    out_len: *mut u32,
) -> *const SkeletonEdge {
    let edges = &(*tree).skeleton_edges;
    *out_len = edges.len() as u32;
    edges.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_node_mapping_raw(
    tree: *const SpqrTree,
    out_offsets: *mut *const u32,
    out_mapping: *mut *const u32,
    out_mapping_len: *mut u32,
) {
    let t = &*tree;
    *out_offsets = t.node_mapping_offsets.as_ptr();
    *out_mapping = t.node_mapping.as_ptr() as *const u32;
    *out_mapping_len = t.node_mapping.len() as u32;
}

#[no_mangle]
pub unsafe extern "C" fn spqr_tree_skeleton_num_nodes_raw(tree: *const SpqrTree) -> *const u32 {
    (*tree).skeleton_num_nodes.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn spqr_format_to_string(
    graph: *const Graph,
    result: *const SpqrResult,
) -> *mut c_char {
    let mut buffer = Cursor::new(Vec::new());
    if write_spqr_format(&mut buffer, &*graph, &*result).is_err() {
        return ptr::null_mut();
    }
    let mut bytes = buffer.into_inner();
    bytes.push(0);
    let ptr = bytes.as_mut_ptr() as *mut c_char;
    std::mem::forget(bytes);
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn spqr_string_free(s: *mut c_char) {
    if !s.is_null() {
        let len = CStr::from_ptr(s).to_bytes_with_nul().len();
        drop(Vec::from_raw_parts(s as *mut u8, len, len));
    }
}

#[cfg(test)]
mod ffi64_tests {
    use super::*;

    #[test]
    fn ffi_u64_graph_builds_k4_with_wide_backend() {
        unsafe {
            let g = spqr_graph_new_u64(4, 6);
            assert!(!g.is_null());
            assert_eq!(spqr_graph_add_nodes_u64(g, 4), 0);
            assert_eq!(spqr_graph_add_edge_u64(g, 0, 1), 0);
            assert_eq!(spqr_graph_add_edge_u64(g, 0, 2), 1);
            assert_eq!(spqr_graph_add_edge_u64(g, 0, 3), 2);
            assert_eq!(spqr_graph_add_edge_u64(g, 1, 2), 3);
            assert_eq!(spqr_graph_add_edge_u64(g, 1, 3), 4);
            assert_eq!(spqr_graph_add_edge_u64(g, 2, 3), 5);
            assert_eq!(spqr_graph_num_nodes_u64(g), 4);
            assert_eq!(spqr_graph_num_edges_u64(g), 6);

            let r = spqr_build_u64(g);
            assert!(!r.is_null());
            let t = spqr_result_tree_u64(r);
            assert!(!t.is_null());
            assert!(spqr_tree_len_u64(t) > 0);
            assert_ne!(spqr_tree_root_u64(t), u64::MAX);
            assert_ne!(spqr_tree_node_type_u64(t, spqr_tree_root_u64(t)), u8::MAX);

            let mut p1 = u64::MAX;
            let mut p2 = u64::MAX;
            spqr_tree_skeleton_poles_u64(t, spqr_tree_root_u64(t), &mut p1, &mut p2);
            assert_ne!(p1, u64::MAX);
            assert_ne!(p2, u64::MAX);

            spqr_result_free_u64(r);
            spqr_graph_free_u64(g);
        }
    }

    fn tree_snapshot_u32(tree: &crate::SpqrTree) -> Vec<String> {
        fn wid(v: u32) -> u64 {
            if v == u32::MAX {
                u64::MAX
            } else {
                v as u64
            }
        }
        let mut out = Vec::new();
        out.push(format!("root:{}", wid(tree.root.0)));
        out.push(format!(
            "parents:{:?}",
            tree.node_parents
                .iter()
                .map(|v| wid(v.0))
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "types:{:?}",
            tree.node_types
                .iter()
                .map(|t| match t {
                    crate::SpqrNodeType::S => 0u8,
                    crate::SpqrNodeType::P => 1,
                    crate::SpqrNodeType::R => 2,
                })
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "children_offsets:{:?}",
            tree.children_offsets
                .iter()
                .map(|&v| v as u64)
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "children:{:?}",
            tree.children.iter().map(|v| wid(v.0)).collect::<Vec<_>>()
        ));
        out.push(format!(
            "skeleton_offsets:{:?}",
            tree.skeleton_offsets
                .iter()
                .map(|&v| v as u64)
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "skeleton_num_nodes:{:?}",
            tree.skeleton_num_nodes
                .iter()
                .map(|&v| v as u64)
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "node_mapping_offsets:{:?}",
            tree.node_mapping_offsets
                .iter()
                .map(|&v| v as u64)
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "node_mapping:{:?}",
            tree.node_mapping
                .iter()
                .map(|v| wid(v.0))
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "edge_to_tree:{:?}",
            tree.edge_to_tree_node
                .iter()
                .map(|v| wid(v.0))
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "skel:{:?}",
            tree.skeleton_edges
                .iter()
                .map(|e| (
                    wid(e.src.0),
                    wid(e.dst.0),
                    wid(e.real_edge.0),
                    wid(e.virtual_id),
                    wid(e.twin_tree_node.0),
                    wid(e.twin_edge_idx)
                ))
                .collect::<Vec<_>>()
        ));
        out
    }

    fn tree_snapshot_u64(tree: &crate::wide::SpqrTree) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!("root:{}", tree.root.0));
        out.push(format!(
            "parents:{:?}",
            tree.node_parents.iter().map(|v| v.0).collect::<Vec<_>>()
        ));
        out.push(format!(
            "types:{:?}",
            tree.node_types
                .iter()
                .map(|t| match t {
                    crate::wide::SpqrNodeType::S => 0u8,
                    crate::wide::SpqrNodeType::P => 1,
                    crate::wide::SpqrNodeType::R => 2,
                })
                .collect::<Vec<_>>()
        ));
        out.push(format!("children_offsets:{:?}", tree.children_offsets));
        out.push(format!(
            "children:{:?}",
            tree.children.iter().map(|v| v.0).collect::<Vec<_>>()
        ));
        out.push(format!("skeleton_offsets:{:?}", tree.skeleton_offsets));
        out.push(format!("skeleton_num_nodes:{:?}", tree.skeleton_num_nodes));
        out.push(format!(
            "node_mapping_offsets:{:?}",
            tree.node_mapping_offsets
        ));
        out.push(format!(
            "node_mapping:{:?}",
            tree.node_mapping.iter().map(|v| v.0).collect::<Vec<_>>()
        ));
        out.push(format!(
            "edge_to_tree:{:?}",
            tree.edge_to_tree_node
                .iter()
                .map(|v| v.0)
                .collect::<Vec<_>>()
        ));
        out.push(format!(
            "skel:{:?}",
            tree.skeleton_edges
                .iter()
                .map(|e| (
                    e.src.0,
                    e.dst.0,
                    e.real_edge.0,
                    e.virtual_id,
                    e.twin_tree_node.0,
                    e.twin_edge_idx
                ))
                .collect::<Vec<_>>()
        ));
        out
    }

    #[test]
    fn wide_backend_matches_u32_backend_on_canonical_small_graphs() {
        let cases: &[(usize, &[(u32, u32)])] = &[
            (2, &[(0, 1)]),
            (2, &[(0, 1), (0, 1), (0, 1)]),
            (5, &[(0, 1), (1, 2), (2, 3), (3, 4), (4, 0)]),
            (4, &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)]),
            (4, &[(0, 1), (1, 2), (2, 0), (0, 3), (3, 1)]),
            (4, &[(0, 1), (1, 2), (2, 0), (1, 3), (3, 2)]),
        ];
        for (case_idx, (n, edges)) in cases.iter().enumerate() {
            let mut g32 = crate::Graph::with_capacity(*n, edges.len());
            g32.add_nodes_fast(*n);
            let mut g64 = crate::wide::Graph::with_capacity(*n, edges.len());
            g64.add_nodes_fast(*n);
            for &(u, v) in *edges {
                g32.add_edge(crate::NodeId(u), crate::NodeId(v));
                g64.add_edge(crate::wide::NodeId(u as u64), crate::wide::NodeId(v as u64));
            }
            let r32 = crate::build_spqr(&g32);
            let r64 = crate::wide::build_spqr(&g64);
            assert_eq!(
                r32.self_loops
                    .iter()
                    .map(|e| e.0 as u64)
                    .collect::<Vec<_>>(),
                r64.self_loops.iter().map(|e| e.0).collect::<Vec<_>>(),
                "self-loops mismatch in case {case_idx}"
            );
            assert_eq!(
                tree_snapshot_u32(&r32.tree),
                tree_snapshot_u64(&r64.tree),
                "tree mismatch in case {case_idx}"
            );
        }
    }

    #[test]
    fn ffi_u64_uses_wide_ids_and_checks_graph_bounds() {
        assert_eq!(std::mem::size_of::<crate::wide::NodeId>(), 8);
        assert_eq!(std::mem::size_of::<crate::wide::EdgeId>(), 8);
        assert_eq!(std::mem::size_of::<crate::wide::TreeNodeId>(), 8);

        unsafe {
            let g = spqr_graph_new_u64(u32::MAX as u64 + 1, 0);
            assert!(!g.is_null());
            assert_eq!(spqr_graph_num_nodes_u64(g), 0);
            assert_eq!(spqr_graph_add_edge_u64(g, 0, u32::MAX as u64 + 1), u64::MAX);
            spqr_graph_free_u64(g);
        }

        unsafe {
            let src = [0u64, 1, 2];
            let dst = [1u64, 2, u32::MAX as u64 + 1];
            assert!(spqr_graph_from_arrays_u64(3, src.as_ptr(), dst.as_ptr(), 3).is_null());
        }
    }
}
