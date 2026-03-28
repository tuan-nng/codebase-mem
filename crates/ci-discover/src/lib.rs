//! Gitignore-aware file discovery with language classification.
//!
//! Walks a directory tree (respecting `.gitignore`, global excludes, and
//! `.git/info/exclude`) and emits [`DiscoveredFile`] items as a parallel
//! iterator.  Language detection uses a static sorted array with binary search
//! over the file extension.
//!
//! # Performance
//!
//! - Directory traversal: [`ignore::WalkBuilder`] (gitignore-aware)
//! - Extension lookup: O(log N) binary search, fully inlined — ~5 comparisons
//!   for the current language set
//! - Output: [`rayon::ParallelIterator`] via [`into_par_iter`](rayon::iter::IntoParallelIterator::into_par_iter)

use std::path::{Path, PathBuf};

use ignore::DirEntry;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

pub use ci_core::{from_extension, from_path, Language};

// ── DiscoveredFile ────────────────────────────────────────────────────────────

/// A file discovered by the walker, annotated with its detected language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Detected programming language, based on file extension.
    pub language: Language,
}

// ── Discovery configuration ───────────────────────────────────────────────────

/// Configuration for the discovery process.
#[derive(Debug, Clone)]
pub struct DiscoverConfig {
    /// Skip hidden files (those starting with `.`). Default: `true`.
    pub skip_hidden: bool,
    /// Skip directories starting with `.` (e.g. `.git`, `.venv`). Default: `true`.
    pub skip_dotdirs: bool,
    /// Maximum directory depth. `None` means unlimited. Default: `None`.
    pub max_depth: Option<usize>,
}

impl DiscoverConfig {
    /// Default configuration with all filters enabled.
    #[inline]
    pub fn new() -> Self {
        Self {
            skip_hidden: true,
            skip_dotdirs: true,
            max_depth: None,
        }
    }

    /// Disable skipping hidden files.
    #[inline]
    pub fn include_hidden(mut self) -> Self {
        self.skip_hidden = false;
        self.skip_dotdirs = false;
        self
    }

    /// Set the maximum directory depth.
    #[inline]
    pub fn max_depth(mut self, depth: Option<usize>) -> Self {
        self.max_depth = depth;
        self
    }
}

impl Default for DiscoverConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ── Discovery ────────────────────────────────────────────────────────────────

/// Convert a `DirEntry` to a `DiscoveredFile`, returning `None` if the file
/// should be skipped (not a regular file, unrecognized extension, etc.).
fn entry_to_file(entry: DirEntry) -> Option<DiscoveredFile> {
    // Skip non-regular files (directories, symlinks, etc.)
    if !entry.file_type()?.is_file() {
        return None;
    }

    let path = entry.path().to_path_buf();
    let language = from_path(&path)?;

    Some(DiscoveredFile { path, language })
}

/// Discover source files in `root`, respecting `.gitignore`, global excludes,
/// and `.git/info/exclude`.
///
/// Returns a parallel iterator over all files with recognized language
/// extensions.  Unrecognized extensions are silently skipped.
///
/// # Example
///
/// ```
/// use ci_discover::{discover, DiscoverConfig};
/// use rayon::iter::ParallelIterator;
///
/// discover("/path/to/repo", DiscoverConfig::new())
///     .for_each(|file| {
///         println!("{:?} → {}", file.path, file.language);
///     });
/// ```
pub fn discover(
    root: impl AsRef<Path>,
    config: DiscoverConfig,
) -> impl ParallelIterator<Item = DiscoveredFile> {
    let root = root.as_ref().to_path_buf();
    let skip_hidden = config.skip_hidden;
    let skip_dotdirs = config.skip_dotdirs;

    let entries: Vec<DiscoveredFile> = ignore::WalkBuilder::new(&root)
        // Enable all ignore sources. Note: hidden filtering is done post-discovery
        // so we can distinguish dotdirs (needed for ignore files) from dotfiles.
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .max_depth(config.max_depth)
        .build()
        .filter_map(|entry| entry.ok())
        .filter_map(entry_to_file)
        .filter(move |f| {
            // Filter dot-prefixed files (hidden files)
            if skip_hidden {
                let name = f.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.') {
                    return false;
                }
            }
            // Filter files from dot-prefixed directories (but still allow the
            // walker to enter those dirs for reading .gitignore files).
            // Check only components relative to root to avoid matching temp
            // directory paths like /tmp/.tmpXXXXX/.
            if skip_dotdirs {
                let rel = f.path.strip_prefix(&root).unwrap_or(&f.path);
                for component in rel.components() {
                    if let std::path::Component::Normal(s) = component {
                        if let Some(s) = s.to_str() {
                            if s.starts_with('.') {
                                return false;
                            }
                        }
                    }
                }
            }
            true
        })
        .collect();

    entries.into_par_iter()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── Temp project helpers ───────────────────────────────────────────────

    fn temp_project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let p = dir.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, content).unwrap();
        }
        dir
    }

    // ── Basic language detection ───────────────────────────────────────────

    #[test]
    fn finds_rust_files() {
        let dir = temp_project(&[
            ("src/main.rs", ""),
            ("src/lib.rs", ""),
            ("tests/integration.rs", ""),
        ]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(paths.contains(&"main.rs".into()));
        assert!(paths.contains(&"lib.rs".into()));
        assert!(paths.contains(&"integration.rs".into()));
    }

    #[test]
    fn finds_python_files() {
        let dir = temp_project(&[("main.py", ""), ("tests/test_main.py", "")]);

        let count = discover(dir.path(), DiscoverConfig::new())
            .filter(|f| f.language == Language::Python)
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn finds_typescript_files() {
        let dir = temp_project(&[("index.ts", ""), ("Button.tsx", "")]);

        let langs: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.language)
            .collect();

        assert!(langs.iter().all(|l| *l == Language::TypeScript));
    }

    #[test]
    fn finds_go_files() {
        let dir = temp_project(&[("main.go", ""), ("server/server.go", "")]);

        let count = discover(dir.path(), DiscoverConfig::new())
            .filter(|f| f.language == Language::Go)
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn finds_java_files() {
        let dir = temp_project(&[("Main.java", ""), ("utils/Helper.java", "")]);

        let count = discover(dir.path(), DiscoverConfig::new())
            .filter(|f| f.language == Language::Java)
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn finds_c_cpp_files() {
        let dir = temp_project(&[
            ("main.c", ""),
            ("util.h", ""),
            ("util.cc", ""),
            ("math.cpp", ""),
        ]);

        let count = discover(dir.path(), DiscoverConfig::new())
            .filter(|f| matches!(f.language, Language::C | Language::Cpp))
            .count();
        assert_eq!(count, 4);
    }

    #[test]
    fn finds_config_files() {
        let dir = temp_project(&[
            ("Cargo.toml", ""),
            ("config.yaml", ""),
            ("data.json", ""),
            ("script.sh", ""),
        ]);

        let langs: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.language)
            .collect();

        assert!(langs.contains(&Language::Toml));
        assert!(langs.contains(&Language::Yaml));
        assert!(langs.contains(&Language::Json));
        assert!(langs.contains(&Language::Shell));
    }

    // ── Skipping unrecognized extensions ────────────────────────────────────

    #[test]
    fn skips_unknown_extensions() {
        let dir = temp_project(&[("data.json", ""), ("blob.dat", ""), ("image.png", "")]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(paths.contains(&"data.json".into()));
        assert!(!paths.contains(&"blob.dat".into()));
        assert!(!paths.contains(&"image.png".into()));
    }

    #[test]
    fn skips_files_without_extension() {
        let dir = temp_project(&[("Makefile", ""), ("script", ""), ("main.rs", "")]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(paths.contains(&"main.rs".into()));
        assert!(!paths.contains(&"Makefile".into()));
        assert!(!paths.contains(&"script".into()));
    }

    // ── Dotfile / dotdir filtering ─────────────────────────────────────────

    #[test]
    fn skips_hidden_files_by_default() {
        let dir = temp_project(&[
            ("src/main.rs", ""),
            (".hidden.rs", ""),
            ("src/.hidden.rs", ""),
        ]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();

        assert!(paths.iter().any(|p| p.contains("main.rs")));
        assert!(!paths.iter().any(|p| p.contains(".hidden.rs")));
    }

    #[test]
    fn skips_dot_directories_by_default() {
        let dir = temp_project(&[
            ("src/main.rs", ""),
            (".git/config", "dummy"), // should be skipped even if file is recognizable
            (".hidden_dir/main.rs", ""),
        ]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();

        assert!(paths.iter().any(|p| p.contains("src/main.rs")));
        assert!(!paths.iter().any(|p| p.contains(".git/")));
        assert!(!paths.iter().any(|p| p.contains(".hidden_dir/")));
    }

    #[test]
    fn include_hidden_files() {
        let dir = temp_project(&[("src/main.rs", ""), (".hidden.rs", "")]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new().include_hidden())
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(paths.contains(&"main.rs".into()));
        assert!(paths.contains(&".hidden.rs".into()));
    }

    // ── Gitignore ──────────────────────────────────────────────────────────

    #[test]
    fn respects_gitignore() {
        let dir = temp_project(&[
            ("src/lib.rs", ""),
            ("target/debug/lib.rs", ""),
            (".gitignore", "target/\n"),
        ]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();

        // ignore crate's standard_filters skip target/ automatically
        assert!(paths.iter().any(|p| p.contains("src/lib.rs")));
        assert!(!paths.iter().any(|p| p.contains("target/")));
    }

    #[test]
    fn respects_custom_gitignore() {
        let dir = temp_project(&[
            ("src/lib.rs", ""),
            ("skip_me/main.rs", ""),
            (".gitignore", "skip_me/\n"),
        ]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();

        assert!(paths.iter().any(|p| p.contains("src/lib.rs")));
        assert!(!paths.iter().any(|p| p.contains("skip_me/")));
    }

    // ── Max depth ─────────────────────────────────────────────────────────

    #[test]
    fn respects_max_depth() {
        let dir = temp_project(&[
            ("root.rs", ""),
            ("src/nested.rs", ""),
            ("src/deep/deep.rs", ""),
        ]);

        let paths: Vec<_> = discover(dir.path(), DiscoverConfig::new().max_depth(Some(2)))
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();

        assert!(paths.iter().any(|p| p.ends_with("root.rs")));
        assert!(paths.iter().any(|p| p.ends_with("nested.rs")));
        assert!(!paths.iter().any(|p| p.ends_with("deep.rs")));
    }

    // ── Parallel iteration ─────────────────────────────────────────────────

    #[test]
    fn parallel_iterator_covers_all_files() {
        let dir = temp_project(&[
            ("a.rs", ""),
            ("b.rs", ""),
            ("c.rs", ""),
            ("d.rs", ""),
            ("e.rs", ""),
        ]);

        let count = discover(dir.path(), DiscoverConfig::new()).count();
        assert_eq!(count, 5);
    }

    #[test]
    fn parallel_iterator_returns_correct_languages() {
        let dir = temp_project(&[("main.rs", ""), ("main.py", ""), ("main.go", "")]);

        let files: Vec<_> = discover(dir.path(), DiscoverConfig::new()).collect();

        let rust = files.iter().find(|f| f.language == Language::Rust);
        let python = files.iter().find(|f| f.language == Language::Python);
        let go = files.iter().find(|f| f.language == Language::Go);

        assert!(rust.is_some());
        assert!(python.is_some());
        assert!(go.is_some());
    }

    // ── Edge cases ─────────────────────────────────────────────────────────

    #[test]
    fn empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let count = discover(dir.path(), DiscoverConfig::new()).count();
        assert_eq!(count, 0);
    }

    #[test]
    fn discovers_file_at_root() {
        let dir = temp_project(&[("root.rs", "")]);

        let count = discover(dir.path(), DiscoverConfig::new())
            .filter(|f| f.path.file_name().unwrap() == "root.rs")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn nested_directories() {
        let dir = temp_project(&[("a/b/c/d/e.rs", "")]);

        let count = discover(dir.path(), DiscoverConfig::new()).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn case_insensitive_extensions() {
        let dir = temp_project(&[("main.RS", ""), ("app.PY", ""), ("Server.GO", "")]);

        let langs: Vec<_> = discover(dir.path(), DiscoverConfig::new())
            .map(|f| f.language)
            .collect();

        assert!(langs.contains(&Language::Rust));
        assert!(langs.contains(&Language::Python));
        assert!(langs.contains(&Language::Go));
    }

    #[test]
    fn path_preserved() {
        let dir = temp_project(&[("src/lib.rs", "")]);

        let files: Vec<_> = discover(dir.path(), DiscoverConfig::new()).collect();
        assert_eq!(files.len(), 1);
        assert!(
            files[0].path.ends_with("src/lib.rs"),
            "path should be absolute: {:?}",
            files[0].path
        );
    }
}
