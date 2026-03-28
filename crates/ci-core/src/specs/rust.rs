//! Rust extraction specification.
//!
//! Grammar reference: <https://tree-sitter.github.io/tree-sitter/creating-parsers/supported-languages>
//!
//! # Scope anchors (pushing the scope stack)
//!
//! | ts_kind            | ScopeSegment name | Label       |
//! |--------------------|-------------------|-------------|
//! | `source_file`      | (root — empty)    | Namespace   |
//! | `mod_item`         | identifier        | Namespace   |
//! | `struct_item`      | identifier        | Class       |
//! | `enum_item`        | identifier        | Class       |
//! | `union_item`       | identifier        | Class       |
//! | `impl_item`        | type_identifier   | (scope only) |
//! | `trait_item`       | identifier        | Trait       |
//!
//! # Node rules
//!
//! `function_item` inside `impl_item` → `NodeLabel::Method` (parent context handles this).
//! `function_item` at module level → `NodeLabel::Function`.
//!
//! # Edge rules
//!
//! - `call_expression` → `Calls`
//! - `use_declaration` → `Imports`
//! - `inherit_type` / supertrait list → `Inherits`
//! - `impl_item` → `Implements` (trait reference)
//! - `attribute_item` → `Decorates`
//! - `type_identifier` in field/parameter/return context → `Uses`

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
// Sorted order is a GLOBAL merge of all kinds (not grouped).
// Order: const_item, enum_item, field_identifier, function_item,
//        identifier, impl_item, mod_item, scoped_identifier, source_file,
//        static_item, struct_item, trait_item, type_alias_item,
//        type_item, union_item

const NODE_RULES: &[NodeRule] = &[
    NodeRule::leaf("const_item", NodeLabel::Variable), // const FOO: i32 = 1
    NodeRule::container("enum_item", NodeLabel::Class), // enum is a type namespace
    NodeRule::leaf("field_identifier", NodeLabel::Field), // struct fields
    NodeRule::leaf("function_item", NodeLabel::Function), // standalone fn
    NodeRule::leaf("identifier", NodeLabel::Variable), // bare identifiers at module level
    NodeRule::scope_only("impl_item"),                 // pushes scope for methods; no node emission
    NodeRule::container("mod_item", NodeLabel::Namespace),
    NodeRule::leaf("scoped_identifier", NodeLabel::Variable), // path segments
    NodeRule::container("source_file", NodeLabel::Namespace),
    NodeRule::leaf("static_item", NodeLabel::Variable), // static mut FOO: i32 = 0
    NodeRule::container("struct_item", NodeLabel::Class),
    NodeRule::container("trait_item", NodeLabel::Trait),
    NodeRule::leaf("type_alias_item", NodeLabel::TypeAlias),
    NodeRule::leaf("type_item", NodeLabel::TypeAlias), // `type Foo = ...`
    NodeRule::container("union_item", NodeLabel::Class),
];

// ── Edge rules ────────────────────────────────────────────────────────────────
// Edge rules don't need sorting — edge rules are looked up by linear scan.

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ────────────────────────────────────────────────────────────────
    EdgeRule::calls("call_expression", TargetPattern::FromNodeText),
    EdgeRule::calls("identifier", TargetPattern::FromNodeText), // bare function calls
    EdgeRule::calls("scoped_identifier", TargetPattern::FromNodeText), // path::function()
    // ── Imports / use declarations ───────────────────────────────────────────
    EdgeRule::imports("scoped_identifier", TargetPattern::FromNodeText),
    EdgeRule::imports("use_declaration", TargetPattern::FromNodeText),
    // ── Inheritance / implements ─────────────────────────────────────────────
    EdgeRule::implements("impl_item", TargetPattern::FromChildType), // impl<T> for Foo
    EdgeRule::inherits("generic_type", TargetPattern::FromChildType),
    EdgeRule::inherits("type_identifier", TargetPattern::FromChildType), // struct Foo<T: Bar>
    // ── Uses / type annotations ──────────────────────────────────────────────
    EdgeRule::uses("field_identifier", TargetPattern::FromNodeText), // field type annotation
    EdgeRule::uses("generic_type", TargetPattern::FromChildType),
    EdgeRule::uses("type_identifier", TargetPattern::FromNodeText),
    // ── Decorators / attributes ────────────────────────────────────────────────
    EdgeRule::decorates("attribute_item", TargetPattern::FromNodeText),
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The Rust extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::Rust,
    root_kind: "source_file",
    qn_separator: "::",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
