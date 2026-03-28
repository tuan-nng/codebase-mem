//! Thread-local tree-sitter [`Parser`] pool.
//!
//! Each calling thread maintains its own `HashMap<Language, Parser>`,
//! lazily populated on first use.  This sidesteps the `!Send` bound on
//! `Parser` while keeping parse overhead near-zero on the hot path.
//!
//! # Memory model
//!
//! Each Rayon worker thread accumulates at most one `Parser` per language it
//! has ever processed.  At the current language count (~22 grammars) this
//! is bounded at ~22 × parser-size ≈ < 1 MB per thread — negligible.

use std::cell::RefCell;

use ci_core::Language as CoreLanguage;

use crate::grammar;

// Per-thread cache of `Parser` instances, one per language.
// `None` means the entry exists but the parser needs language setup on first use.
thread_local! {
    static PARSERS: RefCell<std::collections::HashMap<CoreLanguage, Option<tree_sitter::Parser>>> =
        RefCell::new(std::collections::HashMap::new());
}

/// A lazily-initialized, thread-local tree-sitter parser pool.
///
/// Use [`ParserPool::new`] to construct, then call [`ParserPool::parse`].
///
/// # Example
///
/// ```
/// use ci_core::Language;
/// use ci_parser::ParserPool;
///
/// let mut pool = ParserPool::new();
/// let tree = pool.parse("fn main() {}", Language::Rust);
/// ```
#[derive(Default)]
pub struct ParserPool;

impl ParserPool {
    /// Construct a new parser pool.
    ///
    /// The pool itself holds no state — all `Parser` instances live in
    /// thread-local storage.  Multiple `ParserPool` instances are equivalent.
    #[inline]
    pub fn new() -> Self {
        Self
    }

    /// Parse `source` using the grammar for `lang`.
    ///
    /// Returns `Some(tree)` on success and `None` if:
    ///  - `lang` has no grammar compiled in (see `Cargo.toml` feature flags), or
    ///  - tree-sitter could not construct a tree
    ///
    /// tree-sitter always returns a tree — even for invalid source — so `None`
    /// here means "unsupported language" only.
    #[inline]
    pub fn parse(&mut self, source: &str, lang: CoreLanguage) -> Option<tree_sitter::Tree> {
        get_parser_mut(lang)?.parse(source, None)
    }

    /// Like [`parse`](Self::parse) but returns a structured error on failure.
    ///
    /// Returns `Err` if the language is unsupported or if tree-sitter fails.
    /// Prefer `parse` when only the tree matters.
    #[inline]
    pub fn parse_result(
        &mut self,
        source: &str,
        lang: CoreLanguage,
    ) -> Result<tree_sitter::Tree, ParseError> {
        get_parser_mut(lang)
            .ok_or(ParseError::Unsupported(lang))?
            .parse(source, None)
            .ok_or(ParseError::ParseFailed(lang))
    }

    /// Returns the number of cached parsers on the current thread.
    #[inline]
    pub fn cached_count(&self) -> usize {
        PARSERS.with(|cell| cell.borrow().values().filter(|p| p.is_some()).count())
    }

    /// Drops all cached parsers on the current thread.
    ///
    /// Useful between batches when memory pressure is a concern.
    #[inline]
    pub fn clear_cache(&self) {
        PARSERS.with(|cell| cell.borrow_mut().clear());
    }

    /// Returns `true` if `lang` has a grammar compiled in.
    #[inline]
    pub fn supports(&self, lang: CoreLanguage) -> bool {
        grammar::is_supported(lang)
    }
}

/// Error returned by [`ParserPool::parse_result`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// No grammar was compiled in for this language.
    Unsupported(CoreLanguage),

    /// tree-sitter failed to produce a parse tree.
    ParseFailed(CoreLanguage),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Unsupported(lang) => {
                write!(f, "no grammar compiled for language: {lang}")
            }
            ParseError::ParseFailed(lang) => {
                write!(f, "tree-sitter failed to parse {lang} source")
            }
        }
    }
}

impl std::error::Error for ParseError {}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Get or create a thread-local [`tree_sitter::Parser`] for `lang`.
///
/// SAFETY: the returned `&'static mut Parser` points into TLS on the current
/// thread.  Since `Parser` is `!Send`, it can never cross threads, so the
/// static lifetime is safe within this thread's scope.
fn get_parser_mut(lang: CoreLanguage) -> Option<&'static mut tree_sitter::Parser> {
    // Check fast path with an immutable borrow so we can drop it before
    // the potentially-nested mutable borrow of the entry-or-insert path.
    if PARSERS.with(|cell| matches!(cell.borrow().get(&lang), Some(Some(_)))) {
        // SAFETY: we've confirmed the entry is initialized.  Acquire a single
        // exclusive RefCell borrow to get a &mut Parser and hand out a raw ptr.
        return PARSERS.with(|cell| {
            let mut parsers = cell.borrow_mut();
            let ptr = parsers.get_mut(&lang).unwrap().as_mut().unwrap() as *mut tree_sitter::Parser;
            Some(unsafe { &mut *ptr })
        });
    }

    // Slow path: create the parser.  First check with a mutable borrow
    // to distinguish "not present" from "present but None".
    let ts_lang = {
        let needs_init = PARSERS.with(|cell| {
            let parsers = cell.borrow();
            parsers.get(&lang).map_or(true, |v| v.is_none())
        });
        if !needs_init {
            // Race-free (single-threaded TLS): another call initialized it.
            return PARSERS.with(|cell| {
                let mut parsers = cell.borrow_mut();
                let ptr = parsers.get_mut(&lang).unwrap().as_mut().unwrap() as *mut tree_sitter::Parser;
                Some(unsafe { &mut *ptr })
            });
        }
        grammar::get_language(lang)?
    };

    let ts_lang = tree_sitter::Language::new(ts_lang);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_lang).ok()?;

    // Insert into the map and return a mutable reference.
    PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        parsers.insert(lang, Some(parser));
        let ptr = parsers.get_mut(&lang).unwrap().as_mut().unwrap() as *mut tree_sitter::Parser;
        Some(unsafe { &mut *ptr })
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ParserPool construction ────────────────────────────────────────────

    #[test]
    fn pool_default_constructs() {
        let _pool = ParserPool::default();
        let pool = ParserPool::new();
        assert_eq!(pool.cached_count(), 0);
    }

    #[test]
    fn supports_returns_false_for_unsupported_language() {
        let pool = ParserPool::new();
        assert!(!pool.supports(CoreLanguage::Unknown));
        assert!(!pool.supports(CoreLanguage::Sql));
    }

    #[test]
    fn supports_returns_true_for_supported_language() {
        let pool = ParserPool::new();
        // All languages in lang-all (the default feature).
        assert!(pool.supports(CoreLanguage::Rust));
        assert!(pool.supports(CoreLanguage::Python));
        assert!(pool.supports(CoreLanguage::TypeScript));
        assert!(pool.supports(CoreLanguage::Go));
        assert!(pool.supports(CoreLanguage::Java));
        assert!(pool.supports(CoreLanguage::C));
        assert!(pool.supports(CoreLanguage::Cpp));
        assert!(pool.supports(CoreLanguage::Html));
        assert!(pool.supports(CoreLanguage::Json));
        assert!(pool.supports(CoreLanguage::Yaml));
        assert!(pool.supports(CoreLanguage::Zig));
        assert!(pool.supports(CoreLanguage::Ruby));
        assert!(pool.supports(CoreLanguage::Php));
        assert!(pool.supports(CoreLanguage::Scala));
        assert!(pool.supports(CoreLanguage::Kotlin));
        assert!(pool.supports(CoreLanguage::CSharp));
        assert!(pool.supports(CoreLanguage::Swift));
        assert!(pool.supports(CoreLanguage::Css));
        assert!(pool.supports(CoreLanguage::Shell));
    }

    #[test]
    fn parse_returns_none_for_unsupported_language() {
        let mut pool = ParserPool::new();
        assert!(pool.parse("fn main() {}", CoreLanguage::Unknown).is_none());
        assert!(pool.parse("fn main() {}", CoreLanguage::Sql).is_none());
    }

    #[test]
    fn parse_result_unsupported_error() {
        let mut pool = ParserPool::new();
        let result = pool.parse_result("fn main() {}", CoreLanguage::Unknown);
        assert!(matches!(result, Err(ParseError::Unsupported(CoreLanguage::Unknown))));
    }

    #[test]
    fn cached_count_starts_at_zero() {
        let pool = ParserPool::new();
        assert_eq!(pool.cached_count(), 0);
    }

    #[test]
    fn clear_cache_resets_count() {
        let mut pool = ParserPool::new();
        let _ = pool.parse("fn main() {}", CoreLanguage::Unknown);
        pool.clear_cache();
        assert_eq!(pool.cached_count(), 0);
    }

    // ── Thread isolation ─────────────────────────────────────────────────────

    #[test]
    fn threads_have_isolated_caches() {
        let mut pool = ParserPool::new();

        // Cache something on the main thread.
        let _ = pool.parse("fn main() {}", CoreLanguage::Rust);
        let main_count = pool.cached_count();

        // Each spawned thread has its own empty cache.
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    assert_eq!(pool.cached_count(), 0);
                });
            }
        });

        // Main thread cache is unaffected.
        assert_eq!(pool.cached_count(), main_count);
    }

    // ── ParseError ─────────────────────────────────────────────────────────

    #[test]
    fn parse_error_display_unsupported() {
        let err = ParseError::Unsupported(CoreLanguage::Rust);
        let s = err.to_string();
        assert!(s.contains("Rust"));
        assert!(s.contains("no grammar"));
    }

    #[test]
    fn parse_error_display_parse_failed() {
        let err = ParseError::ParseFailed(CoreLanguage::Python);
        let s = err.to_string();
        assert!(s.contains("Python"));
        assert!(s.contains("failed to parse"));
    }

    #[test]
    fn parse_error_debug() {
        let err = ParseError::Unsupported(CoreLanguage::Go);
        let debug = format!("{:?}", err);
        assert!(debug.contains("Unsupported"));
        assert!(debug.contains("Go"));
    }

    // ── Round-trip parse ──────────────────────────────────────────────────

    #[test]
    fn parse_rust_source() {
        let mut pool = ParserPool::new();
        let tree = pool.parse("fn main() {}", CoreLanguage::Rust);
        assert!(tree.is_some());
        let tree = tree.unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn parse_python_source() {
        let mut pool = ParserPool::new();
        let tree = pool.parse("def foo():\n    pass", CoreLanguage::Python);
        assert!(tree.is_some());
        assert_eq!(tree.unwrap().root_node().kind(), "module");
    }

    #[test]
    fn parse_cached_reuses_parser() {
        let mut pool = ParserPool::new();
        let _ = pool.parse("fn main() {}", CoreLanguage::Rust);
        assert_eq!(pool.cached_count(), 1);
        let _ = pool.parse("fn other() {}", CoreLanguage::Rust);
        assert_eq!(pool.cached_count(), 1); // Still 1, reused
    }

    // ── Top-level parse() ────────────────────────────────────────────────

    #[test]
    fn top_level_parse_function() {
        // The top-level parse() uses SHARED_POOL internally.
        let tree = crate::parse("fn main() {}", CoreLanguage::Rust);
        assert!(tree.is_some());
        let tree = crate::parse("def foo():\n    pass", CoreLanguage::Python);
        assert!(tree.is_some());
        // Unknown language returns None.
        assert!(crate::parse("fn main() {}", CoreLanguage::Unknown).is_none());
    }

    #[test]
    fn parse_invalid_source_returns_tree() {
        // tree-sitter always returns a tree, even for invalid source.
        let mut pool = ParserPool::new();
        let tree = pool.parse("fn { invalid rust", CoreLanguage::Rust);
        assert!(tree.is_some()); // Returns a tree with error nodes
        assert!(tree.unwrap().root_node().has_error());
    }

    #[test]
    fn parse_tree_structure() {
        let mut pool = ParserPool::new();
        let tree = pool.parse("fn add(a: i32, b: i32) -> i32 { a + b }", CoreLanguage::Rust).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        // Walk children using a cursor.
        let mut cursor = tree.walk();
        let children: Vec<_> = root.children(&mut cursor).collect();
        assert!(!children.is_empty());
    }
}
