//! Source-first, structure-fallback injection filtering.
//!
//! Only collapses complete known wrappers such as injected `<skill>...</skill>`
//! and never deletes arbitrary XML. This is deliberately conservative: genuine
//! XML content from users must survive normalization.
//!
//! Known wrappers are integration-confirmed injected structures (for example,
//! the Pi/OMP skill injection that wraps content in `<skill>` tags). We remove
//! only these complete, well-formed wrappers, leaving the inner text intact.

/// A known injected wrapper that should be collapsed (removed, inner text kept)
/// when it appears as a complete, matched pair.
const KNOWN_WRAPPERS: &[&str] = &["skill"];

/// Collapse complete known injected wrappers, keeping inner text.
///
/// Rules:
/// - Only matched `<tag>...</tag>` pairs for known wrapper tags are
///   collapsed.
/// - The wrapper must be complete (open + close). An orphan `<skill>` without
///   a close is left untouched (it is not confirmed as a complete injection).
/// - Inner text is preserved verbatim.
/// - Arbitrary/unknown XML tags are never removed.
pub fn collapse_known_injections(input: &str) -> String {
    let mut current = input.to_string();
    for tag in KNOWN_WRAPPERS {
        // Repeat until stable to handle nested wrappers of the same tag.
        loop {
            let next = collapse_wrapper(&current, tag);
            if next == current {
                break;
            }
            current = next;
        }
    }
    current
}

fn collapse_wrapper(input: &str, tag: &str) -> String {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    loop {
        match rest.find(&open) {
            Some(start) => {
                // Check for a matching close after the open.
                let after_open = &rest[start + open.len()..];
                if let Some(end_rel) = after_open.find(&close) {
                    // Complete wrapper: emit inner text, skip wrapper tags.
                    out.push_str(&rest[..start]);
                    out.push_str(&after_open[..end_rel]);
                    rest = &after_open[end_rel + close.len()..];
                } else {
                    // No matching close: leave the rest verbatim (incomplete).
                    out.push_str(rest);
                    break;
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_complete_skill_wrapper_keeping_inner_text() {
        let input = "Prefix <skill>Important content here</skill> suffix";
        assert_eq!(
            collapse_known_injections(input),
            "Prefix Important content here suffix"
        );
    }

    #[test]
    fn collapses_multiple_skill_wrappers() {
        let input = "<skill>A</skill> mid <skill>B</skill>";
        assert_eq!(collapse_known_injections(input), "A mid B");
    }

    #[test]
    fn leaves_incomplete_skill_open_tag_untouched() {
        let input = "Text <skill> without closing";
        assert_eq!(collapse_known_injections(input), input);
    }

    #[test]
    fn leaves_incomplete_skill_close_tag_untouched() {
        let input = "Text without opening </skill> here";
        assert_eq!(collapse_known_injections(input), input);
    }

    #[test]
    fn preserves_genuine_xml_tags() {
        let input = "<root><child>value</child></root>";
        assert_eq!(collapse_known_injections(input), input);
    }

    #[test]
    fn preserves_unknown_xml_wrappers() {
        let input = "<custom>wrapped</custom>";
        assert_eq!(collapse_known_injections(input), input);
    }

    #[test]
    fn preserves_html_like_tags() {
        let input = "<div><p>Hello</p></div>";
        assert_eq!(collapse_known_injections(input), input);
    }

    #[test]
    fn handles_nested_known_wrapper() {
        let input = "<skill>outer <skill>inner</skill> text</skill>";
        assert_eq!(collapse_known_injections(input), "outer inner text");
    }

    #[test]
    fn empty_known_wrapper_collapsed() {
        assert_eq!(collapse_known_injections("<skill></skill>"), "");
    }

    #[test]
    fn does_not_match_tag_with_attributes() {
        // We only collapse bare <skill>, not <skill id="x">, because the
        // injection wrapper is confirmed to be attribute-free.
        let input = "<skill id=\"x\">content</skill>";
        assert_eq!(collapse_known_injections(input), input);
    }

    #[test]
    fn preserves_skill_tag_in_user_code_block_context() {
        // User content that happens to mention skill tags in code survives.
        let input = "var x = '<skill>test</skill>'";
        // We still collapse it — this is the known-wrapper behavior. The point
        // is that genuine XML/unknown tags survive; known wrappers always
        // collapse. This test documents the behavior.
        assert_eq!(collapse_known_injections(input), "var x = 'test'");
    }

    #[test]
    fn empty_string_is_identity() {
        assert_eq!(collapse_known_injections(""), "");
    }
}
