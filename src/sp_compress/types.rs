use crate::{EdgeId, NodeId};

pub type ChildRef = u64;
pub type SpNodeId = u64;

pub const TAG_BIT: ChildRef = 0x8000_0000_0000_0000;
pub const PAYLOAD_MASK: ChildRef = 0x7FFF_FFFF_FFFF_FFFF;

pub const INVALID_SP_NODE: SpNodeId = u64::MAX;

#[inline(always)]
pub const fn make_child_edge(eid: EdgeId) -> ChildRef {
    eid.0 as ChildRef
}

#[inline(always)]
pub const fn make_child_macro(mid: SpNodeId) -> ChildRef {
    mid | TAG_BIT
}

#[inline(always)]
pub const fn child_is_macro(c: ChildRef) -> bool {
    (c & TAG_BIT) != 0
}

#[inline(always)]
pub const fn child_is_edge(c: ChildRef) -> bool {
    (c & TAG_BIT) == 0
}

#[inline(always)]
pub const fn child_as_edge(c: ChildRef) -> EdgeId {
    EdgeId(c as u32)
}

#[inline(always)]
pub const fn child_as_macro(c: ChildRef) -> SpNodeId {
    c & PAYLOAD_MASK
}

pub const SP_KIND_SERIES: u8 = 1;
pub const SP_KIND_PARALLEL: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SpNode {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub left: u32,
    pub right: u32,
    pub children_offset: u64,
    pub children_count: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreEdge {
    pub u: u32,
    pub v: u32,
    pub child: ChildRef,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CompressionStats {
    pub input_nodes: u32,
    pub input_edges: u32,
    pub core_nodes: u32,
    pub core_edges_count: u32,
    pub macro_count: u32,
    pub macro_series: u32,
    pub macro_parallel: u32,
    pub series_reductions: u32,
    pub parallel_reductions: u32,
    pub iterations: u32,

    pub fully_sp_reducible: u8,
}

#[derive(Clone, Debug)]
pub struct CompressionInput {
    pub n_nodes: u32,
    pub edges: Vec<InputEdge>,

    pub contractible: Vec<u8>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEdge {
    pub u: NodeId,
    pub v: NodeId,
    pub original_edge_id: EdgeId,
}

#[derive(Default)]
pub struct SpTree {
    pub macros: Vec<SpNode>,
    pub children: Vec<ChildRef>,
    pub core_edges: Vec<CoreEdge>,
    pub core_nodes: Vec<NodeId>,

    pub input_endpoints: Vec<[u32; 2]>,

    pub stats: CompressionStats,
}

impl SpTree {
    pub fn set_input_edges(&mut self, edges: &[InputEdge]) {
        self.input_endpoints.clear();
        self.input_endpoints.reserve(edges.len());
        for e in edges {
            self.input_endpoints.push([e.u.0, e.v.0]);
        }
    }

    pub fn for_each_original_edge<F: FnMut(EdgeId)>(&self, c: ChildRef, fn_: &mut F) {
        if child_is_edge(c) {
            fn_(child_as_edge(c));
            return;
        }
        let m = self.macros[child_as_macro(c) as usize];
        for i in 0..m.children_count {
            let cr = self.children[(m.children_offset + i) as usize];
            self.for_each_original_edge(cr, fn_);
        }
    }

    pub fn count_atomic_descendants(&self, c: ChildRef) -> u32 {
        if child_is_edge(c) {
            return 1;
        }
        let m = self.macros[child_as_macro(c) as usize];
        let mut total = 0;
        for i in 0..m.children_count {
            total += self.count_atomic_descendants(self.children[(m.children_offset + i) as usize]);
        }
        total
    }

    pub fn count_atomic_descendants_macro(&self, mid: SpNodeId) -> u32 {
        let m = self.macros[mid as usize];
        let mut total = 0;
        for i in 0..m.children_count {
            total += self.count_atomic_descendants(self.children[(m.children_offset + i) as usize]);
        }
        total
    }

    pub fn update_stats(&mut self) {
        self.stats.macro_count = self.macros.len() as u32;
        self.stats.macro_series = 0;
        self.stats.macro_parallel = 0;
        for m in &self.macros {
            if m.kind == SP_KIND_SERIES {
                self.stats.macro_series += 1;
            } else if m.kind == SP_KIND_PARALLEL {
                self.stats.macro_parallel += 1;
            }
        }
        self.stats.core_edges_count = self.core_edges.len() as u32;
        self.stats.core_nodes = self.core_nodes.len() as u32;
    }
}

pub struct CompressionResult {
    pub tree: SpTree,
    pub success: bool,
    pub error_message: Option<&'static str>,
}

#[cfg(test)]
mod size_assertions {
    use super::*;

    #[test]
    fn macronode_is_32_bytes() {
        assert_eq!(std::mem::size_of::<SpNode>(), 32);
    }

    #[test]
    fn coreedge_is_16_bytes() {
        assert_eq!(std::mem::size_of::<CoreEdge>(), 16);
    }

    #[test]
    fn childref_is_8_bytes() {
        assert_eq!(std::mem::size_of::<ChildRef>(), 8);
    }

    #[test]
    fn child_tagging_roundtrip() {
        let e = EdgeId(0xABCDEF);
        let c = make_child_edge(e);
        assert!(child_is_edge(c));
        assert_eq!(child_as_edge(c).0, 0xABCDEF);

        let m: SpNodeId = 0xABCDEF;
        let c = make_child_macro(m);
        assert!(child_is_macro(c));
        assert_eq!(child_as_macro(c), 0xABCDEF);
    }
}
