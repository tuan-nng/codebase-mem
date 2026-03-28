//! Registry of language extraction specifications.
//!
//! Adding a new language:
//! 1. Create `src/specs/<lang>.rs` with a `SPEC: LanguageSpec` const
//! 2. Add `mod <lang>;` below
//! 3. Add one match arm to `get_spec`
//!
//! No other changes needed anywhere in the codebase.

use std::sync::OnceLock;

use crate::lang_spec::LanguageSpec;
use crate::Language;

mod c;
mod cpp;
mod go;
mod java;
mod kotlin;
mod python;
mod rust;
mod typescript;

/// Runtime check that a spec's node rules are sorted by ts_kind.
/// Panics if the spec is misconfigured.
fn check_sorted(spec: &LanguageSpec) {
    for i in 0..spec.node_rules.len().saturating_sub(1) {
        assert!(
            spec.node_rules[i].ts_kind < spec.node_rules[i + 1].ts_kind,
            "node_rules must be sorted by ts_kind: {:?} should come before {:?}",
            spec.node_rules[i].ts_kind,
            spec.node_rules[i + 1].ts_kind,
        );
    }
}

/// Returns the declarative spec for `lang`, or `None` if unsupported.
pub fn get_spec(lang: Language) -> Option<&'static LanguageSpec> {
    static SPECS: OnceLock<std::collections::HashMap<Language, &'static LanguageSpec>> =
        OnceLock::new();

    // Only initialize once. The assert! inside will panic on first use if misconfigured.
    if SPECS.get().is_none() {
        let mut m = std::collections::HashMap::new();
        m.insert(Language::C, &c::SPEC);
        check_sorted(&c::SPEC);
        m.insert(Language::Cpp, &cpp::SPEC);
        check_sorted(&cpp::SPEC);
        m.insert(Language::Go, &go::SPEC);
        check_sorted(&go::SPEC);
        m.insert(Language::Java, &java::SPEC);
        check_sorted(&java::SPEC);
        m.insert(Language::Kotlin, &kotlin::SPEC);
        check_sorted(&kotlin::SPEC);
        m.insert(Language::Python, &python::SPEC);
        check_sorted(&python::SPEC);
        m.insert(Language::Rust, &rust::SPEC);
        check_sorted(&rust::SPEC);
        m.insert(Language::TypeScript, &typescript::SPEC);
        check_sorted(&typescript::SPEC);
        SPECS.set(m).ok();
    }

    SPECS.get().and_then(|m| m.get(&lang).copied())
}

/// Returns `true` if a spec is registered for `lang`.
pub fn has_spec(lang: Language) -> bool {
    get_spec(lang).is_some()
}
