use super::*;
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
    time::{Duration, SystemTime},
};

struct StubBreadcrumbs {
    tty: OsString,
    session: PathBuf,
}

impl omp::BreadcrumbSource for StubBreadcrumbs {
    fn session_for_tty(&self, tty: &OsStr) -> Option<PathBuf> {
        (tty == self.tty).then(|| self.session.clone())
    }
}

fn process_table(tty: Option<&str>) -> crate::proc::ProcessTable {
    crate::proc::ProcessTable::from_entries(
        vec![crate::proc::ProcEntry {
            pid: 42,
            command: "omp".into(),
            tty: tty.map(OsString::from),
            elapsed: Some(Duration::from_secs(1)),
        }],
        SystemTime::UNIX_EPOCH,
    )
}

#[test]
fn correlate_live_requires_process_tty_breadcrumb_and_existing_transcript() {
    let fixture = Fixture::new();
    let transcript = fixture.write_flat(
        &fixture.default_agent_root,
        "active.jsonl",
        &[header_v3("active", &fixture.workspace, 1)],
    );
    let breadcrumbs = StubBreadcrumbs {
        tty: "ttys004".into(),
        session: transcript.clone(),
    };

    assert!(
        omp::correlate_live_with(&process_table(None), &breadcrumbs, SystemTime::UNIX_EPOCH)
            .is_empty()
    );
    let live = omp::correlate_live_with(
        &process_table(Some("ttys004")),
        &breadcrumbs,
        SystemTime::UNIX_EPOCH + Duration::from_secs(5),
    );
    assert!(live.for_transcript(&transcript).is_some());

    let missing = StubBreadcrumbs {
        tty: "ttys004".into(),
        session: fixture.base_root.join("missing.jsonl"),
    };
    assert!(
        omp::correlate_live_with(&process_table(Some("ttys004")), &missing, SystemTime::now())
            .is_empty()
    );
}

#[test]
fn breadcrumb_directory_uses_xdg_only_for_native_default_agent_roots() {
    let fixture = Fixture::new();
    let roots = fixture.roots_default();
    let xdg = fixture.home().join("xdg-state");
    let xdg_default = xdg.join("omp");
    std::fs::create_dir_all(&xdg_default).unwrap();

    assert_eq!(
        omp::activity::breadcrumb_directory(&roots, false, Some(&xdg)),
        xdg_default.join("terminal-sessions")
    );
    assert_eq!(
        omp::activity::breadcrumb_directory(&roots, true, Some(&xdg)),
        roots.agent_root.join("terminal-sessions")
    );
}

#[test]
fn breadcrumb_directory_requires_exact_profile_xdg_path() {
    let fixture = Fixture::new();
    let roots = fixture.roots_named("work");
    let xdg = fixture.home().join("xdg-state");
    std::fs::create_dir_all(xdg.join("omp")).unwrap();
    assert_eq!(
        omp::activity::breadcrumb_directory(&roots, false, Some(&xdg)),
        roots.agent_root.join("terminal-sessions")
    );

    let profile_state = xdg.join("omp/profiles/work");
    std::fs::create_dir_all(&profile_state).unwrap();
    assert_eq!(
        omp::activity::breadcrumb_directory(&roots, false, Some(&xdg)),
        profile_state.join("terminal-sessions")
    );
}

#[test]
fn real_breadcrumb_reader_parses_bare_text_and_rejects_path_tty() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    std::fs::write(
        temp.path().join("ttys004"),
        format!("/workspace\n{}\n", transcript.display()),
    )
    .unwrap();
    let source = omp::OmpBreadcrumbs::from_directory(temp.path().to_path_buf());
    assert_eq!(
        omp::BreadcrumbSource::session_for_tty(&source, OsStr::new("ttys004")),
        Some(transcript)
    );
    assert_eq!(
        omp::BreadcrumbSource::session_for_tty(&source, OsStr::new("../ttys004")),
        None
    );
}

// ===========================================================================
// ACTIVITY: positive-evidence-only (live process + TTY + breadcrumb)
// ===========================================================================

#[test]
fn activity_unknown_without_evidence() {
    let fx = Fixture::new();
    fx.write(
        &fx.default_agent_root,
        "ws",
        "act.jsonl",
        &[
            header_v3("act", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    assert_eq!(
        omp::activity_status(&outcome.parsed[0], None),
        crate::session::ActivityStatus::Unknown
    );
}

#[test]
fn activity_active_only_with_live_process_tty_and_matching_breadcrumb() {
    let fx = Fixture::new();
    let path = fx.write(
        &fx.default_agent_root,
        "ws",
        "act2.jsonl",
        &[
            header_v3("act2", &fx.workspace, 1700000000),
            user_message_string("hi", 1700000010),
        ],
    );
    let outcome = fx.discover(fx.roots_default());
    let parsed = &outcome.parsed[0];
    let now = SystemTime::now();

    // Full positive evidence → Active.
    let evidence = ActivityEvidence {
        live_process: true,
        tty: Some(OsString::from("/dev/ttys001")),
        breadcrumb_alive: true,
        breadcrumb_session_path: path.clone(),
        observed_at: now,
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&evidence)),
        crate::session::ActivityStatus::Active { observed_at: now }
    );

    // No live process → Unknown.
    let no_proc = ActivityEvidence {
        live_process: false,
        ..evidence.clone()
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&no_proc)),
        crate::session::ActivityStatus::Unknown
    );

    // Stale breadcrumb alone → Unknown.
    let stale = ActivityEvidence {
        breadcrumb_alive: false,
        ..evidence.clone()
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&stale)),
        crate::session::ActivityStatus::Unknown
    );

    // No TTY → Unknown.
    let no_tty = ActivityEvidence {
        tty: None,
        ..evidence.clone()
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&no_tty)),
        crate::session::ActivityStatus::Unknown
    );

    // Mismatched breadcrumb path → Unknown.
    let bad_path = ActivityEvidence {
        breadcrumb_session_path: fx.workspace.join("nope.jsonl"),
        ..evidence
    };
    assert_eq!(
        omp::activity_status(parsed, Some(&bad_path)),
        crate::session::ActivityStatus::Unknown
    );
}
