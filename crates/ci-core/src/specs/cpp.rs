//! C++ extraction specification.
//!
//! Grammar reference: <https://github.com/tree-sitter/tree-sitter-cpp>
//!
//! # Scope anchors
//!
//! | ts_kind              | ScopeSegment name | Label     |
//! |---------------------|-------------------|-----------|
//! | `translation_unit`    | (root — empty)   | Namespace |
//! | `namespace_definition` | name field      | Namespace |
//! | `class_specifier`    | name field       | Class     |
//! | `struct_specifier`   | name field       | Class     |
//!
//! C++ has three levels of scope relevant to extraction:
//! 1. Global (translation_unit) — flat unless wrapped in a namespace
//! 2. Namespace — `namespace Foo { ... }` establishes a named scope
//! 3. Class/struct — methods and fields are qualified by the class name
//!
//! Both member functions (inside class_body) and standalone functions use
//! `function_definition`. The extractor qualifies them by the parent scope
//! (class or namespace) automatically via the scope stack.
//!
//! # Edge rules
//!
//! - `call_expression` → `Calls`
//! - `identifier` → `Calls` (bare function calls)
//! - `base_class_clause` → `Inherits` (via type child)
//! - `field_declaration` → `Uses` (type annotations)
//!
//! # Notes
//!
//! - Unlike C, C++ has separate `_type_identifier` and `_field_identifier` leaf
//!   node types, in addition to plain `identifier`.
//! - Templates (`template_declaration`) are not yet mapped — the generic type
//!   parameter extraction requires more complex handling in the extractor.

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
//
// Order: class_specifier, enum_specifier, field_identifier, function_definition,
//        identifier, namespace_definition, struct_specifier, translation_unit
//        (sorted by ts_kind: class < enum < field < function < identifier < namespace < struct < translation)

const NODE_RULES: &[NodeRule] = &[
    // ── Class ────────────────────────────────────────────────────────────────
    NodeRule::container("class_specifier", NodeLabel::Class),
    // ── Enum ─────────────────────────────────────────────────────────────────
    // Covers both `enum Name` and `enum class Name`.
    NodeRule::leaf("enum_specifier", NodeLabel::Class),
    // ── Field ────────────────────────────────────────────────────────────────
    NodeRule::leaf("field_identifier", NodeLabel::Field),
    // ── Function / Method ────────────────────────────────────────────────────
    // Both standalone functions and class methods use `function_definition`.
    // Qualified by enclosing namespace/class scope via the scope stack.
    NodeRule::leaf("function_definition", NodeLabel::Function),
    // ── Identifier ──────────────────────────────────────────────────────────
    // Bare identifier references at namespace scope. Suppression prevents
    // duplicates when an identifier is the child of a node that already emits.
    NodeRule::leaf("identifier", NodeLabel::Variable),
    // ── Namespace ───────────────────────────────────────────────────────────
    NodeRule::container("namespace_definition", NodeLabel::Namespace),
    // ── Struct ──────────────────────────────────────────────────────────────
    // C++ struct is semantically a class with public default access.
    NodeRule::container("struct_specifier", NodeLabel::Class),
    // ── Root ─────────────────────────────────────────────────────────────────
    NodeRule::container("translation_unit", NodeLabel::Namespace),
];

// ── Edge rules ────────────────────────────────────────────────────────────────

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ────────────────────────────────────────────────────────────────
    EdgeRule::calls("call_expression", TargetPattern::FromNodeText),
    EdgeRule::calls("identifier", TargetPattern::FromNodeText), // bare function call
    // ── Inheritance ──────────────────────────────────────────────────────────
    // base_class_clause contains the base class type. The FromChildType
    // pattern finds the type_identifier child with the base class name.
    EdgeRule::inherits("base_class_clause", TargetPattern::FromChildType),
    // ── Uses ────────────────────────────────────────────────────────────────
    // Field declarations reference types. Extract via FromNodeText to capture
    // the type identifier used in the field declaration.
    EdgeRule::uses("field_declaration", TargetPattern::FromNodeText),
    EdgeRule::uses("type_identifier", TargetPattern::FromNodeText),
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The C++ extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::Cpp,
    root_kind: "translation_unit",
    qn_separator: "::",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
