//! rkyv-based serialization and mmap-backed zero-copy loading for FrozenGraph.
//!
//! E1-6 implements:
//! - `save()`: serialize FrozenGraph with rkyv, write magic header + checksum,
//!   atomic rename to destination
//! - `load()`: mmap file, validate header, zero-copy deserialize via rkyv,
//!   return MmapFrozenGraph

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use memmap2::{Mmap, MmapOptions};
use xxhash_rust::xxh3::xxh3_64;

use crate::FrozenGraph;

// ── Header format ──────────────────────────────────────────────────────────────

/// Magic bytes identifying this file format.
pub(crate) const MAGIC: &[u8; 4] = b"CIFF";
/// File format version. Bump when the schema changes.
const VERSION: u16 = 1;
/// Header is always exactly 32 bytes.
pub(crate) const HEADER_SIZE: usize = 32;

/// The 32-byte header prepended to every serialized graph file.
///
/// ```text
/// Offset  Size  Field             Description
///   0      4    magic             Fixed: b"CIFF"
///   4      2    version            File format version (little-endian)
///   6      1    reserved          Must be 0
///   7      1    checksum_algo     0 = xxh3_64
///   8      8    checksum           xxh3_64 of the rkyv payload (little-endian)
///  16      8    payload_len       Length of rkyv payload in bytes (little-endian)
///  24      8    reserved          Must be 0
/// ```
#[repr(C, packed)]
pub(crate) struct FrozenGraphHeader {
    magic: [u8; 4],
    version: u16,
    _reserved1: u8,
    checksum_algo: u8,
    checksum: u64,
    payload_len: u64,
    _reserved2: u64,
}

impl FrozenGraphHeader {
    fn new(payload_len: u64, checksum: u64) -> Self {
        Self {
            magic: *MAGIC,
            version: VERSION,
            _reserved1: 0,
            checksum_algo: 0,
            checksum,
            payload_len,
            _reserved2: 0,
        }
    }

    fn validate(&self) -> Option<HeaderError> {
        if &self.magic != MAGIC {
            return Some(HeaderError::InvalidMagic(self.magic));
        }
        if self.version != VERSION {
            return Some(HeaderError::UnsupportedVersion(self.version));
        }
        if self.checksum_algo != 0 {
            return Some(HeaderError::UnknownChecksumAlgo(self.checksum_algo));
        }
        if self._reserved1 != 0 {
            return Some(HeaderError::InvalidReservedByte(self._reserved1));
        }
        None
    }

    fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..6].copy_from_slice(&self.version.to_le_bytes());
        buf[6] = self._reserved1;
        buf[7] = self.checksum_algo;
        buf[8..16].copy_from_slice(&self.checksum.to_le_bytes());
        buf[16..24].copy_from_slice(&self.payload_len.to_le_bytes());
        buf[24..32].copy_from_slice(&self._reserved2.to_le_bytes());
        buf
    }

    fn from_bytes(buf: &[u8; HEADER_SIZE]) -> Self {
        Self {
            magic: buf[0..4].try_into().unwrap(),
            version: u16::from_le_bytes([buf[4], buf[5]]),
            _reserved1: buf[6],
            checksum_algo: buf[7],
            checksum: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            payload_len: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            _reserved2: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
        }
    }
}

/// Errors that can occur when parsing a `FrozenGraphHeader`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("invalid magic number: expected b\"CIFF\", got {0:?}")]
    InvalidMagic([u8; 4]),

    #[error("unsupported file format version: {0}")]
    UnsupportedVersion(u16),

    #[error("unknown checksum algorithm id: {0}")]
    UnknownChecksumAlgo(u8),

    #[error("reserved byte at offset 6 must be 0, got {0}")]
    InvalidReservedByte(u8),

    #[error("checksum mismatch: expected {expected}, computed {actual}")]
    ChecksumMismatch { expected: u64, actual: u64 },

    #[error("payload truncated: header claims {expected} bytes, file has {actual}")]
    PayloadTruncated { expected: u64, actual: u64 },
}

impl From<HeaderError> for std::io::Error {
    fn from(e: HeaderError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    }
}

// ── MmapFrozenGraph ───────────────────────────────────────────────────────────

/// A memory-mapped view of a serialized [`FrozenGraph`].
///
/// The underlying file is memory-mapped for efficient access. On load,
/// the archived bytes are deserialized into an owned `FrozenGraph`.
/// Dropping `MmapFrozenGraph` releases the mapping.
#[derive(Debug)]
pub struct MmapFrozenGraph {
    /// Owns the memory-mapped region. Dropped last.
    mmap: Mmap,
    /// Deserialized graph. All queries use this owned copy.
    graph: FrozenGraph,
}

impl MmapFrozenGraph {
    /// Loads a graph from `path` with full validation:
    /// 1. Validates magic + version in the header
    /// 2. Verifies the xxh3_64 checksum of the payload
    /// 3. Validates and deserializes the rkyv payload into an owned `FrozenGraph`
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).open(path)?;
        let mmap = unsafe { Mmap::map(&file) }?;
        Self::from_mmap(mmap)
    }

    /// Loads from an already-open file descriptor at a specific offset and length.
    /// Useful for advanced use cases where the caller manages the file.
    pub fn load_from(file: &File, offset: u64, len: usize) -> std::io::Result<Self> {
        let mmap = unsafe { MmapOptions::new().offset(offset).len(len).map(file)? };
        Self::from_mmap(mmap)
    }

    /// Returns the total number of nodes in the graph.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Returns the total number of edges in the graph.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Returns the label for `node`.
    #[inline]
    pub fn node_label(&self, node: ci_core::NodeId) -> ci_core::NodeLabel {
        self.graph.node_label(node)
    }

    /// Returns the interned name for `node`.
    #[inline]
    pub fn node_name(&self, node: ci_core::NodeId) -> ci_core::InternedStr {
        self.graph.node_name(node)
    }

    /// Returns the interned source file for `node`.
    #[inline]
    pub fn node_file(&self, node: ci_core::NodeId) -> ci_core::InternedStr {
        self.graph.node_file(node)
    }

    /// Returns the line number for `node` (1-based, 0 = unknown).
    #[inline]
    pub fn node_line(&self, node: ci_core::NodeId) -> u32 {
        self.graph.node_line(node)
    }

    /// Returns the column number for `node` (1-based, 0 = unknown).
    #[inline]
    pub fn node_column(&self, node: ci_core::NodeId) -> u32 {
        self.graph.node_column(node)
    }

    /// Returns the range of forward (outgoing) edges for `node`.
    #[inline]
    pub fn forward_edge_range(&self, node: ci_core::NodeId) -> core::ops::Range<usize> {
        self.graph.forward_edge_range(node)
    }

    /// Iterates over all outgoing edges of `node`.
    #[inline]
    pub fn forward_edges(
        &self,
        node: ci_core::NodeId,
    ) -> impl Iterator<Item = (ci_core::NodeId, ci_core::EdgeType)> + '_ {
        self.graph.forward_edges(node)
    }

    /// Returns the range of reverse (incoming) edges for `node`.
    #[inline]
    pub fn reverse_edge_range(&self, node: ci_core::NodeId) -> core::ops::Range<usize> {
        self.graph.reverse_edge_range(node)
    }

    /// Iterates over all incoming edges of `node`.
    #[inline]
    pub fn reverse_edges(
        &self,
        node: ci_core::NodeId,
    ) -> impl Iterator<Item = (ci_core::NodeId, ci_core::EdgeType)> + '_ {
        self.graph.reverse_edges(node)
    }

    /// Returns the string interner for resolving `InternedStr` handles.
    #[inline]
    pub fn interner(&self) -> &ci_core::FrozenInterner {
        self.graph.interner()
    }

    /// Returns the `RoaringBitmap` of all node IDs with the given label.
    #[inline]
    pub fn nodes_with_label(&self, label: ci_core::NodeLabel) -> roaring::RoaringBitmap {
        self.graph.nodes_with_label(label)
    }

    /// Returns the `NodeId` for the given qualified-name handle, if any.
    #[inline]
    pub fn lookup_qn(&self, name: ci_core::InternedStr) -> Option<ci_core::NodeId> {
        self.graph.lookup_qn(name)
    }

    /// Returns all nodes belonging to the file with the given path handle.
    #[inline]
    pub fn nodes_in_file(&self, file: ci_core::InternedStr) -> &[ci_core::NodeId] {
        self.graph.nodes_in_file(file)
    }

    /// Returns the FST over all distinct symbol names, reconstructed from bytes.
    #[inline]
    pub fn bare_name_fst(&self) -> fst::Map<Vec<u8>> {
        fst::Map::new(self.graph.bare_name_fst_bytes().to_vec())
            .expect("bare_name_fst_bytes must be valid FST bytes")
    }

    /// Returns the raw FST bytes for the bare-name index.
    #[inline]
    pub fn bare_name_fst_bytes(&self) -> &[u8] {
        self.graph.bare_name_fst_bytes()
    }

    /// Returns all `NodeId`s whose symbol name maps to `slot` in the FST.
    #[inline]
    pub fn bare_name_nodes(&self, slot: u64) -> &[ci_core::NodeId] {
        self.graph.bare_name_nodes(slot)
    }

    /// Consumes `self` and returns the underlying memory map.
    /// All graph accessors will be invalidated.
    pub fn into_mmap(self) -> Mmap {
        self.mmap
    }

    fn from_mmap(mmap: Mmap) -> std::io::Result<Self> {
        if mmap.len() < HEADER_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "file too small: {} bytes, expected at least {}",
                    mmap.len(),
                    HEADER_SIZE
                ),
            ));
        }

        let header = FrozenGraphHeader::from_bytes((&mmap[..HEADER_SIZE]).try_into().unwrap());

        if let Some(e) = header.validate() {
            return Err(e.into());
        }

        let payload_start = HEADER_SIZE;
        let payload_len = header.payload_len as usize;
        let payload_end = payload_start + payload_len;

        if mmap.len() < payload_end {
            return Err(HeaderError::PayloadTruncated {
                expected: header.payload_len,
                actual: (mmap.len() - HEADER_SIZE) as u64,
            }
            .into());
        }

        // Verify checksum
        let payload = &mmap[payload_start..payload_end];
        let computed = xxh3_64(payload);
        if computed != header.checksum {
            return Err(HeaderError::ChecksumMismatch {
                expected: header.checksum,
                actual: computed,
            }
            .into());
        }

        // Deserialize with rkyv — includes CheckBytes validation.
        let graph = rkyv::from_bytes::<FrozenGraph, rkyv::rancor::Error>(payload)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

        Ok(MmapFrozenGraph { mmap, graph })
    }
}

// ── Save ──────────────────────────────────────────────────────────────────────

/// Errors that can occur during graph serialization.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("rkyv serialization failed: {0}")]
    Serialization(String),

    #[error("failed to write file: {0}")]
    Io(std::io::Error),
}

/// Serializes `graph` and writes it to `path` with a magic header and checksum.
///
/// The write goes to `path.with_extension("bin.tmp")` first, then atomically
/// renamed to `path`. Readers never see a partially-written file.
pub fn save(graph: &FrozenGraph, path: &Path) -> Result<u64, SaveError> {
    // Serialize with rkyv high-level API.
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(graph)
        .map_err(|e| SaveError::Serialization(e.to_string()))?;

    let checksum = xxh3_64(&bytes);
    let payload_len = u64::try_from(bytes.len())
        .map_err(|_| SaveError::Serialization("graph exceeds 2^64 bytes".into()))?;

    let header = FrozenGraphHeader::new(payload_len, checksum);

    let tmp_path = path.with_extension("bin.tmp");
    {
        let mut file = File::create(&tmp_path).map_err(SaveError::Io)?;
        file.write_all(&header.to_bytes()).map_err(SaveError::Io)?;
        file.write_all(&bytes).map_err(SaveError::Io)?;
        file.sync_all().map_err(SaveError::Io)?;
    }
    std::fs::rename(&tmp_path, path).map_err(SaveError::Io)?;

    Ok(HEADER_SIZE as u64 + payload_len)
}

/// Returns the size in bytes of the serialized form of `graph`,
/// without writing to disk.
pub fn serialized_size(graph: &FrozenGraph) -> std::io::Result<u64> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(graph)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(HEADER_SIZE as u64 + bytes.len() as u64)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ci_core::{EdgeType, FrozenInterner, InternedStr, NodeId, NodeLabel, StringInterner};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::Path;

    use super::{FrozenGraphHeader, HeaderError, HEADER_SIZE};
    use crate::{save, FrozenGraph, MmapFrozenGraph, MutableGraph};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_interner(strings: &[&str]) -> (FrozenInterner, Vec<InternedStr>) {
        let si = StringInterner::new();
        let raw: Vec<InternedStr> = strings.iter().map(|s| si.intern(s)).collect();
        let (fi, remap) = si.compact();
        (fi, raw.into_iter().map(remap).collect())
    }

    fn empty_interner() -> FrozenInterner {
        StringInterner::new().compact().0
    }

    fn build_simple_frozen() -> FrozenGraph {
        let (interner, handles) = make_interner(&["file", "MyClass", "render", "src/a.rs"]);
        let mutable = MutableGraph::new();
        let n0 = mutable.add_node(NodeLabel::File, handles[0], handles[3], 0, 0);
        let n1 = mutable.add_node(NodeLabel::Class, handles[1], handles[3], 0, 0);
        let n2 = mutable.add_node(NodeLabel::Method, handles[2], handles[3], 0, 0);
        mutable.add_edge(n0, n1, EdgeType::Contains);
        mutable.add_edge(n0, n2, EdgeType::Contains);
        mutable.add_edge(n1, n2, EdgeType::Calls);
        mutable.freeze(interner)
    }

    // ── FrozenGraphHeader ────────────────────────────────────────────────────

    #[test]
    fn header_is_exactly_32_bytes() {
        assert_eq!(std::mem::size_of::<FrozenGraphHeader>(), 32);
    }

    #[test]
    fn header_validate_accepts_valid_header() {
        let header = FrozenGraphHeader::new(100, 0xDEADBEEF);
        assert!(header.validate().is_none());
    }

    #[test]
    fn header_validate_rejects_bad_magic() {
        let mut header = FrozenGraphHeader::new(100, 0);
        header.magic = *b"XXXX";
        assert!(matches!(
            header.validate(),
            Some(HeaderError::InvalidMagic(_))
        ));
    }

    #[test]
    fn header_validate_rejects_bad_version() {
        let mut header = FrozenGraphHeader::new(100, 0);
        header.version = 99;
        assert!(matches!(
            header.validate(),
            Some(HeaderError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn header_validate_rejects_bad_checksum_algo() {
        let mut header = FrozenGraphHeader::new(100, 0);
        header.checksum_algo = 1;
        assert!(matches!(
            header.validate(),
            Some(HeaderError::UnknownChecksumAlgo(1))
        ));
    }

    #[test]
    fn header_validate_rejects_bad_reserved_byte() {
        let mut header = FrozenGraphHeader::new(100, 0);
        header._reserved1 = 0xFF;
        assert!(matches!(
            header.validate(),
            Some(HeaderError::InvalidReservedByte(0xFF))
        ));
    }

    // ── Save / Load round-trip ───────────────────────────────────────────────

    mod save_and_load {
        use super::*;
        use std::fs::OpenOptions;
        use tempfile::tempdir;

        #[test]
        fn save_produces_non_empty_file() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");

            let bytes = save(&frozen, &path).unwrap();
            assert!(bytes > 0);
            assert!(path.exists());
        }

        #[test]
        fn load_recovers_graph_metadata() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let loaded = MmapFrozenGraph::load(&path).unwrap();

            assert_eq!(loaded.node_count(), frozen.node_count());
            assert_eq!(loaded.edge_count(), frozen.edge_count());
        }

        #[test]
        fn load_recovers_node_data() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let loaded = MmapFrozenGraph::load(&path).unwrap();

            assert_eq!(loaded.node_label(NodeId(0)), NodeLabel::File);
            assert_eq!(loaded.node_label(NodeId(1)), NodeLabel::Class);
            assert_eq!(loaded.node_label(NodeId(2)), NodeLabel::Method);
        }

        #[test]
        fn load_recovers_forward_edges() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let loaded = MmapFrozenGraph::load(&path).unwrap();

            let n0_edges: Vec<_> = loaded.forward_edges(NodeId(0)).collect();
            assert_eq!(n0_edges.len(), 2);

            let n1_edges: Vec<_> = loaded.forward_edges(NodeId(1)).collect();
            assert_eq!(n1_edges.len(), 1);
            assert_eq!(n1_edges[0].0, NodeId(2));
            assert_eq!(n1_edges[0].1, EdgeType::Calls);
        }

        #[test]
        fn load_recovers_reverse_edges() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let loaded = MmapFrozenGraph::load(&path).unwrap();

            let n2_incoming: Vec<_> = loaded.reverse_edges(NodeId(2)).collect();
            assert_eq!(n2_incoming.len(), 2);
        }

        #[test]
        fn load_recovers_label_index() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let loaded = MmapFrozenGraph::load(&path).unwrap();

            let methods = loaded.nodes_with_label(NodeLabel::Method);
            assert!(methods.contains(2)); // "render" is a Method

            let files = loaded.nodes_with_label(NodeLabel::File);
            assert!(files.contains(0));
        }

        #[test]
        fn load_recovers_interner() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let loaded = MmapFrozenGraph::load(&path).unwrap();
            let interner = loaded.interner();

            let name_h = loaded.node_name(NodeId(2));
            assert_eq!(interner.resolve(name_h), "render");
        }

        #[test]
        fn load_recovers_fst() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let loaded = MmapFrozenGraph::load(&path).unwrap();
            let fst = loaded.bare_name_fst();

            assert!(fst.get("render").is_some());
            assert!(fst.get("MyClass").is_some());
            assert!(fst.get("file").is_some());
            assert!(fst.get("nonexistent").is_none());
        }

        #[test]
        fn save_atomic_rename_creates_final_file() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");

            save(&frozen, &path).unwrap();

            assert!(path.exists());
            let tmp_path = path.with_extension("bin.tmp");
            assert!(!tmp_path.exists());
        }

        #[test]
        fn load_rejects_truncated_file() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let file = OpenOptions::new().write(true).open(&path).unwrap();
            file.set_len(10).unwrap();
            drop(file);

            let err = MmapFrozenGraph::load(&path).unwrap_err();
            assert!(
                err.to_string().contains("truncated")
                    || err.to_string().contains("too small")
                    || err.to_string().contains("invalid"),
                "got: {}",
                err
            );
        }

        #[test]
        fn load_rejects_corrupted_checksum() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            // Flip a byte in the payload area.
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            file.seek(SeekFrom::Start(HEADER_SIZE as u64 + 10)).unwrap();
            let mut byte = [0u8];
            file.read_exact(&mut byte).unwrap();
            byte[0] ^= 0xFF;
            file.seek(SeekFrom::Start(HEADER_SIZE as u64 + 10)).unwrap();
            file.write_all(&byte).unwrap();
            drop(file);

            let err = MmapFrozenGraph::load(&path).unwrap_err();
            assert!(
                err.to_string().contains("checksum") || err.to_string().contains("Checksum"),
                "got: {}",
                err
            );
        }

        #[test]
        fn load_rejects_bad_magic() {
            let frozen = build_simple_frozen();
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");
            save(&frozen, &path).unwrap();

            let mut file = OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all(b"XXXX").unwrap();
            drop(file);

            let err = MmapFrozenGraph::load(&path).unwrap_err();
            assert!(
                err.to_string().contains("magic") || err.to_string().contains("Magic"),
                "got: {}",
                err
            );
        }

        #[test]
        fn load_rejects_nonexistent_file() {
            let err = MmapFrozenGraph::load(Path::new("/nonexistent/path/graph.bin")).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        }

        #[test]
        fn save_and_load_empty_graph() {
            let mutable = MutableGraph::new();
            let frozen = mutable.freeze(empty_interner());
            let dir = tempdir().unwrap();
            let path = dir.path().join("graph.bin");

            save(&frozen, &path).unwrap();
            let loaded = MmapFrozenGraph::load(&path).unwrap();

            assert_eq!(loaded.node_count(), 0);
            assert_eq!(loaded.edge_count(), 0);
        }
    }

    // ── serialized_size ─────────────────────────────────────────────────────

    mod serialized_size {
        use super::*;

        #[test]
        fn serialized_size_matches_actual_write() {
            let frozen = build_simple_frozen();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("graph.bin");

            let expected = crate::serialized_size(&frozen).unwrap();
            let actual = save(&frozen, &path).unwrap();

            assert_eq!(expected, actual);
        }

        #[test]
        fn serialized_size_is_reasonable() {
            let frozen = build_simple_frozen();
            let size = crate::serialized_size(&frozen).unwrap();
            // 3 nodes + 3 edges + strings + FST: at least a few hundred bytes
            assert!(size > 200);
            // But still small for a tiny graph
            assert!(size < 50_000);
        }
    }
}
