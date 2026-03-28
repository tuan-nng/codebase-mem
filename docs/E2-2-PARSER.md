# E2-2: Thread-Safe Tree-Sitter Parser Pool — Design Document

**Epic**: 2 — Parsing Factory
**Status**: Implemented
**Crate**: `crates/ci-parser`

---

## 1. What This Does

`ci-parser` provides a thread-safe, lazily-initialized pool of `tree_sitter::Parser` instances. Downstream pipeline stages (Pass 3, Pass 5, etc.) call `parse(source, language)` to turn raw source text into tree-sitter `Tree` objects, which are then traversed to extract code definitions, calls, and imports.

---

## 2. System Context

E2-2 is the parsing engine consumed by the indexing pipeline after E2-1 produces the list of source files.

```
┌─────────────────────────────────────────────────────────────────┐
│                     ci-pipeline  (E3-1, future)                │
│                                                                 │
│   Pass 1 ───► Pass 2 ───► Pass 3 ───► ... ───► Pass 9        │
│  Discovery   Structure   Definitions                    Freeze    │
└─────────────────────────────────────────────────────────────────┘
                                  │
                                  │ parse(source, lang) → Tree
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│  ci-parser (this crate)                                          │
│                                                                 │
│  tree-sitter Parser instances managed per-thread in TLS          │
└─────────────────────────────────────────────────────────────────┘
```

**Dependency rule**: `ci-parser` depends on `ci-core` (for the `Language` enum) and tree-sitter grammar crates. It has no dependencies on `ci-graph`, `ci-discover`, or pipeline code.

---

## 3. Architecture

Three modules, each with a single responsibility:

```
ci-parser
├── lib.rs       — public API, SHARED_POOL (top-level convenience function)
├── grammar.rs   — Language → tree_sitter_language::LanguageFn mapping
└── pool.rs      — Parser instantiation, caching, and parsing logic
```

### 3.1 The Fundamental Constraint

`tree_sitter::Parser` is `!Send`. This means it cannot be moved between threads — a single `Parser` instance must live entirely on the thread that created it. The standard thread-pool pattern (hand a parser to a worker) doesn't work here.

The solution is **thread-local storage**: each thread gets its own `HashMap<Language, Parser>`, lazily initialized. This sidesteps the `!Send` bound entirely — parsers never cross thread boundaries.

### 3.2 Data Flow

```
parse(source, lang) / ParserPool::parse(source, lang)
      │
      ▼
+---------------+
| grammar::     | → Some(LanguageFn) ──► continue
| get_language  | → None ──────────────► return None
+---------------+
      │
      ▼
+------------------------------------------+
| thread_local! HashMap<Lang, Parser>      |
|  entry exists (Some(Some(parser)))?       │
|    → reuse Parser  (fast path)            │
|  entry missing or None?                   │
|    → Parser::new() + set_language()      │
|    → cache and return  (slow path)        |
+---------------------+--------------------+
                      │
                      ▼
+--------------------------------+
| parser.parse(source, None)    | → Some(Tree) / None
+--------------------------------+
```

---

## 4. Module Design

### 4.1 `grammar.rs` — Language → Grammar Mapping

**Single point of truth** for the `ci_core::Language` → tree-sitter grammar mapping. Returns `Option<tree_sitter_language::LanguageFn>`.

```rust
pub fn get_language(lang: CoreLanguage) -> Option<tree_sitter_language::LanguageFn>
```

**Why return `LanguageFn` directly?** Because `LanguageFn` is `Send + Sync` — it is a `repr(transparent)` wrapper around `unsafe extern "C" fn() -> *const ()`. It can be created and stored freely on any thread. The conversion to `tree_sitter::Language` happens inside `get_parser_mut()`, where we are already thread-local.

**Conditional compilation**: each grammar is gated by a `#[cfg(feature = "tree-sitter-*")]` attribute. Languages without a grammar (Toml, Markdown, Sql, Unknown) fall through to the catch-all `None` arm.

### 4.2 `pool.rs` — Parser Pool

`ParserPool` is a zero-sized unit struct. The actual state lives in thread-local storage:

```rust
thread_local! {
    static PARSERS: RefCell<HashMap<CoreLanguage, Option<tree_sitter::Parser>>> =
        RefCell::new(HashMap::new());
}
```

#### The `HashMap<K, Option<V>>` Pattern

The inner `Option` is the critical design choice. It distinguishes three states:

| State | Meaning |
|-------|---------|
| `HashMap::get(key)` returns `None` | Entry was never inserted — language has not been processed on this thread |
| `HashMap::get(key)` returns `Some(None)` | Entry exists but parser is not yet initialized |
| `HashMap::get(key)` returns `Some(Some(parser))` | Entry exists and parser is ready |

Without `Option`, we'd need a separate `contains_key()` check before `get()`, doubling the hash lookups on every parse call.

#### Fast Path / Slow Path

The `get_parser_mut()` function uses two borrow levels:

1. **Fast path** (immutable borrow): Check `cell.borrow().get(&lang)` for `Some(Some(_))`. This confirms initialization without entering the `RefCell` borrow conflict. If found, fall through to the mutable borrow only for the cast.

2. **Slow path** (mutable borrow): Create and cache the parser. The mutable borrow is unavoidable here.

#### Returning `&'static mut Parser` from `RefCell`

The core lifetime challenge: `RefCell::borrow_mut()` returns `RefMut<'a, T>` scoped to the closure, but we need to return a `&'static mut Parser` to the caller. The solution is the raw pointer cast pattern:

```rust
PARSERS.with(|cell| {
    let mut parsers = cell.borrow_mut();
    let ptr = parsers.get_mut(&lang).unwrap().as_mut().unwrap()
        as *mut tree_sitter::Parser;
    Some(unsafe { &mut *ptr })
})
```

Within the exclusive `borrow_mut()` scope, we extract a `*mut Parser`, then reborrow it as `&'static mut Parser` via `unsafe { &mut *ptr }`. The `'static` lifetime is valid because:
- The `RefCell` is tied to the thread — it cannot be accessed from another thread (TLS)
- The `Parser` inside the `HashMap` lives for the lifetime of the `RefCell`
- The raw pointer escapes the `RefMut` scope but remains valid because the backing `HashMap` still exists

This pattern is sound but requires `unsafe`. It is encapsulated in `get_parser_mut()`; callers see only safe interfaces.

### 4.3 `lib.rs` — Public API

Two entry points:

```rust
// Convenience: process-wide shared pool, one RefCell-protected ParserPool
pub fn parse(source: &str, lang: Language) -> Option<tree_sitter::Tree>

// Explicit: manage pool lifecycle yourself
pub struct ParserPool;
impl ParserPool {
    pub fn parse(&mut self, source: &str, lang: Language) -> Option<tree_sitter::Tree>
    pub fn parse_result(&mut self, source: &str, lang: Language) -> Result<Tree, ParseError>
    pub fn supports(&self, lang: Language) -> bool
    pub fn cached_count(&self) -> usize
    pub fn clear_cache(&self) -> ()
}
```

`SHARED_POOL` is `thread_local! RefCell<ParserPool>`, not a global. Each thread gets its own RefCell-backed pool. This is consistent with the per-thread parser model — the "shared" in the name refers to the pool concept (many parse calls sharing one pool), not thread sharing.

---

## 5. Grammar Version Compatibility

### 5.1 The Core Problem

tree-sitter grammars are compiled C code that expose a `tree_sitter_language::LanguageFn`. The `LanguageFn` produces a pointer to a `TSLanguage` struct whose ABI must match the tree-sitter runtime (the `tree-sitter` crate).

If a grammar was compiled against tree-sitter 0.20 and loaded against tree-sitter 0.26, the `TSLanguage` pointer sizes differ — the C struct layout has changed across major versions. This produces undefined behavior (wrong memory layout, incorrect function pointers).

### 5.2 Compatibility Matrix

All grammar crates in the default build target tree-sitter 0.26:

| Grammar | Version | tree-sitter dep | Compatible? |
|---------|---------|-----------------|-------------|
| tree-sitter-rust | 0.24 | tree-sitter-language 0.1 | Yes |
| tree-sitter-c | 0.24 | tree-sitter-language 0.1 | Yes |
| tree-sitter-cpp | 0.23 | tree-sitter-language 0.1 | Yes |
| tree-sitter-go | 0.25 | tree-sitter-language 0.1 | Yes |
| tree-sitter-javascript | 0.23 | tree-sitter-language 0.1 | Yes |
| tree-sitter-typescript | 0.23 | tree-sitter-language 0.1 | Yes |
| tree-sitter-html | 0.23 | tree-sitter-language 0.1 | Yes |
| tree-sitter-css | 0.25 | tree-sitter-language 0.1 | Yes |
| tree-sitter-python | 0.25 | tree-sitter-language 0.1 | Yes |
| tree-sitter-bash | 0.25 | tree-sitter-language 0.1 | Yes |
| tree-sitter-java | 0.23 | tree-sitter-language 0.1 | Yes |
| tree-sitter-scala | 0.25 | tree-sitter-language 0.1 | Yes |
| **tree-sitter-kotlin-ng** | **1.1** | **tree-sitter-language 0.1** | **Yes** (was excluded: old kotlin 0.3.8 uses tree-sitter 0.22) |
| tree-sitter-c-sharp | 0.23 | tree-sitter-language 0.1 | Yes |
| tree-sitter-swift | 0.7 | tree-sitter-language 0.1 | Yes |
| tree-sitter-json | 0.24 | tree-sitter-language 0.1 | Yes |
| tree-sitter-yaml | 0.7 | tree-sitter-language 0.1 | Yes |
| tree-sitter-ruby | 0.23 | tree-sitter-language 0.1 | Yes |
| tree-sitter-php | 0.24 | tree-sitter-language 0.1 | Yes |
| tree-sitter-zig | 1.1 | tree-sitter-language 0.1 | Yes |
| ~~tree-sitter-kotlin~~ | ~~0.3.8~~ | ~~tree-sitter 0.22~~ | **No** — excluded |
| ~~tree-sitter-toml~~ | ~~0.20.0~~ | ~~tree-sitter 0.20~~ | **No** — excluded |
| ~~tree-sitter-markdown~~ | ~~0.7.1~~ | ~~tree-sitter 0.19~~ | **No** — excluded |

**Total: 20 grammars in `lang-all`.**

### 5.3 Why `tree-sitter-kotlin-ng`?

The original `tree-sitter-kotlin` (0.3.8) depends on tree-sitter 0.22. `tree-sitter-kotlin-ng` (1.1.0) is the community fork targeting tree-sitter 0.24 with `tree-sitter-language` 0.1. It exports `tree_sitter_kotlin_ng::LANGUAGE` as a `LanguageFn`, fully compatible with our runtime.

---

## 6. Feature Flags

Grammars are organized into composable feature groups:

```toml
default = ["lang-all"]    # 20 grammars

lang-all      = [all 20 grammars]
lang-systems  = [Rust, C, C++, Go]
lang-web      = [JavaScript, TypeScript, HTML, CSS]
lang-script   = [Python, Bash]
lang-jvm      = [Java, Scala, Kotlin]       # ← Kotlin added via kotlin-ng
lang-dotnet   = [C#]
lang-mobile   = [Swift]
lang-config   = [JSON, YAML]
lang-other    = [Ruby, PHP, Zig]
```

Users can pick a subset to reduce binary size and compile time:

```rust
// Only JVM languages — fast compile, small binary
ci-parser = { features = ["lang-jvm"] }

// Only systems languages
ci-parser = { features = ["lang-systems"] }
```

---

## 7. Memory Model

Each Rayon worker thread accumulates at most one `Parser` per language it has ever processed. A `tree_sitter::Parser` is lightweight — a few KB each (the compiled grammar is the heavy part, shared via mmap). At 20 grammars, total memory per thread is negligible (~1 MB).

`clear_cache()` drops all parsers on the current thread. Useful between pipeline batches when memory pressure is a concern — parsers are re-created lazily on demand.

---

## 8. Error Handling

```rust
pub enum ParseError {
    /// No grammar was compiled in for this language.
    Unsupported(Language),
    /// tree-sitter failed to produce a parse tree.
    ParseFailed(Language),
}
```

`Unsupported` means the language feature flag is not enabled. `ParseFailed` means tree-sitter genuinely couldn't produce a tree (extremely rare — tree-sitter always returns a tree, even for invalid input).

The top-level `parse()` returns `Option<Tree>`. Callers who need diagnostic information (e.g., pipeline error reporting) use `parse_result()`.

---

## 9. Test Coverage

| Group | Count | Coverage |
|-------|-------|----------|
| Construction | 3 | Default construction, cached_count, supports false/true |
| Thread isolation | 1 | Each thread's cache is independent |
| Parse correctness | 4 | Rust, Python, cached reuse, invalid source |
| Error handling | 5 | Unsupported lang (Option), Unsupported lang (Result), Display, Debug |
| Tree structure | 1 | Root node kind, child traversal via cursor |
| Top-level API | 1 | `parse()` convenience function |
| **Total** | **17 unit + 2 doc** | |

---

## 10. Integration Points

```
ci-parser
    │
    ├── public API: parse(source, lang) → Option<Tree>
    │                      ParserPool::parse/supported/cached_count/clear_cache
    │
    ├── depends on: ci-core (Language enum)
    │
    └── downstream consumer (E3-1, future)
            │
            ▼
        ci-pipeline
            │
            ├── Pass 3: parse each file → extract definitions
            ├── Pass 5: parse each file → extract call sites
            └── Pass 8: parse each file → extract metadata
```

---

## 11. Performance

| Path | Cost | Notes |
|------|------|-------|
| Fast path (cached) | 1 immutable HashMap lookup + 1 mutable HashMap lookup + parse | ~100 ns for lookup, parse dominates |
| Slow path (first parse) | grammar lookup + `Parser::new()` + `set_language()` + parse | ~10 us for parser init, parse dominates |
| Clear cache | O(grammars per thread) | Constant (max ~20), deallocates parsers |

**Parse time dominates** over pool overhead. On a warm cache, pool lookup is ~100 ns — effectively zero overhead.

---

## 12. Future Work

| Item | Status | Notes |
|------|--------|-------|
| TOML grammar | Deferred | No tree-sitter 0.26-compatible crate exists yet |
| Markdown grammar | Deferred | No tree-sitter 0.26-compatible crate exists yet |
| SQL grammar | Deferred | Limited coverage; tree-sitter-sql not 0.26-compatible |
| Tier 2 grammars | Deferred | Loadable via `libloading` (E6-3) |
| Parser reuse across files | Done | Parser caches language state across parse calls |
| Memory pressure relief | Done | `clear_cache()` between batches |
