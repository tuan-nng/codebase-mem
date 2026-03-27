//! Mutable and frozen graph structures for the Codebase Intelligence Engine.
//!
//! # MutableGraph
//!
//! The mutable graph is built incrementally during the indexing pipeline.
//! It uses a Structure-of-Arrays (SoA) layout for cache efficiency during
//! the freeze process.  All appends are append-only; there are no reads
//! during the build phase.
//!
//! ## Concurrency
//!
//! Concurrent node and edge appends from Rayon worker threads are supported
//! via per-array mutexes.  Thread-local buffer batching (the "contention
//! escape hatch" from E1-2) can be layered on top if benchmarks show measurable
//! lock contention at scale.
//!
//! # FrozenGraph
//!
//! Produced by calling `freeze()` on a `MutableGraph`.  Edges are sorted by
//! source `NodeId` and stored in CSR (Compressed Sparse Row) format for
//! efficient forward and reverse traversal.  Node and edge SoA arrays are
//! moved (not copied) from the mutable phase.

mod mutable_graph;
mod frozen_graph;
pub mod persistence;

pub use mutable_graph::MutableGraph;
pub use frozen_graph::FrozenGraph;
pub use persistence::{MmapFrozenGraph, save, serialized_size};

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use ci_core::{EdgeId, EdgeType, InternedStr, NodeId, NodeLabel};

    use crate::MutableGraph;

    /// Helper: build a small graph with nodes and edges.
    fn build_small_graph() -> MutableGraph {
        let graph = MutableGraph::new();

        let project = graph.add_node(NodeLabel::Project, InternedStr(0), InternedStr(0), 0, 0);
        let file = graph.add_node(NodeLabel::File, InternedStr(1), InternedStr(0), 0, 0);
        let class = graph.add_node(NodeLabel::Class, InternedStr(2), InternedStr(1), 10, 0);
        let method = graph.add_node(NodeLabel::Method, InternedStr(3), InternedStr(1), 20, 4);

        graph.add_edge(project, file, EdgeType::Contains);
        graph.add_edge(file, class, EdgeType::Contains);
        graph.add_edge(class, method, EdgeType::Contains);
        graph.add_edge(method, method, EdgeType::Calls); // self-call for testing

        graph
    }

    // ── MutableGraph::new ─────────────────────────────────────────────────────

    mod new_graph {
        use super::*;

        #[test]
        fn new_graph_has_zero_nodes() {
            let graph = MutableGraph::new();
            assert_eq!(graph.node_count(), 0);
            assert_eq!(graph.edge_count(), 0);
        }

        #[test]
        fn new_graph_has_zero_capacity() {
            let graph = MutableGraph::new();
            assert!(graph.node_labels().is_empty());
            assert!(graph.node_names().is_empty());
            assert!(graph.edge_sources().is_empty());
        }
    }

    // ── add_node ───────────────────────────────────────────────────────────────

    mod add_node {
        use super::*;

        #[test]
        fn add_single_node_returns_node_id_zero() {
            let graph = MutableGraph::new();
            let id = graph.add_node(NodeLabel::Function, InternedStr(100), InternedStr(200), 5, 10);
            assert_eq!(id, NodeId(0));
        }

        #[test]
        fn add_multiple_nodes_increments_counter() {
            let graph = MutableGraph::new();
            let id0 = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let id1 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(1), 1, 1);
            let id2 = graph.add_node(NodeLabel::Method, InternedStr(2), InternedStr(2), 2, 2);

            assert_eq!(id0, NodeId(0));
            assert_eq!(id1, NodeId(1));
            assert_eq!(id2, NodeId(2));
        }

        #[test]
        fn node_count_increments() {
            let graph = MutableGraph::new();
            assert_eq!(graph.node_count(), 0);

            graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            assert_eq!(graph.node_count(), 1);

            graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(1), 1, 1);
            assert_eq!(graph.node_count(), 2);
        }

        #[test]
        fn add_node_stores_hot_data() {
            let graph = MutableGraph::new();
            let name = InternedStr(42);
            let file = InternedStr(99);

            let id = graph.add_node(NodeLabel::Function, name, file, 10, 5);

            let labels = graph.node_labels();
            let names = graph.node_names();
            let idx = u32::from(id) as usize;

            assert_eq!(labels[idx], NodeLabel::Function);
            assert_eq!(names[idx], name);
        }

        #[test]
        fn add_node_stores_cold_data() {
            let graph = MutableGraph::new();
            let name = InternedStr(10);
            let file = InternedStr(20);

            let id = graph.add_node(NodeLabel::Method, name, file, 42, 7);

            let files = graph.node_files();
            let lines = graph.node_lines();
            let columns = graph.node_columns();
            let idx = u32::from(id) as usize;

            assert_eq!(files[idx], file);
            assert_eq!(lines[idx], 42);
            assert_eq!(columns[idx], 7);
        }

        #[test]
        fn add_node_handles_all_node_labels() {
            let graph = MutableGraph::new();
            let file = InternedStr(0);

            let all_labels = [
                NodeLabel::Project,
                NodeLabel::Package,
                NodeLabel::Directory,
                NodeLabel::File,
                NodeLabel::Class,
                NodeLabel::Interface,
                NodeLabel::Trait,
                NodeLabel::Function,
                NodeLabel::Method,
                NodeLabel::TypeAlias,
                NodeLabel::Variable,
                NodeLabel::Field,
                NodeLabel::Namespace,
            ];

            for label in all_labels {
                let id = graph.add_node(label, InternedStr(0), file, 0, 0);
                let idx = u32::from(id) as usize;
                assert_eq!(graph.node_labels()[idx], label);
            }
        }
    }

    // ── add_edge ──────────────────────────────────────────────────────────────

    mod add_edge {
        use super::*;

        #[test]
        fn add_single_edge_returns_edge_id_zero() {
            let graph = MutableGraph::new();
            let n1 = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n2 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);

            let eid = graph.add_edge(n1, n2, EdgeType::Contains);
            assert_eq!(eid, EdgeId(0));
        }

        #[test]
        fn add_multiple_edges_increments_counter() {
            let graph = MutableGraph::new();
            let n1 = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n2 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);

            let e0 = graph.add_edge(n1, n2, EdgeType::Contains);
            let e1 = graph.add_edge(n1, n2, EdgeType::Contains);
            let e2 = graph.add_edge(n2, n1, EdgeType::Calls);

            assert_eq!(e0, EdgeId(0));
            assert_eq!(e1, EdgeId(1));
            assert_eq!(e2, EdgeId(2));
        }

        #[test]
        fn edge_count_increments() {
            let graph = MutableGraph::new();
            let n1 = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n2 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);

            assert_eq!(graph.edge_count(), 0);

            graph.add_edge(n1, n2, EdgeType::Contains);
            assert_eq!(graph.edge_count(), 1);

            graph.add_edge(n2, n1, EdgeType::Calls);
            assert_eq!(graph.edge_count(), 2);
        }

        #[test]
        fn add_edge_stores_source() {
            let graph = MutableGraph::new();
            let n1 = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n2 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);

            let eid = graph.add_edge(n1, n2, EdgeType::Calls);
            let idx = u32::from(eid) as usize;

            assert_eq!(graph.edge_sources()[idx], n1);
        }

        #[test]
        fn add_edge_stores_target() {
            let graph = MutableGraph::new();
            let n1 = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n2 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);

            let eid = graph.add_edge(n1, n2, EdgeType::Calls);
            let idx = u32::from(eid) as usize;

            assert_eq!(graph.edge_targets()[idx], n2);
        }

        #[test]
        fn add_edge_stores_type() {
            let graph = MutableGraph::new();
            let n1 = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let n2 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);

            let eid = graph.add_edge(n1, n2, EdgeType::Inherits);
            let idx = u32::from(eid) as usize;

            assert_eq!(graph.edge_types()[idx], EdgeType::Inherits);
        }

        #[test]
        fn add_edge_handles_self_loop() {
            let graph = MutableGraph::new();
            let n = graph.add_node(NodeLabel::Method, InternedStr(0), InternedStr(0), 0, 0);

            let eid = graph.add_edge(n, n, EdgeType::Calls);
            let idx = u32::from(eid) as usize;

            assert_eq!(graph.edge_sources()[idx], n);
            assert_eq!(graph.edge_targets()[idx], n);
        }

        #[test]
        fn add_edge_handles_all_edge_types() {
            let graph = MutableGraph::new();
            let n1 = graph.add_node(NodeLabel::Class, InternedStr(0), InternedStr(0), 0, 0);
            let n2 = graph.add_node(NodeLabel::Class, InternedStr(1), InternedStr(0), 0, 0);

            let all_types = [
                EdgeType::Contains,
                EdgeType::Calls,
                EdgeType::CallsHttp,
                EdgeType::Imports,
                EdgeType::ReExports,
                EdgeType::Inherits,
                EdgeType::Implements,
                EdgeType::Decorates,
                EdgeType::Uses,
                EdgeType::Tests,
            ];

            for edge_type in all_types {
                let eid = graph.add_edge(n1, n2, edge_type);
                let idx = u32::from(eid) as usize;
                assert_eq!(graph.edge_types()[idx], edge_type);
            }
        }
    }

    // ── concurrent append ──────────────────────────────────────────────────────

    mod concurrent_append {
        use super::*;

        #[test]
        fn concurrent_add_node_from_multiple_threads() {
            let graph = Arc::new(MutableGraph::new());
            let thread_count = 8;
            let nodes_per_thread = 100;

            let handles: Vec<_> = (0..thread_count)
                .map(|tid| {
                    let graph = Arc::clone(&graph);
                    thread::spawn(move || {
                        for i in 0..nodes_per_thread {
                            graph.add_node(
                                NodeLabel::Function,
                                InternedStr((tid * nodes_per_thread + i) as u32),
                                InternedStr(0),
                                i as u32,
                                0,
                            );
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }

            assert_eq!(graph.node_count(), (thread_count * nodes_per_thread) as u32);
        }

        #[test]
        fn concurrent_add_edge_from_multiple_threads() {
            let graph = Arc::new(MutableGraph::new());

            // Pre-populate nodes
            let node_count = 100;
            let base_id = graph.add_node(NodeLabel::Project, InternedStr(0), InternedStr(0), 0, 0);
            let base_index = u32::from(base_id);
            for i in 1..node_count {
                graph.add_node(NodeLabel::File, InternedStr(i as u32), InternedStr(0), 0, 0);
            }

            let graph_arc = Arc::clone(&graph);
            let handles: Vec<_> = (0..8)
                .map(|tid| {
                    let graph = Arc::clone(&graph_arc);
                    thread::spawn(move || {
                        for i in 0..50 {
                            let src = NodeId(base_index + ((tid * 50 + i) % node_count) as u32);
                            let dst = NodeId(base_index + ((tid * 50 + i + 1) % node_count) as u32);
                            graph.add_edge(src, dst, EdgeType::Calls);
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }

            assert_eq!(graph.edge_count(), 400);
        }

        #[test]
        fn concurrent_node_and_edge_append() {
            let graph = Arc::new(MutableGraph::new());

            // Thread 1: adds nodes
            let h1 = {
                let graph = Arc::clone(&graph);
                thread::spawn(move || {
                    for i in 0..200 {
                        graph.add_node(
                            NodeLabel::Function,
                            InternedStr(i as u32),
                            InternedStr(0),
                            i as u32,
                            0,
                        );
                    }
                })
            };

            // Thread 2: adds edges
            let h2 = {
                let graph = Arc::clone(&graph);
                thread::spawn(move || {
                    thread::sleep(std::time::Duration::from_micros(10));
                    for i in 0..100 {
                        let src = NodeId(i * 2);
                        let dst = NodeId(i * 2 + 1);
                        graph.add_edge(src, dst, EdgeType::Calls);
                    }
                })
            };

            // Thread 3: more nodes
            let h3 = {
                let graph = Arc::clone(&graph);
                thread::spawn(move || {
                    for i in 200..400 {
                        graph.add_node(
                            NodeLabel::Method,
                            InternedStr(i as u32),
                            InternedStr(0),
                            i as u32,
                            0,
                        );
                    }
                })
            };

            h1.join().unwrap();
            h2.join().unwrap();
            h3.join().unwrap();

            assert_eq!(graph.node_count(), 400);
            assert_eq!(graph.edge_count(), 100);
        }
    }

    // ── node/edge ID generation ──────────────────────────────────────────────

    mod id_generation {
        use super::*;

        #[test]
        fn node_ids_are_sequential() {
            let graph = MutableGraph::new();
            let mut ids = Vec::new();

            for i in 0..50 {
                let id = graph.add_node(NodeLabel::File, InternedStr(i as u32), InternedStr(0), 0, 0);
                ids.push(id);
            }

            for (i, id) in ids.into_iter().enumerate() {
                assert_eq!(id, NodeId(i as u32));
            }
        }

        #[test]
        fn edge_ids_are_sequential() {
            let graph = MutableGraph::new();
            let n = graph.add_node(NodeLabel::File, InternedStr(0), InternedStr(0), 0, 0);
            let mut eids = Vec::new();

            for _ in 0..50 {
                let eid = graph.add_edge(n, n, EdgeType::Calls);
                eids.push(eid);
            }

            for (i, eid) in eids.into_iter().enumerate() {
                assert_eq!(eid, EdgeId(i as u32));
            }
        }
    }

    // ── small graph integration ───────────────────────────────────────────────

    mod small_graph_integration {
        use super::*;

        #[test]
        fn small_graph_node_and_edge_counts() {
            let graph = build_small_graph();
            assert_eq!(graph.node_count(), 4);
            assert_eq!(graph.edge_count(), 4);
        }

        #[test]
        fn small_graph_labels_correct() {
            let graph = build_small_graph();
            let labels = graph.node_labels();

            assert_eq!(labels[0], NodeLabel::Project);
            assert_eq!(labels[1], NodeLabel::File);
            assert_eq!(labels[2], NodeLabel::Class);
            assert_eq!(labels[3], NodeLabel::Method);
        }

        #[test]
        fn small_graph_edges_correct() {
            let graph = build_small_graph();
            let sources = graph.edge_sources();
            let targets = graph.edge_targets();
            let types = graph.edge_types();

            // Project → File (Contains)
            assert_eq!(sources[0], NodeId(0));
            assert_eq!(targets[0], NodeId(1));
            assert_eq!(types[0], EdgeType::Contains);

            // File → Class (Contains)
            assert_eq!(sources[1], NodeId(1));
            assert_eq!(targets[1], NodeId(2));
            assert_eq!(types[1], EdgeType::Contains);

            // Class → Method (Contains)
            assert_eq!(sources[2], NodeId(2));
            assert_eq!(targets[2], NodeId(3));
            assert_eq!(types[2], EdgeType::Contains);

            // Method → Method (Calls, self-loop)
            assert_eq!(sources[3], NodeId(3));
            assert_eq!(targets[3], NodeId(3));
            assert_eq!(types[3], EdgeType::Calls);
        }
    }
}
