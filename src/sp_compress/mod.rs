pub mod adj;
pub mod arena;
pub mod direct;
pub(crate) mod direct_wide;
#[allow(unsafe_code)]
pub mod ffi;
pub mod integration;
pub mod iso;
pub mod pmap;
pub mod reconstruct;
pub mod reconstruct_wide;
pub mod reduction;
pub mod types;

pub use integration::{
    compress_and_build_spqr, compress_and_build_spqr_borrowed, CompressAndSpqrResult,
};
pub use reduction::{compress, compress_borrowed};
pub use types::{
    child_as_edge, child_as_macro, child_is_edge, child_is_macro, make_child_edge,
    make_child_macro, ChildRef, CompressionInput, CompressionResult, CompressionStats, CoreEdge,
    InputEdge, SpNode, SpNodeId, SpTree, INVALID_SP_NODE, SP_KIND_PARALLEL, SP_KIND_SERIES,
};
pub mod wide;
