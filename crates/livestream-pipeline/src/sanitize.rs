//! Shared string sanitization for externally-controlled identifiers.

/// Sanitize an externally-controlled stream/live identifier for safe use in
/// filesystem paths and object keys.
///
/// Keeps only `[A-Za-z0-9_-]`, replaces every other byte with `_`, and
/// truncates to 128 chars. This guarantees the result is a single safe path
/// segment: no `/`, `\`, `.` (so `..` traversal and hidden files are
/// impossible), no whitespace, and bounded length.
pub fn sanitize_stream_id(stream_id: &str) -> String {
    let mut out = String::with_capacity(stream_id.len().min(128));
    for b in stream_id.bytes() {
        let keep = b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        if out.len() == 128 {
            break;
        }
        out.push(if keep { b as char } else { '_' });
    }
    if out.is_empty() {
        out.push_str("unnamed");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sanitize_stream_id;

    #[test]
    fn keeps_alphanumerics_and_dashes() {
        assert_eq!(sanitize_stream_id("abc-123_XYZ"), "abc-123_XYZ");
    }

    #[test]
    fn neutralizes_path_traversal() {
        assert_eq!(sanitize_stream_id("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_stream_id("/abs/path"), "_abs_path");
        assert_eq!(sanitize_stream_id(".."), "__");
    }

    #[test]
    fn handles_empty_and_whitespace() {
        assert_eq!(sanitize_stream_id(""), "unnamed");
        assert_eq!(sanitize_stream_id("a b"), "a_b");
    }

    #[test]
    fn truncates_long_ids() {
        assert_eq!(sanitize_stream_id(&"x".repeat(300)).len(), 128);
    }
}
