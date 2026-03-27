//! Criterion benchmarks for the Codebase Intelligence graph pipeline.
//!
//! E1-7: Validates performance targets for the frozen graph pipeline.
//!
//! Synthetic workload: 1M nodes, 5M edges, 10K files.
//! Edge distribution: 60% within-file, 25% cross-file, 15% random.
//!
//! Results (12-core Ryzen 5900X):
//! | Operation             | Result      | Target    | Pass |
//! |-----------------------|-------------|-----------|------|
//! | Build + freeze + save | **1.47 s**  | < 5 s     | ✓    |
//! | BFS depth-2          | **34–128 ns**| < 100 µs  | ✓    |
//! | mmap cold load        | **60 ms**   | < 10 ms   | —    |
//! | FST prefix search     | **198 ns–27 µs** | < 1 ms | ✓    |
//!
//! mmap target missed (~6×) — see crates/ci-graph/README.md for analysis.

use std::path::PathBuf;

use ci_core::{EdgeType, InternedStr, NodeId, NodeLabel, StringInterner};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
use fst::automaton::Automaton;
use fst::{IntoStreamer, Streamer};
use rayon::prelude::*;

use ci_graph::{save, FrozenGraph, MmapFrozenGraph, MutableGraph};

// ── Constants ─────────────────────────────────────────────────────────────────

const NODE_COUNT: usize = 1_000_000;
const EDGE_COUNT: usize = 5_000_000;
const FILE_COUNT: usize = 10_000;
const NODES_PER_FILE: usize = NODE_COUNT / FILE_COUNT; // 100

// ── Shared graph fixture ───────────────────────────────────────────────────────

/// Pre-built graph shared across all query benchmarks (BFS, FST, mmap).
/// Constructed once at startup; each benchmark iterates over it.
struct GraphFixture {
    frozen: FrozenGraph,
    path: PathBuf,
    #[allow(dead_code)]
    temp_dir: tempfile::TempDir,
}

impl GraphFixture {
    fn new() -> Self {
        let num_cpus = num_cpus::get();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus)
            .build()
            .expect("failed to build Rayon pool");

        pool.install(Self::build)
    }

    fn build() -> Self {
        let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let path = tmp_dir.path().join("graph.bin");

        // Build the string interner.
        let si = StringInterner::new();

        // Intern all unique node names in parallel.
        let name_handles: Vec<InternedStr> = (0..NODE_COUNT)
            .into_par_iter()
            .map(|i| si.intern(&format!("fn_{i:07}")))
            .collect();

        // Intern file path handles.
        let file_handles: Vec<InternedStr> = (0..FILE_COUNT)
            .map(|i| si.intern(&format!("src/file_{:05}.rs", i)))
            .collect();

        // Compact and remap.
        let (frozen_interner, remap) = si.compact();
        let name_handles: Vec<InternedStr> = name_handles.into_iter().map(&remap).collect();
        let file_handles: Vec<InternedStr> = file_handles.into_iter().map(&remap).collect();

        // Build the mutable graph.
        let graph = MutableGraph::new();

        for (i, &name_handle) in name_handles.iter().enumerate().take(NODE_COUNT) {
            let file_idx = i / NODES_PER_FILE;
            graph.add_node(
                NodeLabel::Function,
                name_handle,
                file_handles[file_idx],
                (i % 1000) as u32,
                0,
            );
        }

        // Add edges with the configured distribution.
        let mut rng = fastrand::Rng::with_seed(42);

        // Within-file edges (60%)
        let within_count = (EDGE_COUNT * 60) / 100;
        for _ in 0..within_count {
            let file_idx = rng.usize(..FILE_COUNT);
            let base = file_idx * NODES_PER_FILE;
            let src = base + rng.usize(..NODES_PER_FILE);
            let dst = base + rng.usize(..NODES_PER_FILE);
            graph.add_edge(NodeId(src as u32), NodeId(dst as u32), EdgeType::Calls);
        }

        // Cross-file edges to next file (25%)
        let cross_count = (EDGE_COUNT * 25) / 100;
        for _ in 0..cross_count {
            let file_idx = rng.usize(..FILE_COUNT.saturating_sub(1));
            let src_base = file_idx * NODES_PER_FILE;
            let dst_base = (file_idx + 1) * NODES_PER_FILE;
            let src = src_base + rng.usize(..NODES_PER_FILE);
            let dst = dst_base + rng.usize(..NODES_PER_FILE);
            graph.add_edge(NodeId(src as u32), NodeId(dst as u32), EdgeType::Calls);
        }

        // Random edges (15%)
        let random_count = EDGE_COUNT - within_count - cross_count;
        for _ in 0..random_count {
            let src = rng.usize(..NODE_COUNT);
            let dst = rng.usize(..NODE_COUNT);
            graph.add_edge(NodeId(src as u32), NodeId(dst as u32), EdgeType::Calls);
        }

        let frozen = graph.freeze(frozen_interner);
        save(&frozen, &path).expect("save should succeed");

        Self {
            frozen,
            path,
            temp_dir: tmp_dir,
        }
    }
}

impl Default for GraphFixture {
    fn default() -> Self {
        Self::new()
    }
}

// ── BFS depth-2 ─────────────────────────────────────────────────────────────

fn bfs_depth2(frozen: &FrozenGraph, start: NodeId) -> Vec<NodeId> {
    let mut visited = Vec::new();
    let mut frontier = Vec::new();
    visited.reserve(256);
    frontier.reserve(64);

    for (target, _) in frozen.forward_edges(start) {
        frontier.push(target);
        visited.push(target);
    }

    for node in frontier.drain(..) {
        for (target, _) in frozen.forward_edges(node) {
            if !visited.contains(&target) {
                visited.push(target);
            }
        }
    }

    visited
}

// ── Benchmark groups ───────────────────────────────────────────────────────────

fn bench_build_freeze_save(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_freeze_save");
    group.sample_size(10).sampling_mode(SamplingMode::Flat);

    group.bench_function("1M_nodes_5M_edges", |b| {
        b.iter(|| {
            let fixture = GraphFixture::new();
            black_box(&fixture.frozen);
            black_box(&fixture.path);
        });
    });
}

fn bench_bfs(c: &mut Criterion) {
    let fixture = GraphFixture::new();
    let frozen = &fixture.frozen;
    let node_count = frozen.node_count() as u32;

    let start_nodes = [
        NodeId(0),
        NodeId(node_count / 4),
        NodeId(node_count / 2),
        NodeId(node_count - 1),
    ];

    let mut group = c.benchmark_group("bfs_depth2");

    for (i, &start) in start_nodes.iter().enumerate() {
        group.bench_function(BenchmarkId::from_parameter(i), |b| {
            b.iter(|| {
                let result = bfs_depth2(black_box(frozen), black_box(start));
                black_box(result.len());
            });
        });
    }
}

fn bench_mmap_load(c: &mut Criterion) {
    let fixture = GraphFixture::new();
    let path = fixture.path.clone();

    // Keep temp_dir alive (via fixture.temp_dir) for the duration of the benchmark.
    // We only need the path, but must not drop fixture until we're done measuring.

    let mut group = c.benchmark_group("mmap_cold_load");

    group.bench_function("1M_nodes", |b| {
        b.iter(|| {
            let loaded = MmapFrozenGraph::load(&path).expect("load should succeed");
            black_box(loaded.node_count());
            drop(loaded);
        });
    });
}

fn bench_fst_search(c: &mut Criterion) {
    let fixture = GraphFixture::new();
    let frozen = &fixture.frozen;

    // Build FST once outside the measurement loop.
    let fst = frozen.bare_name_fst();

    let prefixes = [
        "fn_0000", "fn_0001", "fn_0010", "fn_0100", "fn_0101", "fn_1000", "fn_1234", "fn_5000",
        "fn_9999", "src/fi",
    ];

    let mut group = c.benchmark_group("fst_prefix_search");

    for (i, prefix) in prefixes.iter().enumerate() {
        group.bench_function(BenchmarkId::from_parameter(i), |b| {
            b.iter(|| {
                let stream = fst.search(fst::automaton::Str::new(prefix).starts_with());
                let mut stream = stream.into_stream();
                let mut count = 0u64;
                while let Some(item) = stream.next() {
                    black_box(item);
                    count += 1;
                }
                black_box(count);
            });
        });
    }
}

// ── Criterion entry point ──────────────────────────────────────────────────────

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(std::time::Duration::from_secs(10))
        .warm_up_time(std::time::Duration::from_secs(2));
    targets = bench_build_freeze_save, bench_bfs, bench_mmap_load, bench_fst_search
);
criterion_main!(benches);
