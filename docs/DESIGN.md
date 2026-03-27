# Codebase Intelligence Engine — Technical Design Document

## 1. Problem Statement

AI coding agents (Claude Code, Codex, Gemini CLI, Cursor, etc.) need to understand large codebases efficiently. Today, agents rely on brute-force approaches — reading files, grepping for patterns, running `find` — which consume enormous token budgets and miss structural relationships between code symbols.

The existing `codebase-memory-mcp` system (written in C) solves this by indexing codebases into a **code knowledge graph** and exposing it to agents via the Model Context Protocol (MCP). It supports 66 programming languages, handles codebases as large as the Linux kernel (28M LOC), and achieves sub-millisecond query times.

**Why redesign in Rust?**

The C implementation works but has architectural constraints that limit its next evolution:

- **SQLite for graph queries** — BFS traversals require repeated SQL round-trips, adding unnecessary overhead to the hottest query path
- **Single-client model** — each MCP stdio session owns its index; multiple agents can't share a single indexed graph
- **Manual memory management** — complex, error-prone, limits ability to safely evolve the codebase
- **Monolithic binary** — no way to embed the engine as a library in other tools
- **No streaming** — large query results must be fully materialized before returning

The Rust redesign addresses all of these while targeting measurable performance improvements.

---

## 2. Goals and Non-Goals

### Goals

- **Performance**: Index the Linux kernel in < 2 minutes (vs. 3 min in C). Query latency < 100 microseconds for graph traversals (vs. ~1ms with SQLite)
- **Multi-client**: A daemon process that multiple agents share, avoiding redundant indexing
- **MCP compatibility**: Drop-in replacement for the C version — same 14 tools, same agent integrations
- **Embeddable**: Core engine usable as a Rust library or via C FFI, not just as an MCP server
- **Incremental**: Fast re-indexing when files change (< 500ms for typical edits)
- **Memory-efficient**: < 500 MB for a 2.1M-node graph (Linux kernel scale)

### Non-Goals

- Full Cypher/GQL compliance (we support a practical subset)
- Remote/cloud deployment (this is a local developer tool)
- Real-time LSP integration (we may add this later but it's not in scope)
- Replacing tree-sitter with a custom parser

---

## 3. System Architecture

### 3.1 Layer Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                      Agent Layer                            │
│  Claude Code  │  Codex CLI  │  Gemini  │  Cursor  │  Zed   │
└───────┬───────┴──────┬──────┴────┬─────┴────┬─────┴────┬───┘
        │              │           │          │          │
┌───────▼──────────────▼───────────▼──────────▼──────────▼───┐
│                   Transport Layer                           │
│  ┌──────────┐  ┌──────────────┐  ┌───────────────────────┐ │
│  │MCP stdio │  │MCP HTTP/SSE  │  │ Unix Domain Socket    │ │
│  │(default) │  │(streaming)   │  │ (daemon IPC)          │ │
│  └────┬─────┘  └──────┬───────┘  └───────────┬───────────┘ │
└───────┼───────────────┼───────────────────────┼─────────────┘
        │               │                       │
┌───────▼───────────────▼───────────────────────▼─────────────┐
│                    Engine Layer (core library)               │
│  ┌────────────┐  ┌────────────┐  ┌────────────────────────┐ │
│  │ Query      │  │ Indexing   │  │ Graph                  │ │
│  │ Engine     │  │ Pipeline   │  │ (CSR + SoA in-memory)  │ │
│  └────────────┘  └────────────┘  └────────────────────────┘ │
└─────────────────────────┬───────────────────────────────────┘
                          │
┌─────────────────────────▼───────────────────────────────────┐
│                   Storage Layer                              │
│  ┌─────────────────┐  ┌──────────────┐  ┌────────────────┐ │
│  │ rkyv + mmap     │  │ File State   │  │ Source Files   │ │
│  │ (graph on disk) │  │ (redb)       │  │ (mmap reads)   │ │
│  └─────────────────┘  └──────────────┘  └────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 Crate Organization

```
codebase-intelligence/
  Cargo.toml              # workspace root
  crates/
    ci-core/              # Shared types: NodeId, EdgeType, NodeLabel, etc.
    ci-graph/             # In-memory graph: MutableGraph (build) + FrozenGraph (query)
    ci-parser/            # tree-sitter extraction for 66 languages
    ci-pipeline/          # Multi-pass indexing orchestrator
    ci-query/             # Query engine (structured API + Cypher subset)
    ci-storage/           # Persistence: rkyv serialization, mmap loading
    ci-discover/          # File walking, gitignore, language detection
    ci-protocol/          # Transport adapters: MCP stdio, HTTP/SSE, UDS
    ci-watcher/           # File change detection and auto-reindex triggers
    ci-server/            # Server binary (main entry point)
    ci-cli/               # CLI: install, config, single-shot tool invocations
    ci-ffi/               # C FFI bindings for embedding
```

Each crate compiles and tests independently. `ci-core` is the leaf dependency with zero external deps — everything else depends on it.

---

## 4. Graph Engine

This is the most critical component. The C version stores graph data in SQLite and queries it via SQL. The Rust version keeps the graph **entirely in memory** in a structure optimized for the access patterns of code intelligence queries.

### 4.1 Two-Phase Design

**Build Phase (MutableGraph)**: During indexing, nodes and edges are appended concurrently from multiple threads. Uses append-only vectors with atomic ID counters. Optimized for fast writes, not reads.

**Query Phase (FrozenGraph)**: After indexing completes, the mutable graph is "frozen" into an immutable, cache-optimized representation. This is what serves all queries.

```
Index files ──▶ MutableGraph ──freeze()──▶ FrozenGraph ──▶ Serve queries
                                               │
                                          rkyv serialize
                                               │
                                               ▼
                                          graph.bin (disk)
                                               │
                                          mmap on next startup
                                               │
                                               ▼
                                          FrozenGraph (instant load)
```

**Freeze Memory Budget**: The `freeze()` operation must construct CSR arrays and secondary indexes from the MutableGraph. At Linux kernel scale (2.1M nodes, ~10M edges), both representations temporarily coexist in memory. To keep peak RSS under control on 16 GB machines:

1. **Streaming CSR construction** — sort edges by source ID in-place within MutableGraph's edge vector (avoiding a copy), then build offset arrays by scanning once. The MutableGraph's node vectors are consumed (moved, not copied) into the FrozenGraph's SoA layout.
2. **Phased teardown** — drop the MutableGraph's edge storage immediately after CSR construction, before building FST and bitmap indexes. This bounds peak overhead to ~1.3x the final FrozenGraph size rather than 2x.
3. **Spill-to-disk fallback** — if available memory drops below a configurable threshold during freeze, spill the sorted edge list to a temporary file and stream it back during CSR offset construction. This trades ~2 seconds of I/O for avoiding OOM on constrained machines.

### 4.2 Data Layout

**Struct-of-Arrays (SoA)** instead of Array-of-Structs. Graph traversals and filtered scans typically touch only 1-2 fields per node (label, name). SoA ensures those hot fields are packed in contiguous memory, maximizing cache utilization.

- **Node hot data** (16 bytes/node): label, name, qualified_name, project — touched on every query
- **Node cold data** (16 bytes/node): file_path, start_line, end_line, properties — touched only when materializing results
- **At 2.1M nodes**: hot data = 34 MB (fits in L3 cache), cold data = 34 MB (paged in on demand)

### 4.3 CSR Adjacency

**Compressed Sparse Row** format for graph topology. Two CSR structures: one for outbound edges (forward), one for inbound edges (reverse).

Getting all neighbors of a node is two array lookups + a sequential slice read. No hash tables, no pointer chasing. This makes BFS a tight loop over contiguous memory — the reason we target < 100 microsecond traversals.

Degree (number of connections) is computed in O(1) from the offset array difference.

### 4.4 String Interning

All strings (names, qualified names, file paths, labels) are interned into a single contiguous buffer. References are 4-byte handles instead of pointers. This means:

- String equality is a 4-byte integer comparison, not `strcmp`
- Deduplication is automatic (the same file path referenced by 100 nodes is stored once)
- The entire string table for the Linux kernel is ~60 MB

**Concurrency**: During indexing (Pass 3), many threads intern strings simultaneously. A single-lock interner would become a bottleneck. The interner uses **sharded buckets** (16 shards, selected by hash prefix) with per-shard locks. Each shard owns a portion of the contiguous buffer. After indexing, shards are compacted into a single buffer for the FrozenGraph. This eliminates contention while preserving the zero-copy property of interned handles.

### 4.5 Secondary Indexes

| Index | Structure | Purpose |
|-------|-----------|---------|
| QN → NodeId | HashMap | O(1) qualified name lookup (hottest path for call resolution) |
| Label → Nodes | RoaringBitmap per label | Instant label filtering, fast set intersection |
| Name search | FST (Finite State Transducer) | Sub-millisecond regex/prefix/fuzzy search over all symbol names |
| File → Nodes | HashMap | Find all symbols defined in a file |

The **FST index** is particularly important: it enables regex search over 1.5M symbol names in < 1ms by intersecting a regex automaton with the pre-built transducer. This replaces the C version's approach of SQL LIKE pre-filtering + regex post-filtering.

---

## 5. Communication Layer

### 5.1 Design Principles

1. **MCP stdio is mandatory** — it's the only transport all 10+ AI agents support today
2. **The engine must be transport-agnostic** — the core library has no I/O, no serialization
3. **Multi-client sharing is essential** — multiple agents working on the same repo should share one index
4. **Large results need pagination** — not streaming (MCP stdio doesn't support streaming)

### 5.2 Phased Rollout

**Phase 1 — Single Process (MCP stdio, in-process engine)**

The simplest deployment model, matching the current C version. Agents launch the binary as a subprocess, communicate via JSON-RPC over stdin/stdout. The engine runs in the same process.

This is the minimum viable product. It works with every existing agent.

**Phase 2 — Daemon Mode**

A background daemon process owns the index. The MCP stdio binary becomes a thin adapter that connects to the daemon via Unix domain socket. Multiple agents share one daemon, one index.

```
Agent A ──stdio──▶ ci-mcp-stdio ──UDS──▶ ci-daemon (owns graph)
Agent B ──stdio──▶ ci-mcp-stdio ──UDS──┘
```

The adapter auto-detects whether a daemon is running. If not, it falls back to in-process mode. Backward-compatible — agents see no difference.

**Phase 3 — Streamable HTTP**

For agents that support MCP's Streamable HTTP transport (Codex, Cursor, VS Code), the daemon exposes an HTTP endpoint with SSE streaming. Large results (architecture overviews, broad searches) stream incrementally instead of buffering.

**Phase 4 — Library Embedding**

Publish `ci-core` + `ci-graph` + `ci-pipeline` as a crate. Agents written in Rust link directly. Generate C headers via `cbindgen` for non-Rust consumers.

### 5.3 Internal IPC (Daemon Mode)

Between the MCP stdio adapter and the daemon:

- **Transport**: Unix domain socket on macOS/Linux; **Named Pipes** (`\\.\pipe\ci-daemon`) on Windows. The adapter selects the appropriate transport at compile time via `cfg(target_os)`. Both provide near-zero latency for local communication.
- **Framing**: Length-prefixed messages (4-byte little-endian length + payload)
- **Serialization**: MessagePack (30-50% smaller than JSON, faster to encode/decode, schema-flexible)
- **Backpressure**: Bounded channels — if the adapter can't consume fast enough, the daemon naturally pauses

### 5.4 Pagination for Large Results

MCP has no built-in streaming for stdio. Instead, tools return paginated results with cursors:

```json
{
  "content": [{"type": "text", "text": "...first 200 nodes..."}],
  "_meta": {"cursor": "page2", "hasMore": true, "total": 5000}
}
```

Agents call the tool again with `{"cursor": "page2"}` to fetch the next page. This works within MCP's existing semantics.

---

## 6. Indexing Pipeline

### 6.1 Multi-Pass Architecture

The pipeline processes source files through sequential passes, where each pass builds on the results of previous passes. Within each pass, files are processed in parallel.

```
Pass 1: Discovery      Walk filesystem, apply gitignore, detect languages
Pass 2: Structure      Create Project/Package/Directory/File nodes
Pass 3: Definitions    Extract Function/Class/Method/Interface/Type nodes
                       Build function registry (QN → NodeId) for call resolution
Pass 4: Imports        Extract import statements, resolve to qualified names
Pass 5: Calls+Usages  Resolve function calls via registry, extract type usages
Pass 6: Relations      INHERITS, IMPLEMENTS, DECORATES edges
Pass 7: Semantic       Community detection (Louvain), HTTP route correlation
Pass 8: Metadata       Test detection, git history, environment variable scanning
Pass 9: Freeze         Sort edges, build CSR, build FST indexes, serialize to disk
```

### 6.2 Parallelism Model

- **Rayon** (work-stealing thread pool) for CPU-bound file processing within each pass
- **Tokio** (async runtime) for I/O: MCP event loop, file watching, git operations
- These two runtimes coexist without conflict — Rayon for compute, Tokio for I/O

Each pass produces owned results (vectors of nodes/edges) that are merged into the graph sequentially between passes. No shared mutable state during parallel execution — Rust's ownership model guarantees this at compile time.

### 6.3 Memory Management During Indexing

- **Thread-local bump arenas** (bumpalo): Each file's AST is allocated in a per-thread arena. After extraction, the arena is reset in bulk — zero per-object deallocation overhead, zero fragmentation. **Critical**: tree-sitter `Tree` objects hold references into the arena — extraction must complete and all tree-sitter nodes must be dropped *before* the arena resets. The extraction function enforces this by taking ownership of the `Tree` and dropping it before returning.
- **mimalloc** as global allocator: Thread-local caches, reduced fragmentation for long-running processes
- **Memory-mapped source files**: Large files read via mmap (OS manages page caching), small files read into buffers

### 6.4 Incremental Indexing

On file changes:

1. Compare filesystem state (mtime + size) against stored file state database (redb)
2. Verify changed files with content hash (handles clock skew)
3. Delete all nodes/edges originating from changed files
4. Re-run passes 3-8 on changed files only
5. Rebuild CSR and FST indexes (~200ms)
6. Persist updated graph to disk

**Key enabler**: Each node and edge tracks which source file it originated from. This allows targeted deletion without a full graph rebuild.

**Delta overlay for hot-path updates**: If the full CSR rebuild in step 5 exceeds 200ms at scale (likely for graphs > 5M edges), a **delta overlay** defers the rebuild. New/modified edges accumulate in a small unsorted buffer alongside the frozen CSR. Queries check both: CSR for the bulk of the graph, overlay for recent changes. When the overlay exceeds a threshold (e.g., 10K edges or 5 seconds idle), a background compaction merges it into the CSR. This keeps incremental re-index latency proportional to the number of changed files, not the total graph size.

### 6.5 Call Resolution Strategy

Call resolution (connecting function call sites to their definitions) is the most complex part of the pipeline. Strategies in priority order:

1. **Import-aware lookup** — follow the import chain to resolve the qualified name (highest confidence)
2. **Same-module lookup** — if unresolved, check the same package/module
3. **Registry exact match** — O(1) hash lookup by qualified name
4. **Fuzzy suffix match** — match by bare function name as fallback (lowest confidence)

Each resolved CALLS edge carries a confidence score (0.0-1.0) reflecting which strategy produced it.

**Optional: LSIF/SCIP bootstrapping** — For projects that generate LSIF or SCIP data (common in Go, TypeScript, Java CI pipelines), the pipeline can ingest pre-computed symbol resolution as a high-confidence input to anchor call edges. This runs as an optional pre-pass before strategy 1, producing confidence 0.95+ edges that the heuristic strategies won't override. This is not required for correctness but significantly improves precision for monorepos with complex re-exports.

---

## 7. Persistence

### 7.1 Zero-Copy Serialization (rkyv)

The FrozenGraph is serialized with `rkyv`, which produces a byte buffer that IS the in-memory representation. Loading is just `mmap` — no parsing, no deserialization, no allocation.

- **Cold start**: Open file + mmap system call = microseconds
- **Warm start** (file in page cache): Essentially instant
- **The OS manages memory**: Only accessed pages are loaded into RAM. A query touching 1% of the graph loads ~5 MB

### 7.2 File Layout

```
~/.cache/codebase-intelligence/<project>/
  graph.bin         rkyv-serialized FrozenGraph
  file_state.redb   File hash/mtime database for incremental indexing
  config.toml       Project-specific indexing configuration
```

### 7.3 Crash Safety

Persistence uses atomic file replacement: write to `graph.bin.tmp`, then `rename()` over `graph.bin`. On POSIX, rename is atomic — the file is always either the old valid graph or the new valid graph, never a corrupt intermediate state.

---

## 8. Query Capabilities

### 8.1 Tool Surface (14 MCP Tools)

These match the existing C version for drop-in compatibility:

| Tool | Description | Key Operation |
|------|-------------|---------------|
| `index_repository` | Index or re-index a codebase | Full/incremental pipeline run |
| `list_projects` | List all indexed projects | Metadata scan |
| `delete_project` | Remove a project's index | File deletion + cache eviction |
| `index_status` | Check indexing progress | Pipeline state query |
| `search_graph` | Find symbols by name/label/file/degree | FST search + bitmap filtering |
| `trace_call_path` | Who calls X? What does X call? | CSR BFS traversal |
| `detect_changes` | Map git diff to affected symbols | File → nodes → edge traversal |
| `query_graph` | Execute Cypher-like queries | Parse → plan → CSR execution |
| `get_graph_schema` | Introspect node/edge types and counts | Bitmap cardinalities |
| `get_code_snippet` | Retrieve source code by qualified name | QN → file + lines → read |
| `get_architecture` | High-level codebase overview | Pre-computed on freeze |
| `search_code` | Grep-like text search across files | ripgrep engine (SIMD-accelerated) |
| `manage_adr` | Architecture Decision Records CRUD | Key-value store |
| `ingest_traces` | Validate HTTP call edges with runtime data | Edge confidence update |

### 8.2 Query Language

A Cypher-like syntax for the `query_graph` tool:

```cypher
MATCH (f:Function)-[:CALLS*1..3]->(g:Function)
WHERE f.name =~ "parse.*" AND g.label = "Function"
RETURN f.name, g.name, g.file_path
ORDER BY g.name
LIMIT 50
```

Supported: MATCH with node/edge patterns, variable-length paths, WHERE with AND/OR/NOT and comparisons (=, <>, =~, CONTAINS, IN), RETURN with aggregates (COUNT, SUM, AVG), ORDER BY, LIMIT, SKIP.

Not supported (intentionally): Mutations, WITH clause, OPTIONAL MATCH, subqueries. This is a read-only query engine for code exploration.

### 8.3 Query Optimization

The query engine includes a simple planner that:

1. Extracts literal prefixes from regex patterns to use FST index scans instead of full scans
2. Uses RoaringBitmap intersections for multi-predicate filtering (label AND file pattern)
3. Pushes LIMIT down to avoid materializing unused results
4. Estimates cardinality to choose traversal direction (start from the smaller set)

---

## 9. Multi-Language Support

### 9.1 Parser Architecture

All 66 languages use tree-sitter for AST parsing. Each language has a **spec** — a declarative configuration of which AST node types correspond to functions, classes, calls, imports, etc. This avoids per-language procedural code.

Language tiers by extraction quality:

| Tier | Coverage | Languages |
|------|----------|-----------|
| Excellent (90%+) | Full extraction: defs, calls, imports, types, semantic edges | Go, Python, TypeScript, Java, C++, Rust, C, Kotlin, Ruby, PHP, C# |
| Good (75-89%) | Most features working | JavaScript, Scala, Dart, Swift, Perl, Elixir, Erlang |
| Functional (<75%) | Basic def/call extraction | OCaml, Haskell, Clojure, F#, Julia, and 40+ others |

### 9.2 Grammar Loading Strategy

Including all 66 tree-sitter grammars as static dependencies inflates compile time and binary size (~150 MB), undermining the "embeddable" goal. Grammars are organized into **loadable tiers**:

- **Tier 1 (always compiled in)**: The 11 Excellent-tier languages. These cover the vast majority of real-world usage and are the only grammars needed for the embeddable library.
- **Tier 2-3 (dynamic shared objects)**: Remaining grammars are compiled as `.so`/`.dylib`/`.dll` files, loaded on demand via `libloading` when a project contains files of that language. Grammars are cached in `~/.cache/codebase-intelligence/grammars/`.
- **Feature flags**: The workspace Cargo.toml exposes `lang-tier1` (default), `lang-tier2`, `lang-all` features. CI builds test all tiers; release binaries ship with tier 1 compiled in and tier 2-3 as sidecar downloads.

This keeps the core binary under 30 MB while still supporting all 66 languages.

### 9.3 Language Spec System

Instead of writing extraction code per language, languages are described declaratively:

```
Language: Python
  function_nodes: [function_definition]
  class_nodes: [class_definition]
  call_nodes: [call]
  import_nodes: [import_statement, import_from_statement]
  string_nodes: [string]
  comment_nodes: [comment]
```

The extraction engine interprets these specs uniformly. Language-specific behavior (e.g., Python decorators, Go receivers, Rust traits) is handled via optional spec extensions.

---

## 10. Observability

- **Structured logging** via `tracing` crate — spans for indexing passes, query execution, transport events
- **Metrics** (in daemon mode): index size, query latency histogram, cache hit rates, memory usage
- **Diagnostic HTTP endpoint** (daemon mode): `/debug` serves current state, active queries, index statistics
- **CLI introspection**: `ci status` shows running daemon, indexed projects, resource usage

---

## 11. Implementation Plan

### Phase 1: Core Graph Engine
**Deliverable**: `ci-core` and `ci-graph` crates with in-memory graph, CSR builder, BFS, persistence

- Define core types (NodeId, EdgeType, NodeLabel, InternedStr)
- Implement StringInterner (contiguous buffer + dedup hash map)
- Implement MutableGraph (append-only SoA with atomic counters)
- Implement FrozenGraph (CSR from sorted edges, neighbor access, BFS)
- Implement rkyv serialization + mmap loading
- Build FST index over names and qualified names
- Unit tests: graph ops, CSR correctness, serialization round-trip, BFS
- Benchmarks: graph construction, BFS latency, FST search latency

### Phase 2: File Discovery & Parsing
**Deliverable**: `ci-discover` and `ci-parser` crates

- File walker with hierarchical gitignore support
- Language detection by file extension
- tree-sitter integration with thread-safe parser pooling
- Language spec system (declarative extraction configs)
- Implement extraction for Tier 1 languages (Go, Python, TypeScript, Rust, Java, C/C++)
- Benchmark: extraction throughput per language vs. C version

### Phase 3: Indexing Pipeline
**Deliverable**: `ci-pipeline` crate, end-to-end indexing of real projects

- Multi-pass orchestrator with Rayon parallelism
- Function registry + call resolution (4-strategy priority chain)
- Import resolution pass
- Semantic edges (inherits, implements, tests)
- Incremental indexing (file state tracking in redb, targeted re-extraction)
- Integration test: index ripgrep, verify node/edge counts

### Phase 4: Query Engine
**Deliverable**: `ci-query` crate

- Cypher lexer and parser (hand-rolled for speed)
- Query planner with index selection
- Executor over FrozenGraph (CSR traversal, FST search, bitmap filtering)
- Benchmark: query latency vs. C version's SQLite-backed queries

### Phase 5: MCP Server
**Deliverable**: `ci-protocol`, `ci-server`, `ci-cli` crates — functional MCP server

- MCP stdio transport (JSON-RPC 2.0 over stdin/stdout)
- All 14 tool handlers
- Watcher for git-based auto-reindex
- CLI: install, uninstall, config, single-shot tool invocation
- Compatibility test: run with Claude Code, verify all tools work

### Phase 6: Daemon & Advanced Transports
**Deliverable**: Multi-client daemon, HTTP/SSE, remaining languages

- Daemon mode with Unix domain socket IPC
- Auto-discovery: stdio adapter connects to daemon if running
- MCP Streamable HTTP transport
- Port remaining 50+ language grammars
- C FFI bindings via cbindgen
- Stress test: Linux kernel — target < 2 min index, < 100us queries, < 500 MB memory

---

## 12. Risk Assessment

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| rkyv breaking changes | Persistence format invalidated | Medium | Pin version, wrap behind trait, migration tool |
| 66 tree-sitter grammars inflate build time | Slow CI, slow development | High | Feature flags per language tier, prebuilt grammar objects |
| CSR rebuild on every incremental update | Slow re-index for small changes | Medium | Delta overlay buffer: base CSR + unsorted delta, compact periodically |
| mmap safety (external file modification) | Undefined behavior | Low | File locking (flock), magic number + checksum header |
| Daemon process management complexity | Hard to debug, orphan processes | Medium | Phase 1 is in-process (no daemon). Daemon uses well-known PID file + health checks |
| Agent MCP incompatibilities | Tools don't work with some agents | Low | Comprehensive compatibility test suite across all 10 agents |
| Freeze OOM on constrained machines | Crash during index build on 16 GB machines | Medium | Streaming CSR construction with phased teardown; spill-to-disk fallback (§4.1) |
| Call resolution ambiguity in large monorepos | Low-precision CALLS edges degrade query usefulness | Medium | Optional LSIF/SCIP ingestion for high-confidence anchoring (§6.5); expose confidence scores in query results so agents can filter |
| String interner contention during indexing | Reduced parallelism in Pass 3 | Medium | Sharded interner with 16 lock-striped buckets (§4.4) |
| Windows daemon IPC | UDS not reliable across all Windows versions | Low | Named Pipes as Windows transport (§5.3); UDS only on POSIX |

---

## 13. Success Criteria

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Index throughput | Linux kernel < 2 min | Benchmark suite |
| Query latency (BFS) | < 100 microseconds | Benchmark suite |
| Query latency (name search) | < 1 millisecond | Benchmark suite |
| Graph load time | < 10 milliseconds | Cold start benchmark |
| Memory (steady state) | < 500 MB at 2.1M nodes | RSS measurement |
| Token efficiency | 99%+ reduction vs grep-based exploration | Compare tool output sizes |
| Agent compatibility | Works with Claude Code, Codex, Gemini, Cursor, Zed | Integration tests |
| Incremental re-index | < 500 ms for typical edits | Benchmark with realistic diffs |
