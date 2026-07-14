use crate::sp_compress::types::{CompressionInput, CompressionStats, SpTree};
use crate::{
    build_spqr_raw, build_spqr_raw_no_multi_edges, build_spqr_raw_no_self_loops, Graph, NodeId,
    SpqrResult,
};

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
        Self {
            macro_tree,
            core_spqr,
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

    let spqr = if core_edges_have_no_non_loop_parallel(macro_tree) {
        build_spqr_raw_no_multi_edges(&graph)
    } else if has_self_loop {
        build_spqr_raw(&graph)
    } else {
        build_spqr_raw_no_self_loops(&graph)
    };

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
    use crate::sp_compress::types::InputEdge;
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
