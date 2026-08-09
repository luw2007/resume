//! OMP Agent Integration (Step 7).
//!
//! OMP has the highest isolation risk of the four v0.1.0 integrations: a single
//! base config directory can host a default profile plus arbitrarily many named
//! profiles, each with its own agent root and optional XDG split roots, and
//! duplicate native IDs across those isolation boundaries must never collide.
//! Profile and effective root are therefore part of [`SessionKey`] identity.
//!
//! Evidence: OMP 17.2.10.
//!
//! ## Storage and profiles
//!
//! - Base: `PI_CONFIG_DIR`, defaulting to `~/.omp`.
//! - Default profile agent root: `<base>/agent`.
//! - Named profile agent root: `<base>/profiles/<name>/agent`.
//! - Profile selection precedence: `--profile` flag, then `OMP_PROFILE`,
//!   then `PI_PROFILE`.
//! - `PI_CODING_AGENT_DIR` overrides only the **unprofiled** agent root; named
//!   profiles deliberately ignore it.
//! - `--session-dir` overrides Session lookup for an invocation.
//! - Existing XDG OMP directories (`XDG_DATA_HOME`, `XDG_STATE_HOME`,
//!   `XDG_CACHE_HOME`) can split data/state/cache; root resolution mirrors the
//!   installed OMP behavior and is fixture-driven. Profile and effective root
//!   are part of Session provenance and identity. Workspace is never inferred
//!   from encoded or migrated directory names when the header is readable.
//!
//! ## Format
//!
//! JSONL normally begins with a padded title record (`type = "title"`, `v = 1`)
//! followed by a v3 `type = "session"` header with `id`, `timestamp`, absolute
//! `cwd`, and optional title metadata. Filenames are not authoritative. OMP's
//! title sidecar sits **before** the v3 header; we must not reuse Pi's
//! header-position assumptions (Pi's first session record is the header).
//!
//! User messages are typed envelopes with `message.role = "user"`, block
//! content, and attribution. Attribution is used to remove agent-injected
//! inputs. `title_change` records update title state.
//!
//! Imported Sessions receive a new OMP ID and a `foreign_session_import`
//! custom entry containing source kind, origin ID/path/cwd. Resume uses the
//! OMP header ID; only a safe origin badge is shown. The imported origin
//! Codex/Claude Session is never merged with this OMP Session.
//!
//! ## Resume and activity
//!
//! Default: `omp --resume <id>`. Named profile:
//! `omp --profile <name> --resume <id>`. `--session-dir <root>` is added when
//! discovery used it, and the process runs from the header `cwd`. An explicit
//! `PI_CONFIG_DIR` override is preserved through the environment.
//!
//! Terminal breadcrumbs map TTY names to cwd/session path but can be stale and
//! contain no PID. Active is reported only after correlating a live OMP
//! process, its TTY, and a matching breadcrumb Session path. A stale marker
//! alone is Unknown.
//!
//! Discovery may enumerate the OS process table read-only, but never invokes OMP during discovery/preview. It reads JSONL
//! read-only through the shared [`crate::jsonl`] reader and interprets records
//! with the shared [`crate::message`], [`crate::injection`], and
//! [`crate::summary`] helpers.

pub const AGENT: &str = "omp";

mod activity;
mod discover;
mod format;
mod resume;
mod roots;

pub use self::{
    activity::{
        ActivityEvidence, ActivityEvidenceMap, BreadcrumbSource, OmpBreadcrumbs, activity_status,
        correlate_live, correlate_live_with,
    },
    discover::{DiscoverConfig, DiscoverOutcome, discover},
    format::{ImportBadge, ParsedSession, extract_session_pub, parse_import_pub, risk_status},
    roots::{
        AGENT_DIR_NAME, DEFAULT_BASE_RELATIVE, ENV_AGENT_DIR, ENV_CONFIG_DIR, ENV_OMP_PROFILE,
        ENV_PI_PROFILE, ENV_XDG_DATA_HOME, EffectiveRoots, FLAG_SESSION_DIR,
        PROFILE_AGENT_DIR_NAME, PROFILES_DIR_NAME, ProfileSelection, ResolutionInputs, resolve,
        select_profile,
    },
};

#[cfg(test)]
mod tests;
