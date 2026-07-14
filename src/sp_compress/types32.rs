use crate::{EdgeId, NodeId};

pub use crate::sp_compress::types::{
    CompressionStats, InputEdge, SP_KIND_PARALLEL, SP_KIND_SERIES,
};

pub type ChildRef = u32;
pub type SpNodeId = u32;

pub const TAG_BIT: ChildRef = 0x8000_0000;
pub const PAYLOAD_MASK: ChildRef = 0x7FFF_FFFF;

#[inline(always)]
pub const fn make_child_edge(eid: EdgeId) -> ChildRef {
    eid.0
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
    EdgeId(c)
}

#[inline(always)]
pub const fn child_as_macro(c: ChildRef) -> SpNodeId {
    c & PAYLOAD_MASK
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SpNode {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub left: u32,
    pub right: u32,
    pub children_offset: u32,
    pub children_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreEdge {
    pub u: u32,
    pub v: u32,
    pub child: ChildRef,
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
    fn compact_types_have_expected_sizes() {
        assert_eq!(std::mem::size_of::<SpNode>(), 20);
        assert_eq!(std::mem::size_of::<CoreEdge>(), 12);
        assert_eq!(std::mem::size_of::<ChildRef>(), 4);
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
