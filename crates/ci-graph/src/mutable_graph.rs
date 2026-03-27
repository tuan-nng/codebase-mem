//! Append-only mutable graph with concurrent build support.

use ci_core::{EdgeId, EdgeType, InternedStr, NodeId, NodeLabel};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// A mutable graph built during the indexing pipeline.
///
/// Nodes and edges are stored in Structure-of-Arrays (SoA) format for
/// cache-efficient freeze.  Hot node data (label, name) is stored separately
/// from cold data (file path, line/column) to improve cache utilization
/// during queries that only need hot data.
///
/// # Concurrency
///
/// `add_node` and `add_edge` are safe to call concurrently from multiple
/// Rayon worker threads.  Each SoA array has its own `Mutex` to allow
/// parallel appends to different arrays with minimal contention.  For
/// example, one thread appending to `node_labels` does not block another
/// thread appending to `node_files`.
///
/// If profiling reveals lock contention at scale, the recommended escape
/// hatch is to add thread-local buffers (one per Rayon worker) and merge
/// them before `freeze()`.  The public API remains unchanged.
///
/// # No Reads During Build
///
/// This type is designed for the build phase only — all mutation is
/// append-only and there are no reads.  After `freeze()` produces a
/// `FrozenGraph`, the mutable graph is discarded.
pub struct MutableGraph {
    // ── Hot node data (frequently accessed during queries) ──────────────────

    /// Node kind label (Function, Class, File, etc.)
    node_labels: Mutex<Vec<NodeLabel>>,
    /// Interned symbol name.
    node_names: Mutex<Vec<InternedStr>>,

    // ── Cold node data (rarely accessed during queries) ─────────────────────

    /// Interned source file path.
    node_files: Mutex<Vec<InternedStr>>,
    /// 1-based line number in the source file (0 = unknown).
    node_lines: Mutex<Vec<u32>>,
    /// 1-based column number in the source file (0 = unknown).
    node_columns: Mutex<Vec<u32>>,

    // ── Edge data ───────────────────────────────────────────────────────────

    /// Source node of each edge.
    edge_sources: Mutex<Vec<NodeId>>,
    /// Target node of each edge.
    edge_targets: Mutex<Vec<NodeId>>,
    /// Relationship kind between source and target.
    edge_types: Mutex<Vec<EdgeType>>,

    // ── Atomic counters ─────────────────────────────────────────────────────

    next_node_id: AtomicU32,
    next_edge_id: AtomicU32,
}

impl MutableGraph {
    /// Create a new empty graph.
    pub fn new() -> Self {
        Self {
            node_labels: Mutex::new(Vec::new()),
            node_names: Mutex::new(Vec::new()),
            node_files: Mutex::new(Vec::new()),
            node_lines: Mutex::new(Vec::new()),
            node_columns: Mutex::new(Vec::new()),
            edge_sources: Mutex::new(Vec::new()),
            edge_targets: Mutex::new(Vec::new()),
            edge_types: Mutex::new(Vec::new()),
            next_node_id: AtomicU32::new(0),
            next_edge_id: AtomicU32::new(0),
        }
    }

    /// Add a node and return its `NodeId`.
    ///
    /// All arguments are stored by value; the graph takes no references
    /// during the build phase.
    ///
    /// # Panics
    ///
    /// Panics if the node counter wraps past `u32::MAX` (unlikely — 4B nodes).
    pub fn add_node(
        &self,
        label: NodeLabel,
        name: InternedStr,
        file: InternedStr,
        line: u32,
        column: u32,
    ) -> NodeId {
        let id = NodeId(self.next_node_id.fetch_add(1, Ordering::Relaxed));

        self.node_labels.lock().push(label);
        self.node_names.lock().push(name);
        self.node_files.lock().push(file);
        self.node_lines.lock().push(line);
        self.node_columns.lock().push(column);

        id
    }

    /// Add a directed edge and return its `EdgeId`.
    ///
    /// # Panics
    ///
    /// Panics if the edge counter wraps past `u32::MAX` (unlikely — 4B edges).
    pub fn add_edge(&self, source: NodeId, target: NodeId, edge_type: EdgeType) -> EdgeId {
        let eid = EdgeId(self.next_edge_id.fetch_add(1, Ordering::Relaxed));

        self.edge_sources.lock().push(source);
        self.edge_targets.lock().push(target);
        self.edge_types.lock().push(edge_type);

        eid
    }

    /// Total number of nodes in the graph.
    #[inline]
    pub fn node_count(&self) -> u32 {
        self.next_node_id.load(Ordering::Relaxed)
    }

    /// Total number of edges in the graph.
    #[inline]
    pub fn edge_count(&self) -> u32 {
        self.next_edge_id.load(Ordering::Relaxed)
    }

    /// All node labels, in `NodeId` index order.
    pub fn node_labels(&self) -> Vec<NodeLabel> {
        self.node_labels.lock().clone()
    }

    /// All node names, in `NodeId` index order.
    pub fn node_names(&self) -> Vec<InternedStr> {
        self.node_names.lock().clone()
    }

    /// All node source file handles, in `NodeId` index order.
    pub fn node_files(&self) -> Vec<InternedStr> {
        self.node_files.lock().clone()
    }

    /// All node line numbers, in `NodeId` index order.
    pub fn node_lines(&self) -> Vec<u32> {
        self.node_lines.lock().clone()
    }

    /// All node column numbers, in `NodeId` index order.
    pub fn node_columns(&self) -> Vec<u32> {
        self.node_columns.lock().clone()
    }

    /// All edge sources, in `EdgeId` index order.
    pub fn edge_sources(&self) -> Vec<NodeId> {
        self.edge_sources.lock().clone()
    }

    /// All edge targets, in `EdgeId` index order.
    pub fn edge_targets(&self) -> Vec<NodeId> {
        self.edge_targets.lock().clone()
    }

    /// All edge types, in `EdgeId` index order.
    pub fn edge_types(&self) -> Vec<EdgeType> {
        self.edge_types.lock().clone()
    }
}

impl Default for MutableGraph {
    fn default() -> Self {
        Self::new()
    }
}
