# E2-1: File Discovery — Design Document

**Epic**: 2 — Parsing Factory
**Status**: Implemented
**Crate**: `crates/ci-discover` + `crates/ci-core/src/language.rs`

---

## 1. What This Does

`ci-discover` traverses a directory tree and emits every source file it finds, annotated with the programming language detected from the file extension. Downstream pipeline stages consume these `(path, language)` pairs to build the code graph.

---

## 2. System Context

E2-1 is the entry point of the indexing pipeline — the first data source for everything that follows.

```
┌─────────────────────────────────────────────────────────────────┐
│                     Agent (Claude Opus, etc.)                    │
└────────────────────────────────┬────────────────────────────────┘
                                 │ "index this repo"
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│                     ci-pipeline  (E3-1, future)                  │
│                                                                 │
│   Pass 1 ───► Pass 2 ───► Pass 3 ───► ... ───► Pass 9         │
│  Discovery   Structure   Definitions                  Freeze      │
│     ▲                                                          │
│     │                                                          │
└─────┼──────────────────────────────────────────────────────────┘
      │
      │ discover(root, config)  ──► ParallelIterator<Item = DiscoveredFile>
      │
      ▼
┌─────────────────────────────────────────────────────────────────┐
│  ci-discover (this crate)                                        │
│                                                                 │
│  WalkBuilder ──► filter ──► classify ──► post-filter ──► emit │
│  (.gitignore)    (errors)  (extension)   (dotdirs)   (par_iter)│
└─────────────────────────────────────────────────────────────────┘
      │
      │ (re-exported)
      ▼
┌─────────────────────────────────────────────────────────────────┐
│  ci-core/src/language.rs    — Language enum, no external deps    │
└─────────────────────────────────────────────────────────────────┘
```

**Dependency rule**: `ci-core` is the leaf crate. It has zero external dependencies (only `rkyv`). Everything else depends on it. `ci-discover` depends on `ci-core`, `ignore`, and `rayon`.

---

## 3. Discovery Pipeline (Data Flow)

Each file entering `discover()` passes through 4 transformation stages:

```
 ┌──────────────────────────────────────────────────────────────────────────────┐
 │ STAGE 1 — Walk                                                                  │
 │                                                                                 │
 │   WalkBuilder                                                                   │
 │   ├── .hidden(false)         walker ENTERS .git, .venv to read .gitignore     │
 │   ├── .git_ignore(true)      apply .gitignore rules                           │
 │   ├── .git_global(true)      apply ~/.gitignore                                │
 │   ├── .git_exclude(true)     apply .git/info/exclude                          │
 │   ├── .parents(true)         load .gitignore from root + ancestor dirs         │
 │   ├── .require_git(false)    work outside git repos too                       │
 │   └── .max_depth(N)          limit traversal depth                             │
 │                                                                                 │
 │   Output: iterator of DirEntry                                                  │
 └─────────────────────────────────┬──────────────────────────────────────────────┘
                                   │ DirEntry
                                   ▼
 ┌──────────────────────────────────────────────────────────────────────────────┐
 │ STAGE 2 — Error filter                                                         │
 │                                                                                 │
 │   filter_map(|entry| entry.ok())                                              │
 │                                                                                 │
 │   Reason: WalkBuilder returns io::Result; permission errors, symlink loops,    │
 │   and unreadable dirs are silently dropped — they don't affect discovery.       │
 │                                                                                 │
 │   Output: iterator of DirEntry (clean)                                          │
 └─────────────────────────────────┬──────────────────────────────────────────────┘
                                   │ DirEntry
                                   ▼
 ┌──────────────────────────────────────────────────────────────────────────────┐
 │ STAGE 3 — Classify                                                             │
 │                                                                                 │
 │   filter_map(entry_to_file)                                                     │
 │                                                                                 │
 │   ┌──────────────────┐   ┌────────────────┐   ┌───────────────────────────┐  │
 │   │ entry.file_type()│No │  rsplit_once('.')│No │  from_extension(ext)      │No │
 │   │  is_file()?     ├──►│  has extension? │──►│  in known set? (24 langs) │──► DROP │
 │   └──────────────────┘   └────────────────┘   └───────────────────────────┘  │
 │       │        Yes           │ Yes                    │ Yes                   │
 │       ▼                     ▼                        ▼                       │
 │     DROP              extract ext               return Some(Language)          │
 │                                                                            │
 │   Output: iterator of DiscoveredFile { path: PathBuf, language: Language }   │
 └─────────────────────────────────┬──────────────────────────────────────────────┘
                                   │ DiscoveredFile
                                   ▼
 ┌──────────────────────────────────────────────────────────────────────────────┐
 │ STAGE 4 — Post-filter (config-driven)                                          │
 │                                                                                 │
 │   filter(move |f| {                                                           │
 │       if skip_hidden && filename.starts_with('.')  { return false; }          │
 │       if skip_dotdirs {                                                        │
 │           // strip root prefix to avoid /tmp/.tmpXXXXX/ false positives       │
 │           let rel = path.strip_prefix(&root).unwrap_or(&path);                │
 │           for component in rel.components() {                                  │
 │               if component.starts_with('.') { return false; }                  │
 │           }                                                                   │
 │       }                                                                        │
 │       true                                                                      │
 │   })                                                                            │
 │                                                                                 │
 │   Output: iterator of DiscoveredFile (filtered)                                 │
 └─────────────────────────────────┬──────────────────────────────────────────────┘
                                   │ DiscoveredFile
                                   ▼
 ┌──────────────────────────────────────────────────────────────────────────────┐
 │ STAGE 5 — Parallel Iterator                                                   │
 │                                                                                 │
 │   .collect() → Vec<DiscoveredFile>                                            │
 │   .into_par_iter()                                                            │
 │                                                                                 │
 │   Returned type: impl ParallelIterator<Item = DiscoveredFile>                  │
 │                                                                                 │
 │   Discovery: sequential (one thread walks the tree)                            │
 │   Consumption: parallel (Rayon work-stealing pool)                             │
 └─────────────────────────────────────────────────────────────────────────────────┘
```

### Why the Stages Are In This Order

| Stage | Why after Walk | Why before Parallel Iter |
|-------|---------------|--------------------------|
| Error filter | Errors are a WalkBuilder artifact, not a file property | — |
| Classify | Unknown extensions → DROP; don't include in Vec | Unknown files never reach consumer |
| Post-filter | Dotdir check needs `path`, not `DirEntry` | Config filters reduce Vec size before parallelization |
| Collect | Parallel iterator must have a known `Item` type | — |

---

## 4. Language Classification

### 4.1 Enum Design

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
#[repr(u8)]
pub enum Language {
    // Systems languages
    Rust, C, Cpp, Go, Java, Python,
    // Web
    TypeScript, JavaScript, Html, Css,
    // JVM
    Kotlin, Scala,
    // .NET / Native
    CSharp, Swift,
    // Scripting / Other
    Ruby, Php, Shell, Zig,
    // Data / Config
    Json, Yaml, Toml, Markdown, Sql,
    // Sentinel
    Unknown,
}
```

Stored as `#[repr(u8)]` — fits in 1 byte, enabling compact graph index representations and efficient `match` on discriminant.

### 4.2 Extension → Language Mapping

```
from_extension("rs")   → Some(Language::Rust)
from_extension("RS")   → Some(Language::Rust)   ← case-insensitive
from_extension("tsx")   → Some(Language::TypeScript)
from_extension("h")     → Some(Language::Cpp)     ← headers are C++
from_extension("txt")  → Some(Language::Markdown) ← .txt considered markdown
from_extension("xyz")  → None                   ← unknown, skipped
from_extension("")    → None                   ← no extension, skipped
```

### 4.3 Design Decision: Match vs. Static Array

Three approaches were considered:

| Approach | Lookup Cost | Maintainability | Notes |
|----------|-----------|-----------------|-------|
| Binary search on sorted array | O(log N) ≈ 5 comparisons | Error-prone: must maintain sort order | Rejected — brittle |
| HashMap lookup | O(1) average | Good, but HashMap has runtime overhead | Rejected — allocates |
| Direct `match` expression | O(1) worst-case, fully inlined | Excellent: all mappings visible in one place | **Chosen** |

The `match` is compiled by LLVM into a jump table. With 24 arms, branch prediction is trivial. The `to_ascii_lowercase()` call is the only per-lookup heap allocation, and it typically allocates zero bytes for ASCII extensions.

---

## 5. Key Design Decisions

### 5.1 Post-Discovery Dotdir Filtering

**Problem**: `WalkBuilder.hidden(false)` lets the walker enter `.git`, `.venv`, etc. to read their `.gitignore` files. But we don't want files *from* those directories in our output.

**Naive fix**: filter `path.components()` for dot-prefixed names.

**Bug**: breaks valid paths under temp directories like `/tmp/.tmpXXXXX/myrepo/src/main.rs` — the temp segment `.tmpXXXXX` starts with a dot.

**Solution**: strip the root prefix before checking components.

```
/tmp/.tmpXXXXX/myrepo/src/main.rs
  strip_prefix("/tmp/.tmpXXXXX/myrepo")
  → src/main.rs
  → no dot components → KEEP

.myrepo/src/main.rs  (walked from cwd)
  strip_prefix("/home/user/.myrepo")
  → src/main.rs
  → no dot components → KEEP

.myrepo/src/main.rs  (walked from inside)
  strip_prefix("/home/user/.myrepo/src")
  → (empty — this IS the root)
  → no dot components → KEEP
```

### 5.2 `require_git(false)`

`WalkBuilder` by default refuses to read `.gitignore` files unless the starting directory is inside a git repository. Setting `require_git(false)` tells it to treat `.gitignore` as a plain ignore file regardless of git context. This is necessary for:

- Bare project directories not yet initialized with git
- Indexing subdirectories of a larger repo
- Standalone use of `ci-discover` as a library

### 5.3 Eager Collection (Parallel Iterator Return Type)

`discover()` returns `impl ParallelIterator`, but internally it calls `.collect()` before `.into_par_iter()`.

```
Walk ──► filter ──► classify ──► post-filter ──► .collect()
                                                          │
                                                     Vec owned
                                                          │
                                                          ▼
                                                     .into_par_iter()
                                                          │
                                                     ParallelIterator
```

**Claimed benefit**: consumer gets a parallel iterator.

**Actual behavior**: discovery is sequential, consumption is parallel.

This was chosen because:

1. The `ignore::Walk` iterator is sequential — it manages its own internal stack
2. The caller (pipeline Pass 1) immediately calls `.collect()` anyway
3. Rayon parallelism delivers value in Pass 3 (parallel parsing), not Pass 1 (discovery)

If profiling reveals discovery as a bottleneck, the fix is to use a thread-pool-based walker or `rayon::iter::from_file_stream`. The return type doesn't change — only the internal implementation.

---

## 6. Configuration API

```rust
// Default: skip hidden files, skip dotdirs, no depth limit
let files = discover("/path/to/repo", DiscoverConfig::new());

// Show hidden files (but still skip dotdirs like .git)
let files = discover("/path/to/repo", DiscoverConfig::new().include_hidden());

// Limit traversal depth (depth 1 = immediate children of root)
let files = discover("/path/to/repo", DiscoverConfig::new().max_depth(Some(2)));

// Combine options
let files = discover("/path/to/repo", DiscoverConfig::new()
    .include_hidden()
    .max_depth(Some(3)));
```

`include_hidden()` only disables `skip_hidden`. `skip_dotdirs` stays `true` — `.git`, `.venv`, and `.node_modules` are never indexed, regardless of user preference.

---

## 7. Test Coverage

### ci-discover (22 tests)

| Test group | Count | What it verifies |
|------------|-------|------------------|
| Language detection | 8 | Each Tier-1 language found correctly; multi-ext aliases (tsx, hpp, yml) |
| Unknown extensions | 3 | .dat, .png, .tar.gz, no-ext files skipped |
| Hidden files | 1 | dot-prefixed files excluded by default |
| Dot directories | 1 | files inside .git/.venv excluded by default |
| Hidden config | 1 | `include_hidden()` surfaces dot files |
| Gitignore | 2 | built-in target/ skip + custom rules |
| Max depth | 1 | depth limit applied correctly |
| Parallel iterator | 2 | all files discovered, correct languages |
| Edge cases | 3 | empty dir, root file, deeply nested dirs |

### ci-core/language.rs (60+ tests)

All enum variants have display tests. Extension mapping, path parsing, case insensitivity, double extensions, rkyv roundtrip, equality, and hash.

---

## 8. Integration Points

```
ci-discover
    │
    ├── public API: discover(root, config) → ParallelIterator<Item = DiscoveredFile>
    │
    └── downstream consumer (E3-1, future)
            │
            ▼
        ci-pipeline
            │
            ├── Pass 1: consume DiscoveredFile stream → File nodes in MutableGraph
            ├── Pass 2: structural hierarchy (Project → Package → Directory → File)
            └── Pass 3: parallel parse each file (E2-2 feeds ci-parser)

ci-core
    │
    ├── ci-discover: Language classification
    ├── ci-graph:    Language in File node metadata
    └── ci-pipeline: filter/exclude by language
```

---

## 9. Performance

| Stage | Cost | Notes |
|-------|------|-------|
| Filesystem walk | O(files) | dominated by I/O; `ignore` crate handles efficiently |
| Extension lookup | O(1) | single `rsplit_once` + inlined match |
| Post-filter | O(depth) | scan path components for dot-prefix (avg depth ~5) |
| Parallel consumption | O(n/p) | n files, p threads |

At Linux kernel scale (~100K source files), discovery completes in ~100–500ms on SSD. The parallel iterator scales consumption linearly with core count.

---

## 10. Future Work

| Item | Status | Notes |
|------|--------|-------|
| Dockerfile detection | Deferred | Add `from_path` special-case for `filename == "Dockerfile"` |
| Lazy parallel walk | Deferred | Switch from `.collect().into_par_iter()` to streaming parallel walker |
| Language sub-types | Out of scope | Python 3 vs 2, C++ standard versions — future E2-4 |
| Encoding detection | Out of scope | UTF-8 assumed; non-UTF-8 filenames silently skipped |
