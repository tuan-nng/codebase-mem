//! Declarative language extraction specifications.
//!
//! Defines the data model for mapping tree-sitter AST nodes to graph nodes
//! and edges — pure data, zero logic. Language specs are `const` values
//! owned by `ci-core`; the extraction engine in `ci-parser` interprets them.
//!
//! # Architecture
//!
//! ```text
//! tree-sitter Tree
//!        |
//!        v
//! ci-parser: Extractor  (interprets LanguageSpec)
//!        |
//!        v
//! Iterator<Item = ExtractedItem>  (decoupled from graph)
//!        |
//!        v
//! ci-graph: Builder  (converts ExtractedItem → MutableGraph nodes/edges)
//! ```
//!
//! # Adding a new language
//!
//! 1. Create `src/specs/<lang>.rs` with a `LANG_SPEC: LanguageSpec` const
//! 2. Add `mod <lang>;` and one match arm to `src/specs/mod.rs`
//! 3. Add tests verifying ts_kind strings match actual grammar output
//! 4. Zero changes needed anywhere else in the codebase

use rkyv::{Archive, Deserialize, Serialize};

use crate::{EdgeType, Language, NodeLabel};

// ── ScopeKey ─────────────────────────────────────────────────────────────────

/// The identity of a lexical scope, used to disambiguate symbol names.
///
/// Constructed by the extractor during tree traversal. Each scope-anchor node
/// pushes a [`ScopeSegment`] onto the stack. `qualify()` assembles the
/// qualified name from the stack.
///
/// Two nodes in the same scope produce identical `ScopeKey` values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[allow(dead_code)]
pub struct ScopeKey {
    /// Ordered from outermost scope inward, e.g. `[("source_file", ""), ("mod_item", "foo")]`.
    segments: Vec<ScopeSegment>,
}

impl ScopeKey {
    #[allow(dead_code)] // used by extractor (ci-parser) and unit tests
    /// Create an empty root scope (module/file level).
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Returns the qualified name for a local `name` within this scope.
    ///
    /// Non-empty segments are joined with `separator`, then `name` is appended.
    /// Empty-segment names (e.g. the root `source_file`) are skipped.
    ///
    /// ```
    /// use ci_core::{ScopeKey, ScopeSegment};
    ///
    /// // Module-level: a struct inside a mod
    /// let mut key = ScopeKey::new();
    /// key.push(ScopeSegment::new("mod_item".into(), "foo".into()));
    /// key.push(ScopeSegment::new("struct_item".into(), "Bar".into()));
    /// assert_eq!(key.qualify("baz", "::"), "foo::Bar::baz");
    ///
    /// // Top-level: no segments, name is returned as-is
    /// let root = ScopeKey::new();
    /// assert_eq!(root.qualify("add", "::"), "add");
    /// ```
    pub fn qualify(&self, name: &str, separator: &str) -> String {
        let non_empty: Vec<_> = self
            .segments
            .iter()
            .filter(|s| !s.name.is_empty())
            .collect();
        if non_empty.is_empty() {
            return name.to_string();
        }
        let mut result = non_empty
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(separator);
        result.push_str(separator);
        result.push_str(name);
        result
    }

    /// Number of segments on the stack.
    #[inline]
    pub fn depth(&self) -> usize {
        self.segments.len()
    }

    /// Returns the outermost (root) segment, if any.
    #[inline]
    pub fn root_segment(&self) -> Option<&ScopeSegment> {
        self.segments.first()
    }

    /// Returns the innermost (current) segment, if any.
    #[inline]
    pub fn current_segment(&self) -> Option<&ScopeSegment> {
        self.segments.last()
    }

    /// Returns an iterator over all segments, outermost first.
    #[inline]
    pub fn segments(&self) -> impl Iterator<Item = &ScopeSegment> {
        self.segments.iter()
    }

    /// Returns the nth segment from the root (0 = outermost), or `None` if out of range.
    #[inline]
    pub fn nth_from_root(&self, n: usize) -> Option<&ScopeSegment> {
        self.segments.get(n)
    }

    /// Returns the nth segment from the current scope (0 = innermost), or `None`.
    #[inline]
    pub fn nth_from_current(&self, n: usize) -> Option<&ScopeSegment> {
        self.segments.iter().rev().nth(n)
    }

    /// Push a new scope segment (called by the extractor on scope-anchor entry).
    pub fn push(&mut self, segment: ScopeSegment) {
        self.segments.push(segment);
    }

    /// Pop the innermost scope segment (called by the extractor on scope-anchor exit).
    pub fn pop(&mut self) -> Option<ScopeSegment> {
        self.segments.pop()
    }

    #[cfg(test)]
    pub(crate) fn from_segments(segments: Vec<ScopeSegment>) -> Self {
        Self { segments }
    }
}

/// A single lexical scope level — the kind of the anchor node and its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeSegment {
    /// The tree-sitter node kind that established this scope.
    /// e.g. `"source_file"`, `"mod_item"`, `"struct_item"`, `"impl_item"`.
    pub kind: String,
    /// The bare name of this scope (function name, module name, etc.).
    /// Empty string for anonymous scopes (e.g. the root file node).
    pub name: String,
}

impl ScopeSegment {
    /// Create a new scope segment.
    pub fn new(kind: String, name: String) -> Self {
        Self { kind, name }
    }
}

// ── ExtractedItem ─────────────────────────────────────────────────────────────

/// The atomic output unit of the extraction engine.
///
/// `ExtractedItem` is the single, decoupled contract between the
/// `ci-parser` extractor and the `ci-graph` builder. It contains no
/// graph types (`NodeId`, `InternedStr`) — the builder interns strings
/// and assigns IDs.
///
/// `ExtractedItem` is `Clone` but the intended use is pass-by-reference
/// via the `Iterator` interface.
#[derive(Debug, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(attr(derive(Debug, PartialEq)))]
pub enum ExtractedItem {
    /// Declare a named symbol node in the graph.
    Node(ExtractedNode),
    /// Declare a directed relationship between two symbol nodes.
    Edge(ExtractedEdge),
}

impl Clone for ExtractedItem {
    fn clone(&self) -> Self {
        match self {
            ExtractedItem::Node(n) => ExtractedItem::Node(n.clone()),
            ExtractedItem::Edge(e) => ExtractedItem::Edge(e.clone()),
        }
    }
}

/// A named symbol node produced by extraction.
#[derive(Debug, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(attr(derive(Debug, PartialEq)))]
pub struct ExtractedNode {
    /// The graph node label (Function, Class, Method, etc.).
    pub label: NodeLabel,
    /// Fully qualified name assembled from the scope chain.
    /// e.g. `"foo::Bar::baz"`, `"MyClass.do_thing"`.
    pub qualified_name: String,
    /// Raw tree-sitter node kind string for debugging and spec authoring.
    /// e.g. `"function_item"`, `"class_definition"`.
    pub ts_kind: String,
    /// 0-based byte offset of the node's start position in the source.
    pub start_byte: u32,
    /// 0-based byte offset of the node's end position in the source.
    pub end_byte: u32,
    /// 1-based source line number (0 = unknown/unavailable).
    pub line: u32,
    /// 1-based source column number (0 = unknown/unavailable).
    pub column: u32,
}

impl Clone for ExtractedNode {
    fn clone(&self) -> Self {
        Self {
            label: self.label,
            qualified_name: self.qualified_name.clone(),
            ts_kind: self.ts_kind.clone(),
            start_byte: self.start_byte,
            end_byte: self.end_byte,
            line: self.line,
            column: self.column,
        }
    }
}

/// A directed edge between two symbol nodes.
#[derive(Debug, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(attr(derive(Debug, PartialEq)))]
pub struct ExtractedEdge {
    /// Fully qualified name of the source node.
    pub source_qualified: String,
    /// Fully qualified name of the target node.
    pub target_qualified: String,
    /// The kind of relationship (Calls, Imports, Inherits, etc.).
    pub edge_type: EdgeType,
}

impl Clone for ExtractedEdge {
    fn clone(&self) -> Self {
        Self {
            source_qualified: self.source_qualified.clone(),
            target_qualified: self.target_qualified.clone(),
            edge_type: self.edge_type,
        }
    }
}

// ── NodeRule ─────────────────────────────────────────────────────────────────

/// A declarative rule: "when you see a tree-sitter node of kind `ts_kind`,
/// emit a graph node of label `label`."
///
/// Node rules are kept in a **sorted slice** for O(log n) binary search
/// by the extractor. Must be sorted by `ts_kind` ascending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRule {
    /// tree-sitter node kind, e.g. `"function_item"`, `"class_definition"`.
    pub ts_kind: &'static str,
    /// Graph label for the emitted node.
    pub label: NodeLabel,
    /// Whether this node establishes a new lexical scope.
    /// If `true`, the extractor pushes a [`ScopeSegment`] on enter and pops on leave.
    pub scope_anchor: bool,
    /// Whether this rule should be skipped when emitting edge rules.
    /// Default `false`. Set to `true` for anonymous/wrapper nodes.
    pub skip_edges: bool,
    /// Whether to skip emitting the node itself (only push/pop scope).
    /// Useful for nodes like `impl_item` that establish scope but whose name
    /// would duplicate a sibling/parent's node emission.
    pub skip_node: bool,
}

impl NodeRule {
    /// Construct a node rule for a leaf symbol type.
    pub const fn leaf(ts_kind: &'static str, label: NodeLabel) -> Self {
        Self {
            ts_kind,
            label,
            scope_anchor: false,
            skip_edges: false,
            skip_node: false,
        }
    }

    /// Construct a node rule for a container/namespace type (scope anchor).
    pub const fn container(ts_kind: &'static str, label: NodeLabel) -> Self {
        Self {
            ts_kind,
            label,
            scope_anchor: true,
            skip_edges: false,
            skip_node: false,
        }
    }

    /// Construct a rule that only establishes a lexical scope without emitting a node.
    /// Use for nodes like `impl_item` that qualify descendants but whose own name
    /// would duplicate a sibling or parent's symbol.
    pub const fn scope_only(ts_kind: &'static str) -> Self {
        Self {
            ts_kind,
            label: NodeLabel::Namespace,
            scope_anchor: true,
            skip_edges: false,
            skip_node: true,
        }
    }

    /// Mark this rule as skip_edges (e.g. for anonymous wrapper nodes).
    pub const fn with_skip_edges(mut self) -> Self {
        self.skip_edges = true;
        self
    }
}

// ── EdgeRule ─────────────────────────────────────────────────────────────────

/// How to extract the target name from a tree-sitter node for an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPattern {
    /// The target name is the first matching child node's text.
    /// e.g. `struct Foo;` → "Foo" via `type_identifier` child.
    FromChildType,
    /// The target name is the node's own text.
    /// e.g. `use foo::bar;` → "bar" (the identifier child).
    FromNodeText,
    /// The target is a special identifier resolved by the extractor.
    /// e.g. `super` → resolve to the enclosing class/struct name.
    ResolveSpecial,
}

/// A declarative rule: "when you visit a tree-sitter node of kind `source_kind`,
/// emit a [`EdgeType::Calls` / `Imports` / `Inherits` / ...] edge."
///
/// Edge rules are evaluated during tree traversal. The extractor resolves
/// the target name from the current node using `target_pattern`, then
/// looks up the corresponding symbol node by qualified name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeRule {
    /// The tree-sitter node kind that triggers this edge.
    pub source_kind: &'static str,
    /// The edge type to emit.
    pub edge_type: EdgeType,
    /// How to extract the target symbol name from the node.
    pub target_pattern: TargetPattern,
}

impl EdgeRule {
    /// Construct a call edge rule.
    pub const fn calls(source_kind: &'static str, target_pattern: TargetPattern) -> Self {
        Self {
            source_kind,
            edge_type: EdgeType::Calls,
            target_pattern,
        }
    }

    /// Construct an import edge rule.
    pub const fn imports(source_kind: &'static str, target_pattern: TargetPattern) -> Self {
        Self {
            source_kind,
            edge_type: EdgeType::Imports,
            target_pattern,
        }
    }

    /// Construct an inheritance edge rule.
    pub const fn inherits(source_kind: &'static str, target_pattern: TargetPattern) -> Self {
        Self {
            source_kind,
            edge_type: EdgeType::Inherits,
            target_pattern,
        }
    }

    /// Construct an implements edge rule.
    pub const fn implements(source_kind: &'static str, target_pattern: TargetPattern) -> Self {
        Self {
            source_kind,
            edge_type: EdgeType::Implements,
            target_pattern,
        }
    }

    /// Construct a decorator/annotation edge rule.
    pub const fn decorates(source_kind: &'static str, target_pattern: TargetPattern) -> Self {
        Self {
            source_kind,
            edge_type: EdgeType::Decorates,
            target_pattern,
        }
    }

    /// Construct a uses/type-annotation edge rule.
    pub const fn uses(source_kind: &'static str, target_pattern: TargetPattern) -> Self {
        Self {
            source_kind,
            edge_type: EdgeType::Uses,
            target_pattern,
        }
    }
}

// ── LanguageSpec ─────────────────────────────────────────────────────────────

/// The complete declarative extraction specification for one programming language.
///
/// All fields are `const` data — zero heap allocation, zero logic. Defined
/// as a `const` in `src/specs/<lang>.rs` and stored in a `LazyLock` map
/// keyed by `Language` discriminant.
///
/// # Memory
///
/// A `LanguageSpec` is small: the string slices are `&'static str`
/// and all arrays are `&'static [&'static str]`. Total heap allocation: zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageSpec {
    /// The language this spec targets.
    pub lang: Language,
    /// tree-sitter root node kind for this language's grammar.
    /// e.g. `"source_file"` for Rust, `"module"` for Python.
    pub root_kind: &'static str,
    /// Separator used to join scope segments in qualified names.
    /// Rust: `"::"`, Python: `"."`, Go: `"."`.
    pub qn_separator: &'static str,
    /// Node extraction rules, **must be sorted by `ts_kind` ascending** for binary search.
    pub node_rules: &'static [NodeRule],
    /// Edge extraction rules.
    pub edge_rules: &'static [EdgeRule],
}

impl LanguageSpec {
    /// Look up a node rule by tree-sitter kind using binary search.
    /// Returns `None` for anonymous/unrecognized node kinds.
    ///
    /// # Panics
    ///
    /// Panics if `node_rules` is not sorted by `ts_kind`. This is enforced
    /// at LazyLock initialization in `specs/mod.rs` via `check_sorted()`.
    pub fn get_node_rule(&self, ts_kind: &str) -> Option<&'static NodeRule> {
        self.node_rules
            .binary_search_by(|r| r.ts_kind.cmp(ts_kind))
            .ok()
            .map(|idx| &self.node_rules[idx])
    }

    /// Returns the `NodeRule` for a scope-anchor kind, if one exists.
    #[inline]
    pub fn get_scope_anchor(&self, ts_kind: &str) -> Option<&'static NodeRule> {
        self.get_node_rule(ts_kind).filter(|r| r.scope_anchor)
    }

    /// Look up an edge rule by source tree-sitter kind.
    /// Returns the first matching rule (linear scan; at most ~10 edge rules per language).
    #[inline]
    pub fn get_edge_rule(&self, source_kind: &str) -> Option<&'static EdgeRule> {
        self.edge_rules
            .iter()
            .find(|r| r.source_kind == source_kind)
    }

    /// Returns `true` if `ts_kind` is the root node kind.
    #[inline]
    pub fn is_root_kind(&self, ts_kind: &str) -> bool {
        ts_kind == self.root_kind
    }
}

// ── Spec registry ─────────────────────────────────────────────────────────────

/// Returns the spec for `lang`, or `None` if no spec is registered.
///
/// Adding a new language: create `src/specs/<lang>.rs`, add `mod <lang>;`
/// to `src/specs/mod.rs`, and add one match arm here.
pub fn spec_for(lang: Language) -> Option<&'static LanguageSpec> {
    crate::specs::get_spec(lang)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ScopeKey ──────────────────────────────────────────────────────────────

    mod scope_key {
        use super::*;

        #[test]
        fn qualify_nested_scope() {
            let key = ScopeKey::from_segments(vec![
                ScopeSegment::new("source_file".into(), String::new()),
                ScopeSegment::new("mod_item".into(), "foo".into()),
                ScopeSegment::new("impl_item".into(), "Bar".into()),
            ]);
            assert_eq!(key.qualify("baz", "::"), "foo::Bar::baz");
        }

        #[test]
        fn qualify_single_scope() {
            let key = ScopeKey::from_segments(vec![ScopeSegment::new(
                "source_file".into(),
                "utils".into(),
            )]);
            assert_eq!(key.qualify("helper", "::"), "utils::helper");
        }

        #[test]
        fn qualify_top_level() {
            let key = ScopeKey::new();
            assert_eq!(key.qualify("add", "::"), "add");
        }

        #[test]
        fn qualify_skips_empty_segments() {
            let key = ScopeKey::from_segments(vec![
                ScopeSegment::new("source_file".into(), String::new()),
                ScopeSegment::new("mod_item".into(), "foo".into()),
            ]);
            assert_eq!(key.qualify("bar", "::"), "foo::bar");
        }

        #[test]
        fn depth() {
            let key = ScopeKey::from_segments(vec![
                ScopeSegment::new("source_file".into(), String::new()),
                ScopeSegment::new("mod_item".into(), "foo".into()),
            ]);
            assert_eq!(key.depth(), 2);
        }

        #[test]
        fn root_segment() {
            let key = ScopeKey::from_segments(vec![
                ScopeSegment::new("source_file".into(), String::new()),
                ScopeSegment::new("mod_item".into(), "foo".into()),
            ]);
            assert_eq!(key.root_segment().map(|s| s.name.as_str()), Some(""));
            assert_eq!(key.current_segment().map(|s| s.name.as_str()), Some("foo"));
        }

        #[test]
        fn nth_from_root() {
            let key = ScopeKey::from_segments(vec![
                ScopeSegment::new("source_file".into(), String::new()),
                ScopeSegment::new("mod_item".into(), "foo".into()),
                ScopeSegment::new("struct_item".into(), "Bar".into()),
            ]);
            assert_eq!(key.nth_from_root(0).map(|s| s.name.as_str()), Some(""));
            assert_eq!(key.nth_from_root(1).map(|s| s.name.as_str()), Some("foo"));
            assert_eq!(key.nth_from_root(2).map(|s| s.name.as_str()), Some("Bar"));
            assert_eq!(key.nth_from_root(3), None);
        }

        #[test]
        fn nth_from_current() {
            let key = ScopeKey::from_segments(vec![
                ScopeSegment::new("source_file".into(), String::new()),
                ScopeSegment::new("mod_item".into(), "foo".into()),
                ScopeSegment::new("struct_item".into(), "Bar".into()),
            ]);
            assert_eq!(
                key.nth_from_current(0).map(|s| s.name.as_str()),
                Some("Bar")
            ); // innermost first
            assert_eq!(
                key.nth_from_current(1).map(|s| s.name.as_str()),
                Some("foo")
            );
            assert_eq!(key.nth_from_current(2).map(|s| s.name.as_str()), Some(""));
            assert_eq!(key.nth_from_current(3), None);
        }

        #[test]
        fn push_pop() {
            let mut key = ScopeKey::new();
            assert_eq!(key.depth(), 0);

            key.push(ScopeSegment::new("mod_item".into(), "foo".into()));
            assert_eq!(key.depth(), 1);
            assert_eq!(key.current_segment().map(|s| s.name.as_str()), Some("foo"));

            key.push(ScopeSegment::new("struct_item".into(), "Bar".into()));
            assert_eq!(key.depth(), 2);
            assert_eq!(key.qualify("baz", "::"), "foo::Bar::baz");

            let popped = key.pop();
            assert_eq!(popped.map(|s| s.name.clone()), Some("Bar".to_string()));
            assert_eq!(key.depth(), 1);
            assert_eq!(key.qualify("qux", "::"), "foo::qux");

            key.pop();
            assert_eq!(key.depth(), 0);
        }
    }

    // ── NodeRule ──────────────────────────────────────────────────────────────

    mod node_rule {
        use super::*;

        #[test]
        fn leaf_constructor() {
            let r = NodeRule::leaf("function_item", NodeLabel::Function);
            assert_eq!(r.ts_kind, "function_item");
            assert_eq!(r.label, NodeLabel::Function);
            assert!(!r.scope_anchor);
            assert!(!r.skip_edges);
        }

        #[test]
        fn container_constructor() {
            let r = NodeRule::container("struct_item", NodeLabel::Class);
            assert_eq!(r.ts_kind, "struct_item");
            assert_eq!(r.label, NodeLabel::Class);
            assert!(r.scope_anchor);
            assert!(!r.skip_edges);
        }

        #[test]
        fn with_skip_edges() {
            let r = NodeRule::leaf("struct_item", NodeLabel::Class).with_skip_edges();
            assert!(r.skip_edges);
        }
    }

    // ── EdgeRule ──────────────────────────────────────────────────────────────

    mod edge_rule {
        use super::*;

        #[test]
        fn calls_constructor() {
            let r = EdgeRule::calls("call_expression", TargetPattern::FromNodeText);
            assert_eq!(r.source_kind, "call_expression");
            assert_eq!(r.edge_type, EdgeType::Calls);
            assert_eq!(r.target_pattern, TargetPattern::FromNodeText);
        }

        #[test]
        fn imports_constructor() {
            let r = EdgeRule::imports("use_declaration", TargetPattern::FromChildType);
            assert_eq!(r.edge_type, EdgeType::Imports);
        }

        #[test]
        fn inherits_constructor() {
            let r = EdgeRule::inherits("type_identifier", TargetPattern::FromChildType);
            assert_eq!(r.edge_type, EdgeType::Inherits);
        }
    }

    // ── ExtractedItem ────────────────────────────────────────────────────────

    mod extracted_item {
        use super::*;

        #[test]
        fn node_variant() {
            let node = ExtractedNode {
                label: NodeLabel::Function,
                qualified_name: "foo::Bar::baz".to_string(),
                ts_kind: "function_item".to_string(),
                start_byte: 10,
                end_byte: 30,
                line: 5,
                column: 1,
            };
            let item = ExtractedItem::Node(node.clone());
            match item {
                ExtractedItem::Node(n) => {
                    assert_eq!(n.label, NodeLabel::Function);
                    assert_eq!(n.qualified_name, "foo::Bar::baz");
                }
                _ => unreachable!(),
            }
        }

        #[test]
        fn edge_variant() {
            let edge = ExtractedEdge {
                source_qualified: "foo::Bar::call".to_string(),
                target_qualified: "foo::helper".to_string(),
                edge_type: EdgeType::Calls,
            };
            let item = ExtractedItem::Edge(edge);
            match item {
                ExtractedItem::Edge(e) => {
                    assert_eq!(e.edge_type, EdgeType::Calls);
                }
                _ => unreachable!(),
            }
        }

        #[test]
        fn extracted_node_clone() {
            let node = ExtractedNode {
                label: NodeLabel::Class,
                qualified_name: "MyClass".to_string(),
                ts_kind: "class_definition".to_string(),
                start_byte: 0,
                end_byte: 50,
                line: 1,
                column: 0,
            };
            let cloned = node.clone();
            assert_eq!(cloned.label, NodeLabel::Class);
            assert_eq!(cloned.qualified_name, "MyClass");
        }
    }

    // ── rkyv roundtrip ───────────────────────────────────────────────────────

    mod rkyv_roundtrip {
        use super::*;

        #[test]
        fn extracted_node_roundtrip() {
            let node = ExtractedNode {
                label: NodeLabel::Function,
                qualified_name: "foo::bar".to_string(),
                ts_kind: "function_item".to_string(),
                start_byte: 0,
                end_byte: 100,
                line: 10,
                column: 5,
            };
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&node).unwrap();
            let recovered: ExtractedNode =
                rkyv::from_bytes::<ExtractedNode, rkyv::rancor::Error>(&bytes).unwrap();
            assert_eq!(recovered.label, node.label);
            assert_eq!(recovered.qualified_name, node.qualified_name);
            assert_eq!(recovered.ts_kind, node.ts_kind);
            assert_eq!(recovered.line, node.line);
            assert_eq!(recovered.column, node.column);
        }

        #[test]
        fn extracted_edge_roundtrip() {
            let edge = ExtractedEdge {
                source_qualified: "A::B".to_string(),
                target_qualified: "C::D".to_string(),
                edge_type: EdgeType::Calls,
            };
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&edge).unwrap();
            let recovered: ExtractedEdge =
                rkyv::from_bytes::<ExtractedEdge, rkyv::rancor::Error>(&bytes).unwrap();
            assert_eq!(recovered.edge_type, EdgeType::Calls);
        }

        #[test]
        fn extracted_item_node_roundtrip() {
            let item = ExtractedItem::Node(ExtractedNode {
                label: NodeLabel::Trait,
                qualified_name: "MyTrait".to_string(),
                ts_kind: "trait_item".to_string(),
                start_byte: 0,
                end_byte: 50,
                line: 1,
                column: 0,
            });
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&item).unwrap();
            let recovered: ExtractedItem =
                rkyv::from_bytes::<ExtractedItem, rkyv::rancor::Error>(&bytes).unwrap();
            match recovered {
                ExtractedItem::Node(n) => assert_eq!(n.label, NodeLabel::Trait),
                _ => unreachable!(),
            }
        }
    }
}
