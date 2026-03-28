# E2-3: Declarative Language Spec System — Design Document

**Epic**: 2 — Parsing Factory
**Status**: Implemented
**Crates**: `ci-core`, `ci-parser`

---

## 1. What This Does

E2-3 replaces per-language extraction logic with a **declarative data model**. Rather than writing Rust code to handle each language's AST structure, language authors define `LanguageSpec` structs — sorted arrays of `NodeRule` and `EdgeRule` entries — that the shared extraction engine interprets uniformly. Adding Python 3.13 support requires zero changes to the extraction engine.

---

## 2. System Context

E2-3 plugs into the indexing pipeline between parsing and graph construction:

```
File source text
       │
       ▼
┌─────────────────────────────────────┐
│  ci-parser / pool.rs (E2-2)        │  ← Thread-safe parser pool
│  parse(source, lang) → Tree        │
└────────────────┬──────────────────┘
                  │ Tree
                  ▼
┌─────────────────────────────────────┐
│  ci-parser / extractor.rs (E2-3)   │  ← This crate
│  Extractor::new(&tree, src, lang)  │
│  → Iterator<Item = ExtractedItem>  │
└────────────────┬──────────────────┘
                  │ ExtractedItem (node or edge)
                  ▼
┌─────────────────────────────────────┐
│  ci-graph / builder.rs (future E3) │
│  Converts ExtractedItem → graph     │
└─────────────────────────────────────┘
```

The output API is intentionally minimal: `Iterator<Item = ExtractedItem>`. The graph layer owns all graph-specific decisions (interning, deduplication, ID allocation). This keeps the extraction engine stateless and trivially testable.

---

## 3. Data Model

### 3.1 NodeRule

```rust
pub struct NodeRule {
    pub ts_kind:  &'static str,   // tree-sitter node kind, e.g. "function_item"
    pub label:    NodeLabel,       // graph label, e.g. Function, Class, Namespace
    pub scope_anchor: bool,        // pushes ScopeSegment on enter
    pub skip_edges:  bool,         // suppresses edge emission for this node
    pub skip_node:   bool,        // suppresses node emission (scope-only)
}
```

Three constructors:

| Constructor | When to use |
|------------|-------------|
| `NodeRule::leaf("function_item", Function)` | Standalone symbol with no child scope |
| `NodeRule::container("struct_item", Class)` | Container that emits a node AND pushes scope |
| `NodeRule::scope_only("impl_item")` | Container that only pushes scope (no node emission) |

Node rules are stored in a **globally sorted slice** and looked up by binary search: O(log n) regardless of rule count.

### 3.2 EdgeRule

```rust
pub enum TargetPattern {
    FromChildType,   // target is a named child node's text (e.g. impl Foo → "Foo")
    FromNodeText,    // target is the node's own text
    ResolveSpecial,  // resolve "super" / "self" / "Self" to enclosing class
}

pub struct EdgeRule {
    pub source_kind:   &'static str,
    pub edge_type:     EdgeType,       // Calls, Imports, Inherits, etc.
    pub target_pattern: TargetPattern,
}
```

Edge rules are stored in unsorted arrays and looked up by linear scan (~10 rules per language, so O(n) is fine).

### 3.3 ExtractedItem

```rust
pub enum ExtractedItem {
    Node(ExtractedNode),
    Edge(ExtractedEdge),
}

pub struct ExtractedNode {
    pub label:           NodeLabel,
    pub qualified_name:  String,       // e.g. "foo::Bar::baz"
    pub ts_kind:         String,       // e.g. "function_item"
    pub start_byte:      u32,          // 0-based byte offset in source
    pub end_byte:         u32,
    pub line:             u32,          // 1-based
    pub column:          u32,          // 1-based
}

pub struct ExtractedEdge {
    pub source_qualified:  String,
    pub target_qualified:  String,
    pub edge_type:        EdgeType,
}
```

`ExtractedItem` and its variants carry `#[derive(Archive, Serialize, Deserialize)]` from `rkyv` — the entire extraction output is serializable to a flat byte buffer without heap allocation.

---

## 4. Scope and Qualified Names

### 4.1 ScopeKey Stack

Qualified names are assembled during traversal using a stack of `ScopeSegment`:

```rust
pub struct ScopeSegment {
    pub kind: String,   // ts_kind of the scope anchor, e.g. "impl_item"
    pub name: String,   // extracted name, e.g. "Counter"
}

pub struct ScopeKey {
    segments: Vec<ScopeSegment>,  // outermost → innermost
}
```

`ScopeKey::qualify(name, separator)` joins all non-empty segment names, then appends the symbol name:

```
segments: [("mod_item", "foo"), ("impl_item", "Counter")]
name:     "inc"
sep:      "::"
result:   "foo::Counter::inc"
```

Empty-name segments (e.g. `source_file`) are filtered out so they don't pollute the qualified name.

### 4.2 Scope Anchors

A node is a scope anchor if it has a `NodeRule` with `scope_anchor = true`. During traversal:

```
visit(node):
    emit node (if rule applies)
    emit edge (if rule applies)
    if node is scope anchor && node != root:
        name = extract_identifier(node)
        scope.push(("node_kind", name))
    recurse children
    if scope was pushed:
        scope.pop()
```

`impl_item` is a scope anchor via `NodeRule::scope_only("impl_item")`. This means methods inside `impl Counter { fn inc() {} }` are qualified as `Counter::inc`, not bare `inc`.

---

## 5. The Suppress Mechanism

The most subtle part of the extractor is preventing **duplicate or incorrect** symbol entries.

### 5.1 The Problem

A tree-sitter `function_item` node has an `identifier` child. Both nodes have node rules:

```
source_file
  └── function_item  [NodeRule: label = Function]
        └── identifier [NodeRule: label = Variable]
```

Without suppression, both emit symbols. The `identifier` emits bare `"inc"` at module scope; the `function_item` emits `"inc"` (same name, no scope yet since the parent hasn't pushed). Both are wrong — we want `"Counter::inc"` from the `function_item` with the impl scope.

### 5.2 The Solution: Suppress by Name Equality

```rust
let name       = extract_identifier(node);
let own_text   = node.utf8_text(source).map(|s| s.trim().to_string());
let suppress   = own_text == name && name.is_some() && node.kind() != "field_identifier";
```

If a node's extracted name **equals its own source text**, it is purely a syntactic name token (e.g. the `identifier` child `"inc"` inside `function_item`). The parent container already emits the qualified name; emitting the bare text would produce a spurious symbol entry.

Exceptions:
- **`field_identifier`** is never suppressed — struct fields are semantically meaningful even when name equals text, and suppressing them would lose field nodes entirely.
- **`skip_node`** rules (`scope_only("impl_item")`) never emit a node, so the suppress condition is irrelevant for scope-only nodes.

### 5.3 The `impl_item` Special Case

Rust's `impl_item` has no named field containing the type name. `child_by_field_name("type_identifier")` returns `None` because tree-sitter doesn't expose `type_identifier` as a named field on `impl_item`. Instead, the name must be extracted by scanning positional children:

```rust
"impl_item" => {
    // Find the first type_identifier child (skip unnamed "impl" keyword)
    node.children(&mut node.walk())
        .find(|c| c.kind() == "type_identifier")
        .and_then(|c| c.utf8_text(source).ok()?.trim())
        .filter(|s| !s.is_empty())
}
```

This returns `Some("Counter")`, so `impl_item`'s scope segment is `("impl_item", "Counter")`, and all descendant methods are qualified correctly.

### 5.4 Edge Emission Gate

Edge rules are gated by the suppress condition AND the `skip_edges` flag:

```rust
if !suppress {
    let node_rule = spec.get_node_rule(ts_kind);
    if node_rule.map_or(true, |r| !r.skip_edges) {
        if let Some(rule) = spec.get_edge_rule(ts_kind) {
            // emit edge
        }
    }
}
```

Without the `skip_edges` check, a node could have its node emission suppressed but still fire edge rules — producing orphaned edges with no corresponding source node.

---

## 6. Architecture

```
ci-core/
  lang_spec.rs        — Core types: NodeRule, EdgeRule, LanguageSpec,
                         ScopeKey, ScopeSegment, ExtractedItem/Node/Edge
  specs/
    mod.rs            — OnceLock registry, check_sorted() guard
    rust.rs           — Rust spec (15 node rules, 12 edge rules)
    python.rs         — Python spec (11 node rules, 7 edge rules)

ci-parser/
  extractor.rs        — Eager DFS traversal, Iterator<Item = ExtractedItem>
```

**Dependency rule**: `ci-core` has zero external dependencies (only `rkyv`). `ci-parser` depends on `ci-core` + `tree-sitter`. The `ci-graph` crate (future) depends on both.

---

## 7. Adding a New Language

### 7.1 Create the Spec File

```rust
// crates/ci-core/src/specs/go.rs

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::{EdgeType, Language, NodeLabel};

const NODE_RULES: &[NodeRule] = &[
    NodeRule::leaf("function_declaration",  NodeLabel::Function),
    NodeRule::container("type_declaration",  NodeLabel::Class),  // struct / interface
    NodeRule::leaf("identifier",            NodeLabel::Variable),
    // ... globally alphabetically sorted
];

const EDGE_RULES: &[EdgeRule] = &[
    EdgeRule::calls("call_expression",   TargetPattern::FromNodeText),
    EdgeRule::imports("import_declaration", TargetPattern::FromNodeText),
];

pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::Go,
    root_kind: "source_file",
    qn_separator: ".",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
```

**Critical**: `NODE_RULES` must be sorted in **global alphabetical order** by `ts_kind`. `check_sorted()` runs at registry initialization and panics on violation.

### 7.2 Register the Spec

```rust
// crates/ci-core/src/specs/mod.rs

mod go;
// ...
m.insert(Language::Go, &go::SPEC);
check_sorted(&go::SPEC);
```

### 7.3 Verify with Tests

```rust
// crates/ci-core/src/lang_spec.rs (or in the spec file)

#[test]
fn go_spec_rules_sorted() {
    let spec = spec_for(Language::Go).unwrap();
    assert!(spec.get_node_rule("function_declaration").is_some());
    assert!(spec.get_edge_rule("call_expression").is_some());
}
```

### 7.4 Verify Extraction

```rust
#[test]
fn go_function_extraction() {
    let source = "func Add(a int, b int) int { return a + b }";
    let tree = parse(source, Language::Go).unwrap();
    let items: Vec<_> = Extractor::new(&tree, source, Language::Go).collect();
    let names: Vec<_> = items.iter().filter_map(|i| match i {
        ExtractedItem::Node(n) => Some(n.qualified_name.clone()),
        _ => None,
    }).collect();
    assert!(names.contains(&"Add".into()));
}
```

---

## 8. Key Design Decisions

### 8.1 Eager DFS, Not Streaming Cursor

The original design attempted to use tree-sitter's `TreeCursor` as a streaming iterator. This failed because:

- After `goto_parent()`, the cursor lands back at the parent, but the traversal state machine cannot distinguish "just arrived from above" from "just finished a child subtree and returned."
- Attempting to track this with flags and pending edges produced subtle infinite loops.
- Recursive DFS with all items pre-collected into a `Vec` is simpler and provably correct. The `Iterator` wrapper is a `Vec::IntoIter`, so the API remains streaming (no heap allocation at iteration time).

### 8.2 OnceLock, Not LazyLock

`std::sync::OnceLock` is used for the spec registry instead of `std::sync::LazyLock`. `LazyLock` poisons permanently on panic — if a test panics inside `LazyLock::force()` (e.g. `check_sorted` failure), all subsequent tests in the process fail with a poison error. `OnceLock` sets once and doesn't poison.

### 8.3 `skip_node` for Scope-Only Containers

`impl_item` needs to push scope (so methods are qualified) but must NOT emit a node (which would duplicate the struct's `Class` entry). Rather than overloading an existing field, `NodeRule::scope_only("impl_item")` makes the intent explicit: this node establishes a scope, nothing more.

### 8.4 Name-over-OwnText Fallback

For `field_identifier`, `extract_identifier` returns `None` (no named field, wrong kind for the named-kinds match). The emit code uses `name.or(own_text)` as a fallback, so field identifiers are emitted using their own text as the name. This avoids the suppress condition entirely for fields — a field's name is always its own text, and that's intentional.

---

## 9. Test Coverage

| Test suite | Count | Coverage |
|------------|-------|----------|
| ci-core unit tests | 128 | Core types, spec registry, sort order |
| ci-parser extractor tests | 39 | Rust and Python extraction correctness |
| ci-parser pool tests | 21 | Thread safety, error handling |
| Doc tests | 2 | `ScopeKey::qualify`, `ParserPool` example |

**Total: 190+ tests**, all passing. Pre-commit runs `cargo fmt` + `cargo clippy` with `-D warnings`.

---

## 10. Known Limitations

1. **Multi-inheritance edge targets** — Python `class Foo(Bar, Baz)` emits `Inherits` edge to `"Bar, Baz"` (multi-target string). Fixing this requires a new `TargetPattern` variant that emits multiple edges, or a language-specific case in `resolve_target`.

2. **Generic impl name extraction** — `impl<T> Foo<T>` uses `.find()` to locate `type_identifier`, which returns the first match. In pathological cases (multiple type identifiers), the wrong one may be selected. Currently not covered by test corpus.

3. **Soft limit on rule count** — Binary search assumes rules are in a flat array. At ~50+ rules per language, consider a two-level structure (node rules by category: definitions, expressions, literals), but this is not yet warranted.
