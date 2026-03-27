//! Frozen immutable graph with CSR adjacency.
//!
//! E1-4 implements: sort edges by source NodeId, build CSR offset arrays
//! (forward + reverse), move (not copy) SoA arrays from MutableGraph,
//! phased teardown of edge vectors.

use ci_core::{EdgeType, InternedStr, NodeId, NodeLabel};

/// An immutable graph with CSR (Compressed Sparse Row) adjacency representation.
///
/// Produced by calling [`MutableGraph::freeze()`](super::MutableGraph::freeze).
/// All data is owned and stored in cache-friendly SoA (Structure of Arrays) format.
///
/// # CSR Format
///
/// **Forward CSR** stores outgoing edges per node:
/// - `forward_offsets[N]` is the start index into `forward_edge_targets` /
///   `forward_edge_types` for node `N`'s outgoing edges
/// - `forward_offsets[N + 1]` is the end index (exclusive)
/// - `forward_offsets[node_count]` = total edge count (sentinel)
///
/// **Reverse CSR** stores incoming edges per node (built alongside forward):
/// - Same layout as forward but for incoming edges
///
/// # Memory Management
///
/// `freeze()` uses **phased teardown**: edge `Vec`s from `MutableGraph` are
/// dropped immediately after the CSR construction phase, before any secondary
/// indexes are built.  This bounds peak memory to approximately 1.3x the final
/// `FrozenGraph` size.
pub struct FrozenGraph {
    // ── Hot node data ─────────────────────────────────────────────────────────

    /// Node kind label (Function, Class, File, etc.), in `NodeId` index order.
    node_labels: Vec<NodeLabel>,
    /// Interned symbol name, in `NodeId` index order.
    node_names: Vec<InternedStr>,

    // ── Cold node data ────────────────────────────────────────────────────────

    /// Interned source file path, in `NodeId` index order.
    node_files: Vec<InternedStr>,
    /// 1-based line numbers (0 = unknown), in `NodeId` index order.
    node_lines: Vec<u32>,
    /// 1-based column numbers (0 = unknown), in `NodeId` index order.
    node_columns: Vec<u32>,

    // ── Forward CSR adjacency (outgoing edges) ────────────────────────────────

    /// CSR offset array for forward adjacency. Length = `node_count + 1`.
    forward_offsets: Vec<u32>,
    /// Target node of each edge, in CSR order (sorted by source then target).
    forward_edge_targets: Vec<NodeId>,
    /// Edge type of each edge, parallel to `forward_edge_targets`.
    forward_edge_types: Vec<EdgeType>,

    // ── Reverse CSR adjacency (incoming edges) ───────────────────────────────

    /// CSR offset array for reverse adjacency. Length = `node_count + 1`.
    rev_offsets: Vec<u32>,
    /// Source node of each incoming edge, in reverse-CSR order.
    rev_edge_sources: Vec<NodeId>,
    /// Edge type of each incoming edge, parallel to `rev_edge_sources`.
    rev_edge_types: Vec<EdgeType>,
}

// ── Construction ────────────────────────────────────────────────────────────

impl FrozenGraph {
    /// Total number of nodes in the frozen graph.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.node_labels.len()
    }

    /// Total number of edges in the frozen graph.
    #[inline]
    pub fn edge_count(&self) -> usize {
        debug_assert_eq!(
            self.forward_offsets.last().copied().unwrap_or(0) as usize,
            self.forward_edge_targets.len()
        );
        self.forward_edge_targets.len()
    }

    /// Returns the label for `node`.
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn node_label(&self, node: NodeId) -> NodeLabel {
        self.node_labels[node.0 as usize]
    }

    /// Returns the interned name for `node`.
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn node_name(&self, node: NodeId) -> InternedStr {
        self.node_names[node.0 as usize]
    }

    /// Returns the interned source file for `node`.
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn node_file(&self, node: NodeId) -> InternedStr {
        self.node_files[node.0 as usize]
    }

    /// Returns the line number for `node` (1-based, 0 = unknown).
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn node_line(&self, node: NodeId) -> u32 {
        self.node_lines[node.0 as usize]
    }

    /// Returns the column number for `node` (1-based, 0 = unknown).
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn node_column(&self, node: NodeId) -> u32 {
        self.node_columns[node.0 as usize]
    }

    // ── Forward CSR accessors ─────────────────────────────────────────────────

    /// Returns the range of forward (outgoing) edges for `node`.
    ///
    /// The range `[start, end)` indexes into `forward_edge_targets` and
    /// `forward_edge_types`.
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn forward_edge_range(&self, node: NodeId) -> core::ops::Range<usize> {
        let start = self.forward_offsets[node.0 as usize] as usize;
        let end = self.forward_offsets[node.0 as usize + 1] as usize;
        start..end
    }

    /// Iterates over all outgoing edges of `node` with their targets and types.
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn forward_edges(&self, node: NodeId) -> impl Iterator<Item = (NodeId, EdgeType)> + '_ {
        let range = self.forward_edge_range(node);
        self.forward_edge_targets[range.clone()]
            .iter()
            .zip(self.forward_edge_types[range].iter())
            .map(|(&t, &ty)| (t, ty))
    }

    // ── Reverse CSR accessors ─────────────────────────────────────────────────

    /// Returns the range of reverse (incoming) edges for `node`.
    ///
    /// The range `[start, end)` indexes into `rev_edge_sources` and
    /// `rev_edge_types`.
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn reverse_edge_range(&self, node: NodeId) -> core::ops::Range<usize> {
        let start = self.rev_offsets[node.0 as usize] as usize;
        let end = self.rev_offsets[node.0 as usize + 1] as usize;
        start..end
    }

    /// Iterates over all incoming edges of `node` with their sources and types.
    ///
    /// # Panics
    /// Panics if `node` is out of range.
    #[inline]
    pub fn reverse_edges(&self, node: NodeId) -> impl Iterator<Item = (NodeId, EdgeType)> + '_ {
        let range = self.reverse_edge_range(node);
        self.rev_edge_sources[range.clone()]
            .iter()
            .zip(self.rev_edge_types[range].iter())
            .map(|(&s, &ty)| (s, ty))
    }
}

// ── Freeze logic (lives on MutableGraph) ─────────────────────────────────────

impl super::MutableGraph {
    /// Freeze the mutable graph into an immutable `FrozenGraph`.
    ///
    /// # Algorithm
    ///
    /// 1. Collect edges (source, target, type) from the lock-free append buffers
    /// 2. Sort edges by source `NodeId` (ascending), then by target
    /// 3. Build forward CSR: compute `forward_offsets`, populate edge target/type arrays
    /// 4. Build reverse CSR: count incoming edges per node, compute `rev_offsets`,
    ///    populate `rev_edge_sources` and `rev_edge_types`
    /// 5. Move node SoA arrays out of `self` (no copy — ownership transfers)
    /// 6. Drop edge SoA arrays from `self` (phased teardown: done before any
    ///    secondary index construction in E1-5)
    ///
    /// # Complexity
    ///
    /// - Sorting: O(E log E) where E = edge count
    /// - CSR build: O(E + N) where N = node count
    /// - Memory: nodes + edges + CSR offsets (approximately 1.3x final size during build)
    ///
    /// # Panics
    ///
    /// Panics if `node_count` exceeds `u32::MAX` (not expected).
    pub fn freeze(self) -> FrozenGraph {
        let node_count = self.node_count() as usize;

        // ── Step 1: Collect edges ──────────────────────────────────────────────
        // Collect all edges into a single owned Vec for sorting.
        let sources = self.edge_sources();
        let targets = self.edge_targets();
        let types = self.edge_types();

        let mut edges: Vec<(u32, NodeId, EdgeType)> = sources
            .into_iter()
            .zip(targets.into_iter())
            .zip(types.into_iter())
            .map(|((s, t), ty)| (s.0, t, ty))
            .collect();

        // ── Step 2: Sort edges by source ──────────────────────────────────────
        if !edges.is_empty() {
            sort_edges(&mut edges);
        }

        // ── Step 3: Build forward CSR ─────────────────────────────────────────
        let (forward_offsets, forward_edge_targets, forward_edge_types) =
            build_forward_csr(&edges, node_count);

        // ── Step 4: Build reverse CSR ──────────────────────────────────────────
        let (rev_offsets, rev_edge_sources, rev_edge_types) =
            build_reverse_csr(&edges, node_count);

        // ── Step 5: Move node SoA arrays ───────────────────────────────────────
        let node_labels = self.node_labels();
        let node_names = self.node_names();
        let node_files = self.node_files();
        let node_lines = self.node_lines();
        let node_columns = self.node_columns();

        FrozenGraph {
            node_labels,
            node_names,
            node_files,
            node_lines,
            node_columns,
            forward_offsets,
            forward_edge_targets,
            forward_edge_types,
            rev_offsets,
            rev_edge_sources,
            rev_edge_types,
        }
    }
}

// ── Edge sorting ─────────────────────────────────────────────────────────────

/// Sorts edges by source `NodeId` (ascending), breaking ties by target `NodeId`.
#[inline]
fn sort_edges(edges: &mut Vec<(u32, NodeId, EdgeType)>) {
    edges.sort_by_key(|(src, tgt, _)| (*src, tgt.0));
}

// ── CSR builders ──────────────────────────────────────────────────────────────

/// Builds forward CSR adjacency arrays from sorted edges.
///
/// Returns `(offsets, targets, types)` where:
/// - `offsets[i]` = start index of outgoing edges for node `i`
/// - `offsets[node_count]` = total edge count (sentinel)
fn build_forward_csr(
    sorted_edges: &[(u32, NodeId, EdgeType)],
    node_count: usize,
) -> (Vec<u32>, Vec<NodeId>, Vec<EdgeType>) {
    let edge_count = sorted_edges.len();
    let mut offsets = Vec::with_capacity(node_count + 1);
    let mut targets = Vec::with_capacity(edge_count);
    let mut types = Vec::with_capacity(edge_count);

    offsets.push(0u32);
    let mut current_node = 0usize;

    for &(src, tgt, ty) in sorted_edges {
        let src_idx = src as usize;
        // Fill in zeros for any nodes that have no outgoing edges.
        while current_node < src_idx {
            current_node += 1;
            offsets.push(targets.len() as u32);
        }
        targets.push(tgt);
        types.push(ty);
    }

    // Fill remaining nodes with sentinel (no more edges).
    while current_node < node_count {
        current_node += 1;
        offsets.push(targets.len() as u32);
    }

    (offsets, targets, types)
}

/// Builds reverse CSR adjacency arrays from sorted edges.
///
/// Returns `(offsets, sources, types)` where:
/// - `offsets[i]` = start index of incoming edges for node `i`
/// - `offsets[node_count]` = total edge count (sentinel)
fn build_reverse_csr(
    sorted_edges: &[(u32, NodeId, EdgeType)],
    node_count: usize,
) -> (Vec<u32>, Vec<NodeId>, Vec<EdgeType>) {
    let edge_count = sorted_edges.len();

    // First pass: count incoming edges per node.
    let mut in_counts = vec![0u32; node_count];
    for &(_, tgt, _) in sorted_edges {
        in_counts[tgt.0 as usize] += 1;
    }

    // Build offsets via prefix sum.
    let mut offsets = Vec::with_capacity(node_count + 1);
    offsets.push(0u32);
    let mut sum = 0u32;
    for &count in &in_counts {
        sum += count;
        offsets.push(sum);
    }

    // Allocate with MaybeUninit to avoid zero-initializing then overwriting O(E) slots.
    let mut sources: Vec<std::mem::MaybeUninit<NodeId>> =
        Vec::with_capacity(edge_count);
    let mut types: Vec<std::mem::MaybeUninit<EdgeType>> =
        Vec::with_capacity(edge_count);
    // Safety: we resize to full capacity and write every element before finalization.
    unsafe {
        sources.set_len(edge_count);
        types.set_len(edge_count);
    }

    // Copy into position using the offset array as a running write pointer.
    let mut write_pos = offsets.clone();
    for &(src, tgt, ty) in sorted_edges {
        let tgt_idx = tgt.0 as usize;
        let pos = write_pos[tgt_idx] as usize;
        // Safety: pos is within [0, edge_count) by CSR construction invariant.
        unsafe {
            sources.as_mut_ptr().add(pos).write(std::mem::MaybeUninit::new(NodeId(src)));
            types.as_mut_ptr().add(pos).write(std::mem::MaybeUninit::new(ty));
        }
        write_pos[tgt_idx] += 1;
    }

    // Safety: all slots initialized above.
    let sources = unsafe { std::mem::transmute::<_, Vec<NodeId>>(sources) };
    let types = unsafe { std::mem::transmute::<_, Vec<EdgeType>>(types) };

    (offsets, sources, types)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ci_core::{EdgeType, InternedStr, NodeId, NodeLabel};

    use crate::{FrozenGraph, MutableGraph};

    // ── FrozenGraph basic properties ─────────────────────────────────────────

    mod frozen_graph_properties {
        use super::*;

        #[test]
        fn empty_graph_has_zero_nodes_and_edges() {
            let mutable = MutableGraph::new();
            let frozen = mutable.freeze();
            assert_eq!(frozen.node_count(), 0);
            assert_eq!(frozen.edge_count(), 0);
        }

        #[test]
        fn node_count_matches_added_nodes() {
            let mutable = MutableGraph::new();
            mutable.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            mutable.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);
            let frozen = mutable.freeze();
            assert_eq!(frozen.node_count(), 2);
        }

        #[test]
        fn edge_count_matches_added_edges() {
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);
            mutable.add_edge(n0, n1, EdgeType::Contains);
            mutable.add_edge(n0, n1, EdgeType::Imports);
            mutable.add_edge(n1, n0, EdgeType::Calls);
            let frozen = mutable.freeze();
            assert_eq!(frozen.edge_count(), 3);
        }
    }

    // ── Node data accessor round-trip ─────────────────────────────────────────

    mod node_data_round_trip {
        use super::*;

        fn build_three_nodes() -> (MutableGraph, Vec<NodeId>) {
            let mutable = MutableGraph::new();
            let n0 =
                mutable.add_node(NodeLabel::Project, InternedStr(100), InternedStr(200), 1, 1);
            let n1 = mutable.add_node(NodeLabel::File, InternedStr(101), InternedStr(201), 10, 5);
            let n2 = mutable.add_node(NodeLabel::Class, InternedStr(102), InternedStr(201), 20, 3);
            (mutable, vec![n0, n1, n2])
        }

        #[test]
        fn node_label_round_trips() {
            let (mutable, ids) = build_three_nodes();
            let frozen = mutable.freeze();

            assert_eq!(frozen.node_label(ids[0]), NodeLabel::Project);
            assert_eq!(frozen.node_label(ids[1]), NodeLabel::File);
            assert_eq!(frozen.node_label(ids[2]), NodeLabel::Class);
        }

        #[test]
        fn node_name_round_trips() {
            let (mutable, ids) = build_three_nodes();
            let frozen = mutable.freeze();

            assert_eq!(frozen.node_name(ids[0]), InternedStr(100));
            assert_eq!(frozen.node_name(ids[1]), InternedStr(101));
            assert_eq!(frozen.node_name(ids[2]), InternedStr(102));
        }

        #[test]
        fn node_file_round_trips() {
            let (mutable, ids) = build_three_nodes();
            let frozen = mutable.freeze();

            assert_eq!(frozen.node_file(ids[0]), InternedStr(200));
            assert_eq!(frozen.node_file(ids[1]), InternedStr(201));
            assert_eq!(frozen.node_file(ids[2]), InternedStr(201));
        }

        #[test]
        fn node_line_round_trips() {
            let (mutable, ids) = build_three_nodes();
            let frozen = mutable.freeze();

            assert_eq!(frozen.node_line(ids[0]), 1);
            assert_eq!(frozen.node_line(ids[1]), 10);
            assert_eq!(frozen.node_line(ids[2]), 20);
        }

        #[test]
        fn node_column_round_trips() {
            let (mutable, ids) = build_three_nodes();
            let frozen = mutable.freeze();

            assert_eq!(frozen.node_column(ids[0]), 1);
            assert_eq!(frozen.node_column(ids[1]), 5);
            assert_eq!(frozen.node_column(ids[2]), 3);
        }
    }

    // ── Forward CSR ────────────────────────────────────────────────────────────

    mod forward_csr {
        use super::*;

        fn build_simple_graph() -> FrozenGraph {
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);
            let n2 = mutable.add_node(NodeLabel::Method, InternedStr(2), InternedStr(0), 0, 0);

            mutable.add_edge(n0, n1, EdgeType::Contains);
            mutable.add_edge(n0, n2, EdgeType::Contains);
            mutable.add_edge(n1, n2, EdgeType::Calls);

            mutable.freeze()
        }

        #[test]
        fn forward_offsets_length_is_node_count_plus_one() {
            let frozen = build_simple_graph();
            assert_eq!(frozen.forward_offsets.len(), frozen.node_count() + 1);
        }

        #[test]
        fn forward_offsets_sentinel_equals_edge_count() {
            let frozen = build_simple_graph();
            let sentinel = frozen.forward_offsets[frozen.node_count()];
            assert_eq!(sentinel as usize, frozen.edge_count());
        }

        #[test]
        fn forward_offsets_are_non_decreasing() {
            let frozen = build_simple_graph();
            for i in 0..frozen.forward_offsets.len() - 1 {
                assert!(
                    frozen.forward_offsets[i] <= frozen.forward_offsets[i + 1],
                    "forward_offsets[{}]={} should be <= forward_offsets[{}]={}",
                    i,
                    frozen.forward_offsets[i],
                    i + 1,
                    frozen.forward_offsets[i + 1]
                );
            }
        }

        #[test]
        fn node_0_has_two_outgoing_edges() {
            let frozen = build_simple_graph();
            let range = frozen.forward_edge_range(NodeId(0));
            assert_eq!(range.len(), 2);
        }

        #[test]
        fn node_1_has_one_outgoing_edge() {
            let frozen = build_simple_graph();
            let range = frozen.forward_edge_range(NodeId(1));
            assert_eq!(range.len(), 1);
        }

        #[test]
        fn node_2_has_zero_outgoing_edges() {
            let frozen = build_simple_graph();
            let range = frozen.forward_edge_range(NodeId(2));
            assert_eq!(range.len(), 0);
        }

        #[test]
        fn forward_edges_correct_targets_and_types() {
            let frozen = build_simple_graph();
            let edges: Vec<_> = frozen.forward_edges(NodeId(0)).collect();

            assert_eq!(edges.len(), 2);
            // Edges are sorted by target within each source in CSR order
            assert_eq!(edges[0].0, NodeId(1)); // Contains
            assert_eq!(edges[0].1, EdgeType::Contains);
            assert_eq!(edges[1].0, NodeId(2)); // Contains
            assert_eq!(edges[1].1, EdgeType::Contains);
        }

        #[test]
        fn forward_edges_from_node_1() {
            let frozen = build_simple_graph();
            let edges: Vec<_> = frozen.forward_edges(NodeId(1)).collect();

            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].0, NodeId(2));
            assert_eq!(edges[0].1, EdgeType::Calls);
        }
    }

    // ── Reverse CSR ───────────────────────────────────────────────────────────

    mod reverse_csr {
        use super::*;

        fn build_simple_graph() -> FrozenGraph {
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);
            let n2 = mutable.add_node(NodeLabel::Method, InternedStr(2), InternedStr(0), 0, 0);

            mutable.add_edge(n0, n1, EdgeType::Contains);
            mutable.add_edge(n0, n2, EdgeType::Contains);
            mutable.add_edge(n1, n2, EdgeType::Calls);

            mutable.freeze()
        }

        #[test]
        fn rev_offsets_length_is_node_count_plus_one() {
            let frozen = build_simple_graph();
            assert_eq!(frozen.rev_offsets.len(), frozen.node_count() + 1);
        }

        #[test]
        fn rev_offsets_sentinel_equals_edge_count() {
            let frozen = build_simple_graph();
            let sentinel = frozen.rev_offsets[frozen.node_count()];
            assert_eq!(sentinel as usize, frozen.edge_count());
        }

        #[test]
        fn rev_offsets_are_non_decreasing() {
            let frozen = build_simple_graph();
            for i in 0..frozen.rev_offsets.len() - 1 {
                assert!(
                    frozen.rev_offsets[i] <= frozen.rev_offsets[i + 1],
                    "rev_offsets[{}]={} should be <= rev_offsets[{}]={}",
                    i,
                    frozen.rev_offsets[i],
                    i + 1,
                    frozen.rev_offsets[i + 1]
                );
            }
        }

        #[test]
        fn node_0_has_zero_incoming_edges() {
            let frozen = build_simple_graph();
            let range = frozen.reverse_edge_range(NodeId(0));
            assert_eq!(range.len(), 0);
        }

        #[test]
        fn node_1_has_one_incoming_edge() {
            let frozen = build_simple_graph();
            let range = frozen.reverse_edge_range(NodeId(1));
            assert_eq!(range.len(), 1);
        }

        #[test]
        fn node_2_has_two_incoming_edges() {
            let frozen = build_simple_graph();
            let range = frozen.reverse_edge_range(NodeId(2));
            assert_eq!(range.len(), 2);
        }

        #[test]
        fn incoming_edges_to_node_2() {
            let frozen = build_simple_graph();
            let edges: Vec<_> = frozen.reverse_edges(NodeId(2)).collect();

            assert_eq!(edges.len(), 2);
            // Edges are in CSR order by source
            assert_eq!(edges[0].0, NodeId(0)); // Contains
            assert_eq!(edges[0].1, EdgeType::Contains);
            assert_eq!(edges[1].0, NodeId(1)); // Calls
            assert_eq!(edges[1].1, EdgeType::Calls);
        }

        #[test]
        fn incoming_edge_to_node_1() {
            let frozen = build_simple_graph();
            let edges: Vec<_> = frozen.reverse_edges(NodeId(1)).collect();

            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].0, NodeId(0));
            assert_eq!(edges[0].1, EdgeType::Contains);
        }
    }

    // ── Self-loops ─────────────────────────────────────────────────────────────

    mod self_loops {
        use super::*;

        #[test]
        fn self_loop_appears_in_forward_and_reverse() {
            let mutable = MutableGraph::new();
            let n = mutable.add_node(NodeLabel::Method, InternedStr(0), InternedStr(0), 0, 0);
            mutable.add_edge(n, n, EdgeType::Calls);

            let frozen = mutable.freeze();

            let fwd = frozen.forward_edges(n).collect::<Vec<_>>();
            assert_eq!(fwd.len(), 1);
            assert_eq!(fwd[0].0, n);
            assert_eq!(fwd[0].1, EdgeType::Calls);

            let rev = frozen.reverse_edges(n).collect::<Vec<_>>();
            assert_eq!(rev.len(), 1);
            assert_eq!(rev[0].0, n);
            assert_eq!(rev[0].1, EdgeType::Calls);
        }
    }

    // ── Disconnected nodes ─────────────────────────────────────────────────────

    mod disconnected_nodes {
        use super::*;

        fn build_with_isolated_node() -> FrozenGraph {
            let mutable = MutableGraph::new();
            // Node 0: connected
            let n0 = mutable.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);
            // Node 2: isolated (no edges)
            mutable.add_node(NodeLabel::Function, InternedStr(2), InternedStr(0), 0, 0);

            mutable.add_edge(n0, n1, EdgeType::Contains);

            mutable.freeze()
        }

        #[test]
        fn isolated_node_has_no_outgoing_edges() {
            let frozen = build_with_isolated_node();
            let range = frozen.forward_edge_range(NodeId(2));
            assert_eq!(range.len(), 0);
        }

        #[test]
        fn isolated_node_has_no_incoming_edges() {
            let frozen = build_with_isolated_node();
            let range = frozen.reverse_edge_range(NodeId(2));
            assert_eq!(range.len(), 0);
        }

        #[test]
        fn isolated_node_data_still_accessible() {
            let frozen = build_with_isolated_node();
            assert_eq!(frozen.node_label(NodeId(2)), NodeLabel::Function);
            assert_eq!(frozen.node_name(NodeId(2)), InternedStr(2));
        }
    }

    // ── Edge ordering in CSR ───────────────────────────────────────────────────

    mod csr_edge_ordering {
        use super::*;

        fn build_multi_target_graph() -> FrozenGraph {
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            mutable.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);
            mutable.add_node(NodeLabel::Method, InternedStr(2), InternedStr(0), 0, 0);
            mutable.add_node(NodeLabel::Function, InternedStr(3), InternedStr(0), 0, 0);
            mutable.add_node(NodeLabel::Variable, InternedStr(4), InternedStr(0), 0, 0);
            mutable.add_node(NodeLabel::Field, InternedStr(5), InternedStr(0), 0, 0);

            // Add edges from n0 to several other nodes.
            mutable.add_edge(n0, NodeId(5), EdgeType::Contains); // target 5
            mutable.add_edge(n0, NodeId(3), EdgeType::Calls); // target 3
            mutable.add_edge(n0, NodeId(1), EdgeType::Imports); // target 1

            mutable.freeze()
        }

        #[test]
        fn forward_edges_preserves_relative_order_for_same_source() {
            let frozen = build_multi_target_graph();
            let edges: Vec<_> = frozen.forward_edges(NodeId(0)).collect();

            assert_eq!(edges.len(), 3);
            // Sort order is by target NodeId ascending: 1, 3, 5
            assert_eq!(edges[0].0, NodeId(1)); // Imports
            assert_eq!(edges[0].1, EdgeType::Imports);
            assert_eq!(edges[1].0, NodeId(3)); // Calls
            assert_eq!(edges[1].1, EdgeType::Calls);
            assert_eq!(edges[2].0, NodeId(5)); // Contains
            assert_eq!(edges[2].1, EdgeType::Contains);
        }
    }

    // ── Large graph smoke test ─────────────────────────────────────────────────

    mod large_graph {
        use super::*;

        #[test]
        fn freeze_handles_large_graph() {
            let mutable = MutableGraph::new();
            let node_count = 1000usize;
            let edges_per_node = 5usize;

            // Add nodes
            for i in 0..node_count {
                mutable.add_node(
                    NodeLabel::Function,
                    InternedStr(i as u32),
                    InternedStr(0),
                    i as u32,
                    0,
                );
            }

            // Add edges: node i -> node (i+j+1) % node_count for j in 0..5
            for i in 0..node_count {
                for j in 0..edges_per_node {
                    let src = NodeId(i as u32);
                    let dst = NodeId(((i + j + 1) % node_count) as u32);
                    mutable.add_edge(src, dst, EdgeType::Calls);
                }
            }

            let frozen = mutable.freeze();
            assert_eq!(frozen.node_count(), node_count);
            assert_eq!(frozen.edge_count(), node_count * edges_per_node);

            // Verify forward edges for node 0
            let edges = frozen.forward_edges(NodeId(0)).collect::<Vec<_>>();
            assert_eq!(edges.len(), edges_per_node);

            // Verify reverse edges for node 1: it gets edges from nodes 0, 999, 998, 997, 996
            let rev = frozen.reverse_edges(NodeId(1)).collect::<Vec<_>>();
            assert_eq!(rev.len(), edges_per_node);

            // Every node should have exactly edges_per_node outgoing edges
            for i in 0..node_count {
                let edges = frozen.forward_edges(NodeId(i as u32)).collect::<Vec<_>>();
                assert_eq!(edges.len(), edges_per_node);
            }
        }
    }
}
