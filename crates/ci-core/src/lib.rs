//! Core types for the Codebase Intelligence Engine.
//!
//! This is the leaf crate — every other `ci-*` crate depends on it.
//! Its only external dependency is `rkyv` for zero-copy persistence.
//!
//! # Types
//!
//! | Type               | Repr  | Role                                      |
//! |--------------------|-------|-------------------------------------------|
//! | [`NodeId`]         | `u32` | Stable index into SoA node arrays         |
//! | [`EdgeId`]         | `u32` | Stable index into SoA edge arrays         |
//! | [`NodeLabel`]      | enum  | Kind of a node; drives bitmap indexes     |
//! | [`EdgeType`]       | enum  | Relationship between two nodes            |
//! | [`InternedStr`]    | `u32` | 4-byte handle into the string interner    |
//! | [`StringInterner`] | —     | Concurrent 16-shard string interner       |
//! | [`FrozenInterner`] | —     | Compacted single-buffer string table      |

mod interner;
pub use interner::{FrozenInterner, StringInterner};

use std::fmt;

use rkyv::{Archive, Serialize};

// ── NodeId ────────────────────────────────────────────────────────────────────

/// Stable identifier for a node in the code graph.
///
/// `NodeId` is a densely-packed index into the SoA node arrays inside
/// `MutableGraph` and `FrozenGraph`.  It is 4 bytes, `Copy`, and cheap to
/// compare (a single `u32` comparison).
///
/// # Ordering
/// `NodeId` values are ordered numerically; the ordering carries no semantic
/// meaning beyond providing a total order for use in sorted structures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize)]
#[rkyv(attr(derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)))]
pub struct NodeId(pub u32);

impl fmt::Display for NodeId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<u32> for NodeId {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<NodeId> for u32 {
    #[inline]
    fn from(id: NodeId) -> u32 {
        id.0
    }
}

// ── EdgeId ────────────────────────────────────────────────────────────────────

/// Stable identifier for an edge in the code graph.
///
/// Mirrors [`NodeId`] in design: a 4-byte index into the SoA edge arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize)]
#[rkyv(attr(derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)))]
pub struct EdgeId(pub u32);

impl fmt::Display for EdgeId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<u32> for EdgeId {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<EdgeId> for u32 {
    #[inline]
    fn from(id: EdgeId) -> u32 {
        id.0
    }
}

// ── NodeLabel ─────────────────────────────────────────────────────────────────

/// The kind of a node in the code graph.
///
/// Used as the discriminant for `RoaringBitmap` label indexes in `FrozenGraph`,
/// enabling O(1) "all functions in the graph" queries via bitmap lookup.
///
/// Variants are grouped into two categories:
/// - **Structural** — project hierarchy nodes (project → package → dir → file)
/// - **Symbols** — code entities extracted from source files
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize)]
#[rkyv(attr(derive(Debug, Clone, Copy, PartialEq, Eq, Hash)))]
#[repr(u8)]
pub enum NodeLabel {
    // ── Structural ──────────────────────────────────────────────────────────
    /// Root node for an indexed repository.
    Project,
    /// A build package or module (e.g., a Go module, Rust crate, Python package).
    Package,
    /// A directory within the project.
    Directory,
    /// A single source file.
    File,

    // ── Symbols ─────────────────────────────────────────────────────────────
    /// A class definition (OOP languages).
    Class,
    /// An interface definition (Java, TypeScript, Go interface).
    Interface,
    /// A trait definition (Rust, Scala).
    Trait,
    /// A standalone function or free function.
    Function,
    /// A method attached to a class, struct, or impl block.
    Method,
    /// A type alias or typedef.
    TypeAlias,
    /// A variable or constant binding at module or class scope.
    Variable,
    /// A field inside a struct, class, or record.
    Field,
    /// A namespace or module-level grouping (C++, C#, TypeScript namespaces).
    Namespace,
}

impl fmt::Display for NodeLabel {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NodeLabel::Project   => "Project",
            NodeLabel::Package   => "Package",
            NodeLabel::Directory => "Directory",
            NodeLabel::File      => "File",
            NodeLabel::Class     => "Class",
            NodeLabel::Interface => "Interface",
            NodeLabel::Trait     => "Trait",
            NodeLabel::Function  => "Function",
            NodeLabel::Method    => "Method",
            NodeLabel::TypeAlias => "TypeAlias",
            NodeLabel::Variable  => "Variable",
            NodeLabel::Field     => "Field",
            NodeLabel::Namespace => "Namespace",
        })
    }
}

// ── EdgeType ──────────────────────────────────────────────────────────────────

/// The relationship kind between two nodes in the code graph.
///
/// Edge types drive query semantics: traversal filters, confidence scoring,
/// and the Cypher-like query language all pattern-match on `EdgeType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize)]
#[rkyv(attr(derive(Debug, Clone, Copy, PartialEq, Eq, Hash)))]
#[repr(u8)]
pub enum EdgeType {
    /// Structural containment: Project → Package → Directory → File → Symbol.
    Contains,
    /// Direct function/method call: Caller → Callee.
    Calls,
    /// Distributed HTTP call resolved from runtime traces: Service A → Service B.
    CallsHttp,
    /// Static import/require: importing file → imported module or symbol.
    Imports,
    /// Re-export: a module publicly re-exports a symbol it imported.
    /// Relevant for JavaScript/TypeScript barrel files and Python `__init__.py`.
    ReExports,
    /// OOP inheritance: subclass → superclass.
    Inherits,
    /// OOP implementation: concrete class → interface or abstract class.
    Implements,
    /// Decorator/annotation applied to a function or class.
    Decorates,
    /// Type usage: a variable, parameter, or return type references a type node.
    Uses,
    /// Test coverage: a test function exercises a target function or class.
    Tests,
}

impl fmt::Display for EdgeType {
    /// Returns the canonical SCREAMING_SNAKE_CASE name used in query output
    /// and the Cypher-like query language (e.g. `[:CALLS]`).
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EdgeType::Contains   => "CONTAINS",
            EdgeType::Calls      => "CALLS",
            EdgeType::CallsHttp  => "CALLS_HTTP",
            EdgeType::Imports    => "IMPORTS",
            EdgeType::ReExports  => "REEXPORTS",
            EdgeType::Inherits   => "INHERITS",
            EdgeType::Implements => "IMPLEMENTS",
            EdgeType::Decorates  => "DECORATES",
            EdgeType::Uses       => "USES",
            EdgeType::Tests      => "TESTS",
        })
    }
}

// ── InternedStr ───────────────────────────────────────────────────────────────

/// A 4-byte handle referencing a string in the `StringInterner`'s buffer.
///
/// All string data in the graph (symbol names, qualified names, file paths)
/// is stored once in a contiguous buffer inside `StringInterner` (built in
/// `ci-graph`). Nodes and edges hold `InternedStr` handles instead of
/// `String`s, keeping the SoA arrays compact and enabling O(1) string
/// equality (a single `u32` comparison).
///
/// # Display
/// `Display` intentionally shows the raw handle value (`InternedStr(N)`).
/// To get the string contents, pass the handle to the `StringInterner` or
/// `FrozenGraph`.  This avoids any hidden global state in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize)]
#[rkyv(attr(derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)))]
pub struct InternedStr(pub u32);

impl fmt::Display for InternedStr {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InternedStr(")?;
        fmt::Display::fmt(&self.0, f)?;
        f.write_str(")")
    }
}

impl From<u32> for InternedStr {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<InternedStr> for u32 {
    #[inline]
    fn from(s: InternedStr) -> u32 {
        s.0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;

    // ── NodeId ────────────────────────────────────────────────────────────────

    mod node_id {
        use super::*;

        #[test]
        fn from_u32_roundtrip() {
            for v in [0_u32, 1, 42, u32::MAX] {
                let id = NodeId::from(v);
                assert_eq!(u32::from(id), v);
            }
        }

        #[test]
        fn display_shows_inner_value() {
            assert_eq!(NodeId(0).to_string(), "0");
            assert_eq!(NodeId(42).to_string(), "42");
            assert_eq!(NodeId(u32::MAX).to_string(), u32::MAX.to_string());
        }

        #[test]
        fn debug_wraps_value() {
            assert_eq!(format!("{:?}", NodeId(5)), "NodeId(5)");
        }

        #[test]
        fn total_order() {
            assert!(NodeId(0) < NodeId(1));
            assert!(NodeId(100) > NodeId(50));
            assert_eq!(NodeId(7), NodeId(7));

            let mut ids = vec![NodeId(3), NodeId(1), NodeId(2)];
            ids.sort();
            assert_eq!(ids, [NodeId(1), NodeId(2), NodeId(3)]);
        }

        #[test]
        fn usable_as_hashset_key() {
            let mut set = HashSet::new();
            set.insert(NodeId(1));
            set.insert(NodeId(2));
            set.insert(NodeId(1)); // duplicate
            assert_eq!(set.len(), 2);
            assert!(set.contains(&NodeId(1)));
        }

        #[test]
        fn usable_as_hashmap_key() {
            let mut map = HashMap::new();
            map.insert(NodeId(10), "alpha");
            map.insert(NodeId(20), "beta");
            assert_eq!(map[&NodeId(10)], "alpha");
            assert_eq!(map[&NodeId(20)], "beta");
            assert!(!map.contains_key(&NodeId(99)));
        }

        #[test]
        fn into_u32_via_trait() {
            let id = NodeId(77);
            let v: u32 = id.into();
            assert_eq!(v, 77);
        }
    }

    // ── EdgeId ────────────────────────────────────────────────────────────────

    mod edge_id {
        use super::*;

        #[test]
        fn from_u32_roundtrip() {
            for v in [0_u32, 1, 99, u32::MAX] {
                let id = EdgeId::from(v);
                assert_eq!(u32::from(id), v);
            }
        }

        #[test]
        fn display_shows_inner_value() {
            assert_eq!(EdgeId(0).to_string(), "0");
            assert_eq!(EdgeId(123).to_string(), "123");
        }

        #[test]
        fn debug_wraps_value() {
            assert_eq!(format!("{:?}", EdgeId(9)), "EdgeId(9)");
        }

        #[test]
        fn total_order() {
            let mut ids = vec![EdgeId(30), EdgeId(10), EdgeId(20)];
            ids.sort();
            assert_eq!(ids, [EdgeId(10), EdgeId(20), EdgeId(30)]);
        }
    }

    // ── NodeLabel ─────────────────────────────────────────────────────────────

    mod node_label {
        use super::*;

        /// Spec requires exactly 4 structural + 9 symbol variants = 13 total.
        #[test]
        fn variant_count_matches_spec() {
            assert_eq!(all_node_labels().len(), 13);
        }

        #[test]
        fn structural_variants_present() {
            let structural = [
                NodeLabel::Project,
                NodeLabel::Package,
                NodeLabel::Directory,
                NodeLabel::File,
            ];
            for label in structural {
                assert!(all_node_labels().contains(&label));
            }
        }

        #[test]
        fn symbol_variants_present() {
            let symbols = [
                NodeLabel::Class,
                NodeLabel::Interface,
                NodeLabel::Trait,
                NodeLabel::Function,
                NodeLabel::Method,
                NodeLabel::TypeAlias,
                NodeLabel::Variable,
                NodeLabel::Field,
                NodeLabel::Namespace,
            ];
            for label in symbols {
                assert!(all_node_labels().contains(&label));
            }
        }

        #[test]
        fn display_matches_variant_name() {
            assert_eq!(NodeLabel::Project.to_string(), "Project");
            assert_eq!(NodeLabel::Package.to_string(), "Package");
            assert_eq!(NodeLabel::Directory.to_string(), "Directory");
            assert_eq!(NodeLabel::File.to_string(), "File");
            assert_eq!(NodeLabel::Class.to_string(), "Class");
            assert_eq!(NodeLabel::Interface.to_string(), "Interface");
            assert_eq!(NodeLabel::Trait.to_string(), "Trait");
            assert_eq!(NodeLabel::Function.to_string(), "Function");
            assert_eq!(NodeLabel::Method.to_string(), "Method");
            assert_eq!(NodeLabel::TypeAlias.to_string(), "TypeAlias");
            assert_eq!(NodeLabel::Variable.to_string(), "Variable");
            assert_eq!(NodeLabel::Field.to_string(), "Field");
            assert_eq!(NodeLabel::Namespace.to_string(), "Namespace");
        }

        #[test]
        fn usable_as_hashmap_key() {
            let mut map: HashMap<NodeLabel, usize> = HashMap::new();
            map.insert(NodeLabel::Function, 100);
            map.insert(NodeLabel::Class, 50);
            map.insert(NodeLabel::Function, 200); // overwrite
            assert_eq!(map[&NodeLabel::Function], 200);
            assert_eq!(map.len(), 2);
        }

        fn all_node_labels() -> Vec<NodeLabel> {
            vec![
                NodeLabel::Project,
                NodeLabel::Package,
                NodeLabel::Directory,
                NodeLabel::File,
                NodeLabel::Class,
                NodeLabel::Interface,
                NodeLabel::Trait,
                NodeLabel::Function,
                NodeLabel::Method,
                NodeLabel::TypeAlias,
                NodeLabel::Variable,
                NodeLabel::Field,
                NodeLabel::Namespace,
            ]
        }
    }

    // ── EdgeType ──────────────────────────────────────────────────────────────

    mod edge_type {
        use super::*;

        /// Spec requires exactly 10 variants.
        #[test]
        fn variant_count_matches_spec() {
            assert_eq!(all_edge_types().len(), 10);
        }

        /// Display must use SCREAMING_SNAKE_CASE as required by the query language.
        #[test]
        fn display_is_screaming_snake_case() {
            assert_eq!(EdgeType::Contains.to_string(), "CONTAINS");
            assert_eq!(EdgeType::Calls.to_string(), "CALLS");
            assert_eq!(EdgeType::CallsHttp.to_string(), "CALLS_HTTP");
            assert_eq!(EdgeType::Imports.to_string(), "IMPORTS");
            assert_eq!(EdgeType::ReExports.to_string(), "REEXPORTS");
            assert_eq!(EdgeType::Inherits.to_string(), "INHERITS");
            assert_eq!(EdgeType::Implements.to_string(), "IMPLEMENTS");
            assert_eq!(EdgeType::Decorates.to_string(), "DECORATES");
            assert_eq!(EdgeType::Uses.to_string(), "USES");
            assert_eq!(EdgeType::Tests.to_string(), "TESTS");
        }

        #[test]
        fn usable_as_hashset_key() {
            let mut set = HashSet::new();
            set.insert(EdgeType::Calls);
            set.insert(EdgeType::Imports);
            set.insert(EdgeType::Calls); // duplicate
            assert_eq!(set.len(), 2);
        }

        #[test]
        fn usable_as_hashmap_key() {
            let mut counts: HashMap<EdgeType, u32> = HashMap::new();
            *counts.entry(EdgeType::Calls).or_insert(0) += 1;
            *counts.entry(EdgeType::Calls).or_insert(0) += 1;
            *counts.entry(EdgeType::Imports).or_insert(0) += 1;
            assert_eq!(counts[&EdgeType::Calls], 2);
            assert_eq!(counts[&EdgeType::Imports], 1);
        }

        fn all_edge_types() -> Vec<EdgeType> {
            vec![
                EdgeType::Contains,
                EdgeType::Calls,
                EdgeType::CallsHttp,
                EdgeType::Imports,
                EdgeType::ReExports,
                EdgeType::Inherits,
                EdgeType::Implements,
                EdgeType::Decorates,
                EdgeType::Uses,
                EdgeType::Tests,
            ]
        }
    }

    // ── InternedStr ───────────────────────────────────────────────────────────

    mod interned_str {
        use super::*;

        #[test]
        fn from_u32_roundtrip() {
            for v in [0_u32, 1, 42, u32::MAX] {
                let s = InternedStr::from(v);
                assert_eq!(u32::from(s), v);
            }
        }

        #[test]
        fn display_shows_interned_str_prefix() {
            assert_eq!(InternedStr(0).to_string(), "InternedStr(0)");
            assert_eq!(InternedStr(42).to_string(), "InternedStr(42)");
            assert_eq!(
                InternedStr(u32::MAX).to_string(),
                format!("InternedStr({})", u32::MAX)
            );
        }

        #[test]
        fn debug_shows_struct_syntax() {
            assert_eq!(format!("{:?}", InternedStr(1)), "InternedStr(1)");
        }

        /// Two handles with the same value must compare equal, regardless of
        /// what string they might resolve to — equality is handle equality.
        #[test]
        fn equality_is_handle_equality() {
            assert_eq!(InternedStr(5), InternedStr(5));
            assert_ne!(InternedStr(5), InternedStr(6));
        }

        #[test]
        fn total_order() {
            assert!(InternedStr(0) < InternedStr(1));
            assert!(InternedStr(100) > InternedStr(50));

            let mut handles = vec![InternedStr(3), InternedStr(1), InternedStr(2)];
            handles.sort();
            assert_eq!(handles, [InternedStr(1), InternedStr(2), InternedStr(3)]);
        }

        #[test]
        fn usable_as_hashmap_key() {
            let mut map: HashMap<InternedStr, &str> = HashMap::new();
            map.insert(InternedStr(0), "hello");
            map.insert(InternedStr(1), "world");
            assert_eq!(map[&InternedStr(0)], "hello");
            assert_eq!(map[&InternedStr(1)], "world");
            assert_eq!(map.len(), 2);
        }
    }
}
