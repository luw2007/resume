//! Stable-Rust, bounded parser fuzz/property tests.
//!
//! We intentionally use proptest instead of `cargo-fuzz`: cargo-fuzz requires
//! nightly Rust while the project promises Rust 1.91 support. These cases run
//! on the stable/MSRV CI lanes, cap generated inputs at 16 KiB and use 64 cases
//! per property, giving deterministic resource bounds while continuously
//! exercising JSONL, terminal text, Scope paths, durations, and strict config.

use std::{io::Cursor, path::PathBuf, str::FromStr};

use proptest::prelude::*;
use resume::{
    cli::{Distance, Since},
    config::Config,
    jsonl::{Bounds, FileOutcome, read_buffered},
    scope::{DefaultScope, Direction, Scope, WorkspaceCandidate},
    text::{Mode, normalize, strip_terminal_controls},
};

const CASES: u32 = 64;
const MAX_INPUT: usize = 16 * 1024;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn jsonl_reader_is_bounded_and_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..=MAX_INPUT)) {
        let bounds = Bounds {
            max_line_bytes: 4096,
            max_file_bytes: MAX_INPUT as u64,
            max_records: 128,
            max_nesting: 32,
        };
        let mut cursor = Cursor::new(bytes);
        let result = read_buffered(&mut cursor, &bounds, 0).expect("Cursor reads cannot fail");
        prop_assert!(result.bytes_read <= bounds.max_file_bytes);
        prop_assert!(result.records.len() <= bounds.max_records);
        prop_assert!(matches!(result.outcome, FileOutcome::Complete | FileOutcome::IncompleteTail | FileOutcome::BoundExceeded));
    }

    #[test]
    fn text_normalization_never_emits_terminal_controls(input in ".{0,8192}") {
        for mode in [Mode::Normalized, Mode::Raw] {
            let output = normalize(&input, mode);
            prop_assert!(!output.contains(char::from(0x1b)));
            prop_assert!(!output.contains(char::from(0x07)));
            prop_assert!(!output.chars().any(is_unsafe_control));
        }
        let stripped = strip_terminal_controls(&input);
        prop_assert!(!stripped.contains(char::from(0x1b)));
    }

    #[test]
    fn scope_path_distances_obey_component_edges(
        components in prop::collection::vec("[a-z]{1,8}", 0..12),
        extra in prop::collection::vec("[a-z]{1,8}", 0..12),
        limit in 0usize..12,
    ) {
        let base = components.iter().fold(PathBuf::from("/"), |path, part| path.join(part));
        let descendant = extra.iter().fold(base.clone(), |path, part| path.join(part));
        let scope = Scope::new(
            base,
            Some(Direction::Down(Distance::Finite(limit))),
            DefaultScope::Exact { git_warning: None },
        );
        let candidate = WorkspaceCandidate {
            real_path: &descendant,
            git_common_dir: None,
            exists: true,
        };
        prop_assert_eq!(scope.contains(candidate), extra.len() <= limit);
    }

    #[test]
    fn duration_parser_accepts_only_bounded_grammar(value in ".{0,128}") {
        let parsed = Since::from_str(&value);
        if let Ok(since) = parsed {
            match since {
                Since::All => prop_assert_eq!(value.to_ascii_lowercase(), "all"),
                Since::Date(date) => prop_assert_eq!(date.len(), 10),
                Since::Duration(duration) => {
                    prop_assert!(matches!(duration.as_bytes().last(), Some(b'm' | b'h' | b'd' | b'w')));
                    prop_assert!(duration[..duration.len() - 1].bytes().all(|byte| byte.is_ascii_digit()));
                }
            }
        }
    }

    #[test]
    fn strict_config_parser_is_bounded_and_never_panics(value in ".{0,4096}") {
        let parsed = toml::from_str::<Config>(&value);
        if let Ok(config) = parsed
            && let Some(since) = config.since
        {
            let _ = Since::from_str(&since);
        }
    }
}

fn is_unsafe_control(character: char) -> bool {
    matches!(character, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}
