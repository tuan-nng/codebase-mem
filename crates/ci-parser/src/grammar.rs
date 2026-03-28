//! Bridge between [`ci_core::Language`] and tree-sitter's [`Language`] trait.
//!
//! Maps each [`ci_core::Language`] variant to the corresponding
//! `tree-sitter-<lang>::LANGUAGE` constant.  Languages without a grammar crate
//! return `None`.

use ci_core::Language as CoreLanguage;

/// Returns the tree-sitter [`tree_sitter_language::LanguageFn`] for `lang`, or
/// `None` if no grammar was compiled in (see feature flags in
/// [`Cargo.toml`](crate::CrateCargo)).
///
/// `LanguageFn` is `Send + Sync` and can be shared across threads.  It is
/// passed to [`tree_sitter::Language::new`] on each thread to construct a
/// fresh `Language` instance for the parser.
///
/// This function is the single point of truth for the language → grammar mapping.
#[inline]
pub fn get_language(lang: CoreLanguage) -> Option<tree_sitter_language::LanguageFn> {
    match lang {
        // ── Systems ────────────────────────────────────────────────────────────
        #[cfg(feature = "tree-sitter-rust")]
        CoreLanguage::Rust => Some(tree_sitter_rust::LANGUAGE),

        #[cfg(feature = "tree-sitter-c")]
        CoreLanguage::C => Some(tree_sitter_c::LANGUAGE),

        #[cfg(feature = "tree-sitter-cpp")]
        CoreLanguage::Cpp => Some(tree_sitter_cpp::LANGUAGE),

        #[cfg(feature = "tree-sitter-go")]
        CoreLanguage::Go => Some(tree_sitter_go::LANGUAGE),

        // ── Web ────────────────────────────────────────────────────────────────
        #[cfg(feature = "tree-sitter-javascript")]
        CoreLanguage::JavaScript => Some(tree_sitter_javascript::LANGUAGE),

        #[cfg(feature = "tree-sitter-typescript")]
        CoreLanguage::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),

        #[cfg(feature = "tree-sitter-html")]
        CoreLanguage::Html => Some(tree_sitter_html::LANGUAGE),

        #[cfg(feature = "tree-sitter-css")]
        CoreLanguage::Css => Some(tree_sitter_css::LANGUAGE),

        // ── Script ─────────────────────────────────────────────────────────────
        #[cfg(feature = "tree-sitter-python")]
        CoreLanguage::Python => Some(tree_sitter_python::LANGUAGE),

        #[cfg(feature = "tree-sitter-bash")]
        CoreLanguage::Shell => Some(tree_sitter_bash::LANGUAGE),

        // ── JVM ────────────────────────────────────────────────────────────────
        #[cfg(feature = "tree-sitter-java")]
        CoreLanguage::Java => Some(tree_sitter_java::LANGUAGE),

        #[cfg(feature = "tree-sitter-scala")]
        CoreLanguage::Scala => Some(tree_sitter_scala::LANGUAGE),

        #[cfg(feature = "tree-sitter-kotlin-ng")]
        CoreLanguage::Kotlin => Some(tree_sitter_kotlin_ng::LANGUAGE),

        // ── .NET / Mobile ─────────────────────────────────────────────────────
        #[cfg(feature = "tree-sitter-c-sharp")]
        CoreLanguage::CSharp => Some(tree_sitter_c_sharp::LANGUAGE),

        #[cfg(feature = "tree-sitter-swift")]
        CoreLanguage::Swift => Some(tree_sitter_swift::LANGUAGE),

        // ── Config / data formats ─────────────────────────────────────────────
        #[cfg(feature = "tree-sitter-json")]
        CoreLanguage::Json => Some(tree_sitter_json::LANGUAGE),

        #[cfg(feature = "tree-sitter-yaml")]
        CoreLanguage::Yaml => Some(tree_sitter_yaml::LANGUAGE),

        // ── Other ─────────────────────────────────────────────────────────────
        #[cfg(feature = "tree-sitter-ruby")]
        CoreLanguage::Ruby => Some(tree_sitter_ruby::LANGUAGE),

        #[cfg(feature = "tree-sitter-php")]
        CoreLanguage::Php => Some(tree_sitter_php::LANGUAGE_PHP),

        #[cfg(feature = "tree-sitter-zig")]
        CoreLanguage::Zig => Some(tree_sitter_zig::LANGUAGE),

        // Languages with no grammar compiled in.
        CoreLanguage::Toml
        | CoreLanguage::Markdown
        | CoreLanguage::Sql
        | CoreLanguage::Unknown => None,
    }
}

/// Returns `true` if `lang` has a grammar compiled in.
#[inline]
pub fn is_supported(lang: CoreLanguage) -> bool {
    get_language(lang).is_some()
}
