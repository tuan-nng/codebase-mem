//! Python extraction specification.
//!
//! Grammar reference: <https://tree-sitter.github.io/tree-sitter-python>
//!
//! # Scope anchors (pushing the scope stack)
//!
//! | ts_kind                   | ScopeSegment name | Label    |
//! |---------------------------|-------------------|----------|
//! | `module`                  | (root — empty)    | Namespace|
//! | `class_definition`        | name              | Class    |
//! | `function_definition`   | name              | Function |
//! | `async_function_definition`| name             | Function |
//!
//! Python has no explicit `impl` blocks — methods are `function_definition`
//! nodes inside `class_definition` scopes. The extractor handles this via
//! the parent context in the scope stack.
//!
//! # Edge rules
//!
//! - `call` → `Calls` (function/method invocations)
//! - `import_statement` / `import_from_statement` → `Imports`
//! - `class_definition` base classes → `Inherits`
//! - `decorated_definition` → `Decorates`

use crate::lang_spec::{EdgeRule, LanguageSpec, NodeRule, TargetPattern};
use crate::Language;
use crate::NodeLabel;

// ── Node rules ────────────────────────────────────────────────────────────────
// MUST be sorted by ts_kind ascending for binary search.
// Sorted order is a GLOBAL merge of all kinds.
// Order: alias, assignment, async_function_definition, class_definition,
//        decorated_definition, function_definition, identifier, import_from_statement,
//        import_statement, module, type_alias

const NODE_RULES: &[NodeRule] = &[
    NodeRule::leaf("alias", NodeLabel::Variable), // `x as y` in imports
    NodeRule::leaf("assignment", NodeLabel::Variable), // module-level x = 1
    NodeRule::container("async_function_definition", NodeLabel::Function),
    NodeRule::container("class_definition", NodeLabel::Class),
    NodeRule::leaf("decorated_definition", NodeLabel::Function), // decorated function/class
    NodeRule::container("function_definition", NodeLabel::Function),
    NodeRule::leaf("identifier", NodeLabel::Variable), // bare names
    NodeRule::leaf("import_from_statement", NodeLabel::Variable), // imported names
    NodeRule::leaf("import_statement", NodeLabel::Variable), // imported modules
    NodeRule::container("module", NodeLabel::Namespace),
    NodeRule::leaf("type_alias", NodeLabel::TypeAlias), // PEP 695: type Foo = ...
];

// ── Edge rules ────────────────────────────────────────────────────────────────

const EDGE_RULES: &[EdgeRule] = &[
    // ── Calls ───────────────────────────────────────────────────────────────
    EdgeRule::calls("attribute", TargetPattern::FromNodeText), // obj.method()
    EdgeRule::calls("call", TargetPattern::FromNodeText),
    EdgeRule::calls("identifier", TargetPattern::FromNodeText), // bare function call
    // ── Imports ─────────────────────────────────────────────────────────────
    EdgeRule::imports("dotted_name", TargetPattern::FromNodeText),
    EdgeRule::imports("import_from_statement", TargetPattern::FromNodeText),
    EdgeRule::imports("import_statement", TargetPattern::FromNodeText),
    // ── Inheritance ──────────────────────────────────────────────────────
    EdgeRule::inherits("argument_list", TargetPattern::FromNodeText), // parent: Bar in (Bar,)
    EdgeRule::inherits("class_definition", TargetPattern::FromChildType), // class Foo(Bar):
    // ── Decorators ───────────────────────────────────────────────────────────
    EdgeRule::decorates("decorated_definition", TargetPattern::FromNodeText),
    EdgeRule::decorates("identifier", TargetPattern::FromNodeText), // standalone @decorator
];

// ── Sort check happens at runtime in specs/mod.rs ──────────────────────────────

/// The Python extraction specification.
pub const SPEC: LanguageSpec = LanguageSpec {
    lang: Language::Python,
    root_kind: "module",
    qn_separator: ".",
    node_rules: NODE_RULES,
    edge_rules: EDGE_RULES,
};
