//! Frozen immutable graph with CSR adjacency.
//
//! E1-4 will implement: sort edges by source NodeId, build CSR offset
//! arrays, move (not copy) SoA arrays from MutableGraph, build secondary
//! indexes (RoaringBitmap, FST, HashMap lookups).

/// Placeholder — full implementation in E1-4.
pub struct FrozenGraph;

impl FrozenGraph {
    /// Returns 0 for the placeholder.
    #[allow(dead_code)]
    pub fn node_count(&self) -> usize {
        0
    }
}
