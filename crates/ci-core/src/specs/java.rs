//! Java extraction specification.
//!
//! Grammar reference: <https://github.com/tree-sitter/tree-sitter-java>
//!
//! # Scope anchors
//!
//! | ts_kind              | ScopeSegment name | Label     |
//! |----------------------|-------------------|-----------|
//! | `program`            | (root — empty)   | Namespace |
//! | `class_declaration`  | name field       | Class     |
//! | `interface_declaration` | name field    | Interface |
//! | `enum_declaration`   | name field       | Class     |
//! | `record_declaration` | name field       | Class     |
//!
//! Java has package-level scope, class/interface/enum/record scope, and method scope.
//!
//! # Edge rules
//!
//! - `method_invocation` → `Calls` (Java's call expression, not call_expression)
//! - `identifier` → `Calls` (bare method call within the same class)
//! - `import_declaration` → `Imports`
//! - `superclass` → `Inherits` (extends)
//! - `super_interfaces` → `Implements` (implements)
//! - `annotation` → `Decorates` (for annotations applied to declarations)
//!
//! # Notes
//!
//! - Java call expressions are `method_invocation`, not `call_expression`.
//!   The method name is in a `name` field pointing to an `identifier`.
//! - Qualified names use `scoped_identifier` (recursive: a.b.c).
//! - Annotations (`@Override`, etc.) are both type usages and decorator edges.

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
//
// Order: annotation_type_declaration, class_declaration, enum_declaration,
//        field_declaration, identifier, import_declaration, interface_declaration,
//        method_declaration, program, record_declaration, variable_declarator

const NODE_RULES: &[NodeRule] = &[
    // ── Annotation type ──────────────────────────────────────────────────────
    // `@interface Foo` — annotation type declaration. Treat as Interface.
    NodeRule::container("annotation_type_declaration", NodeLabel::Interface),
    // ── Class ────────────────────────────────────────────────────────────────
    NodeRule::container("class_declaration", NodeLabel::Class),
    // ── Enum ─────────────────────────────────────────────────────────────────
    // Java enum is a class with enum constants. Treat as Class.
    NodeRule::container("enum_declaration", NodeLabel::Class),
    // ── Field ───────────────────────────────────────────────────────────────
    // `field_declaration` emits the field node; inside it, `variable_declarator`
    // holds the actual field name. We emit field_declaration as Field directly.
    NodeRule::leaf("field_declaration", NodeLabel::Field),
    // ── Identifier ───────────────────────────────────────────────────────────
    // Identifier usages in expressions. Suppression prevents duplicates from
    // parent containers that already emit qualified names.
    NodeRule::leaf("identifier", NodeLabel::Variable),
    // ── Import ───────────────────────────────────────────────────────────────
    NodeRule::leaf("import_declaration", NodeLabel::Variable),
    // ── Interface ────────────────────────────────────────────────────────────
    NodeRule::container("interface_declaration", NodeLabel::Interface),
    // ── Method ───────────────────────────────────────────────────────────────
    // Method inside a class. Qualified by the enclosing class name via scope stack.
    // Constructor is also a `method_declaration` — indistinguishable by node type.
    NodeRule::leaf("method_declaration", NodeLabel::Method),
    // ── Root ─────────────────────────────────────────────────────────────────
    NodeRule::container("program", NodeLabel::Namespace),
    // ── Record ───────────────────────────────────────────────────────────────
    // Java record (compact data class). Treated as Class.
    NodeRule::container("record_declaration", NodeLabel::Class),
    // ── Variable declarator ──────────────────────────────────────────────────
    // Inside `field_declaration`, `variable_declarator` holds the name.
    // Suppression handles the case where the parent already emitted.
    NodeRule::leaf("variable_declarator", NodeLabel::Variable),
];

// ── Edge rules ────────────────────────────────────────────────────────────────

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ────────────────────────────────────────────────────────────────
    // Java's call expression is `method_invocation`, not `call_expression`.
    EdgeRule::calls("method_invocation", TargetPattern::FromNodeText),
    EdgeRule::calls("identifier", TargetPattern::FromNodeText), // bare call
    // ── Imports ─────────────────────────────────────────────────────────────
    EdgeRule::imports("import_declaration", TargetPattern::FromNodeText),
    // ── Inheritance / Implements ────────────────────────────────────────────
    // `superclass` is a field of `class_declaration` containing the parent type.
    EdgeRule::inherits("superclass", TargetPattern::FromChildType),
    // `super_interfaces` is a field containing the implemented interface types.
    EdgeRule::implements("super_interfaces", TargetPattern::FromChildType),
    // ── Decorators ───────────────────────────────────────────────────────────
    // Java annotations are `@interface` types applied via `@` syntax.
    // Both `annotation` (with args) and `marker_annotation` (no args) link to Decorates.
    EdgeRule::decorates("annotation", TargetPattern::FromNodeText),
    EdgeRule::decorates("marker_annotation", TargetPattern::FromNodeText),
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The Java extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::Java,
    root_kind: "program",
    qn_separator: ".",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
