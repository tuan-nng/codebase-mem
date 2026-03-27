//! Programming language classification for source files.
//!
//! Each variant represents a language detected by file extension.
//! The mapping from extension to language is implemented via a direct
//! `match` on the lowercased extension string.

use std::fmt;

use rkyv::{Archive, Deserialize, Serialize};

// ── Language ─────────────────────────────────────────────────────────────────

/// Programming language detected by file extension.
///
/// Stored as a `u8` discriminant, enabling compact representations in
/// index structures and archived graph data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
#[rkyv(attr(derive(Debug, Clone, Copy, PartialEq, Eq, Hash)))]
#[repr(u8)]
pub enum Language {
    // Systems / Native
    Rust,
    C,
    Cpp,
    Go,
    Java,
    Python,
    TypeScript,
    JavaScript,
    CSharp,
    Swift,
    Kotlin,
    Ruby,
    Scala,
    Php,
    Shell,
    Zig,
    Html,
    Css,
    Json,
    Yaml,
    Toml,
    Markdown,
    Sql,
    // Sentinel
    Unknown,
}

impl Language {
    /// Total number of variants including the `Unknown` sentinel.
    pub const COUNT: usize = 24;

}

/// Compile-time guard: adding a variant without updating `COUNT` is a hard error.
const _: () = {
    let n: usize = match Language::Rust {
        Language::Rust
        | Language::C
        | Language::Cpp
        | Language::Go
        | Language::Java
        | Language::Python
        | Language::TypeScript
        | Language::JavaScript
        | Language::CSharp
        | Language::Swift
        | Language::Kotlin
        | Language::Ruby
        | Language::Scala
        | Language::Php
        | Language::Shell
        | Language::Zig
        | Language::Html
        | Language::Css
        | Language::Json
        | Language::Yaml
        | Language::Toml
        | Language::Markdown
        | Language::Sql
        | Language::Unknown => 24,
    };
    assert!(
        n == Language::COUNT,
        "Language::COUNT is out of sync with the actual variant count",
    );
};

impl fmt::Display for Language {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Language::Rust => "Rust",
            Language::C => "C",
            Language::Cpp => "C++",
            Language::Go => "Go",
            Language::Java => "Java",
            Language::Python => "Python",
            Language::TypeScript => "TypeScript",
            Language::JavaScript => "JavaScript",
            Language::CSharp => "C#",
            Language::Swift => "Swift",
            Language::Kotlin => "Kotlin",
            Language::Ruby => "Ruby",
            Language::Scala => "Scala",
            Language::Php => "PHP",
            Language::Shell => "Shell",
            Language::Zig => "Zig",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::Json => "JSON",
            Language::Yaml => "YAML",
            Language::Toml => "TOML",
            Language::Markdown => "Markdown",
            Language::Sql => "SQL",
            Language::Unknown => "Unknown",
        };
        f.write_str(s)
    }
}

// ── Extension lookup ──────────────────────────────────────────────────────────

/// Returns the language for a file extension (without leading dot), or `None`
/// if the extension is not recognized.
#[inline]
pub fn from_extension(ext: &str) -> Option<Language> {
    let ext = ext.to_ascii_lowercase();
    let lang = match ext.as_str() {
        "bash" | "csh" | "ps1" | "sh" | "zsh" => Language::Shell,
        "c" => Language::C,
        "cc" | "cpp" | "cxx" | "h" | "hpp" => Language::Cpp,
        "clj" | "scala" => Language::Scala,
        "cs" => Language::CSharp,
        "css" | "scss" => Language::Css,
        "go" => Language::Go,
        "htm" | "html" | "vue" => Language::Html,
        "java" => Language::Java,
        "js" | "jsx" | "mjs" => Language::JavaScript,
        "json" => Language::Json,
        "kt" | "kts" => Language::Kotlin,
        "md" | "markdown" | "txt" => Language::Markdown,
        "php" => Language::Php,
        "py" => Language::Python,
        "rb" => Language::Ruby,
        "rs" => Language::Rust,
        "sql" => Language::Sql,
        "swift" => Language::Swift,
        "toml" => Language::Toml,
        "ts" | "tsx" => Language::TypeScript,
        "yaml" | "yml" => Language::Yaml,
        "zig" => Language::Zig,
        _ => return None,
    };
    Some(lang)
}

/// Returns the language for a file path, based on its extension.
/// Returns `None` if the path has no extension or an unrecognized extension.
///
/// Handles double extensions (e.g. `.tar.gz` → uses `gz`).
#[inline]
pub fn from_path(path: &std::path::Path) -> Option<Language> {
    let name = path.file_name()?.to_str()?;
    // Find the last dot
    let ext = name.rsplit_once('.')?.1;
    from_extension(ext)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Language::COUNT guard
    #[test]
    fn language_count() {
        assert_eq!(Language::COUNT, 24);
    }

    // All variants have a discriminant
    #[test]
    fn all_variants_have_discriminant() {
        let variants = [
            Language::Rust,
            Language::C,
            Language::Cpp,
            Language::Go,
            Language::Java,
            Language::Python,
            Language::TypeScript,
            Language::JavaScript,
            Language::CSharp,
            Language::Swift,
            Language::Kotlin,
            Language::Ruby,
            Language::Scala,
            Language::Php,
            Language::Shell,
            Language::Zig,
            Language::Html,
            Language::Css,
            Language::Json,
            Language::Yaml,
            Language::Toml,
            Language::Markdown,
            Language::Sql,
            Language::Unknown,
        ];
        assert_eq!(variants.len(), 24);
    }

    // Display roundtrip for each variant
    #[test]
    fn display_rust() {
        assert_eq!(Language::Rust.to_string(), "Rust");
    }
    #[test]
    fn display_c() {
        assert_eq!(Language::C.to_string(), "C");
    }
    #[test]
    fn display_cpp() {
        assert_eq!(Language::Cpp.to_string(), "C++");
    }
    #[test]
    fn display_go() {
        assert_eq!(Language::Go.to_string(), "Go");
    }
    #[test]
    fn display_java() {
        assert_eq!(Language::Java.to_string(), "Java");
    }
    #[test]
    fn display_python() {
        assert_eq!(Language::Python.to_string(), "Python");
    }
    #[test]
    fn display_typescript() {
        assert_eq!(Language::TypeScript.to_string(), "TypeScript");
    }
    #[test]
    fn display_javascript() {
        assert_eq!(Language::JavaScript.to_string(), "JavaScript");
    }
    #[test]
    fn display_csharp() {
        assert_eq!(Language::CSharp.to_string(), "C#");
    }
    #[test]
    fn display_swift() {
        assert_eq!(Language::Swift.to_string(), "Swift");
    }
    #[test]
    fn display_kotlin() {
        assert_eq!(Language::Kotlin.to_string(), "Kotlin");
    }
    #[test]
    fn display_ruby() {
        assert_eq!(Language::Ruby.to_string(), "Ruby");
    }
    #[test]
    fn display_scala() {
        assert_eq!(Language::Scala.to_string(), "Scala");
    }
    #[test]
    fn display_php() {
        assert_eq!(Language::Php.to_string(), "PHP");
    }
    #[test]
    fn display_shell() {
        assert_eq!(Language::Shell.to_string(), "Shell");
    }
    #[test]
    fn display_zig() {
        assert_eq!(Language::Zig.to_string(), "Zig");
    }
    #[test]
    fn display_html() {
        assert_eq!(Language::Html.to_string(), "HTML");
    }
    #[test]
    fn display_css() {
        assert_eq!(Language::Css.to_string(), "CSS");
    }
    #[test]
    fn display_json() {
        assert_eq!(Language::Json.to_string(), "JSON");
    }
    #[test]
    fn display_yaml() {
        assert_eq!(Language::Yaml.to_string(), "YAML");
    }
    #[test]
    fn display_toml() {
        assert_eq!(Language::Toml.to_string(), "TOML");
    }
    #[test]
    fn display_markdown() {
        assert_eq!(Language::Markdown.to_string(), "Markdown");
    }
    #[test]
    fn display_sql() {
        assert_eq!(Language::Sql.to_string(), "SQL");
    }
    #[test]
    fn display_unknown() {
        assert_eq!(Language::Unknown.to_string(), "Unknown");
    }

    // from_extension
    #[test]
    fn from_ext_rust() {
        assert_eq!(from_extension("rs"), Some(Language::Rust));
    }
    #[test]
    fn from_ext_python() {
        assert_eq!(from_extension("py"), Some(Language::Python));
    }
    #[test]
    fn from_ext_typescript() {
        assert_eq!(from_extension("ts"), Some(Language::TypeScript));
    }
    #[test]
    fn from_ext_tsx() {
        assert_eq!(from_extension("tsx"), Some(Language::TypeScript));
    }
    #[test]
    fn from_ext_cpp() {
        assert_eq!(from_extension("cpp"), Some(Language::Cpp));
    }
    #[test]
    fn from_ext_go() {
        assert_eq!(from_extension("go"), Some(Language::Go));
    }
    #[test]
    fn from_ext_java() {
        assert_eq!(from_extension("java"), Some(Language::Java));
    }
    #[test]
    fn from_ext_c() {
        assert_eq!(from_extension("c"), Some(Language::C));
    }
    #[test]
    fn from_ext_header() {
        assert_eq!(from_extension("h"), Some(Language::Cpp));
    }
    #[test]
    fn from_ext_json() {
        assert_eq!(from_extension("json"), Some(Language::Json));
    }
    #[test]
    fn from_ext_yaml() {
        assert_eq!(from_extension("yaml"), Some(Language::Yaml));
        assert_eq!(from_extension("yml"), Some(Language::Yaml));
    }
    #[test]
    fn from_ext_toml() {
        assert_eq!(from_extension("toml"), Some(Language::Toml));
    }
    #[test]
    fn from_ext_shell() {
        assert_eq!(from_extension("sh"), Some(Language::Shell));
        assert_eq!(from_extension("bash"), Some(Language::Shell));
        assert_eq!(from_extension("zsh"), Some(Language::Shell));
    }
    #[test]
    fn from_ext_swift() {
        assert_eq!(from_extension("swift"), Some(Language::Swift));
    }
    #[test]
    fn from_ext_kotlin() {
        assert_eq!(from_extension("kt"), Some(Language::Kotlin));
        assert_eq!(from_extension("kts"), Some(Language::Kotlin));
    }
    #[test]
    fn from_ext_ruby() {
        assert_eq!(from_extension("rb"), Some(Language::Ruby));
    }
    #[test]
    fn from_ext_scala() {
        assert_eq!(from_extension("scala"), Some(Language::Scala));
        assert_eq!(from_extension("clj"), Some(Language::Scala));
    }
    #[test]
    fn from_ext_php() {
        assert_eq!(from_extension("php"), Some(Language::Php));
    }
    #[test]
    fn from_ext_csharp() {
        assert_eq!(from_extension("cs"), Some(Language::CSharp));
    }
    #[test]
    fn from_ext_zig() {
        assert_eq!(from_extension("zig"), Some(Language::Zig));
    }
    #[test]
    fn from_ext_html() {
        assert_eq!(from_extension("html"), Some(Language::Html));
        assert_eq!(from_extension("htm"), Some(Language::Html));
    }
    #[test]
    fn from_ext_css() {
        assert_eq!(from_extension("css"), Some(Language::Css));
        assert_eq!(from_extension("scss"), Some(Language::Css));
    }
    #[test]
    fn from_ext_sql() {
        assert_eq!(from_extension("sql"), Some(Language::Sql));
    }
    #[test]
    fn from_ext_markdown() {
        assert_eq!(from_extension("md"), Some(Language::Markdown));
        assert_eq!(from_extension("markdown"), Some(Language::Markdown));
    }
    #[test]
    fn from_ext_unknown() {
        assert_eq!(from_extension("xyz"), None);
        assert_eq!(from_extension("dat"), None);
        assert_eq!(from_extension("png"), None);
        assert_eq!(from_extension("rs.bak"), None);
        assert_eq!(from_extension(""), None);
    }
    #[test]
    fn from_ext_case_insensitive() {
        assert_eq!(from_extension("RS"), Some(Language::Rust));
        assert_eq!(from_extension("PY"), Some(Language::Python));
        assert_eq!(from_extension("Go"), Some(Language::Go));
        assert_eq!(from_extension("JAVA"), Some(Language::Java));
    }
    #[test]
    fn from_ext_too_long() {
        assert_eq!(from_extension("abcdefghijk"), None);
    }
    #[test]
    fn from_ext_jsx() {
        assert_eq!(from_extension("jsx"), Some(Language::JavaScript));
    }
    #[test]
    fn from_ext_mjs() {
        assert_eq!(from_extension("mjs"), Some(Language::JavaScript));
    }
    #[test]
    fn from_ext_vue() {
        assert_eq!(from_extension("vue"), Some(Language::Html));
    }

    // from_path
    #[test]
    fn from_path_main_rs() {
        assert_eq!(
            from_path(std::path::Path::new("src/main.rs")),
            Some(Language::Rust)
        );
    }
    #[test]
    fn from_path_index_ts() {
        assert_eq!(
            from_path(std::path::Path::new("pages/index.ts")),
            Some(Language::TypeScript)
        );
    }
    #[test]
    fn from_path_component_tsx() {
        assert_eq!(
            from_path(std::path::Path::new("components/Button.tsx")),
            Some(Language::TypeScript)
        );
    }
    #[test]
    fn from_path_tar_gz() {
        // .tar.gz should resolve to .gz (not recognized) → None
        assert_eq!(from_path(std::path::Path::new("archive.tar.gz")), None);
    }
    #[test]
    fn from_path_no_extension() {
        assert_eq!(from_path(std::path::Path::new("Makefile")), None);
    }
    #[test]
    fn from_path_makefile() {
        // Makefile has no extension
        assert_eq!(from_path(std::path::Path::new("Makefile")), None);
    }

    #[test]
    fn from_path_cmake() {
        // .txt maps to Markdown
        assert_eq!(
            from_path(std::path::Path::new("CMakeLists.txt")),
            Some(Language::Markdown)
        );
    }
    #[test]
    fn from_path_complex_path() {
        assert_eq!(
            from_path(std::path::Path::new("/home/user/project/src/lib.rs")),
            Some(Language::Rust)
        );
    }

    // rkyv roundtrip
    #[test]
    fn rkyv_roundtrip() {
        let languages = [
            Language::Rust,
            Language::Python,
            Language::TypeScript,
            Language::Unknown,
        ];

        for lang in languages {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&lang).unwrap();
            let recovered = rkyv::from_bytes::<Language, rkyv::rancor::Error>(&bytes).unwrap();
            assert_eq!(lang, recovered);
        }
    }

    // PartialEq / Eq
    #[test]
    fn language_equality() {
        assert_eq!(Language::Rust, Language::Rust);
        assert_ne!(Language::Rust, Language::Python);
    }

    // Hash
    #[test]
    fn language_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Language::Rust);
        set.insert(Language::Python);
        set.insert(Language::Rust); // duplicate
        assert_eq!(set.len(), 2);
    }
}
