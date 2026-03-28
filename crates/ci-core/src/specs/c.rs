//! C extraction specification.
//!
//! Grammar reference: <https://github.com/tree-sitter/tree-sitter-c>
//!
//! # Scope anchors
//!
//! | ts_kind              | ScopeSegment name | Label     |
//! |----------------------|-------------------|-----------|
//! | `translation_unit`   | (root — empty)   | Namespace |
//!
//! C has a flat global namespace — no hierarchical scopes. All symbols live
//! at translation_unit level with empty-name scope segments.
//!
//! # Notes
//!
//! - C uses a single `identifier` token type for all names (no separate
//!   `type_identifier` / `field_identifier` variants like C++).
//! - `field_declaration` nodes contain `declarator` children that hold
//!   the field names. These are emitted via `FromNodeText` on `identifier`.
//! - Preprocessor directives (`#include`, etc.) are skipped — no grammar nodes.
//! - qn_separator is `""` (empty) — C has no qualified names.

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
//
// Order: declaration, enum_specifier, field_identifier, function_definition,
//        identifier, struct_specifier, translation_unit, type_definition,
//        union_specifier

const NODE_RULES: &[NodeRule] = &[
    // ── Declarations ─────────────────────────────────────────────────────────
    // `declaration` covers function prototypes (no body), global variables,
    // and typedefs. Function definitions use `function_definition`.
    NodeRule::leaf("declaration", NodeLabel::Variable),
    // ── Enum ──────────────────────────────────────────────────────────────────
    NodeRule::leaf("enum_specifier", NodeLabel::Class), // enum Foo { ... }
    // ── Field ─────────────────────────────────────────────────────────────────
    // Inside struct/union bodies. field_identifier is the field name leaf.
    // The suppress mechanism lets the parent `field_declaration` emit the
    // struct-qualified name; bare `field_identifier` text is suppressed.
    NodeRule::leaf("field_identifier", NodeLabel::Field),
    // ── Function ───────────────────────────────────────────────────────────────
    NodeRule::leaf("function_definition", NodeLabel::Function),
    // ── Identifiers ────────────────────────────────────────────────────────────
    // Bare identifier usages are emitted as Variables for reference tracking.
    // Suppression handles the case where `identifier` is a child of a
    // container that already emitted a qualified name.
    NodeRule::leaf("identifier", NodeLabel::Variable),
    // ── Struct / Union ────────────────────────────────────────────────────────
    NodeRule::container("struct_specifier", NodeLabel::Class),
    // ── Root ──────────────────────────────────────────────────────────────────
    NodeRule::container("translation_unit", NodeLabel::Namespace),
    // ── Type definition ───────────────────────────────────────────────────────
    // `type_definition` is the typedef node (typedef int MyInt;)
    NodeRule::leaf("type_definition", NodeLabel::TypeAlias),
    // ── Union ─────────────────────────────────────────────────────────────────
    NodeRule::container("union_specifier", NodeLabel::Class),
];

// ── Edge rules ────────────────────────────────────────────────────────────────

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ────────────────────────────────────────────────────────────────
    EdgeRule::calls("call_expression", TargetPattern::FromNodeText),
    EdgeRule::calls("identifier", TargetPattern::FromNodeText), // bare function call
    // ── Uses (type annotations) ─────────────────────────────────────────────
    EdgeRule::uses("type_identifier", TargetPattern::FromNodeText),
    // ── Uses for field types ─────────────────────────────────────────────────
    EdgeRule::uses("field_declaration", TargetPattern::FromNodeText),
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The C extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::C,
    root_kind: "translation_unit",
    qn_separator: "",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
