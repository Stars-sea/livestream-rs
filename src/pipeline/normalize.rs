use regex::Regex;
use std::sync::OnceLock;

/// Normalize a string to be safe for file paths, URLs, etc.
pub(crate) fn normalize_component(input: &str) -> String {
    static RE_NON_SAFE: OnceLock<Regex> = OnceLock::new();
    let re = RE_NON_SAFE.get_or_init(|| Regex::new(r"[^A-Za-z0-9._-]+").expect("valid regex"));
    re.replace_all(input, "-").to_string()
}
