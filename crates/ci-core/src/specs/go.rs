//! Go extraction specification.
//!
//! Grammar reference: <https://github.com/tree-sitter/tree-sitter-go>
//!
//! # Scope anchors
//!
//! | ts_kind             | ScopeSegment name  | Label     |
//! |---------------------|-------------------|-----------|
//! | `source_file`       | (root — empty)   | Namespace |
//!
//! Go has package-level scope. All declarations within a file belong to the
//! same package scope. There are no nested functions or classes at the top level.
//!
//! # Key structural notes
//!
//! - **No separate struct/interface declaration nodes** — Go uses `type_declaration`
//!   containing `type_spec` children. The type kind (struct vs interface) is
//!   determined by whether `type_spec` has a `struct_type` or `interface_type` child.
//! - **Method declarations** use `method_declaration` with a `receiver` field and
//!   `name` field (of kind `field_identifier`, not `identifier`).
//! - **Function declarations** use `function_declaration` with a `name` field
//!   (of kind `identifier`).
//! - **Imports** use `import_declaration` with `import_spec` children.
//! - **Package clause** (`package foo`) is the file-level package name, emitted
//!   as a Namespace anchor for disambiguation in multi-package directories.
//!
//! # Edge rules
//!
//! - `call_expression` → `Calls`
//! - `import_declaration` → `Imports`
//! - `identifier` → `Calls` (bare function call)

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
//
// Order: function_declaration, identifier, import_declaration, method_declaration,
//        package_clause, source_file, type_declaration

const NODE_RULES: &[NodeRule] = &[
    // ── Function ─────────────────────────────────────────────────────────────
    // Top-level function. `name` is an `identifier` child.
    NodeRule::leaf("function_declaration", NodeLabel::Function),
    // ── Identifier ───────────────────────────────────────────────────────────
    // Identifier usages in expressions (bare function calls land here when
    // they don't match a call_expression pattern). Suppression handles
    // duplicates from parent containers.
    NodeRule::leaf("identifier", NodeLabel::Variable),
    // ── Import ───────────────────────────────────────────────────────────────
    // `import_declaration` wraps `import_spec` children. Edge emitted against
    // the import spec's name (the imported package path string).
    NodeRule::leaf("import_declaration", NodeLabel::Variable),
    // ── Method ────────────────────────────────────────────────────────────────
    // Method on a type receiver. `name` is a `field_identifier` child (Go grammar
    // uses field_identifier for method names). Methods are qualified by the
    // receiver type name, handled via scope in the extractor.
    NodeRule::leaf("method_declaration", NodeLabel::Method),
    // ── Package ─────────────────────────────────────────────────────────────
    // `package foo` — establishes package-level scope. Emitted as Namespace
    // anchor so files in the same package share scope context.
    NodeRule::container("package_clause", NodeLabel::Namespace),
    // ── Root ─────────────────────────────────────────────────────────────────
    NodeRule::container("source_file", NodeLabel::Namespace),
    // ── Type ─────────────────────────────────────────────────────────────────
    // `type_declaration` wraps `type_spec` children. `type_spec` emits the
    // named type (struct or interface). We emit type_declaration as a Namespace
    // so that struct/interface fields are scoped under the type name.
    // The actual struct/interface body types are handled by the edge system
    // or suppressed by their parent's name.
    NodeRule::scope_only("type_declaration"),
];

// ── Edge rules ────────────────────────────────────────────────────────────────

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ────────────────────────────────────────────────────────────────
    EdgeRule::calls("call_expression", TargetPattern::FromNodeText),
    EdgeRule::calls("identifier", TargetPattern::FromNodeText), // bare function call
    // ── Imports ─────────────────────────────────────────────────────────────
    EdgeRule::imports("import_declaration", TargetPattern::FromNodeText),
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The Go extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::Go,
    root_kind: "source_file",
    qn_separator: ".",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
