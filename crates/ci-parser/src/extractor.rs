//! Tree-sitter extraction engine.
//!
//! Interprets a [`LanguageSpec`] against a parsed [`tree_sitter::Tree`],
//! yielding [`ExtractedItem`] values in pre-order. Decoupled from the graph:
//! the caller decides how to intern strings and create graph nodes.

use std::marker::PhantomData;

use ci_core::{spec_for, ExtractedEdge, ExtractedItem, ExtractedNode, Language};
use ci_core::{LanguageSpec, ScopeKey, ScopeSegment, TargetPattern};

/// Iterator over extracted items.
///
/// Implementation detail: extraction is performed eagerly into a `Vec` and
/// iteration is a thin wrapper over `IntoIter`. This avoids traversal state bugs
/// and guarantees finite execution.
pub struct Extractor<'tree, 'source> {
    items: std::vec::IntoIter<ExtractedItem>,
    _marker: PhantomData<(&'tree (), &'source ())>,
}

impl<'tree, 'source> Extractor<'tree, 'source> {
    /// Create a new extractor for `tree` using the registered spec for `lang`.
    pub fn new(
        tree: &'tree tree_sitter::Tree,
        source: &'source str,
        lang: Language,
    ) -> Option<Self> {
        let spec = spec_for(lang)?;
        Some(Self::with_spec(tree, source, spec))
    }

    /// Create a new extractor with an explicit spec.
    pub fn with_spec(
        tree: &'tree tree_sitter::Tree,
        source: &'source str,
        spec: &'static LanguageSpec,
    ) -> Self {
        let mut items = Vec::new();
        let mut scope = ScopeKey::new();
        Self::visit(tree.root_node(), source, spec, &mut scope, &mut items);
        Self {
            items: items.into_iter(),
            _marker: PhantomData,
        }
    }

    fn visit(
        node: tree_sitter::Node<'_>,
        source: &str,
        spec: &'static LanguageSpec,
        scope: &mut ScopeKey,
        out: &mut Vec<ExtractedItem>,
    ) {
        if !node.is_named() {
            let mut c = node.walk();
            for child in node.children(&mut c) {
                Self::visit(child, source, spec, scope, out);
            }
            return;
        }

        let ts_kind = node.kind();

        // Extract identifier first (needed for both emit and suppress decision).
        let name = Self::extract_identifier(node, source);

        // Emit node first (qualified against current enclosing scope).
        //
        // Suppress when the node's own text IS its extracted name. This means the
        // node is purely a syntactic token used as the parent's name source (e.g.
        // the `identifier` child inside `function_item`, `mod_item`, `struct_item`).
        // The parent already emits the qualified name; emitting the bare text would
        // produce duplicate/incorrect entries at wrong scope levels.
        //
        // Field identifiers are always emitted — they are semantically meaningful
        // even when name == own_text, and suppressing them would lose struct fields.
        let own_text = node
            .utf8_text(source.as_bytes())
            .ok()
            .map(|s| s.trim().to_string());
        let suppress = own_text.as_ref() == name.as_ref()
            && name.is_some()
            && node.kind() != "field_identifier";

        if let Some(rule) = spec.get_node_rule(ts_kind) {
            if !suppress && !rule.skip_node {
                let name = name.or(own_text);
                if let Some(name) = name {
                    let qn = scope.qualify(&name, spec.qn_separator);
                    let pos = node.start_position();
                    out.push(ExtractedItem::Node(ExtractedNode {
                        label: rule.label,
                        qualified_name: qn,
                        ts_kind: ts_kind.to_string(),
                        start_byte: node.start_byte() as u32,
                        end_byte: node.end_byte() as u32,
                        line: pos.row as u32 + 1,
                        column: pos.column as u32 + 1,
                    }));
                }
            }
        }

        // Emit edge if configured (skip if suppressed as name token or node opts out)
        if !suppress {
            let node_rule = spec.get_node_rule(ts_kind);
            if node_rule.map_or(true, |r| !r.skip_edges) {
                if let Some(rule) = spec.get_edge_rule(ts_kind) {
                    if let Some(target) = Self::resolve_target(node, source, rule.target_pattern) {
                        let source_qn = scope.qualify("", spec.qn_separator);
                        out.push(ExtractedItem::Edge(ExtractedEdge {
                            source_qualified: source_qn,
                            target_qualified: target,
                            edge_type: rule.edge_type,
                        }));
                    }
                }
            }
        }

        // Push scope for descendants
        let is_scope = spec.get_scope_anchor(ts_kind).is_some() && ts_kind != spec.root_kind;
        if is_scope {
            let name = Self::extract_identifier(node, source).unwrap_or_default();
            scope.push(ScopeSegment::new(ts_kind.to_string(), name));
        }

        // Recurse
        let mut c = node.walk();
        for child in node.children(&mut c) {
            Self::visit(child, source, spec, scope, out);
        }

        // Pop scope on unwind
        if is_scope {
            let _ = scope.pop();
        }
    }

    fn extract_identifier(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
        for field in ["identifier", "name", "type_identifier", "field"] {
            if let Some(child) = node.child_by_field_name(field) {
                let text = child.utf8_text(source.as_bytes()).ok()?.trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }

        match node.kind() {
            // impl_item: type name is the 2nd positional named child (type_identifier).
            // tree-sitter doesn't expose this as a named field on impl_item.
            "impl_item" => {
                // impl Foo { ... } — the type name is the 2nd positional child
                // (index 1). child_by_field_name does NOT work here because
                // tree-sitter doesn't expose type_identifier as a named field
                // on impl_item. We skip the first unnamed child ("impl") and
                // take the first named child as the type name.
                node.children(&mut node.walk())
                    .find(|c| c.kind() == "type_identifier")
                    .and_then(|c| {
                        let text = c.utf8_text(source.as_bytes()).ok()?;
                        let text = text.trim();
                        if text.is_empty() {
                            None
                        } else {
                            Some(text.to_string())
                        }
                    })
            }
            "identifier" | "type_identifier" | "scoped_identifier" => {
                let text = node.utf8_text(source.as_bytes()).ok()?.trim();
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            _ => None,
        }
    }

    fn resolve_target(
        node: tree_sitter::Node<'_>,
        source: &str,
        pattern: TargetPattern,
    ) -> Option<String> {
        match pattern {
            TargetPattern::FromChildType => Self::extract_identifier(node, source),
            TargetPattern::FromNodeText => {
                let text = node.utf8_text(source.as_bytes()).ok()?.trim();
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            TargetPattern::ResolveSpecial => {
                let text = node.utf8_text(source.as_bytes()).ok()?.trim();
                if text == "super" || text == "self" || text == "Self" {
                    Some(text.to_string())
                } else {
                    Self::extract_identifier(node, source)
                }
            }
        }
    }
}

impl<'tree, 'source> Iterator for Extractor<'tree, 'source> {
    type Item = ExtractedItem;

    fn next(&mut self) -> Option<Self::Item> {
        self.items.next()
    }
}

/// Extract all items from `source` using the spec for `lang`.
/// Returns `None` if no spec is registered for `lang`.
pub fn extract<'tree, 'source>(
    tree: &'tree tree_sitter::Tree,
    source: &'source str,
    lang: Language,
) -> Option<Extractor<'tree, 'source>> {
    Extractor::new(tree, source, lang)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::extractor::Extractor;
    use ci_core::{spec_for, EdgeType, ExtractedItem, Language, NodeLabel};

    fn extract_all(tree: &tree_sitter::Tree, source: &str, lang: Language) -> Vec<ExtractedItem> {
        match Extractor::new(tree, source, lang) {
            Some(ex) => ex.collect(),
            None => Vec::new(),
        }
    }

    fn node_names(items: &[ExtractedItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                ExtractedItem::Node(n) => Some(n.qualified_name.clone()),
                ExtractedItem::Edge(_) => None,
            })
            .collect()
    }

    fn node_labels(items: &[ExtractedItem]) -> Vec<NodeLabel> {
        items
            .iter()
            .filter_map(|item| match item {
                ExtractedItem::Node(n) => Some(n.label),
                ExtractedItem::Edge(_) => None,
            })
            .collect()
    }

    fn edge_kinds(items: &[ExtractedItem]) -> Vec<EdgeType> {
        items
            .iter()
            .filter_map(|item| match item {
                ExtractedItem::Node(_) => None,
                ExtractedItem::Edge(e) => Some(e.edge_type),
            })
            .collect()
    }

    // ── Rust extraction ───────────────────────────────────────────────────

    #[test]
    fn extract_rust_function() {
        let source = "pub fn add(a: i32, b: i32) -> i32 { a + b }";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        let names = node_names(&items);
        assert!(
            names.iter().any(|n| n == "add"),
            "should find 'add': {:?}",
            names
        );
    }

    #[test]
    fn extract_rust_struct() {
        let source = "struct Point { x: i32, y: i32 }";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        let labels = node_labels(&items);
        assert!(
            labels.contains(&NodeLabel::Class),
            "struct should map to Class: {:?}",
            labels
        );
        let names = node_names(&items);
        assert!(
            names.iter().any(|n| n == "Point"),
            "should find 'Point': {:?}",
            names
        );
    }

    #[test]
    fn extract_rust_impl_method_scope() {
        let source = r#"
            struct Counter { count: i32 }
            impl Counter {
                fn inc(&mut self) { self.count += 1; }
            }
        "#;
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        let names = node_names(&items);
        // Counter should appear as a node (impl Item for Trait uses type_identifier field)
        assert!(
            names.iter().any(|n| n == "Counter"),
            "struct/impl name should appear: {:?}",
            names
        );
        // Field `count` is inside struct scope
        assert!(
            names.iter().any(|n| n == "Counter::count"),
            "field should be qualified: {:?}",
            names
        );
        // Method `inc` is inside impl scope — impl_item pushes scope via type_identifier field
        assert!(
            names.iter().any(|n| n == "Counter::inc"),
            "method should be qualified with impl type: {:?}",
            names
        );
    }

    #[test]
    fn extract_rust_use_declaration() {
        let source = "use std::collections::HashMap;";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        let edges = edge_kinds(&items);
        assert!(
            edges.contains(&EdgeType::Imports),
            "use declaration should produce Imports edge: {:?}",
            edges
        );
    }

    #[test]
    fn extract_rust_call_expression() {
        let source = "fn main() { foo(bar); }";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        let edges = edge_kinds(&items);
        assert!(
            edges.contains(&EdgeType::Calls),
            "call expression should produce Calls edge: {:?}",
            edges
        );
    }

    #[test]
    fn extract_rust_nested_modules() {
        let source = r#"
            mod outer {
                mod inner {
                    fn helper() {}
                }
            }
        "#;
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        let names = node_names(&items);
        assert!(
            names.iter().any(|n| n == "outer"),
            "outer module should appear: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == "outer::inner"),
            "inner module should appear (qualified by outer): {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == "outer::inner::helper"),
            "helper function should appear (qualified by outer::inner): {:?}",
            names
        );
    }

    #[test]
    fn extract_rust_trait() {
        let source = "trait Printable { fn print(&self); }";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        let labels = node_labels(&items);
        assert!(
            labels.contains(&NodeLabel::Trait),
            "trait should map to Trait: {:?}",
            labels
        );
    }

    #[test]
    fn extract_unknown_language_returns_empty() {
        // Use a language that parses successfully (Rust grammar is always available)
        // but has no registered spec — Extractor::new returns None.
        let source = "fn main() {}";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Unknown);
        assert!(
            items.is_empty(),
            "unknown language should return empty items"
        );
    }

    // ── Python extraction ─────────────────────────────────────────────────

    #[test]
    fn extract_python_class_method() {
        let source = r#"
            class Foo:
                def bar(self):
                    pass
        "#;
        let tree = crate::parse(source, Language::Python).unwrap();
        let items = extract_all(&tree, source, Language::Python);
        let names = node_names(&items);
        assert!(
            names.iter().any(|n| n == "Foo"),
            "class should appear: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == "Foo.bar"),
            "method should be qualified with class: {:?}",
            names
        );
    }

    #[test]
    fn extract_python_function() {
        let source = "def top_level(): return 42";
        let tree = crate::parse(source, Language::Python).unwrap();
        let items = extract_all(&tree, source, Language::Python);
        let names = node_names(&items);
        assert!(
            names.iter().any(|n| n == "top_level"),
            "function should appear: {:?}",
            names
        );
    }

    #[test]
    fn extract_python_class_definition() {
        let source = "class MyClass(BaseClass):\n    pass";
        let tree = crate::parse(source, Language::Python).unwrap();
        let items = extract_all(&tree, source, Language::Python);
        let labels = node_labels(&items);
        assert!(
            labels.contains(&NodeLabel::Class),
            "class definition should map to Class: {:?}",
            labels
        );
        let names = node_names(&items);
        assert!(
            names.iter().any(|n| n == "MyClass"),
            "class name should appear: {:?}",
            names
        );
    }

    #[test]
    fn extract_python_import() {
        let source = "import os\nfrom collections import OrderedDict";
        let tree = crate::parse(source, Language::Python).unwrap();
        let items = extract_all(&tree, source, Language::Python);
        let edges = edge_kinds(&items);
        assert!(
            edges.contains(&EdgeType::Imports),
            "import statement should produce Imports edge: {:?}",
            edges
        );
    }

    #[test]
    fn extract_python_call() {
        let source = "result = process(data)";
        let tree = crate::parse(source, Language::Python).unwrap();
        let items = extract_all(&tree, source, Language::Python);
        let edges = edge_kinds(&items);
        assert!(
            edges.contains(&EdgeType::Calls),
            "call should produce Calls edge: {:?}",
            edges
        );
    }

    // ── Spec registry ────────────────────────────────────────────────────

    #[test]
    fn spec_for_rust() {
        let spec = spec_for(Language::Rust);
        assert!(spec.is_some());
        let spec = spec.unwrap();
        assert_eq!(spec.lang, Language::Rust);
        assert_eq!(spec.root_kind, "source_file");
        assert_eq!(spec.qn_separator, "::");
    }

    #[test]
    fn spec_for_python() {
        let spec = spec_for(Language::Python);
        assert!(spec.is_some());
        let spec = spec.unwrap();
        assert_eq!(spec.lang, Language::Python);
        assert_eq!(spec.root_kind, "module");
        assert_eq!(spec.qn_separator, ".");
    }

    #[test]
    fn spec_for_unsupported_language() {
        assert!(spec_for(Language::Unknown).is_none());
        assert!(spec_for(Language::Json).is_none());
    }

    #[test]
    fn rust_spec_node_rules_sorted() {
        let spec = spec_for(Language::Rust).unwrap();
        assert!(spec.get_node_rule("function_item").is_some());
        assert!(spec.get_node_rule("struct_item").is_some());
        assert!(spec.get_node_rule("identifier").is_some());
        assert!(spec.get_node_rule("nonexistent_kind").is_none());
    }

    #[test]
    fn python_spec_node_rules_sorted() {
        let spec = spec_for(Language::Python).unwrap();
        assert!(spec.get_node_rule("class_definition").is_some());
        assert!(spec.get_node_rule("function_definition").is_some());
        assert!(spec.get_node_rule("identifier").is_some());
        assert!(spec.get_node_rule("nonexistent_kind").is_none());
    }

    #[test]
    fn rust_scope_anchors() {
        let spec = spec_for(Language::Rust).unwrap();
        assert!(spec.get_scope_anchor("source_file").is_some());
        assert!(spec.get_scope_anchor("mod_item").is_some());
        assert!(spec.get_scope_anchor("struct_item").is_some());
        assert!(spec.get_scope_anchor("impl_item").is_some());
        assert!(spec.get_scope_anchor("trait_item").is_some());
        assert!(spec.get_scope_anchor("function_item").is_none());
    }

    #[test]
    fn python_scope_anchors() {
        let spec = spec_for(Language::Python).unwrap();
        assert!(spec.get_scope_anchor("module").is_some());
        assert!(spec.get_scope_anchor("class_definition").is_some());
        assert!(spec.get_scope_anchor("function_definition").is_some());
        assert!(spec.get_scope_anchor("identifier").is_none());
    }

    #[test]
    fn rust_edge_rules() {
        let spec = spec_for(Language::Rust).unwrap();
        let rule = spec.get_edge_rule("call_expression");
        assert!(rule.is_some());
        assert_eq!(rule.unwrap().edge_type, EdgeType::Calls);
    }

    #[test]
    fn python_edge_rules() {
        let spec = spec_for(Language::Python).unwrap();
        let rule = spec.get_edge_rule("call");
        assert!(rule.is_some());
        assert_eq!(rule.unwrap().edge_type, EdgeType::Calls);
    }

    #[test]
    fn advance_cursor_basic() {
        // Smoke test: collect with a hard cap to detect infinite loops
        let source = "fn add() {}";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let spec = spec_for(Language::Rust).unwrap();
        let mut ex = Extractor::with_spec(&tree, source, spec);
        let mut count = 0;
        while ex.next().is_some() {
            count += 1;
            assert!(
                count <= 1000,
                "extractor produced >1000 items — likely infinite loop"
            );
        }
        assert!(count > 0, "should emit at least one item, got {}", count);
    }

    #[test]
    fn rust_node_has_line_column() {
        let source = "fn add() {}\nfn sub() {}";
        let tree = crate::parse(source, Language::Rust).unwrap();
        let items = extract_all(&tree, source, Language::Rust);
        for item in &items {
            if let ExtractedItem::Node(n) = item {
                assert!(n.line > 0, "line should be > 0: {:?}", n);
                assert!(n.start_byte < n.end_byte, "start should be < end: {:?}", n);
            }
        }
    }
}
