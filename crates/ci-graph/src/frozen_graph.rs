//! Frozen immutable graph with CSR adjacency and secondary indexes.
//!
//! E1-4 implements: sort edges by source NodeId, build CSR offset arrays
//! (forward + reverse), move (not copy) SoA arrays from MutableGraph,
//! phased teardown of edge vectors.
//!
//! E1-5 adds secondary indexes built during `freeze()`:
//! - `[RoaringBitmap; NodeLabel::COUNT]` — one bitmap per label for O(1) filtering
//! - `HashMap<InternedStr, NodeId>` — qualified-name to node lookup
//! - `HashMap<InternedStr, Vec<NodeId>>` — file path to nodes lookup
//! - `fst::Map` + side-table — bare-name FST for prefix/regex/fuzzy search (E4-1)
//!
//! E1-6 adds rkyv serialization: `save()` writes a magic header + checksum +
//! rkyv payload; `load()` mmaps and validates with zero heap allocation.

use std::collections::HashMap;

use ci_core::{EdgeType, FrozenInterner, InternedStr, NodeId, NodeLabel};
use rkyv::{Archive, Deserialize, Serialize};
use roaring::RoaringBitmap;
use smallvec::SmallVec;

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
#[derive(Archive, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
#[derive(Debug, Clone)]
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

    // ── Secondary indexes (E1-5) ─────────────────────────────────────────────

    /// String interner — source of truth for resolving `InternedStr` handles.
    interner: FrozenInterner,
    /// One `RoaringBitmap` per `NodeLabel` variant, stored as serialized bytes
    /// since `RoaringBitmap` does not implement `rkyv::Archive`. Use
    /// `label_index()` to deserialize on demand.
    label_index_bytes: Vec<u8>,
    /// Qualified-name → `NodeId` lookup. The hottest query path.
    /// Last write wins when multiple nodes share a name handle (same QN string).
    qn_index: HashMap<InternedStr, NodeId>,
    /// File path → list of `NodeId`s. All nodes whose `node_file` equals the key.
    file_index: HashMap<InternedStr, Vec<NodeId>>,
    /// Raw FST bytes for the bare-name index. Stored as bytes so it can be
    /// archived by rkyv (the `fst::Map` type itself does not implement `Archive`).
    /// Reconstructed into an `fst::Map` on demand by [`bare_name_fst()`].
    bare_name_fst_bytes: Vec<u8>,
    /// Side-table for the bare-name FST. `bare_name_nodes[slot]` holds all
    /// `NodeId`s whose symbol name resolves to the same string.
    /// `SmallVec<[NodeId; 1]>` avoids heap allocation for the common case where
    /// a name is unique.
    bare_name_nodes: Vec<SmallVec<[NodeId; 1]>>,
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

impl FrozenGraph {
    // ── Secondary index accessors (E1-5) ─────────────────────────────────────

    /// Returns the string interner for resolving `InternedStr` handles.
    #[inline]
    pub fn interner(&self) -> &FrozenInterner {
        &self.interner
    }

    /// Returns the `RoaringBitmap` of all node IDs with the given label.
    /// Deserialized from the stored byte buffer on each call.
    ///
    /// The byte buffer stores each bitmap as `[4-byteLE-size][serialized-bytes]`
    /// concatenated in `NodeLabel` order.
    pub fn nodes_with_label(&self, label: NodeLabel) -> RoaringBitmap {
        let idx = label as usize;
        let mut offset = 0usize;
        for _ in 0..idx {
            // Skip past each preceding bitmap: [size][data]
            let size_bytes: [u8; 4] = self.label_index_bytes[offset..offset + 4]
                .try_into().unwrap();
            let size = u32::from_le_bytes(size_bytes) as usize;
            offset += 4 + size;
        }
        let size_bytes: [u8; 4] = self.label_index_bytes[offset..offset + 4]
            .try_into().unwrap();
        let size = u32::from_le_bytes(size_bytes) as usize;
        RoaringBitmap::deserialize_from(&self.label_index_bytes[offset + 4..offset + 4 + size])
            .expect("label_index_bytes must contain valid RoaringBitmap data")
    }

    /// Returns the `NodeId` for the given qualified-name handle, if any.
    ///
    /// # Collision policy
    /// If multiple nodes share the same `InternedStr` handle (i.e. the same QN
    /// string), the node with the **highest `NodeId`** is returned (last-insert
    /// wins in the ascending-order build loop inside `freeze()`). Callers that
    /// need all nodes with a given name should use [`nodes_with_label`] +
    /// [`nodes_in_file`] or iterate the FST side-table instead.
    #[inline]
    pub fn lookup_qn(&self, name: InternedStr) -> Option<NodeId> {
        self.qn_index.get(&name).copied()
    }

    /// Returns all nodes belonging to the file with the given path handle.
    #[inline]
    pub fn nodes_in_file(&self, file: InternedStr) -> &[NodeId] {
        self.file_index.get(&file).map_or(&[], Vec::as_slice)
    }

    /// Returns the FST over all distinct symbol names, reconstructed from the
    /// serialized byte representation.
    ///
    /// The returned `fst::Map` is built from the stored bytes. For zero-copy
    /// access patterns that avoid this copy, use [`bare_name_fst_bytes()`] and
    /// the FST stream API directly.
    #[inline]
    pub fn bare_name_fst(&self) -> fst::Map<Vec<u8>> {
        fst::Map::new(self.bare_name_fst_bytes.clone())
            .expect("bare_name_fst_bytes must be valid FST bytes")
    }

    /// Returns the raw FST bytes for the bare-name index.
    ///
    /// This enables zero-copy access via the FST stream API without reconstructing
    /// the owned `fst::Map`. Search methods (prefix, regex, fuzzy) are implemented
    /// in E4-1.
    #[inline]
    pub fn bare_name_fst_bytes(&self) -> &[u8] {
        &self.bare_name_fst_bytes
    }

    /// Returns all `NodeId`s whose symbol name maps to `slot` in the FST.
    ///
    /// # Panics
    /// Panics if `slot` is out of range (use the value returned by the FST).
    #[inline]
    pub fn bare_name_nodes(&self, slot: u64) -> &[NodeId] {
        &self.bare_name_nodes[slot as usize]
    }
}

// ── Freeze logic (lives on MutableGraph) ─────────────────────────────────────

impl super::MutableGraph {
    /// Freeze the mutable graph into an immutable `FrozenGraph`.
    ///
    /// # Algorithm
    ///
    /// 1. Consume `self` via `into_parts()` — moves all SoA Vecs out of their
    ///    Mutexes in O(1) without cloning
    /// 2. Sort edges by source `NodeId` (ascending), then by target
    /// 3. Build forward CSR: compute `forward_offsets`, populate edge target/type arrays
    /// 4. Build reverse CSR: count incoming edges per node, compute `rev_offsets`,
    ///    populate `rev_edge_sources` and `rev_edge_types`
    /// 5. Drop the scratch `edges` Vec (phased teardown: releases memory before
    ///    secondary index construction to bound peak memory to ~1.3x final size)
    /// 6. Build label bitmaps: one `RoaringBitmap` per `NodeLabel` variant
    /// 7. Build QN index: `HashMap<InternedStr, NodeId>` for qualified-name lookup
    /// 8. Build file index: `HashMap<InternedStr, Vec<NodeId>>` for file → nodes
    /// 9. Build FST bare-name index: sort names, group duplicates into side-table,
    ///    write sorted `(name, slot)` pairs into `fst::MapBuilder`
    ///
    /// # Complexity
    ///
    /// - Sorting: O(E log E) where E = edge count
    /// - CSR build: O(E + N) where N = node count
    /// - FST build: O(N log N) for the name sort
    /// - Memory: nodes + edges + CSR offsets (approximately 1.3x final size during build)
    ///
    /// # Panics
    ///
    /// Panics if `node_count` exceeds `u32::MAX` (not expected).
    pub fn freeze(self, interner: FrozenInterner) -> FrozenGraph {
        // ── Step 1: Consume self, moving all SoA Vecs out without cloning ─────
        let (
            node_labels,
            node_names,
            node_files,
            node_lines,
            node_columns,
            edge_sources,
            edge_targets,
            edge_types,
        ) = self.into_parts();

        let node_count = node_labels.len();

        let mut edges: Vec<(u32, NodeId, EdgeType)> = edge_sources
            .into_iter()
            .zip(edge_targets.into_iter())
            .zip(edge_types.into_iter())
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

        // ── Step 5: Phased teardown (drop scratch edge Vec) ───────────────────
        drop(edges);

        // ── Step 6: Label bitmaps ──────────────────────────────────────────────
        let mut label_bitmaps: [RoaringBitmap; NodeLabel::COUNT] =
            std::array::from_fn(|_| RoaringBitmap::new());
        for (idx, &label) in node_labels.iter().enumerate() {
            label_bitmaps[label as usize].insert(idx as u32);
        }
        // Serialize each bitmap as [4-byteLE-size][bytes] for rkyv archiving.
        let label_index_bytes = {
            let mut bytes = Vec::new();
            for bitmap in &label_bitmaps {
                let mut buf = Vec::new();
                bitmap.serialize_into(&mut buf).unwrap();
                let size = u32::try_from(buf.len()).unwrap();
                bytes.extend_from_slice(&size.to_le_bytes());
                bytes.extend_from_slice(&buf);
            }
            bytes
        };

        // ── Step 7: QN index ──────────────────────────────────────────────────
        let mut qn_index = HashMap::with_capacity(node_names.len());
        for (idx, &name_handle) in node_names.iter().enumerate() {
            qn_index.insert(name_handle, NodeId(idx as u32));
        }

        // ── Step 8: File index ─────────────────────────────────────────────────
        let mut file_index: HashMap<InternedStr, Vec<NodeId>> = HashMap::new();
        for (idx, &file_handle) in node_files.iter().enumerate() {
            file_index.entry(file_handle).or_default().push(NodeId(idx as u32));
        }

        // ── Step 9: FST bare-name index ────────────────────────────────────────
        let (bare_name_fst_bytes, bare_name_nodes) =
            build_bare_name_index(&node_names, &interner);

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
            interner,
            label_index_bytes,
            qn_index,
            file_index,
            bare_name_fst_bytes,
            bare_name_nodes,
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

// ── Secondary index builders ──────────────────────────────────────────────────

/// Builds the bare-name FST and its side-table from the node name array.
///
/// Returns `(fst_bytes, side_table)` where:
/// - `fst_bytes` is the raw serialized FST (suitable for rkyv archiving)
/// - `side_table[slot]` holds all `NodeId`s whose name resolves to that string
///
/// `SmallVec<[NodeId; 1]>` avoids heap allocation for the common case where
/// a name is unique across all nodes.
///
/// # Algorithm
/// 1. Resolve every `InternedStr` handle to a `&str` via the interner
/// 2. Sort `(string, NodeId)` pairs lexicographically by string
/// 3. Group equal strings into slots; assign `slot_index` sequentially
/// 4. Feed sorted `(bytes, slot_index)` pairs into `fst::MapBuilder`
fn build_bare_name_index(
    node_names: &[InternedStr],
    interner: &FrozenInterner,
) -> (Vec<u8>, Vec<SmallVec<[NodeId; 1]>>) {
    // 1. Collect (resolved string, NodeId) pairs.
    let mut pairs: Vec<(&str, NodeId)> = node_names
        .iter()
        .enumerate()
        .map(|(idx, &handle)| (interner.resolve(handle), NodeId(idx as u32)))
        .collect();

    // 2. Sort by string (byte-lexicographic — same order fst expects), then by
    //    NodeId to guarantee stable output across runs.
    pairs.sort_unstable_by(|(a, aid), (b, bid)| a.cmp(b).then(aid.cmp(bid)));

    // 3. Group consecutive equal names, build side-table and FST entries.
    let mut side_table: Vec<SmallVec<[NodeId; 1]>> = Vec::new();
    let mut builder = fst::MapBuilder::memory();

    let mut i = 0;
    while i < pairs.len() {
        let key = pairs[i].0;
        let slot = side_table.len() as u64;
        let mut group: SmallVec<[NodeId; 1]> = SmallVec::new();
        while i < pairs.len() && pairs[i].0 == key {
            group.push(pairs[i].1);
            i += 1;
        }
        side_table.push(group);
        // Keys are already in sorted order so this never fails.
        builder
            .insert(key, slot)
            .expect("FST keys must be inserted in lexicographic order");
    }

    // 4. Finalise the FST into raw bytes for rkyv archiving.
    let bytes = builder.into_inner().expect("FST construction failed");
    (bytes, side_table)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ci_core::{EdgeType, FrozenInterner, InternedStr, NodeId, NodeLabel, StringInterner};

    use crate::{FrozenGraph, MutableGraph};

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Interns `strings` into a fresh `StringInterner`, compacts it, and returns
    /// `(FrozenInterner, remapped_handles)` in the same order as `strings`.
    /// (Mirrors the `build` helper in `ci-core/src/interner.rs` tests; kept separate
    /// because test helpers cannot be shared across crates without a test-utils crate.)
    fn make_interner(strings: &[&str]) -> (FrozenInterner, Vec<InternedStr>) {
        let si = StringInterner::new();
        let raw: Vec<InternedStr> = strings.iter().map(|s| si.intern(s)).collect();
        let (fi, remap) = si.compact();
        (fi, raw.into_iter().map(|h| remap(h)).collect())
    }

    /// Returns an empty `FrozenInterner` (no strings interned).
    /// Use only for graphs that have zero nodes (so the FST is never populated).
    fn empty_interner() -> FrozenInterner {
        let (fi, _) = StringInterner::new().compact();
        fi
    }

    // ── FrozenGraph basic properties ─────────────────────────────────────────

    mod frozen_graph_properties {
        use super::*;

        #[test]
        fn empty_graph_has_zero_nodes_and_edges() {
            let mutable = MutableGraph::new();
            let frozen = mutable.freeze(empty_interner());
            assert_eq!(frozen.node_count(), 0);
            assert_eq!(frozen.edge_count(), 0);
        }

        #[test]
        fn node_count_matches_added_nodes() {
            let (interner, handles) = make_interner(&["file", "MyClass", "src/a.rs"]);
            let mutable = MutableGraph::new();
            mutable.add_node(NodeLabel::File, handles[0], handles[2], 0, 0);
            mutable.add_node(NodeLabel::Class, handles[1], handles[2], 0, 0);
            let frozen = mutable.freeze(interner);
            assert_eq!(frozen.node_count(), 2);
        }

        #[test]
        fn edge_count_matches_added_edges() {
            let (interner, handles) = make_interner(&["file", "MyClass", "src/a.rs"]);
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File, handles[0], handles[2], 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class, handles[1], handles[2], 0, 0);
            mutable.add_edge(n0, n1, EdgeType::Contains);
            mutable.add_edge(n0, n1, EdgeType::Imports);
            mutable.add_edge(n1, n0, EdgeType::Calls);
            let frozen = mutable.freeze(interner);
            assert_eq!(frozen.edge_count(), 3);
        }
    }

    // ── Node data accessor round-trip ─────────────────────────────────────────

    mod node_data_round_trip {
        use super::*;

        struct ThreeNodes {
            mutable: MutableGraph,
            ids: Vec<NodeId>,
            interner: FrozenInterner,
            name_handles: [InternedStr; 3],
            file_handles: [InternedStr; 2],
        }

        fn build_three_nodes() -> ThreeNodes {
            let si = StringInterner::new();
            let rh_proj  = si.intern("my_project");
            let rh_file  = si.intern("main.rs");
            let rh_class = si.intern("MyClass");
            let rh_root  = si.intern("/root");
            let rh_src   = si.intern("/src");
            let (fi, remap) = si.compact();
            let [h_proj, h_file, h_class, h_root, h_src] =
                [rh_proj, rh_file, rh_class, rh_root, rh_src].map(|h| remap(h));

            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::Project, h_proj,  h_root, 1,  1);
            let n1 = mutable.add_node(NodeLabel::File,    h_file,  h_src,  10, 5);
            let n2 = mutable.add_node(NodeLabel::Class,   h_class, h_src,  20, 3);
            ThreeNodes {
                mutable,
                ids: vec![n0, n1, n2],
                interner: fi,
                name_handles: [h_proj, h_file, h_class],
                file_handles: [h_root, h_src],
            }
        }

        #[test]
        fn node_label_round_trips() {
            let ThreeNodes { mutable, ids, interner, .. } = build_three_nodes();
            let frozen = mutable.freeze(interner);

            assert_eq!(frozen.node_label(ids[0]), NodeLabel::Project);
            assert_eq!(frozen.node_label(ids[1]), NodeLabel::File);
            assert_eq!(frozen.node_label(ids[2]), NodeLabel::Class);
        }

        #[test]
        fn node_name_round_trips() {
            let ThreeNodes { mutable, ids, interner, name_handles, .. } = build_three_nodes();
            let frozen = mutable.freeze(interner);

            assert_eq!(frozen.node_name(ids[0]), name_handles[0]);
            assert_eq!(frozen.node_name(ids[1]), name_handles[1]);
            assert_eq!(frozen.node_name(ids[2]), name_handles[2]);
        }

        #[test]
        fn node_file_round_trips() {
            let ThreeNodes { mutable, ids, interner, file_handles, .. } = build_three_nodes();
            let frozen = mutable.freeze(interner);

            assert_eq!(frozen.node_file(ids[0]), file_handles[0]);
            assert_eq!(frozen.node_file(ids[1]), file_handles[1]);
            assert_eq!(frozen.node_file(ids[2]), file_handles[1]);
        }

        #[test]
        fn node_line_round_trips() {
            let ThreeNodes { mutable, ids, interner, .. } = build_three_nodes();
            let frozen = mutable.freeze(interner);

            assert_eq!(frozen.node_line(ids[0]), 1);
            assert_eq!(frozen.node_line(ids[1]), 10);
            assert_eq!(frozen.node_line(ids[2]), 20);
        }

        #[test]
        fn node_column_round_trips() {
            let ThreeNodes { mutable, ids, interner, .. } = build_three_nodes();
            let frozen = mutable.freeze(interner);

            assert_eq!(frozen.node_column(ids[0]), 1);
            assert_eq!(frozen.node_column(ids[1]), 5);
            assert_eq!(frozen.node_column(ids[2]), 3);
        }
    }

    // ── Forward CSR ────────────────────────────────────────────────────────────

    mod forward_csr {
        use super::*;

        fn build_simple_graph() -> FrozenGraph {
            let (interner, handles) = make_interner(&["file", "MyClass", "render", "src/a.rs"]);
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File,   handles[0], handles[3], 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class,  handles[1], handles[3], 0, 0);
            let n2 = mutable.add_node(NodeLabel::Method, handles[2], handles[3], 0, 0);

            mutable.add_edge(n0, n1, EdgeType::Contains);
            mutable.add_edge(n0, n2, EdgeType::Contains);
            mutable.add_edge(n1, n2, EdgeType::Calls);

            mutable.freeze(interner)
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
            let (interner, handles) = make_interner(&["file", "MyClass", "render", "src/a.rs"]);
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File,   handles[0], handles[3], 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class,  handles[1], handles[3], 0, 0);
            let n2 = mutable.add_node(NodeLabel::Method, handles[2], handles[3], 0, 0);

            mutable.add_edge(n0, n1, EdgeType::Contains);
            mutable.add_edge(n0, n2, EdgeType::Contains);
            mutable.add_edge(n1, n2, EdgeType::Calls);

            mutable.freeze(interner)
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
            let (interner, handles) = make_interner(&["recurse", "src/a.rs"]);
            let mutable = MutableGraph::new();
            let n = mutable.add_node(NodeLabel::Method, handles[0], handles[1], 0, 0);
            mutable.add_edge(n, n, EdgeType::Calls);

            let frozen = mutable.freeze(interner);

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

        struct IsolatedGraph {
            frozen: FrozenGraph,
            isolated_name: InternedStr,
        }

        fn build_with_isolated_node() -> IsolatedGraph {
            let si = StringInterner::new();
            let rh_file = si.intern("main.rs");
            let rh_class = si.intern("Config");
            let rh_fn = si.intern("isolated_fn");
            let rh_src = si.intern("src/");
            let (fi, remap) = si.compact();
            let [h_file, h_class, h_fn, h_src] =
                [rh_file, rh_class, rh_fn, rh_src].map(|h| remap(h));

            let mutable = MutableGraph::new();
            // Node 0: connected
            let n0 = mutable.add_node(NodeLabel::File,     h_file,  h_src, 0, 0);
            let n1 = mutable.add_node(NodeLabel::Class,    h_class, h_src, 0, 0);
            // Node 2: isolated (no edges)
            mutable.add_node(NodeLabel::Function, h_fn, h_src, 0, 0);

            mutable.add_edge(n0, n1, EdgeType::Contains);

            IsolatedGraph { frozen: mutable.freeze(fi), isolated_name: h_fn }
        }

        #[test]
        fn isolated_node_has_no_outgoing_edges() {
            let IsolatedGraph { frozen, .. } = build_with_isolated_node();
            let range = frozen.forward_edge_range(NodeId(2));
            assert_eq!(range.len(), 0);
        }

        #[test]
        fn isolated_node_has_no_incoming_edges() {
            let IsolatedGraph { frozen, .. } = build_with_isolated_node();
            let range = frozen.reverse_edge_range(NodeId(2));
            assert_eq!(range.len(), 0);
        }

        #[test]
        fn isolated_node_data_still_accessible() {
            let IsolatedGraph { frozen, isolated_name } = build_with_isolated_node();
            assert_eq!(frozen.node_label(NodeId(2)), NodeLabel::Function);
            assert_eq!(frozen.node_name(NodeId(2)), isolated_name);
        }
    }

    // ── Edge ordering in CSR ───────────────────────────────────────────────────

    mod csr_edge_ordering {
        use super::*;

        fn build_multi_target_graph() -> FrozenGraph {
            let (interner, h) = make_interner(&[
                "file", "MyClass", "render", "parse", "count", "score", "src/a.rs",
            ]);
            let mutable = MutableGraph::new();
            let n0 = mutable.add_node(NodeLabel::File,     h[0], h[6], 0, 0);
            mutable.add_node(NodeLabel::Class,    h[1], h[6], 0, 0);
            mutable.add_node(NodeLabel::Method,   h[2], h[6], 0, 0);
            mutable.add_node(NodeLabel::Function, h[3], h[6], 0, 0);
            mutable.add_node(NodeLabel::Variable, h[4], h[6], 0, 0);
            mutable.add_node(NodeLabel::Field,    h[5], h[6], 0, 0);

            // Add edges from n0 to several other nodes.
            mutable.add_edge(n0, NodeId(5), EdgeType::Contains); // target 5
            mutable.add_edge(n0, NodeId(3), EdgeType::Calls);    // target 3
            mutable.add_edge(n0, NodeId(1), EdgeType::Imports);  // target 1

            mutable.freeze(interner)
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
            let node_count = 1000usize;
            let edges_per_node = 5usize;

            // Build an interner with distinct names "fn0".."fn999" plus a file handle.
            let si = StringInterner::new();
            let name_handles: Vec<InternedStr> = (0..node_count)
                .map(|i| si.intern(&format!("fn{}", i)))
                .collect();
            let file_raw = si.intern("src/large.rs");
            let (fi, remap) = si.compact();
            let name_handles: Vec<InternedStr> = name_handles.iter().map(|&h| remap(h)).collect();
            let file_handle = remap(file_raw);

            let mutable = MutableGraph::new();

            // Add nodes
            for i in 0..node_count {
                mutable.add_node(
                    NodeLabel::Function,
                    name_handles[i],
                    file_handle,
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

            let frozen = mutable.freeze(fi);
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

    // ── Secondary indexes (E1-5) ──────────────────────────────────────────────

    mod secondary_indexes {
        use super::*;

        /// Builds a graph with 4 nodes across 2 files and 3 distinct labels.
        ///
        /// Nodes:
        ///   0 — File      "src/a.rs"    file="src/a.rs"
        ///   1 — Function  "parse"       file="src/a.rs"
        ///   2 — Function  "render"      file="src/b.rs"
        ///   3 — Class     "Config"      file="src/b.rs"
        struct IndexGraph {
            frozen: FrozenGraph,
            h_src_a: InternedStr,
            h_src_b: InternedStr,
            h_parse: InternedStr,
            h_render: InternedStr,
            h_config: InternedStr,
        }

        fn build_index_graph() -> IndexGraph {
            let si = StringInterner::new();
            let rh_file_a  = si.intern("src/a.rs");
            let rh_file_b  = si.intern("src/b.rs");
            let rh_src_a   = si.intern("src/a.rs");  // same as rh_file_a: deduped
            let rh_src_b   = si.intern("src/b.rs");  // same as rh_file_b: deduped
            let rh_parse   = si.intern("parse");
            let rh_render  = si.intern("render");
            let rh_config  = si.intern("Config");
            let (fi, remap) = si.compact();
            let [h_file_a, _h_file_b, h_src_a, h_src_b, h_parse, h_render, h_config] =
                [rh_file_a, rh_file_b, rh_src_a, rh_src_b, rh_parse, rh_render, rh_config]
                    .map(|h| remap(h));

            let mutable = MutableGraph::new();
            mutable.add_node(NodeLabel::File,     h_file_a, h_src_a, 1, 1);
            mutable.add_node(NodeLabel::Function, h_parse,  h_src_a, 5, 1);
            mutable.add_node(NodeLabel::Function, h_render, h_src_b, 3, 1);
            mutable.add_node(NodeLabel::Class,    h_config, h_src_b, 1, 1);

            IndexGraph {
                frozen: mutable.freeze(fi),
                h_src_a, h_src_b,
                h_parse, h_render, h_config,
            }
        }

        // ── Label bitmaps ─────────────────────────────────────────────────────

        #[test]
        fn label_bitmap_contains_correct_nodes() {
            let g = build_index_graph();
            let functions = g.frozen.nodes_with_label(NodeLabel::Function);
            assert!(functions.contains(1)); // "parse"
            assert!(functions.contains(2)); // "render"
            assert!(!functions.contains(0)); // File node
            assert!(!functions.contains(3)); // Class node
        }

        #[test]
        fn label_bitmap_cardinalities_sum_to_node_count() {
            let g = build_index_graph();
            let total: u64 = (0..NodeLabel::COUNT)
                .map(|i| {
                    // SAFETY: i < COUNT so the cast is always a valid discriminant.
                    let label: NodeLabel = unsafe { std::mem::transmute(i as u8) };
                    g.frozen.nodes_with_label(label).len()
                })
                .sum();
            assert_eq!(total as usize, g.frozen.node_count());
        }

        #[test]
        fn empty_label_bitmap_has_zero_bits() {
            let g = build_index_graph();
            // No Trait nodes in our graph.
            assert_eq!(g.frozen.nodes_with_label(NodeLabel::Trait).len(), 0);
        }

        // ── QN index ─────────────────────────────────────────────────────────

        #[test]
        fn lookup_qn_finds_existing_node() {
            let g = build_index_graph();
            assert_eq!(g.frozen.lookup_qn(g.h_parse),  Some(NodeId(1)));
            assert_eq!(g.frozen.lookup_qn(g.h_render), Some(NodeId(2)));
            assert_eq!(g.frozen.lookup_qn(g.h_config), Some(NodeId(3)));
        }

        #[test]
        fn lookup_qn_returns_none_for_unknown_handle() {
            let g = build_index_graph();
            let absent = InternedStr(u32::MAX);
            assert_eq!(g.frozen.lookup_qn(absent), None);
        }

        #[test]
        fn lookup_qn_returns_highest_node_id_on_duplicate_name() {
            // Two nodes share the same QN string. freeze() builds qn_index by
            // iterating NodeId 0..N in order; HashMap::insert overwrites on
            // duplicate, so the highest NodeId wins.
            let si = StringInterner::new();
            let rh_name = si.intern("do_thing");
            let rh_file = si.intern("src/a.rs");
            let (fi, remap) = si.compact();
            let [h_name, h_file] = [rh_name, rh_file].map(|h| remap(h));

            let mutable = MutableGraph::new();
            mutable.add_node(NodeLabel::Function, h_name, h_file, 1, 1); // NodeId(0)
            mutable.add_node(NodeLabel::Method,   h_name, h_file, 5, 1); // NodeId(1) — same QN
            let frozen = mutable.freeze(fi);

            assert_eq!(frozen.lookup_qn(h_name), Some(NodeId(1)));
        }

        // ── File index ────────────────────────────────────────────────────────

        #[test]
        fn nodes_in_file_returns_correct_set() {
            let g = build_index_graph();
            let in_a = g.frozen.nodes_in_file(g.h_src_a);
            assert_eq!(in_a.len(), 2);
            assert!(in_a.contains(&NodeId(0)));
            assert!(in_a.contains(&NodeId(1)));

            let in_b = g.frozen.nodes_in_file(g.h_src_b);
            assert_eq!(in_b.len(), 2);
            assert!(in_b.contains(&NodeId(2)));
            assert!(in_b.contains(&NodeId(3)));
        }

        #[test]
        fn nodes_in_file_returns_empty_for_unknown_file() {
            let g = build_index_graph();
            let absent = InternedStr(u32::MAX);
            assert_eq!(g.frozen.nodes_in_file(absent), &[] as &[NodeId]);
        }

        // ── FST bare-name index ───────────────────────────────────────────────

        #[test]
        fn fst_contains_all_distinct_names() {
            let g = build_index_graph();
            let fst = g.frozen.bare_name_fst();
            // All 4 distinct name strings must be present.
            assert!(fst.get("src/a.rs").is_some()); // h_file_a
            assert!(fst.get("parse").is_some());
            assert!(fst.get("render").is_some());
            assert!(fst.get("Config").is_some());
        }

        #[test]
        fn fst_slot_resolves_to_correct_node() {
            let g = build_index_graph();
            let fst = g.frozen.bare_name_fst();

            let slot = fst.get("parse").expect("'parse' must be in FST");
            let nodes = g.frozen.bare_name_nodes(slot);
            assert_eq!(nodes, &[NodeId(1)]);
        }

        #[test]
        fn fst_absent_key_returns_none() {
            let g = build_index_graph();
            assert!(g.frozen.bare_name_fst().get("nonexistent_symbol").is_none());
        }

        #[test]
        fn fst_side_table_groups_duplicate_names() {
            // Two nodes with the same bare name "new".
            let si = StringInterner::new();
            let rh_new1  = si.intern("new");
            let rh_new2  = si.intern("new"); // same string — deduped in interner
            let rh_file  = si.intern("src/c.rs");
            let (fi, remap) = si.compact();
            let [h_new1, h_new2, h_file] = [rh_new1, rh_new2, rh_file].map(|h| remap(h));
            // Both handles must be equal since the string is deduped.
            assert_eq!(h_new1, h_new2);

            let mutable = MutableGraph::new();
            mutable.add_node(NodeLabel::Function, h_new1, h_file, 1, 1);
            mutable.add_node(NodeLabel::Method,   h_new2, h_file, 5, 1);
            let frozen = mutable.freeze(fi);

            let fst  = frozen.bare_name_fst();
            let slot = fst.get("new").expect("'new' must be in FST");
            let nodes = frozen.bare_name_nodes(slot);
            // Both NodeId(0) and NodeId(1) must appear under the same slot.
            assert_eq!(nodes.len(), 2);
            assert!(nodes.contains(&NodeId(0)));
            assert!(nodes.contains(&NodeId(1)));
        }

        #[test]
        fn fst_side_table_slot_count_equals_distinct_name_count() {
            let g = build_index_graph();
            // 4 distinct names: "src/a.rs", "parse", "render", "Config"
            assert_eq!(g.frozen.bare_name_fst().len(), 4);
        }

        // ── Interner accessor ─────────────────────────────────────────────────

        #[test]
        fn interner_resolves_name_handles() {
            let g = build_index_graph();
            assert_eq!(g.frozen.interner().resolve(g.h_parse),  "parse");
            assert_eq!(g.frozen.interner().resolve(g.h_render), "render");
            assert_eq!(g.frozen.interner().resolve(g.h_config), "Config");
        }

        #[test]
        fn interner_resolves_file_handles() {
            let g = build_index_graph();
            assert_eq!(g.frozen.interner().resolve(g.h_src_a), "src/a.rs");
            assert_eq!(g.frozen.interner().resolve(g.h_src_b), "src/b.rs");
        }
    }
}
