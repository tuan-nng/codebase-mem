//! Kotlin extraction specification.
//!
//! Grammar reference: <https://github.com/tree-sitter/tree-sitter-kotlin>
//!
//! # Scope anchors
//!
//! | ts_kind              | ScopeSegment name | Label       |
//! |----------------------|-------------------|-------------|
//! | `source_file`        | (root — empty)   | Namespace    |
//! | `class_declaration`  | name field       | Class        |
//! | `object_declaration` | name field       | Class        |
//!
//! Kotlin is noteworthy because `class_declaration` serves for both classes
//! AND interfaces — the distinction is made by an anonymous `interface` keyword
//! child, not by a different node type. Both are treated as `NodeLabel::Class`
//! in the spec (interface-specific mapping would require extractor support for
//! keyword-child detection, which is future work).
//!
//! # Edge rules
//!
//! - `call_expression` → `Calls`
//! - `identifier` → `Calls` (bare function call)
//! - `import` → `Imports`
//! - `annotation` → `Decorates`
//! - `delegation_specifiers` → `Inherits` (class inheritance via `:` syntax)
//!
//! # Notes
//!
//! - Kotlin uses `function_declaration` for all functions — both top-level
//!   and member functions share the same node type.
//! - Method names on types use `identifier` children (not a separate field).
//! - `property_declaration` covers `val`/`var` fields. The name is inside a
//!   nested `variable_declaration` child.
//! - `type_alias` emits a `TypeAlias` node. Its `type` field contains the alias name.
//! - `object_declaration` is Kotlin's singleton pattern, treated as a Class.
//! - `companion_object` inside a class is a special object, treated as Namespace.

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
//
// Order: annotation, class_declaration, companion_object,
//        function_declaration, identifier, import, object_declaration,
//        property_declaration, source_file, type_alias

const NODE_RULES: &[NodeRule] = &[
    // ── Annotation ───────────────────────────────────────────────────────────
    // Both usage annotations (`@MyAnno`) and `@interface` declaration annotations
    // share this node. For usage: Decorates edge to the annotation type.
    // For declaration: emits a Function node representing the annotation.
    NodeRule::leaf("annotation", NodeLabel::Function),
    // ── Class ────────────────────────────────────────────────────────────────
    // Both `class Foo` and `interface Foo` use `class_declaration`. The grammar
    // distinguishes them by an anonymous `interface` keyword child. We emit
    // as Class — a future extractor enhancement could distinguish via keyword
    // child inspection.
    NodeRule::container("class_declaration", NodeLabel::Class),
    // ── Companion object ─────────────────────────────────────────────────────
    // `companion object` inside a class — Kotlin's companion object pattern.
    // Emitted as Namespace so its members are scoped under it.
    NodeRule::container("companion_object", NodeLabel::Namespace),
    // ── Function ─────────────────────────────────────────────────────────────
    // All functions — top-level, member, and local — use this node type.
    // Inside a class_declaration scope, the enclosing class qualifies the name.
    NodeRule::leaf("function_declaration", NodeLabel::Function),
    // ── Identifier ───────────────────────────────────────────────────────────
    // Identifier usages. Suppression handles duplicates from parent containers.
    NodeRule::leaf("identifier", NodeLabel::Variable),
    // ── Import ───────────────────────────────────────────────────────────────
    NodeRule::leaf("import", NodeLabel::Variable),
    // ── Object ───────────────────────────────────────────────────────────────
    // Kotlin singleton object declaration. Treated as a Class.
    NodeRule::container("object_declaration", NodeLabel::Class),
    // ── Property ─────────────────────────────────────────────────────────────
    // `val`/`var` field declarations. The actual name is in a nested
    // `variable_declaration` child. We emit the property as Field directly.
    NodeRule::leaf("property_declaration", NodeLabel::Field),
    // ── Root ─────────────────────────────────────────────────────────────────
    NodeRule::container("source_file", NodeLabel::Namespace),
    // ── Type alias ───────────────────────────────────────────────────────────
    NodeRule::leaf("type_alias", NodeLabel::TypeAlias),
];

// ── Edge rules ────────────────────────────────────────────────────────────────

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ────────────────────────────────────────────────────────────────
    EdgeRule::calls("call_expression", TargetPattern::FromNodeText),
    EdgeRule::calls("identifier", TargetPattern::FromNodeText), // bare function call
    // ── Imports ─────────────────────────────────────────────────────────────
    EdgeRule::imports("import", TargetPattern::FromNodeText),
    // ── Decorators ───────────────────────────────────────────────────────────
    // Kotlin annotations applied to declarations.
    EdgeRule::decorates("annotation", TargetPattern::FromNodeText),
    // ── Inheritance ──────────────────────────────────────────────────────────
    // `delegation_specifiers` contains the base type(s) after `:` in class Foo : Bar()
    EdgeRule::inherits("delegation_specifiers", TargetPattern::FromChildType),
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The Kotlin extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::Kotlin,
    root_kind: "source_file",
    qn_separator: ".",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
