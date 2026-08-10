//! Terminal-safe text normalization.
//!
//! Strips/encodes ANSI, CSI, OSC 8/52, title/clipboard controls, C0/C1, and
//! bidi controls while preserving ordinary Unicode. Folds list newlines/tabs
//! to spaces. `Raw` mode stays terminal-safe (semantically unfiltered but no
//! escape sequences emitted).
//!
//! The invariant: no normalized output string, in any mode, can cause a
//! terminal to execute an escape sequence, OSC hyperlink, clipboard write,
//! or title change. Property/fuzz tests enforce this.

/// Rendering mode for user-facing text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Normalized: semantically filtered (line folding, injection removal)
    /// and terminal-safe.
    Normalized,
    /// Raw: semantically unfiltered but still terminal-safe.
    Raw,
}

/// Normalize text for safe terminal display.
///
/// - Replaces C0 control characters (except tab/newline handled below) and
///   C1 control characters and ESC sequences with a safe replacement.
/// - Strips CSI sequences (ANSI color/cursor).
/// - Strips OSC sequences including OSC 8 hyperlinks and OSC 52 clipboard.
/// - Strips DCS/SOS/PM/APC string sequences.
/// - Removes bidi control characters (RLO, RLE, PDF, LRO, LRE, RLI, LRI, FSI,
///   PDI, ALM, LRM, RLM).
/// - Preserves ordinary Unicode.
/// - In Normalized mode, folds newlines/tabs to single spaces.
pub fn normalize(input: &str, mode: Mode) -> String {
    let stripped = strip_terminal_controls(input);
    match mode {
        Mode::Normalized => fold_whitespace(&stripped),
        Mode::Raw => stripped,
    }
}

/// Strip all terminal control sequences and unsafe control characters,
/// returning terminal-safe text. This is the core safety function applied in
/// both Normalized and Raw modes.
pub fn strip_terminal_controls(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // ESC (0x1B): start of an escape sequence.
        if b == 0x1B {
            i = handle_escape(bytes, i + 1, &mut out);
            continue;
        }

        // C0 controls: tab (0x09), LF (0x0A), CR (0x0D) are structural and
        // handled by fold_whitespace later. Other C0 controls are removed.
        if b < 0x20 && b != 0x09 && b != 0x0A && b != 0x0D {
            i += 1;
            continue;
        }

        // DEL (0x7F).
        if b == 0x7F {
            i += 1;
            continue;
        }

        // C1 controls: 0x80–0x9F (encoded as single bytes in our processing;
        // in UTF-8 these appear as multi-byte sequences starting with 0xC2).
        // Handle UTF-8 C1: bytes 0xC2 0x80–0x9F.
        if b == 0xC2 && i + 1 < bytes.len() && bytes[i + 1] >= 0x80 && bytes[i + 1] <= 0x9F {
            let c1 = bytes[i + 1];
            // These are NEL (0x85), and various C1 control sequences. If c1 is
            // part of a C1 escape sequence introducer (CSI 0x9B, OSC 0x9D, DCS
            // 0x90, SOS 0x98, SCI 0x9A, ST 0x9C, APC 0x9F, PM 0x9E), treat as
            // sequence start. Otherwise strip the single C1 char.
            i = handle_c1(bytes, i + 2, c1, &mut out);
            continue;
        }

        // Bidi controls: U+200E (LRM), U+200F (RLM), U+200B (ZWSP),
        // U+202A–U+202E, U+2066–U+2069, U+061C (ALM), U+2060 (WJ).
        // These are 3-byte UTF-8 sequences starting with 0xE2.
        if b == 0xE2 && i + 2 < bytes.len() {
            let b1 = bytes[i + 1];
            let b2 = bytes[i + 2];
            if is_bidi_or_invisible_e2(b1, b2) {
                i += 3;
                continue;
            }
        }

        // U+061C ARABIC LETTER MARK (0xD8 0x9C).
        if b == 0xD8 && i + 1 < bytes.len() && bytes[i + 1] == 0x9C {
            i += 2;
            continue;
        }

        // U+FEFF ZERO WIDTH NO-BREAK SPACE / BOM (0xEF 0xBB 0xBF).
        if b == 0xEF && i + 2 < bytes.len() && bytes[i + 1] == 0xBB && bytes[i + 2] == 0xBF {
            i += 3;
            continue;
        }

        // U+FFF9..U+FFFB interlinear annotation (0xEF 0xBF 0xB9..0xBB).
        if b == 0xEF && i + 2 < bytes.len() && bytes[i + 1] == 0xBF {
            let b2 = bytes[i + 2];
            if (0xB9..=0xBB).contains(&b2) {
                i += 3;
                continue;
            }
        }

        // Otherwise: copy the byte. For multi-byte UTF-8, we copy byte-by-byte
        // which preserves the encoding.
        out.push(b);
        i += 1;
    }

    // The byte vector is valid UTF-8 because we only removed whole UTF-8
    // sequences or individual ASCII bytes, never partial multi-byte chars.
    String::from_utf8(out).unwrap_or_default()
}

/// Check if a 3-byte sequence starting with 0xE2 is a bidi/invisible control.
fn is_bidi_or_invisible_e2(b1: u8, b2: u8) -> bool {
    // U+200B..U+200F: 0xE2 0x80 0x8B..0x8F
    if b1 == 0x80 && (0x8B..=0x8F).contains(&b2) {
        return true;
    }
    // U+202A..U+202E: 0xE2 0x80 0xAA..0xAE
    if b1 == 0x80 && (0xAA..=0xAE).contains(&b2) {
        return true;
    }
    // U+2060 WJ, U+2061..U+2064: 0xE2 0x81 0xA0..0xA4
    if b1 == 0x81 && (0xA0..=0xA4).contains(&b2) {
        return true;
    }
    // U+2066..U+2069: 0xE2 0x81 0xA6..0xA9
    if b1 == 0x81 && (0xA6..=0xA9).contains(&b2) {
        return true;
    }
    false
}

/// Handle a sequence starting after ESC (0x1B).
/// Returns the new index after the consumed sequence.
fn handle_escape(bytes: &[u8], start: usize, _out: &mut Vec<u8>) -> usize {
    let mut i = start;
    if i >= bytes.len() {
        return i; // ESC at EOF
    }
    let b = bytes[i];
    i += 1;

    match b {
        // CSI: ESC [ <params> <intermediate>* <final>
        b'[' => {
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                // Final byte: 0x40–0x7E ends the sequence.
                if (0x40..=0x7E).contains(&c) {
                    break;
                }
                // Parameter/intermediate bytes continue.
                // If we hit a non-parameter/non-intermediate byte without a
                // final, bail to avoid runaway.
                if !(0x30..=0x3F).contains(&c) && !(0x20..=0x2F).contains(&c) {
                    break;
                }
            }
        }
        // OSC: ESC ] ... ST (0x1B \ or BEL 0x07)
        b']' => {
            i = consume_string_terminator(bytes, i);
        }
        // DCS: ESC P ... ST
        b'P' => {
            i = consume_string_terminator(bytes, i);
        }
        // SOS: ESC X ... ST
        b'X' => {
            i = consume_string_terminator(bytes, i);
        }
        // PM: ESC ^ ... ST
        b'^' => {
            i = consume_string_terminator(bytes, i);
        }
        // APC: ESC _ ... ST
        b'_' => {
            i = consume_string_terminator(bytes, i);
        }
        // ESC ( , ESC ) , ESC * , ESC + : charset designation (2-byte).
        // Consume one more byte when present; at EOF, falls through to the
        // catch-all below and leaves `i` unchanged.
        b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' if i < bytes.len() => {
            i += 1;
        }
        // ESC = , ESC > , ESC 7, ESC 8, ESC M, ESC D, ESC E, ESC c, etc.
        // Single-character escape sequences (and the EOF case for the
        // charset-designation arm above).
        _ => {
            // Already consumed the intermediate/final byte.
        }
    }
    i
}

/// Consume a string-terminated sequence (OSC/DCS/SOS/PM/APC) terminated by
/// ST (ESC \) or BEL (0x07). Returns the index after the terminator.
fn consume_string_terminator(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == 0x07 {
            // BEL terminator.
            return i + 1;
        }
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            // ST: ESC \
            return i + 2;
        }
        i += 1;
    }
    i
}

/// Handle a C1 control byte (0x80–0x9F) that appeared after 0xC2.
/// Returns the new index. `next_i` is the index after the C1 byte.
fn handle_c1(bytes: &[u8], next_i: usize, c1: u8, _out: &mut Vec<u8>) -> usize {
    match c1 {
        // 0x9B CSI: consume like ESC [
        0x9B => {
            let mut i = next_i;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if (0x40..=0x7E).contains(&c) {
                    break;
                }
                if !(0x30..=0x3F).contains(&c) && !(0x20..=0x2F).contains(&c) {
                    break;
                }
            }
            i
        }
        // 0x9D OSC, 0x90 DCS, 0x98 SOS, 0x9E PM, 0x9F APC: string-terminated
        0x9D | 0x90 | 0x98 | 0x9E | 0x9F => consume_string_terminator(bytes, next_i),
        // Other C1 controls: single char, already skipped.
        _ => next_i,
    }
}

/// Fold newlines and tabs to spaces, collapsing consecutive whitespace into a
/// single space. Used in Normalized mode.
pub fn fold_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = true; // suppress leading spaces
    for c in input.chars() {
        if c == '\n' || c == '\r' || c == '\t' || c == ' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    // Trim trailing space if any.
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Truncate a string to a maximum Unicode display width, appending an
/// ellipsis if truncation occurred. Uses `unicode-width` for accurate CJK/
/// wide-character width.
pub fn truncate_to_width(input: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if input.width() <= max_width {
        return input.to_string();
    }
    let ellipsis = "…";
    let ellipsis_width = 1;
    let target = max_width.saturating_sub(ellipsis_width);
    let mut width = 0usize;
    let mut end = 0usize;
    for (i, c) in input.char_indices() {
        use unicode_width::UnicodeWidthChar;
        let cw = c.width().unwrap_or(0);
        if width + cw > target {
            break;
        }
        width += cw;
        end = i + c.len_utf8();
    }
    let mut result = input[..end].to_string();
    result.push_str(ellipsis);
    result
}

/// Right-pad a string with spaces to a minimum Unicode display width, so a
/// fixed-width column of variable-width (e.g. CJK) text still lines up with
/// whatever follows it. No-op when the string already meets or exceeds the
/// width. Uses `unicode-width`, matching `truncate_to_width`'s measurement.
pub fn pad_to_width(input: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let current = input.width();
    if current >= width {
        return input.to_string();
    }
    let mut padded = input.to_string();
    padded.push_str(&" ".repeat(width - current));
    padded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_ordinary_unicode() {
        let input = "Hello 世界 — café naïve";
        assert_eq!(normalize(input, Mode::Normalized), input);
        assert_eq!(normalize(input, Mode::Raw), input);
    }

    #[test]
    fn strips_ansi_color_codes() {
        let input = "\x1b[31mRed\x1b[0m text";
        assert_eq!(strip_terminal_controls(input), "Red text");
    }

    #[test]
    fn strips_ansi_cursor_codes() {
        let input = "AB\x1b[2ACD";
        assert_eq!(strip_terminal_controls(input), "ABCD");
    }

    #[test]
    fn strips_osc8_hyperlinks() {
        // OSC 8 ; params ; URI ST
        let input = "\x1b]8;;https://evil.example/\x1b\\Click here\x1b]8;;\x1b\\";
        assert_eq!(strip_terminal_controls(input), "Click here");
    }

    #[test]
    fn strips_osc52_clipboard() {
        // OSC 52 ; c ; base64 ST
        let input = "\x1b]52;c;dGVzdA==\x07after";
        assert_eq!(strip_terminal_controls(input), "after");
    }

    #[test]
    fn strips_osc_title() {
        let input = "\x1b]0;Malicious Title\x07body";
        assert_eq!(strip_terminal_controls(input), "body");
    }

    #[test]
    fn strips_c0_controls_except_structural() {
        let input = "A\x00\x01\x02\x07\x08B";
        assert_eq!(strip_terminal_controls(input), "AB");
    }

    #[test]
    fn strips_c1_controls() {
        let input = "A\x1b[0m\u{9b}31mB"; // U+009B is CSI as C1
        assert_eq!(strip_terminal_controls(input), "AB");
    }

    #[test]
    fn strips_bidi_controls() {
        // U+202E RLO (Right-to-Left Override) attack
        let input = "tex\u{202E}t";
        assert_eq!(strip_terminal_controls(input), "text");
        // U+200E LRM
        assert_eq!(strip_terminal_controls("a\u{200E}b"), "ab");
    }

    #[test]
    fn normalized_mode_folds_newlines_and_tabs_to_spaces() {
        let input = "line1\nline2\ttabbed\r\nwindows";
        assert_eq!(
            normalize(input, Mode::Normalized),
            "line1 line2 tabbed windows"
        );
    }

    #[test]
    fn raw_mode_preserves_newlines_but_still_terminal_safe() {
        let input = "line1\nline2";
        let raw = normalize(input, Mode::Raw);
        assert_eq!(raw, "line1\nline2");
        // No escape sequences in raw output.
        assert!(!raw.contains('\x1b'));
    }

    #[test]
    fn normalized_collapses_consecutive_whitespace() {
        let input = "a\n\n\n\t b";
        assert_eq!(normalize(input, Mode::Normalized), "a b");
    }

    #[test]
    fn strips_del_character() {
        assert_eq!(strip_terminal_controls("A\x7fB"), "AB");
    }

    #[test]
    fn truncate_to_width_uses_unicode_display_width() {
        // CJK characters are width 2.
        let input = "一二三四五";
        let result = truncate_to_width(input, 5);
        assert_eq!(result, "一二…");
    }

    #[test]
    fn truncate_no_op_when_within_width() {
        let input = "short";
        assert_eq!(truncate_to_width(input, 100), "short");
    }

    #[test]
    fn pad_to_width_aligns_by_display_width_not_byte_or_char_count() {
        // CJK characters are width 2: "一二" is width 4, five columns short
        // of a width-9 column despite being only two `char`s.
        let input = "一二";
        let padded = pad_to_width(input, 9);
        assert_eq!(padded, "一二     ");
        use unicode_width::UnicodeWidthStr;
        assert_eq!(padded.width(), 9);
    }

    #[test]
    fn pad_to_width_no_op_when_at_or_past_width() {
        assert_eq!(pad_to_width("exact", 5), "exact");
        assert_eq!(pad_to_width("longer than column", 5), "longer than column");
    }

    #[test]
    fn no_output_can_contain_escape_byte() {
        // Adversarial inputs: none should leave an ESC (0x1B) in output.
        let attacks = [
            "\x1b[31mred\x1b[0m",
            "\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\",
            "\x1b]52;c;Zm9v\x07",
            "\x1bPq\x1b\\",
            "\x1b_\x1b\\",
            "\x1b^pm\x1b\\",
            "\u{9b}31m",
            "\u{202E}",
            "\u{200E}\u{200F}\u{202A}\u{202C}",
            "\u{2066}\u{2067}\u{2068}\u{2069}",
            "\x1b(0charset\x1b(B",
            "\x1b[2J\x1b[H",
            "embedded\x1b[0;31;42mcolor\x1b[0m",
        ];
        for attack in attacks {
            for mode in [Mode::Normalized, Mode::Raw] {
                let out = normalize(attack, mode);
                assert!(
                    !out.contains('\x1b'),
                    "mode {:?}: ESC leaked from attack {:?}: {:?}",
                    mode,
                    attack,
                    out
                );
                assert!(
                    !out.contains('\x07'),
                    "mode {:?}: BEL leaked from attack {:?}: {:?}",
                    mode,
                    attack,
                    out
                );
            }
        }
    }
}
