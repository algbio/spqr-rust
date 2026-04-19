//! FFI (interface) for C/C++

#![allow(clippy::missing_safety_doc)]

use crate::biconnected::BCTree;
use crate::connected::{connected_components, ConnectedComponents};
use crate::spqr_format::write_spqr_format;
use crate::{
    build_spqr, EdgeId, Graph, NodeId, SkeletonEdge, SpqrNodeType, SpqrResult, SpqrTree,
    TreeNodeId, FAST_CYCLE_CALLS, FAST_CYCLE_HITS,
};
use std::ffi::CStr;
use std::io::Cursor;
use std::os::raw::c_char;
use std::ptr;
use std::slice;

#[no_mangle]
pub extern "C" fn spqr_get_fast_cycle_hits() -> u64 {
    FAST_CYCLE_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn spqr_get_fast_cycle_calls() -> u64 {
    FAST_CYCLE_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

#[no_mangle]
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
    graph.add_nodes(count as usize);
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
    for i in 0..count as usize {
        graph.add_edge(NodeId(pairs[i * 2]), NodeId(pairs[i * 2 + 1]));
    }
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
