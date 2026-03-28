//! TypeScript extraction specification.
//!
//! Grammar reference: <https://github.com/tree-sitter/tree-sitter-typescript>
//!
//! # Scope anchors
//!
//! | ts_kind                      | ScopeSegment name | Label     |
//! |-----------------------------|-------------------|-----------|
//! | `program`                    | (root — empty)   | Namespace |
//! | `class_declaration`           | name field       | Class     |
//! | `abstract_class_declaration`  | name field       | Class     |
//! | `interface_declaration`       | name field       | Interface |
//!
//! TypeScript has module-level scope, class scope, and interface scope.
//!
//! # Edge rules
//!
//! - `call_expression` → `Calls`
//! - `member_expression` → `Calls` (obj.method() style)
//! - `identifier` → `Calls` (bare function call)
//! - `import_statement` → `Imports`
//! - `decorator` → `Decorates`
//! - `extends_clause` → `Inherits` (class extends)
//! - `extends_type_clause` → `Inherits` (interface extends)
//! - `implements_clause` → `Implements`
//!
//! # Notes
//!
//! - TypeScript uses `identifier` for function declaration names but
//!   `type_identifier` for class/interface/type alias names.
//! - Method names inside classes use `property_identifier`.
//! - `function_declaration` inside a class_body is emitted as Function, not Method.
//!   The class scope is the enclosing scope, so it appears as e.g. `Class.method`.
//! - `type_alias_declaration` maps to `TypeAlias` (PEP 613 style type aliases).
//! - Decorators are attached as a `decorator` field on declarations. No node rule
//!   is needed — the Decorates edge links the declaration to its decorator.

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
//
// Order: abstract_class_declaration, class_declaration, function_declaration,
//        identifier, import_statement, interface_declaration, program,
//        property_identifier, type_alias_declaration

const NODE_RULES: &[NodeRule] = &[
    // ── Class ────────────────────────────────────────────────────────────────
    NodeRule::container("abstract_class_declaration", NodeLabel::Class),
    NodeRule::container("class_declaration", NodeLabel::Class),
    // ── Function ─────────────────────────────────────────────────────────────
    NodeRule::container("function_declaration", NodeLabel::Function),
    // ── Identifier ───────────────────────────────────────────────────────────
    NodeRule::leaf("identifier", NodeLabel::Variable),
    // ── Import ───────────────────────────────────────────────────────────────
    NodeRule::leaf("import_statement", NodeLabel::Variable),
    // ── Interface ────────────────────────────────────────────────────────────
    NodeRule::container("interface_declaration", NodeLabel::Interface),
    // ── Root ─────────────────────────────────────────────────────────────────
    NodeRule::container("program", NodeLabel::Namespace),
    // ── Property ─────────────────────────────────────────────────────────────
    // `property_identifier` is the name of a property/method inside a class.
    // Suppression handles duplicates: parent `method_definition` already emits
    // the qualified name; bare `property_identifier` text is suppressed.
    NodeRule::leaf("property_identifier", NodeLabel::Field),
    // ── Type alias ───────────────────────────────────────────────────────────
    NodeRule::leaf("type_alias_declaration", NodeLabel::TypeAlias),
];

// ── Edge rules ────────────────────────────────────────────────────────────────

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ────────────────────────────────────────────────────────────────
    EdgeRule::calls("call_expression", TargetPattern::FromNodeText),
    EdgeRule::calls("member_expression", TargetPattern::FromNodeText), // obj.method()
    EdgeRule::calls("identifier", TargetPattern::FromNodeText),        // bare function call
    // ── Imports ─────────────────────────────────────────────────────────────
    EdgeRule::imports("import_statement", TargetPattern::FromNodeText),
    // ── Inheritance / Implements ───────────────────────────────────────────────
    EdgeRule::inherits("extends_clause", TargetPattern::FromChildType), // class extends
    EdgeRule::inherits("extends_type_clause", TargetPattern::FromChildType), // interface extends
    EdgeRule::implements("implements_clause", TargetPattern::FromChildType),
    // ── Decorators ────────────────────────────────────────────────────────────
    EdgeRule::decorates("decorator", TargetPattern::FromNodeText),
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The TypeScript extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::TypeScript,
    root_kind: "program",
    qn_separator: ".",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
