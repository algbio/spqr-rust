use crate::sp_compress::types::{ChildRef, CompressionInput, CompressionStats, SpTree};
use crate::{
    build_spqr_raw, build_spqr_raw_no_multi_edges, build_spqr_raw_no_self_loops, Graph, NodeId,
    SpqrResult, INVALID,
};
use std::sync::Arc;

pub(crate) enum CoreNodeMapper {
    Dense(Vec<u32>),
    Sparse {
        keys: Vec<u32>,
        vals: Vec<u32>,
        mask: usize,
    },
    SortedCoreNodes,
}

impl CoreNodeMapper {
    pub(crate) fn new(n_orig: usize, inv: &[NodeId]) -> Self {
        const MIN_DENSE_NODES: usize = 4096;
        const MAX_DENSE_OVERHEAD: usize = 8;
        const MIN_SPARSE_NODES: usize = 1024;

        let dense_limit = inv.len().saturating_mul(MAX_DENSE_OVERHEAD);
        if n_orig <= MIN_DENSE_NODES || n_orig <= dense_limit {
            let mut remap = vec![u32::MAX; n_orig];
            for (idx, v) in inv.iter().enumerate() {
                remap[v.idx()] = idx as u32;
            }
            return Self::Dense(remap);
        }

        if inv.len() >= MIN_SPARSE_NODES {
            let mut cap = 1usize;
            let need = inv.len().saturating_mul(2).max(1);
            while cap < need {
                cap <<= 1;
            }
            let mut keys = vec![u32::MAX; cap];
            let mut vals = vec![u32::MAX; cap];
            let mask = cap - 1;
            for (idx, v) in inv.iter().enumerate() {
                let mut slot = Self::hash_u32(v.0) & mask;
                while keys[slot] != u32::MAX {
                    slot = (slot + 1) & mask;
                }
                keys[slot] = v.0;
                vals[slot] = idx as u32;
            }
            return Self::Sparse { keys, vals, mask };
        }

        debug_assert!(inv.windows(2).all(|w| w[0].0 < w[1].0));
        Self::SortedCoreNodes
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

    #[inline]
    pub(crate) fn lookup(&self, inv: &[NodeId], original: u32) -> u32 {
        match self {
            Self::Dense(remap) => {
                let mapped = remap[original as usize];
                debug_assert!(mapped != u32::MAX);
                mapped
            }
            Self::Sparse { keys, vals, mask } => {
                let mut slot = Self::hash_u32(original) & *mask;
                loop {
                    let key = keys[slot];
                    if key == original {
                        return vals[slot];
                    }
                    if key == u32::MAX {
                        panic!("core edge endpoint missing from sparse core node map");
                    }
                    slot = (slot + 1) & *mask;
                }
            }
            Self::SortedCoreNodes => {
                inv.binary_search_by_key(&original, |v| v.0)
                    .expect("core edge endpoint missing from core_nodes") as u32
            }
        }
    }
}

pub struct CompressAndSpqrResult {
    pub macro_tree: SpTree,

    pub core_spqr: Option<SpqrResult>,
    pub core_scad_export: Option<Arc<CoreScadExport>>,
    pub spqra_minimizer_sidecar: Option<Arc<SpqraMinimizerSidecar>>,

    pub core_node_remap: Vec<u32>,
    pub core_node_inv: Vec<NodeId>,
}

impl CompressAndSpqrResult {
    pub(crate) fn from_parts(
        macro_tree: SpTree,
        core_spqr: Option<SpqrResult>,
        core_node_remap: Vec<u32>,
        core_node_inv: Vec<NodeId>,
    ) -> Self {
        let preassembly_minimizer_sidecar = core_spqr.as_ref().and_then(|s| {
            s.tree
                .preassembly_minimizer_sidecar
                .as_ref()
                .map(Arc::clone)
        });
        let core_scad_export = maybe_export_core_scad(&core_spqr);
        let spqra_minimizer_sidecar = preassembly_minimizer_sidecar.or_else(|| {
            maybe_export_spqra_minimizer_sidecar(&macro_tree, core_scad_export.as_deref())
        });
        Self {
            macro_tree,
            core_spqr,
            core_scad_export,
            spqra_minimizer_sidecar,
            core_node_remap,
            core_node_inv,
        }
    }

    pub fn stats(&self) -> &CompressionStats {
        &self.macro_tree.stats
    }
}

pub fn compress_and_build_spqr(input: &CompressionInput) -> CompressAndSpqrResult {
    compress_and_build_spqr_borrowed(input.n_nodes, &input.edges, &input.contractible)
}

pub fn compress_and_build_spqr_borrowed(
    n_nodes: u32,
    input_edges: &[crate::sp_compress::types::InputEdge],
    contractible: &[u8],
) -> CompressAndSpqrResult {
    let cr = crate::sp_compress::reduction::compress_borrowed(n_nodes, input_edges, contractible);
    let macro_tree = cr.tree;
    let (core_spqr, core_node_remap, core_node_inv) = build_core_spqr_parts(n_nodes, &macro_tree);

    CompressAndSpqrResult::from_parts(macro_tree, core_spqr, core_node_remap, core_node_inv)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiScadComponent {
    pub raw_component_id: u32,
    pub kind: u8,
    pub _pad: [u8; 3],
    pub edge_begin: u32,
    pub edge_end: u32,
    pub inc_begin: u32,
    pub inc_end: u32,
    pub node_begin: u32,
    pub node_end: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiScadEdge {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub src_local: u32,
    pub dst_local: u32,
    pub src_core: u32,
    pub dst_core: u32,
    pub original_edge_id: u32,
    pub macro_id: u32,
    pub virtual_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiScadIncidence {
    pub virtual_id: u32,
    pub component_id: u32,
    pub local_edge_id: u32,
    pub twin_incidence: u32,
    pub sep_u: u32,
    pub sep_v: u32,
}

pub struct CoreScadExport {
    pub components: Vec<FfiScadComponent>,
    pub edges: Vec<FfiScadEdge>,
    pub incidences: Vec<FfiScadIncidence>,
    pub node_mapping: Vec<u32>,
}

pub const SPQRA_MIN_EDGE_VIRTUAL: u32 = 1 << 1;
pub const SPQRA_MIN_EDGE_HAS_CHILD_REF: u32 = 1 << 3;
pub const SPQRA_MIN_EDGE_HAS_BEHAVIOR_ATOM: u32 = 1 << 6;

pub const SPQRA_MIN_ATOM_ITEM_CHILD_REF: u32 = 1 << 0;
pub const SPQRA_MIN_ATOM_ITEM_BEHAVIOR_ATOM: u32 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiSpqraMinimizerComponent {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub raw_component_id: u32,
    pub parent: u32,
    pub child_begin: u32,
    pub child_end: u32,
    pub edge_begin: u32,
    pub edge_end: u32,
    pub inc_begin: u32,
    pub inc_end: u32,
    pub node_begin: u32,
    pub node_end: u32,
    pub port0_core: u32,
    pub port1_core: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiSpqraMinimizerEdge {
    pub twin_component: u32,
    pub twin_local_edge: u32,
    pub child_ref: ChildRef,
    pub flags: u32,
    pub src_local: u32,
    pub dst_local: u32,
}

impl Default for FfiSpqraMinimizerEdge {
    fn default() -> Self {
        Self {
            twin_component: INVALID,
            twin_local_edge: INVALID,
            child_ref: INVALID.into(),
            flags: 0,
            src_local: INVALID,
            dst_local: INVALID,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiSpqraMinimizerSummary {
    pub root: u32,
    pub bad_twin_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiSpqraBehaviorAtom {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub item_begin: u32,
    pub item_end: u32,
    pub port0_core: u32,
    pub port1_core: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiSpqraBehaviorAtomItem {
    pub child_ref: ChildRef,
    pub flags: u32,
    pub src_core: u32,
    pub dst_core: u32,
}

pub struct SpqraMinimizerSidecar {
    pub components: Vec<FfiSpqraMinimizerComponent>,
    pub edges: Vec<FfiSpqraMinimizerEdge>,
    pub behavior_atoms: Vec<FfiSpqraBehaviorAtom>,
    pub behavior_atom_items: Vec<FfiSpqraBehaviorAtomItem>,
    pub node_mapping: Vec<u32>,
    pub children: Vec<u32>,
    pub postorder: Vec<u32>,
    pub summary: FfiSpqraMinimizerSummary,
}

#[inline]
fn node_kind_byte(t: crate::SpqrNodeType) -> u8 {
    match t {
        crate::SpqrNodeType::S => 1,
        crate::SpqrNodeType::P => 2,
        crate::SpqrNodeType::R => 3,
    }
}

pub(crate) fn maybe_export_core_scad(
    core_spqr: &Option<SpqrResult>,
) -> Option<Arc<CoreScadExport>> {
    if !wants_core_scad_export() {
        return None;
    }
    let tree = &core_spqr.as_ref()?.tree;
    if let Some(pre) = &tree.preassembly_scad_export {
        return Some(Arc::clone(pre));
    }

    Some(Arc::new(export_core_scad_postassembly(tree)))
}

pub(crate) fn wants_core_scad_export() -> bool {
    let minimizer_view_collect = std::env::var_os("BF_SPQRA_MINIMIZER_VIEW_COLLECT").is_some();
    let minimizer_message_kernel = std::env::var_os("BF_SPQRA_MINIMIZER_MESSAGE_KERNEL").is_some();
    let minimizer_sidecar_only = minimizer_view_collect
        && minimizer_message_kernel
        && std::env::var_os("BF_SPQRA_DIRECT_CORE_SCAD").is_none()
        && std::env::var_os("BF_SPQRA_MINIMIZER_DIRECT_COLLECT").is_none();
    if minimizer_sidecar_only {
        return false;
    }
    std::env::var_os("BF_SPQRA_DIRECT_CORE_SCAD").is_some()
        || std::env::var_os("BF_SPQRA_SKIP_ASSEMBLE_SPQRTREE").is_some()
        || std::env::var_os("BF_SPQRA_MINIMIZER_DIRECT_COLLECT").is_some()
        || (minimizer_message_kernel && !minimizer_view_collect)
        || std::env::var("BF_SPQRA_EXECUTION_MODE")
            .map(|v| {
                v == "aq_context_direct"
                    || (v == "aq_atoms"
                        && std::env::var_os("BF_SPQRA_ATOMS_USE_SPQRTREE").is_none()
                        && !(minimizer_message_kernel && minimizer_view_collect))
            })
            .unwrap_or(false)
}

pub(crate) fn wants_spqra_minimizer_sidecar() -> bool {
    std::env::var_os("BF_SPQRA_EXPORT_MINIMIZER_SIDECAR").is_some()
        || std::env::var_os("BF_SPQRA_MINIMIZER_VIEW").is_some()
        || std::env::var_os("BF_SPQRA_MINIMIZER_DIRECT_COLLECT").is_some()
        || std::env::var_os("BF_SPQRA_MINIMIZER_MESSAGE_KERNEL").is_some()
}

pub(crate) fn maybe_export_spqra_minimizer_sidecar(
    macro_tree: &SpTree,
    scad: Option<&CoreScadExport>,
) -> Option<Arc<SpqraMinimizerSidecar>> {
    if !wants_spqra_minimizer_sidecar() {
        return None;
    }
    let scad = scad?;
    Some(Arc::new(build_spqra_minimizer_sidecar(macro_tree, scad)))
}

pub(crate) fn build_spqra_minimizer_sidecar(
    macro_tree: &SpTree,
    scad: &CoreScadExport,
) -> SpqraMinimizerSidecar {
    let n = scad.components.len();
    let root = if n == 0 { INVALID } else { 0 };
    let mut components = Vec::with_capacity(n);
    let mut edges = vec![FfiSpqraMinimizerEdge::default(); scad.edges.len()];

    let mut summary = FfiSpqraMinimizerSummary {
        root,
        ..FfiSpqraMinimizerSummary::default()
    };

    for comp in &scad.components {
        let node_begin = comp.node_begin as usize;
        let node_end = comp.node_end as usize;
        let port0_core = scad
            .node_mapping
            .get(node_begin)
            .copied()
            .unwrap_or(INVALID);
        let port1_core = if node_begin + 1 < node_end {
            scad.node_mapping
                .get(node_begin + 1)
                .copied()
                .unwrap_or(INVALID)
        } else {
            INVALID
        };
        components.push(FfiSpqraMinimizerComponent {
            kind: comp.kind,
            _pad: [0; 3],
            raw_component_id: comp.raw_component_id,
            parent: INVALID,
            child_begin: 0,
            child_end: 0,
            edge_begin: comp.edge_begin,
            edge_end: comp.edge_end,
            inc_begin: comp.inc_begin,
            inc_end: comp.inc_end,
            node_begin: comp.node_begin,
            node_end: comp.node_end,
            port0_core,
            port1_core,
        });
    }

    for comp in &scad.components {
        let edge_begin = comp.edge_begin as usize;
        let edge_end = comp.edge_end.min(scad.edges.len() as u32) as usize;
        for ge in edge_begin..edge_end {
            let fe = scad.edges[ge];
            let is_virtual = fe.kind == 3 || fe.virtual_id != INVALID;
            let mut edge = FfiSpqraMinimizerEdge {
                src_local: fe.src_local,
                dst_local: fe.dst_local,
                flags: if is_virtual {
                    SPQRA_MIN_EDGE_VIRTUAL
                } else {
                    0
                },
                ..FfiSpqraMinimizerEdge::default()
            };
            if !is_virtual && fe.original_edge_id != INVALID {
                if let Some(core_edge) = macro_tree.core_edges.get(fe.original_edge_id as usize) {
                    edge.child_ref = core_edge.child;
                    edge.flags |= SPQRA_MIN_EDGE_HAS_CHILD_REF;
                }
            }
            edges[ge] = edge;
        }
    }

    let mut tree_pairs: Vec<(u32, u32)> = Vec::with_capacity(scad.incidences.len() / 2);
    for (ii, inc) in scad.incidences.iter().enumerate() {
        let twin_idx = inc.twin_incidence as usize;
        if inc.component_id as usize >= n || twin_idx >= scad.incidences.len() {
            summary.bad_twin_count = summary.bad_twin_count.saturating_add(1);
            continue;
        }
        let tw = scad.incidences[twin_idx];
        if tw.twin_incidence as usize != ii || tw.component_id as usize >= n {
            summary.bad_twin_count = summary.bad_twin_count.saturating_add(1);
            continue;
        }
        let c0 = scad.components[inc.component_id as usize];
        let c1 = scad.components[tw.component_id as usize];
        let ge0 = c0.edge_begin.saturating_add(inc.local_edge_id);
        let ge1 = c1.edge_begin.saturating_add(tw.local_edge_id);
        if ge0 as usize >= edges.len()
            || ge1 as usize >= edges.len()
            || ge0 >= c0.edge_end
            || ge1 >= c1.edge_end
        {
            summary.bad_twin_count = summary.bad_twin_count.saturating_add(1);
            continue;
        }
        edges[ge0 as usize].twin_component = tw.component_id;
        edges[ge0 as usize].twin_local_edge = tw.local_edge_id;

        if ii > twin_idx {
            if inc.component_id == tw.component_id {
                summary.bad_twin_count = summary.bad_twin_count.saturating_add(1);
            } else {
                tree_pairs.push((inc.component_id, tw.component_id));
            }
        }
    }
    let mut parents = vec![INVALID; n];
    let children = if n == 0 {
        Vec::new()
    } else {
        let mut adj_count = vec![0u32; n];
        for &(a, b) in &tree_pairs {
            let ai = a as usize;
            let bi = b as usize;
            if ai < n && bi < n {
                adj_count[ai] = adj_count[ai].saturating_add(1);
                adj_count[bi] = adj_count[bi].saturating_add(1);
            }
        }
        let mut adj_offsets = vec![0u32; n + 1];
        for i in 0..n {
            adj_offsets[i + 1] = adj_offsets[i].saturating_add(adj_count[i]);
        }
        let mut adj = vec![INVALID; adj_offsets[n] as usize];
        let mut write = adj_offsets[..n].to_vec();
        for &(a, b) in &tree_pairs {
            let ai = a as usize;
            let bi = b as usize;
            if ai < n && bi < n {
                adj[write[ai] as usize] = b;
                write[ai] = write[ai].saturating_add(1);
                adj[write[bi] as usize] = a;
                write[bi] = write[bi].saturating_add(1);
            }
        }

        let mut seen = vec![false; n];
        let mut stack = vec![0u32];
        seen[0] = true;
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
        for i in 1..n {
            if !seen[i] {
                parents[i] = 0;
            }
        }

        let mut child_count = vec![0u32; n];
        for &p in &parents {
            if p != INVALID && (p as usize) < n {
                child_count[p as usize] = child_count[p as usize].saturating_add(1);
            }
        }
        let mut child_offsets = vec![0u32; n + 1];
        for i in 0..n {
            child_offsets[i + 1] = child_offsets[i].saturating_add(child_count[i]);
        }
        let mut children = vec![INVALID; child_offsets[n] as usize];
        write = child_offsets[..n].to_vec();
        for (child, &p) in parents.iter().enumerate() {
            if p != INVALID && (p as usize) < n {
                children[write[p as usize] as usize] = child as u32;
                write[p as usize] = write[p as usize].saturating_add(1);
            }
        }
        for i in 0..n {
            components[i].parent = parents[i];
            components[i].child_begin = child_offsets[i];
            components[i].child_end = child_offsets[i + 1];
        }
        children
    };

    let mut postorder = Vec::with_capacity(n);
    if n > 0 {
        let mut entered = vec![false; n];
        let mut stack = vec![0u32];
        while let Some(&tn) = stack.last() {
            let ti = tn as usize;
            if ti >= n {
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
        behavior_atoms: Vec::new(),
        behavior_atom_items: Vec::new(),
        node_mapping: scad.node_mapping.clone(),
        children,
        postorder,
        summary,
    }
}

pub(crate) fn export_core_scad_postassembly(tree: &crate::SpqrTree) -> CoreScadExport {
    let n = tree.len();
    let mut components = Vec::with_capacity(n);
    let mut edges = Vec::with_capacity(tree.skeleton_edges.len());
    let mut incidences = Vec::new();
    let mut incidence_of_edge = vec![u32::MAX; tree.skeleton_edges.len()];

    for tidx in 0..n {
        let edge_begin = tree.skeleton_offsets[tidx];
        let edge_end = tree.skeleton_offsets[tidx + 1];
        let node_begin = tree.node_mapping_offsets[tidx];
        let node_end = tree.node_mapping_offsets[tidx + 1];
        let inc_begin = incidences.len() as u32;

        for local_edge_idx in 0..(edge_end - edge_begin) {
            let global_edge_idx = (edge_begin + local_edge_idx) as usize;
            let edge = tree.skeleton_edges[global_edge_idx];
            let src_core = tree.node_mapping[(node_begin + edge.src.0) as usize].0;
            let dst_core = tree.node_mapping[(node_begin + edge.dst.0) as usize].0;
            let is_virtual = edge.virtual_id != INVALID;
            edges.push(FfiScadEdge {
                kind: if is_virtual { 3 } else { 1 },
                _pad: [0; 3],
                src_local: edge.src.0,
                dst_local: edge.dst.0,
                src_core,
                dst_core,
                original_edge_id: if edge.real_edge.is_valid() {
                    edge.real_edge.0
                } else {
                    INVALID
                },
                macro_id: INVALID,
                virtual_id: if is_virtual { edge.virtual_id } else { INVALID },
            });
            if is_virtual {
                let inc_idx = incidences.len() as u32;
                incidence_of_edge[global_edge_idx] = inc_idx;
                incidences.push(FfiScadIncidence {
                    virtual_id: edge.virtual_id,
                    component_id: tidx as u32,
                    local_edge_id: local_edge_idx,
                    twin_incidence: u32::MAX,
                    sep_u: src_core,
                    sep_v: dst_core,
                });
            }
        }

        let inc_end = incidences.len() as u32;
        components.push(FfiScadComponent {
            raw_component_id: tidx as u32,
            kind: node_kind_byte(tree.node_types[tidx]),
            _pad: [0; 3],
            edge_begin,
            edge_end,
            inc_begin,
            inc_end,
            node_begin,
            node_end,
        });
    }

    for tidx in 0..n {
        let edge_begin = tree.skeleton_offsets[tidx];
        let edge_end = tree.skeleton_offsets[tidx + 1];
        for local_edge_idx in 0..(edge_end - edge_begin) {
            let global_edge_idx = (edge_begin + local_edge_idx) as usize;
            let edge = tree.skeleton_edges[global_edge_idx];
            if edge.virtual_id == INVALID {
                continue;
            }
            let inc_idx = incidence_of_edge[global_edge_idx];
            let twin_t = edge.twin_tree_node.0;
            let twin_e = edge.twin_edge_idx;
            if twin_t == INVALID || twin_e == INVALID {
                continue;
            }
            let twin_global = (tree.skeleton_offsets[twin_t as usize] + twin_e) as usize;
            if twin_global < incidence_of_edge.len() {
                incidences[inc_idx as usize].twin_incidence = incidence_of_edge[twin_global];
            }
        }
    }

    CoreScadExport {
        components,
        edges,
        incidences,
        node_mapping: tree.node_mapping.iter().map(|v| v.0).collect(),
    }
}

#[inline]
pub(crate) fn build_core_spqr_parts(
    n_nodes: u32,
    macro_tree: &SpTree,
) -> (Option<SpqrResult>, Vec<u32>, Vec<NodeId>) {
    if macro_tree.stats.fully_sp_reducible != 0 || macro_tree.core_edges.is_empty() {
        return (None, Vec::new(), Vec::new());
    }

    let n_orig = n_nodes as usize;
    let mut remap = vec![u32::MAX; n_orig];
    let mut inv: Vec<NodeId> = Vec::with_capacity(macro_tree.core_nodes.len());
    for v in &macro_tree.core_nodes {
        remap[v.idx()] = inv.len() as u32;
        inv.push(*v);
    }

    let n_core = inv.len();
    let m_core = macro_tree.core_edges.len();

    let mut graph = Graph::with_capacity(n_core, m_core);
    graph.add_nodes_fast(n_core);
    let mut has_self_loop = false;
    for ce in &macro_tree.core_edges {
        let u_remap = remap[ce.u as usize];
        let v_remap = remap[ce.v as usize];
        debug_assert!(u_remap != u32::MAX);
        debug_assert!(v_remap != u32::MAX);
        has_self_loop |= u_remap == v_remap;
        graph.add_edge(NodeId(u_remap), NodeId(v_remap));
    }

    let core_child_refs = if wants_spqra_minimizer_sidecar() {
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
        if core_edges_have_no_non_loop_parallel(macro_tree) {
            build_spqr_raw_no_multi_edges(&graph)
        } else if has_self_loop {
            build_spqr_raw(&graph)
        } else {
            build_spqr_raw_no_self_loops(&graph)
        }
    });

    (Some(spqr), remap, inv)
}

#[inline]
pub(crate) fn build_core_spqr_parts_fast(
    n_nodes: u32,
    macro_tree: &SpTree,
) -> (Option<SpqrResult>, Vec<u32>, Vec<NodeId>) {
    if macro_tree.stats.fully_sp_reducible != 0 || macro_tree.core_edges.is_empty() {
        return (None, Vec::new(), Vec::new());
    }

    let inv: &[NodeId] = &macro_tree.core_nodes;
    let mapper = CoreNodeMapper::new(n_nodes as usize, inv);

    let n_core = inv.len();
    let m_core = macro_tree.core_edges.len();

    let mut graph = Graph::with_capacity(n_core, m_core);
    graph.add_nodes_fast(n_core);
    let mut has_self_loop = false;
    for ce in &macro_tree.core_edges {
        let u_remap = mapper.lookup(inv, ce.u);
        let v_remap = mapper.lookup(inv, ce.v);
        has_self_loop |= u_remap == v_remap;
        graph.add_edge(NodeId(u_remap), NodeId(v_remap));
    }

    let spqr = if core_edges_have_no_non_loop_parallel(macro_tree) {
        build_spqr_raw_no_multi_edges(&graph)
    } else if has_self_loop {
        build_spqr_raw(&graph)
    } else {
        build_spqr_raw_no_self_loops(&graph)
    };

    (Some(spqr), Vec::new(), Vec::new())
}

pub(crate) fn core_edges_have_no_non_loop_parallel(macro_tree: &SpTree) -> bool {
    let mut prev: Option<(u32, u32)> = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp_compress::types::{make_child_edge, CoreEdge, InputEdge};
    use crate::EdgeId;

    fn mk_input(n_nodes: u32, edges: &[(u32, u32)], contractible_set: &[u32]) -> CompressionInput {
        let mut input = CompressionInput {
            n_nodes,
            edges: Vec::with_capacity(edges.len()),
            contractible: vec![0u8; n_nodes as usize],
        };
        for &v in contractible_set {
            input.contractible[v as usize] = 1;
        }
        for (i, &(u, v)) in edges.iter().enumerate() {
            input.edges.push(InputEdge {
                u: NodeId(u),
                v: NodeId(v),
                original_edge_id: EdgeId(i as u32),
            });
        }
        input
    }

    #[test]
    fn theta_is_fully_reducible_so_no_spqr() {
        let input = mk_input(5, &[(0, 1), (1, 2), (2, 3), (0, 4), (4, 3)], &[1, 2, 4]);
        let r = compress_and_build_spqr(&input);
        assert_eq!(r.macro_tree.stats.fully_sp_reducible, 1);
        assert!(r.core_spqr.is_none());
    }

    #[test]
    fn k4_is_r_so_spqr_built() {
        let input = mk_input(
            4,
            &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)],
            &[0, 1, 2, 3],
        );
        let r = compress_and_build_spqr(&input);
        assert_eq!(r.macro_tree.stats.fully_sp_reducible, 0);
        assert!(r.core_spqr.is_some());

        let spqr = r.core_spqr.as_ref().unwrap();

        assert!(!spqr.tree.is_empty());
    }

    #[test]
    fn minimizer_sidecar_builds_topology_from_scad() {
        let child_ref = make_child_edge(EdgeId(7));
        let mut macro_tree = SpTree::default();
        macro_tree.core_edges.push(CoreEdge {
            u: 0,
            v: 1,
            child: child_ref,
        });

        let scad = CoreScadExport {
            components: vec![
                FfiScadComponent {
                    raw_component_id: 10,
                    kind: 2,
                    _pad: [0; 3],
                    edge_begin: 0,
                    edge_end: 2,
                    inc_begin: 0,
                    inc_end: 1,
                    node_begin: 0,
                    node_end: 2,
                },
                FfiScadComponent {
                    raw_component_id: 11,
                    kind: 3,
                    _pad: [0; 3],
                    edge_begin: 2,
                    edge_end: 3,
                    inc_begin: 1,
                    inc_end: 2,
                    node_begin: 2,
                    node_end: 4,
                },
            ],
            edges: vec![
                FfiScadEdge {
                    kind: 1,
                    _pad: [0; 3],
                    src_local: 0,
                    dst_local: 1,
                    src_core: 0,
                    dst_core: 1,
                    original_edge_id: 0,
                    macro_id: INVALID,
                    virtual_id: INVALID,
                },
                FfiScadEdge {
                    kind: 3,
                    _pad: [0; 3],
                    src_local: 0,
                    dst_local: 1,
                    src_core: 0,
                    dst_core: 1,
                    original_edge_id: INVALID,
                    macro_id: INVALID,
                    virtual_id: 100,
                },
                FfiScadEdge {
                    kind: 3,
                    _pad: [0; 3],
                    src_local: 0,
                    dst_local: 1,
                    src_core: 0,
                    dst_core: 1,
                    original_edge_id: INVALID,
                    macro_id: INVALID,
                    virtual_id: 100,
                },
            ],
            incidences: vec![
                FfiScadIncidence {
                    virtual_id: 100,
                    component_id: 0,
                    local_edge_id: 1,
                    twin_incidence: 1,
                    sep_u: 0,
                    sep_v: 1,
                },
                FfiScadIncidence {
                    virtual_id: 100,
                    component_id: 1,
                    local_edge_id: 0,
                    twin_incidence: 0,
                    sep_u: 0,
                    sep_v: 1,
                },
            ],
            node_mapping: vec![0, 1, 0, 1],
        };

        let sidecar = build_spqra_minimizer_sidecar(&macro_tree, &scad);

        assert_eq!(sidecar.summary.bad_twin_count, 0);
        assert_eq!(sidecar.components[0].parent, INVALID);
        assert_eq!(sidecar.components[1].parent, 0);
        assert_eq!(sidecar.children, vec![1]);
        assert_eq!(sidecar.postorder, vec![1, 0]);
        assert_eq!(sidecar.components[0].port0_core, 0);
        assert_eq!(sidecar.components[0].port1_core, 1);
        assert_eq!(sidecar.edges[0].child_ref, child_ref);
        assert_ne!(sidecar.edges[0].flags & SPQRA_MIN_EDGE_HAS_CHILD_REF, 0);
        assert_eq!(sidecar.edges[1].twin_component, 1);
        assert_eq!(sidecar.edges[1].twin_local_edge, 0);
        assert_eq!(sidecar.edges[2].twin_component, 0);
        assert_eq!(sidecar.edges[2].twin_local_edge, 1);
    }

    #[test]
    fn chain_is_fully_reducible() {
        let n = 100u32;
        let mut edges = Vec::new();
        for i in 0..=n {
            edges.push((i, i + 1));
        }
        let mut contr = Vec::new();
        for i in 1..=n {
            contr.push(i);
        }
        let input = mk_input(n + 2, &edges, &contr);
        let r = compress_and_build_spqr(&input);
        assert_eq!(r.macro_tree.stats.fully_sp_reducible, 1);
        assert!(r.core_spqr.is_none());
    }
}
