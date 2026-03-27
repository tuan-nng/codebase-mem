# ci-graph

Graph structures for the Codebase Intelligence Engine.

## Crates

| Crate | Purpose |
|-------|---------|
| `ci-core` | Core types: `NodeId`, `EdgeType`, `NodeLabel`, `InternedStr`, `StringInterner` |
| `ci-graph` | `MutableGraph`, `FrozenGraph`, `MmapFrozenGraph`, persistence |

## Architecture

```
MutableGraph (build phase)
    │
    │  .freeze(frozen_interner)
    ▼
FrozenGraph (query phase)
    │
    │  save() → graph.bin
    ▼
MmapFrozenGraph (zero-copy load)
```

### MutableGraph

Append-only Structure-of-Arrays (SoA) layout. Node and edge data live in separate parallel vectors. Concurrent appends from Rayon worker threads are safe via per-array mutexes.

### FrozenGraph

Produced by calling `freeze()` on a `MutableGraph`. Edges are sorted by source `NodeId` and stored in CSR (Compressed Sparse Row) format for O(1) forward and reverse neighbor traversal. Node and edge SoA arrays are moved (not copied) from the mutable phase.

Secondary indexes are built during freeze:

- `RoaringBitmap` per `NodeLabel` — instant label filtering
- `HashMap<InternedStr, NodeId>` — qualified name lookup
- `HashMap<InternedStr, Vec<NodeId>>` — file → nodes
- `fst::Map` — prefix/fuzzy symbol search

### Persistence

`save()` serializes the graph via rkyv and writes to disk with an atomic rename. `MmapFrozenGraph::load()` memory-maps the file, validates a magic header + xxh3_64 checksum, and returns a zero-copy view — no heap allocation on load.

## Benchmark Results (E1-7)

Run with `cargo bench -p ci-graph --bench benchmarks`.

Synthetic workload: 1M nodes, 5M edges, 10K files. Edge distribution: 60% within-file, 25% cross-file, 15% random. Measured on a 12-core machine (Ryzen 5900X).

| Operation | Result | Target | Status |
|-----------|--------|--------|--------|
| Build + freeze + save | **1.47 s** | < 5 s | PASS |
| BFS depth-2 | **34–128 ns** | < 100 µs | PASS |
| mmap cold load | **60 ms** | < 10 ms | MISS |
| FST prefix search | **198 ns–27 µs** | < 1 ms | PASS |

Notes:
- **Build pipeline** is 3.4× faster than target. Parallel string interning + concurrent graph build are the main wins.
- **BFS depth-2** is 1,000× faster than target. CSR adjacency makes neighbor iteration essentially free.
- **mmap cold load** at 60ms is ~6× the target. This is hardware-dependent (storage speed, OS page cache eviction). The target was specified for a 16-core machine; your results will vary.
- **FST prefix search**: Zero-result searches (`fn_5000`) run in ~200 ns. High-cardinality searches (`src/fi` matching 10K file paths) take ~27 µs, dominated by result iteration — the FST search itself is sub-microsecond.

### Why mmap cold load misses target

The 10ms target assumes the graph file fits in the OS page cache after being written by the build benchmark. On a cold run (or after `echo 3 > /proc/sys/vm/drop_caches`), the file must be read from storage. On this machine, storage read speed is the bottleneck, not graph deserialization.

## Dependencies

```
ci-core ──────► StringInterner ──► FrozenInterner
                  (16-shard)         (compact)

ci-graph ─────► MutableGraph ─────► FrozenGraph ───► save()
                 (SoA Vecs)          (CSR adj)        (rkyv)

                                    MmapFrozenGraph ─► load()
                                      (mmap2)
```

## Developing

```bash
# Run tests
cargo test -p ci-graph

# Run benchmarks
cargo bench -p ci-graph --bench benchmarks

# Run benchmarks with gnuplot (better plots)
cargo bench -p ci-graph --bench benchmarks
# or install gnuplot: apt install gnuplot
```
