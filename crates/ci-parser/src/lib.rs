//! Thread-local tree-sitter parser pool.
//!
//! Provides a thread-safe, lazily-initialized pool of [`tree_sitter::Parser`]
//! instances keyed by [`ci_core::Language`].  Each thread maintains its own
//! parser cache, avoiding lock contention on the hot path.
//!
//! # Design
//!
//! `tree_sitter::Parser` is `!Send`, so it cannot be shared across threads.
//! The solution is **thread-local storage**: each Rayon worker thread holds
//! its own `HashMap<ci_core::Language, Parser>`.  The map is lazily populated
//! — the first parse of a language on a thread creates and caches a `Parser`
//! for that language; subsequent parses reuse it.
//!
//! # Grammar availability
//!
//! Grammar crates are conditionally compiled via feature flags.  Call
//! [`ParserPool::supports`] or [`ParserPool::parse`] to check availability.
//! Languages without a compiled grammar return `None` gracefully.
//!
//! # Example
//!
//! ```
//! use ci_core::Language;
//! use ci_parser::{parse, ParserPool};
//!
//! // Convenience function — uses a shared pool under the hood.
//! let tree = parse("fn main() {}", Language::Rust);
//! assert!(tree.is_some());
//!
//! // Or manage the pool explicitly.
//! let mut pool = ParserPool::new();
//! let tree = pool.parse("let x = 1;", Language::Rust);
//! assert!(tree.is_some());
//! ```
//!
//! # Architecture
//!
//! ```text
//! parse(source, lang) / ParserPool::parse(source, lang)
//!       |
//!       v
//! +------------------+
//! | grammar::get_lang | --> Some(fn) --> continue
//! +----------+-------+ --> None --> return None
//!            |
//!            v
//! +-----------------------------------+
//! | thread_local! HashMap<Lang, Parser> |
//! |  entry exists? --> reuse Parser      |
//! |  missing? --> Parser::new + set_language |
//! +-------------+-----------------------+
//!               |
//!               v
//! +-----------------------------+
//! | parser.parse(src, None) | --> Some(Tree) / None
//! +-----------------------------+
//! ```

mod extractor;
mod grammar;
mod pool;

pub use ci_core::Language;
pub use extractor::extract;
pub use pool::{ParseError, ParserPool};

/// Parse `source` with the grammar for `lang`.
///
/// This is a convenience wrapper around [`ParserPool::parse`].  It uses a
/// process-wide shared pool protected by a `Mutex`.
///
/// Returns `Some(tree)` when:
///  - `lang` has a grammar compiled in **and**
///  - tree-sitter successfully parsed the source
///
/// tree-sitter always returns a tree (even for invalid source), so `None`
/// here means the language is unsupported.
#[inline]
pub fn parse(source: &str, lang: Language) -> Option<tree_sitter::Tree> {
    SHARED_POOL.with(|pool| pool.borrow_mut().parse(source, lang))
}

use std::cell::RefCell;

// Process-wide shared pool, one `ParserPool` protected by `RefCell` (safe because
// `parse` needs `&mut self` and we hold the `borrow_mut()` guard for the call).
thread_local! {
    static SHARED_POOL: RefCell<ParserPool> = RefCell::new(ParserPool::new());
}
