//! Sharded string interner for concurrent symbol name interning.
//!
//! # Design
//! [`StringInterner`] uses 16 lock-striped shards so that concurrent `intern()`
//! calls from Rayon threads contend only when they hash to the same shard.
//! Shard selection uses the lower 4 bits of an `ahash` hash of the input string.
//!
//! After indexing, call [`StringInterner::compact()`] to merge all shard buffers
//! into a single contiguous buffer.  `compact()` returns a [`FrozenInterner`] and
//! a remap closure.  The remap closure converts mutable-phase handles (as returned
//! by `intern()`) to the stable handles used by `FrozenInterner::resolve()`.
//! The caller must apply the remap to every `InternedStr` stored in `MutableGraph`
//! nodes and edges before constructing `FrozenGraph`.
//!
//! # Handle encoding (mutable phase)
//! ```text
//! bits 31-28 : shard index (0–15)
//! bits 27-00 : byte offset within that shard's buffer
//! ```
//! After `compact()`, handles are plain global byte offsets into the merged buffer.
//!
//! # Buffer layout
//! Each string is stored as `[u32 length (LE)][UTF-8 bytes]`.
//! The handle value always points to the length header.

use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use ahash::AHashMap;
use rkyv::{Archive, Deserialize, Serialize};

use crate::InternedStr;

// ── Constants ─────────────────────────────────────────────────────────────────

const SHARD_COUNT: usize = 16;
/// Number of bits used to encode the shard index in a mutable-phase handle.
const OFFSET_BITS: u32 = 28;
/// Mask for the shard-local offset portion of a mutable-phase handle.
const OFFSET_MASK: u32 = (1 << OFFSET_BITS) - 1;
/// Maximum bytes per shard buffer (256 MiB).
const MAX_SHARD_BYTES: u32 = 1 << OFFSET_BITS;

// ── ShardInner ────────────────────────────────────────────────────────────────

struct ShardInner {
    buf: Vec<u8>,
    dedup: AHashMap<Box<str>, u32>,
}

impl ShardInner {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            dedup: AHashMap::new(),
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&offset) = self.dedup.get(s) {
            return offset;
        }
        let s_len = u32::try_from(s.len()).expect("string too long to intern (exceeds 4 GiB)");
        let offset =
            u32::try_from(self.buf.len()).expect("StringInterner shard buffer exceeded 4 GiB");
        assert!(
            offset.saturating_add(4).saturating_add(s_len) <= MAX_SHARD_BYTES,
            "StringInterner shard buffer exceeded 256 MiB"
        );
        self.buf.extend_from_slice(&s_len.to_le_bytes());
        self.buf.extend_from_slice(s.as_bytes());
        self.dedup.insert(s.into(), offset);
        offset
    }
}

// ── StringInterner ────────────────────────────────────────────────────────────

/// Concurrent 16-shard string interner for the indexing pipeline.
///
/// `intern()` is safe to call from multiple Rayon threads simultaneously.
/// After all strings have been interned, call `compact()` to produce a
/// [`FrozenInterner`] with a single contiguous buffer.
pub struct StringInterner {
    shards: [Mutex<ShardInner>; SHARD_COUNT],
}

impl StringInterner {
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::new(ShardInner::new())),
        }
    }

    /// Intern `s` and return a handle.
    ///
    /// If `s` was already interned, the existing handle is returned without
    /// any allocation.  Safe to call concurrently from multiple threads.
    pub fn intern(&self, s: &str) -> InternedStr {
        let shard_idx = shard_index(s);
        let local_offset = self.shards[shard_idx]
            .lock()
            .expect("StringInterner shard lock poisoned")
            .intern(s);
        encode_handle(shard_idx, local_offset)
    }

    /// Compact all shard buffers into a single contiguous buffer.
    ///
    /// Returns `(frozen, remap)`:
    /// - `frozen` — the immutable string table.
    /// - `remap` — a closure that converts every `InternedStr` handle produced
    ///   by `intern()` to its corresponding stable handle valid for
    ///   `FrozenInterner::resolve()`.
    ///
    /// The caller must apply `remap` to every `InternedStr` in `MutableGraph`
    /// before building `FrozenGraph`.
    pub fn compact(self) -> (FrozenInterner, impl Fn(InternedStr) -> InternedStr) {
        let shard_bufs: Vec<Vec<u8>> = self
            .shards
            .into_iter()
            .map(|m| {
                m.into_inner()
                    .expect("StringInterner shard lock poisoned")
                    .buf
            })
            .collect();

        // shard_start[i] is the global byte offset at which shard i begins.
        let mut shard_start = [0u32; SHARD_COUNT + 1];
        for i in 0..SHARD_COUNT {
            shard_start[i + 1] = shard_start[i] + shard_bufs[i].len() as u32;
        }

        let total = shard_start[SHARD_COUNT] as usize;
        let mut merged = Vec::with_capacity(total);
        for buf in shard_bufs {
            merged.extend_from_slice(&buf);
        }

        let remap = move |handle: InternedStr| -> InternedStr {
            let shard = (handle.0 >> OFFSET_BITS) as usize;
            let local = handle.0 & OFFSET_MASK;
            InternedStr(shard_start[shard] + local)
        };

        (FrozenInterner { buf: merged }, remap)
    }
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

// ── FrozenInterner ────────────────────────────────────────────────────────────

/// An immutable, single-buffer string table produced by [`StringInterner::compact()`].
///
/// Handles stored in `FrozenGraph` are global byte offsets into `buf`.
/// `resolve()` is a single bounds-checked slice operation — O(1), lock-free.
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
pub struct FrozenInterner {
    buf: Vec<u8>,
}

impl FrozenInterner {
    /// Resolve `handle` to the string it was interned from.
    ///
    /// # Panics
    /// Panics if `handle` does not refer to a valid position in the buffer.
    pub fn resolve(&self, handle: InternedStr) -> &str {
        let offset = handle.0 as usize;
        let len_bytes: [u8; 4] = self.buf[offset..offset + 4]
            .try_into()
            .expect("FrozenInterner: handle offset out of range");
        let len = u32::from_le_bytes(len_bytes) as usize;
        let bytes = &self.buf[offset + 4..offset + 4 + len];
        // SAFETY: Only `&str` values (valid UTF-8) are written to the buffer
        // via `ShardInner::intern`.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    /// Total byte size of the compacted string buffer.
    pub fn buf_len(&self) -> usize {
        self.buf.len()
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Return the shard index for `s` using the lower 4 bits of its ahash.
#[inline]
fn shard_index(s: &str) -> usize {
    let mut h = ahash::AHasher::default();
    s.hash(&mut h);
    (h.finish() & 0xF) as usize
}

/// Pack a shard index and shard-local byte offset into a mutable-phase handle.
#[inline]
fn encode_handle(shard_idx: usize, local_offset: u32) -> InternedStr {
    InternedStr((shard_idx as u32) << OFFSET_BITS | local_offset)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Intern all `strings`, compact, remap, and return `(frozen, handles)`.
    fn build(strings: &[&str]) -> (FrozenInterner, Vec<InternedStr>) {
        let interner = StringInterner::new();
        let raw: Vec<InternedStr> = strings.iter().map(|s| interner.intern(s)).collect();
        let (frozen, remap) = interner.compact();
        let handles = raw.iter().map(|&h| remap(h)).collect();
        (frozen, handles)
    }

    mod string_interner {
        use super::*;

        #[test]
        fn intern_and_resolve_single_string() {
            let (frozen, handles) = build(&["hello"]);
            assert_eq!(frozen.resolve(handles[0]), "hello");
        }

        #[test]
        fn intern_and_resolve_multiple_strings() {
            let strings = ["foo", "bar", "baz", "qux"];
            let (frozen, handles) = build(&strings);
            for (i, &s) in strings.iter().enumerate() {
                assert_eq!(frozen.resolve(handles[i]), s);
            }
        }

        #[test]
        fn duplicate_intern_returns_same_handle() {
            let interner = StringInterner::new();
            let h1 = interner.intern("duplicate");
            let h2 = interner.intern("duplicate");
            assert_eq!(h1, h2);
        }

        #[test]
        fn different_strings_get_different_handles() {
            let interner = StringInterner::new();
            let h1 = interner.intern("alpha");
            let h2 = interner.intern("beta");
            assert_ne!(h1, h2);
        }

        #[test]
        fn intern_empty_string() {
            let (frozen, handles) = build(&[""]);
            assert_eq!(frozen.resolve(handles[0]), "");
        }

        #[test]
        fn intern_unicode() {
            let strings = ["héllo", "日本語", "🦀"];
            let (frozen, handles) = build(&strings);
            for (i, &s) in strings.iter().enumerate() {
                assert_eq!(frozen.resolve(handles[i]), s);
            }
        }

        #[test]
        fn deduplication_caps_buf_len() {
            let interner = StringInterner::new();
            interner.intern("repeated");
            interner.intern("repeated");
            interner.intern("repeated");
            let (frozen, _) = interner.compact();
            // "repeated" = 4 bytes (len) + 8 bytes (data) = 12 bytes, stored once.
            assert_eq!(frozen.buf_len(), 12);
        }

        #[test]
        fn buf_len_accounts_for_all_unique_strings() {
            // Two non-duplicate 2-byte strings: each costs 4 + 2 = 6 bytes.
            let (frozen, _) = build(&["ab", "cd"]);
            assert_eq!(frozen.buf_len(), 12);
        }

        #[test]
        fn remap_produces_valid_frozen_handles() {
            let interner = StringInterner::new();
            let raw = interner.intern("remap_test");
            let (frozen, remap) = interner.compact();
            assert_eq!(frozen.resolve(remap(raw)), "remap_test");
        }

        #[test]
        fn strings_distributed_across_multiple_shards() {
            let interner = StringInterner::new();
            for i in 0..100 {
                interner.intern(&format!("symbol_{i}"));
            }
            let non_empty = interner
                .shards
                .iter()
                .filter(|s| !s.lock().unwrap().buf.is_empty())
                .count();
            assert!(
                non_empty > 1,
                "100 strings should land in more than one shard"
            );
        }

        #[test]
        fn concurrent_intern_is_consistent() {
            use std::sync::Arc;

            let interner = Arc::new(StringInterner::new());
            // 8 threads each intern the same 10 strings.
            let shared = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];

            let all_handles: Vec<Vec<InternedStr>> = std::thread::scope(|scope| {
                (0..8)
                    .map(|_| {
                        let interner = Arc::clone(&interner);
                        scope
                            .spawn(move || {
                                shared
                                    .iter()
                                    .map(|&s| interner.intern(s))
                                    .collect::<Vec<_>>()
                            })
                            .join()
                            .unwrap()
                    })
                    .collect()
            });

            // Every thread must get the same handle for each string.
            for thread_handles in &all_handles[1..] {
                assert_eq!(
                    all_handles[0], *thread_handles,
                    "all threads must return identical handles for the same strings"
                );
            }

            // Handles must resolve correctly after compaction.
            let interner = Arc::try_unwrap(interner).ok().unwrap();
            let (frozen, remap) = interner.compact();
            for (i, &s) in shared.iter().enumerate() {
                assert_eq!(frozen.resolve(remap(all_handles[0][i])), s);
            }
        }
    }

    mod frozen_interner {
        use super::*;

        #[test]
        fn empty_interner_has_zero_buf_len() {
            let interner = StringInterner::new();
            let (frozen, _) = interner.compact();
            assert_eq!(frozen.buf_len(), 0);
        }

        #[test]
        fn resolve_all_strings_after_many_interns() {
            let words = [
                "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog",
            ];
            let interner = StringInterner::new();
            // Intern each word once; collect the unique handle per word.
            let mut word_to_raw: std::collections::HashMap<&str, InternedStr> =
                std::collections::HashMap::new();
            for &w in &words {
                word_to_raw.entry(w).or_insert_with(|| interner.intern(w));
            }
            let (frozen, remap) = interner.compact();
            for (&word, &raw) in &word_to_raw {
                assert_eq!(frozen.resolve(remap(raw)), word);
            }
        }

        #[test]
        fn buf_len_is_sum_of_unique_string_storage() {
            let strings = ["one", "two", "three"];
            let (frozen, _) = build(&strings);
            // Each stored as: 4-byte len header + N bytes of data.
            let expected: usize = strings.iter().map(|s| 4 + s.len()).sum();
            assert_eq!(frozen.buf_len(), expected);
        }
    }
}
