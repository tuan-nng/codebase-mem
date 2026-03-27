# Codebase Intelligence Engine — Implementation Plan

## Dependency Order

```
Epic 1 (Memory Bedrock)
    ├── Epic 2 (Parsing Factory)     ─┐
    │                                 ├── Epic 3 (Indexing Pipeline) ─┐
    └── Epic 4 (Query Engine)        ─┘                               ├── Epic 5 (Protocol & MCP) ── Epic 6 (Daemon & Optimization)
```

Epics 2 and 4 can proceed in parallel once Epic 1 is complete.

---

## Epic 1: Memory Bedrock (`ci-core`, `ci-graph`)

**Goal**: Build the high-performance in-memory containers that hold the codebase graph.

**Complexity**: Medium | **Risk**: High (crucial for all downstream performance targets)

### Tasks

#### [E1-1] Define core types: NodeId, EdgeType, InternedStr
In `ci-core`: implement `NodeId` as a newtype over `u32` with `From`/`Into` impls, `EdgeType` as an enum covering all relationship kinds (`CALLS`, `IMPORTS`, `INHERITS`, etc.), and `InternedStr` as a 4-byte handle with `Debug`/`Display` forwarding through the interner. Zero external deps — this is the leaf crate.

#### [E1-2] Implement sharded StringInterner
Build a 16-shard lock-striped string interner (shard selected by hash prefix). Each shard owns a contiguous buffer segment and a dedup `HashMap`. After indexing, compact all shards into a single buffer for `FrozenGraph`. Handles concurrent `intern()` calls from Rayon threads without global lock contention.

**Contention escape hatch**: If Pass 3 benchmarks show measurable lock contention at Linux kernel scale, switch to **thread-local intern buffers**: each Rayon worker thread interns into a fully local `(buffer, HashMap)` with zero synchronization. At the end of the pass, buffers are merged sequentially — duplicate strings across threads are deduplicated during merge, and handles are remapped. This is slightly more memory-intensive (duplicate strings live in multiple thread buffers until merge) but is effectively zero-latency during parallel indexing. The two strategies share the same public API so the switch is internal to `ci-core`.

#### [E1-3] Build MutableGraph with concurrent append
Implement `MutableGraph` in `ci-graph` using append-only SoA `Vec`s with atomic `NodeId`/`EdgeId` counters. Node hot and cold data stored in separate arrays. Support concurrent node and edge appends from Rayon worker threads via per-array `RwLock`s or lock-free append. No reads during build phase.

#### [E1-4] Implement FrozenGraph with CSR adjacency
Implement `FrozenGraph` in `ci-graph`. `freeze()` sorts the `MutableGraph`'s edge vector in-place by source `NodeId` (radix sort), builds forward and reverse CSR offset arrays, then moves (not copies) node SoA arrays. **Phased teardown**: drop edge `Vec` immediately after CSR construction, before building secondary indexes, bounding peak memory to ~1.3x the final `FrozenGraph` size.

#### [E1-5] Build secondary indexes: RoaringBitmap and FST
After CSR construction, build:
- `RoaringBitmap` per `NodeLabel` for instant label filtering
- `HashMap<InternedStr, NodeId>` for QN → NodeId lookup (hottest query path)
- `HashMap<InternedStr, Vec<NodeId>>` for File → Nodes
- FST (`fst` crate) over all symbol names for sub-millisecond regex/prefix/fuzzy search

All built during `freeze()`, stored in `FrozenGraph`.

#### [E1-6] Integrate rkyv for zero-copy persistence and mmap loading
Derive `rkyv` `Archive`/`Serialize`/`Deserialize` on `FrozenGraph` and all its fields. Implement `save()`: write to `graph.bin.tmp` then atomic `rename()`. Implement `load()`: open `graph.bin`, mmap it, validate magic number + checksum header, return an mmap-backed `FrozenGraph` with zero allocation. Add spill-to-disk fallback path in `freeze()` for machines with low available memory.

#### [E1-7] Checkpoint: benchmark 1M-node build, freeze, and save
Write a criterion benchmark that: (1) generates 1M synthetic nodes and 5M edges, (2) builds `MutableGraph`, (3) calls `freeze()`, (4) saves to disk.

Targets:
| Operation | Target |
|-----------|--------|
| Full build + freeze + save | < 5 seconds on 16-core machine |
| BFS from random node | < 100 µs |
| mmap cold load | < 10 ms |
| FST prefix search over 1M names | < 1 ms |

---

## Epic 2: Parsing Factory (`ci-parser`, `ci-discover`)

**Goal**: Turn raw source code into structured definitions.

**Complexity**: Medium | **Risk**: Medium

**Blocked by**: Epic 1

### Tasks

#### [E2-1] Implement file discovery with gitignore support
In `ci-discover`: use the `ignore` crate to walk the filesystem respecting `.gitignore`, `.git/info/exclude`, and global excludes. Detect language by file extension using a static trie. Emit `(path, language)` pairs. Expose a parallel iterator (`rayon::ParallelBridge`) for the pipeline to consume.

#### [E2-2] Build thread-safe tree-sitter parser pool
`tree-sitter::Parser` is `!Send`. Use a thread-local pool: each Rayon worker thread holds a `HashMap<Language, Parser>` initialized lazily on first use. Parsers are reused across files on the same thread. Grammar `Language` objects (which are `Send`) are created once at startup in a global registry, then referenced per thread-local pool.

#### [E2-3] Implement declarative language spec system
Define a `LanguageSpec` struct (TOML/code-embedded) mapping tree-sitter AST node type names to engine node kinds: `function_nodes`, `class_nodes`, `call_nodes`, `import_nodes`, etc. Implement optional spec extensions for language-specific behavior (Python decorators, Go method receivers, Rust trait impls). The extraction engine interprets specs uniformly — no per-language procedural code.

#### [E2-4] Implement Tier 1 language extraction (C, C++, Go, Python, TypeScript, Rust, Java)
Write `LanguageSpec` definitions and required spec extensions for the 7 Tier 1 languages. For each: extract function/class/method/interface/type definitions with qualified names, extract import statements, extract call sites with call target names. **Critical**: drop the tree-sitter `Tree` before arena reset. Validate against golden fixtures (known symbol counts from real repos).

---

## Epic 3: Indexing Pipeline (`ci-pipeline`)

**Goal**: Parallelize extraction and resolve the connective tissue of the code.

**Complexity**: High | **Risk**: High (call resolution logic complexity)

**Blocked by**: Epic 1, Epic 2

### Tasks

#### [E3-1] Implement Rayon-based multi-pass pipeline orchestrator
In `ci-pipeline`: implement the 9-pass orchestrator. Each pass receives the file list and `MutableGraph` from the previous pass. Within each pass, use `rayon::par_iter()` over files. Between passes, merge owned result `Vec`s into the graph sequentially (no shared mutable state during parallel execution). Track pass timing for observability.

**Pass sequence**:
```
Pass 1: Discovery      Walk filesystem, apply gitignore, detect languages
Pass 2: Structure      Create Project/Package/Directory/File nodes
Pass 3: Definitions    Extract Function/Class/Method/Interface/Type nodes
                       Build function registry (QN → NodeId) for call resolution
Pass 4: Imports        Extract import statements, resolve to qualified names
Pass 5: Calls+Usages  Resolve function calls via registry, extract type usages
Pass 6: Relations      INHERITS, IMPLEMENTS, DECORATES edges
Pass 7: Semantic       [BACKGROUND] Community detection (Louvain), HTTP route correlation
Pass 8: Metadata       Test detection, git history, environment variable scanning
Pass 9: Freeze         Sort edges, build CSR, build FST indexes, serialize to disk
```

#### [E3-2] Implement Pass 3 (definitions) and Pass 4 (imports) with QN registry
Pass 3: extract all `Function`/`Class`/`Method`/`Interface`/`Type` nodes and populate a QN → NodeId registry (`DashMap` for concurrent writes). Pass 4: extract import statements, resolve to qualified names using the registry. Each node and edge records its originating file path for incremental deletion. Registry is frozen to a plain `HashMap` after Pass 3 completes.

#### [E3-3] Implement Pass 5 (call resolution) with 4-strategy chain
Implement the call resolution chain in priority order:

| Strategy | Mechanism | Confidence |
|----------|-----------|-----------|
| LSIF/SCIP pre-pass (optional) | Ingest pre-computed symbol data | 0.95 |
| Import-aware lookup | Follow import chain to resolve QN | 0.90 |
| Same-module lookup | Check same package/module | 0.75 |
| Registry exact match | O(1) HashMap lookup by QN | 0.70 |
| Fuzzy suffix match | Bare function name fallback | 0.40 |

Each `CALLS` edge records which strategy produced it and its confidence score.

**High-noise guard**: If a registry lookup (strategy 3 or 4) matches more than 10 candidate definitions for the same bare name (e.g., `log()`, `init()`, `new()` in a large monorepo), the edge is flagged `noise = true` and assigned confidence 0.1 rather than being fanned out to all 200 candidates. The edge still records the call site and the matched name, but query tools filter high-noise edges by default and only include them when the caller explicitly opts in (e.g., `search_graph` with `include_noise: true`). This prevents `trace_call_path` from returning 200-node call graphs that degrade AI agent reasoning.

#### [E3-4] Integrate bumpalo arenas for per-file AST memory
Configure thread-local `bumpalo::Bump` arenas. Each file's tree-sitter parse allocates intermediate extraction structs in the thread-local arena. After extraction completes and the tree-sitter `Tree` is dropped, reset the arena in bulk. Validate no references into the arena escape extraction (enforced by Rust lifetimes). Add memory usage tracking per pass.

#### [E3-5] Implement incremental indexing with file state tracking and delta overlay
Use `redb` to store per-file `(path → mtime, size, content_hash)`. On change detection:
1. Compare mtime+size (fast path)
2. Verify with xxhash if mtime matches (handles clock skew)
3. Delete all nodes/edges with `file == changed_path`
4. Re-run passes 3-8 on changed files only
5. Rebuild CSR and FST indexes (~200ms)

**Delta overlay**: New/modified edges accumulate in a small unsorted buffer alongside the frozen CSR. Queries check both. When the overlay exceeds 10K edges or 5 seconds of idle, a background task compacts it into the CSR. This keeps incremental latency proportional to changed files, not total graph size.

---

## Epic 4: Query & Search Engine (`ci-query`)

**Goal**: Make the graph useful for AI agents.

**Complexity**: Medium | **Risk**: Low

**Blocked by**: Epic 1 (can run in parallel with Epics 2 & 3)

### Tasks

#### [E4-1] Integrate FST for sub-millisecond symbol name search
During `freeze()`, build an FST (`fst` crate) over all interned symbol names in lexicographic order, mapping name → `NodeId`. Implement:
- `prefix_search(prefix)` — all symbols with given prefix
- `regex_search(pattern)` — using `regex-automata` to intersect a regex DFA with the FST
- `fuzzy_search(term, distance)` — using Levenshtein automata

Target: regex over 1.5M names in < 1 ms.

#### [E4-2] Build Cypher-lite lexer and parser
Hand-roll a lexer and recursive-descent parser for the query language subset:
- `MATCH` with node/edge patterns and variable-length paths (`*min..max`)
- `WHERE` with `AND`/`OR`/`NOT` and comparisons (`=`, `<>`, `=~`, `CONTAINS`, `IN`)
- `RETURN` with `COUNT`/`SUM`/`AVG` aggregates
- `ORDER BY`, `LIMIT`, `SKIP`

Produce an AST (algebraic types). Error messages include position and a suggestion. No external parser combinator dependency.

#### [E4-3] Implement query planner with index-first strategy
Implement a query planner that:
1. Extracts literal prefixes from regex patterns → FST scans instead of full graph scans
2. Selects `RoaringBitmap` intersection for multi-label/file filters
3. Pushes `LIMIT` down to avoid materializing unused results
4. Estimates cardinality from bitmap sizes to choose traversal direction (start from smaller set)

Produces a physical plan over `FrozenGraph` primitives.

**Pass 7 — background semantic pass**: Community detection (Louvain) on a 2.1M-node graph can take 30–120 seconds, which is unacceptable if it blocks the initial query-ready state. Pass 7 is split into two tiers:
- **Synchronous stub**: On freeze, pre-compute a lightweight proxy for architecture overviews — top-level package/directory groupings derived from file paths alone. This takes < 1 second and makes `get_architecture` usable immediately.
- **Background job**: Full Louvain community detection runs in a Tokio `spawn_blocking` task after the graph is serving queries. When it completes, it updates a dedicated `communities` field in the persisted graph without triggering a full CSR rebuild. Tools that use community data (`get_architecture` with `depth=semantic`) check whether the background job has completed and return a `"communities": "pending"` annotation if not.

#### [E4-4] Implement query executor with cursor pagination
Execute physical query plans: BFS/DFS over CSR for path patterns, bitmap intersections for filter predicates, FST traversal for name patterns, aggregation operators, sort and limit. Implement cursor-based pagination: execution can be suspended at a stable checkpoint and resumed with a cursor token. All execution is read-only over the immutable `FrozenGraph`.

---

## Epic 5: Protocol & MCP Integration (`ci-protocol`, `ci-server`, `ci-cli`)

**Goal**: Connect the engine to the outside world.

**Complexity**: Medium | **Risk**: Low

**Blocked by**: Epic 3, Epic 4

### Tasks

#### [E5-1] Implement MCP stdio transport (JSON-RPC 2.0)
In `ci-protocol`: read newline-delimited JSON-RPC 2.0 messages from stdin, dispatch to tool handlers, write responses to stdout. Run the I/O loop on Tokio. Handle batch requests, notifications, and error responses per the JSON-RPC spec. Bridge to Rayon compute threads via `tokio::task::spawn_blocking`.

#### [E5-2] Implement all 14 MCP tool handlers

| Tool | Key Operation |
|------|---------------|
| `index_repository` | Full/incremental pipeline run |
| `list_projects` | Metadata scan |
| `delete_project` | File deletion + cache eviction |
| `index_status` | Pipeline state query |
| `search_graph` | FST search + bitmap filtering |
| `trace_call_path` | CSR BFS traversal |
| `detect_changes` | File → nodes → edge traversal |
| `query_graph` | Parse → plan → CSR execution |
| `get_graph_schema` | Bitmap cardinalities |
| `get_code_snippet` | QN → file + lines → read |
| `get_architecture` | Pre-computed on freeze |
| `search_code` | ripgrep engine (SIMD-accelerated) |
| `manage_adr` | Key-value store |
| `ingest_traces` | Edge confidence update |

Each handler validates its input schema, delegates to the query engine or pipeline, and formats output as MCP content with cursor pagination where needed.

#### [E5-3] Integrate file watcher for auto-reindex
Use the `notify` crate to watch the indexed project root. On file change events, debounce for 200ms (coalesce rapid saves), then trigger incremental reindex for the changed file set. Run the watcher on a Tokio task. Connect to the delta overlay in the pipeline so small edits don't trigger full CSR rebuilds.

#### [E5-4] Build CLI (`ci-cli`): install, uninstall, config, status
Implement the `ci` CLI binary using `clap`:
- `ci install` — register MCP server with agent config
- `ci uninstall` — remove registration
- `ci config` — show/edit project config
- `ci status` — running daemon, indexed projects, resource usage
- `ci index <path>` — single-shot index trigger
- `ci query '<cypher>'` — single-shot query

#### [E5-5] MCP compatibility test suite against real agents
Write integration tests that spawn the MCP server as a subprocess and exercise all 14 tools via JSON-RPC. Verify: correct schema, pagination cursors, error responses, and tool output format. Run against Claude Code as the primary target. Gate for Phase 5 completion.

---

## Epic 6: Daemon & Optimization

**Goal**: Multi-client support and final performance polish.

**Complexity**: Low-Medium | **Risk**: Medium (cross-platform IPC)

**Blocked by**: Epic 5

### Tasks

#### [E6-1] Implement daemon IPC (UDS on POSIX, Named Pipes on Windows)
In `ci-server` daemon mode: listen on a Unix domain socket (POSIX) or Named Pipe (`\\.\pipe\ci-daemon`) on Windows, selected via `cfg(target_os)`. Frame messages as 4-byte LE length-prefix + MessagePack payload. Handle concurrent connections with Tokio tasks. Manage lifecycle via a PID file at `~/.cache/codebase-intelligence/daemon.pid` with health check. Support graceful shutdown on `SIGTERM`.

#### [E6-2] Build thin MCP stdio adapter with daemon auto-discovery
The `ci-mcp-stdio` binary checks for a running daemon (PID file + health ping) on startup. If found, connects via UDS/Named Pipe and proxies JSON-RPC messages. If not found, falls back to in-process mode. Agents see no difference. Backward-compatible with Phase 1 deployment.

```
Agent A ──stdio──▶ ci-mcp-stdio ──UDS──▶ ci-daemon (owns graph)
Agent B ──stdio──▶ ci-mcp-stdio ──UDS──┘
```

#### [E6-3] Implement dynamic grammar loading for Tier 2/3 languages
Compile Tier 2 (JavaScript, Scala, Swift, Elixir, Erlang, Dart, Perl, etc.) and Tier 3 (40+ functional/niche languages) grammars as separate `.so`/`.dylib`/`.dll` shared objects. Use `libloading` to load them on demand. Cache loaded grammars in memory. Provide `ci grammars install --tier=2` to download prebuilt artifacts. Core binary stays under 30 MB with only Tier 1 compiled in.

**Feature flags**:
- `lang-tier1` (default) — 11 Excellent-tier languages compiled in
- `lang-tier2` — Tier 2 as sidecar .so files
- `lang-all` — all 66 languages, for CI

#### [E6-4] Implement MCP Streamable HTTP transport with SSE
Add an HTTP server (`axum`) to the daemon exposing the MCP Streamable HTTP transport. Large results stream via Server-Sent Events. Implement the `/mcp` endpoint per the MCP spec. Target agents: Codex, Cursor, VS Code. Add `/debug` endpoint: active queries, index stats, memory usage.

#### [E6-5] Linux kernel stress test: validate all success criteria
Run the full pipeline against the Linux kernel source (~28M LOC). Validate:

| Metric | Target |
|--------|--------|
| Index throughput | < 2 minutes |
| Query latency (BFS) | < 100 µs |
| Query latency (name search) | < 1 ms |
| Graph load time | < 10 ms |
| Memory (steady state) | < 500 MB at 2.1M nodes |
| Incremental re-index | < 500 ms for a 10-file diff |

Document results and any tuning applied.

---

## Summary

| Epic | Crates | Complexity | Risk | Blocked By |
|------|--------|------------|------|------------|
| 1: Memory Bedrock | ci-core, ci-graph | Medium | **High** | — |
| 2: Parsing Factory | ci-parser, ci-discover | Medium | Medium | E1 |
| 3: Indexing Pipeline | ci-pipeline | **High** | **High** | E1, E2 |
| 4: Query Engine | ci-query | Medium | Low | E1 |
| 5: Protocol & MCP | ci-protocol, ci-server, ci-cli | Medium | Low | E3, E4 |
| 6: Daemon & Optimization | ci-server (daemon), ci-ffi | Low | Medium | E5 |
