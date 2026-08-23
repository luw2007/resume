#!/usr/bin/env python3
"""Execute the non-interactive half of docs/qa/feature-inventory.csv.

Every check here drives the real compiled binary through the isolated fixture
built by docs/qa/fixtures.sh -- never a library call -- because the inventory
documents user-observable behaviour, and a unit test cannot observe argv, exit
codes, or what actually lands on stderr.

A check is registered against the feature_id it verifies and returns either
None (pass) or a failure string. A feature_id with no check here is left at
whatever status it already has; this runner never invents a Pass. Rows whose
behaviour is interactive belong to tests/picker_ux_e2e.rs instead.

Usage:
    python3 docs/qa/run_checks.py [repo-root] [--write]

Without --write it reports only. With --write it records status and
error_notes back into the inventory.
"""
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter

CHECKS = {}


def check(*feature_ids):
    def register(fn):
        for fid in feature_ids:
            CHECKS[fid] = fn
        return fn

    return register


class Fixture:
    """One isolated HOME plus the `run` wrapper that drives the real binary."""

    def __init__(self, root, binary, **knobs):
        env = dict(os.environ, RESUME_BIN=str(binary))
        env.update({k: "1" for k in knobs if knobs[k]})
        out = subprocess.run(
            ["sh", str(root / "docs/qa/fixtures.sh")],
            capture_output=True, text=True, env=env, check=True,
        )
        self.home = pathlib.Path(out.stdout.strip())
        self.workspace = self.home / "workspace"

    def run(self, *args, stdin=""):
        return subprocess.run(
            [str(self.home / "run"), *args],
            capture_output=True, text=True, input=stdin,
        )

    def json(self, *args):
        result = self.run("--json", *args)
        return result, json.loads(result.stdout) if result.stdout.strip() else None

    def cmux_log(self):
        path = self.home / "cmux.log"
        return path.read_text().splitlines() if path.is_file() else []


def expect(condition, message):
    return None if condition else message


# --------------------------------------------------------------------- CLI
@check("doccheck-help-agent-list")
def _(fx, ctx):
    text = fx.run("--help").stdout
    missing = [a for a in ctx["agents"] if a not in text.lower()]
    return expect(not missing, f"--help prose omits supported agent(s): {missing}")


@check("cli-version-flag")
def _(fx, ctx):
    result = fx.run("--version")
    return expect(
        result.returncode == 0 and re.match(r"^resume \d+\.\d+\.\d+", result.stdout),
        f"--version printed {result.stdout!r} exit {result.returncode}",
    )


@check("cli-since-invalid")
def _(fx, ctx):
    result = fx.run("--since", "nope", "--list")
    return expect(
        result.returncode == 2 and "--since" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:120]!r}",
    )


@check("cli-up-down-conflict")
def _(fx, ctx):
    result = fx.run("-U", "1", "-D", "1", "--list")
    return expect(
        result.returncode == 2 and "cannot be used with" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:120]!r}",
    )


@check("cli-invalid-distance")
def _(fx, ctx):
    result = fx.run("-U", "x", "--list")
    return expect(
        result.returncode == 2 and "non-negative integer" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:120]!r}",
    )


@check("cli-agent-unknown-rejected")
def _(fx, ctx):
    result = fx.run("-a", "nope", "--list")
    return expect(
        result.returncode == 2 and "nope" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:120]!r}",
    )


# ---------------------------------------------------------------- opencode
@check("opencode-agent-flag-selects")
def _(fx, ctx):
    if not ctx["opencode_feature"]:
        return "SKIPPED: binary built without --features opencode"
    _, payload = fx.json("-a", "opencode")
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(agents == {"opencode"}, f"-a opencode returned agents {agents}")


@check("opencode-no-profile")
def _(fx, ctx):
    if not ctx["opencode_feature"]:
        return "SKIPPED: binary built without --features opencode"
    _, payload = fx.json("-a", "opencode")
    bad = [s for s in payload["sessions"] if s["profile"] is not None]
    return expect(not bad, f"opencode sessions carry a profile: {bad}")


@check("opencode-db-is-authoritative")
def _(fx, ctx):
    if not ctx["opencode_feature"]:
        return "SKIPPED: binary built without --features opencode"
    legacy = fx.home / "xdg/data/opencode/storage/session"
    legacy.mkdir(parents=True, exist_ok=True)
    (legacy / "legacy.json").write_text(
        json.dumps({"id": "legacy-id", "directory": str(fx.workspace),
                    "title": "legacy title"})
    )
    _, payload = fx.json("-a", "opencode")
    ids = {s["id"] for s in payload["sessions"]}
    return expect("legacy-id" not in ids, f"legacy JSON storage was read: {ids}")


@check("opencode-readonly-open")
def _(fx, ctx):
    if not ctx["opencode_feature"]:
        return "SKIPPED: binary built without --features opencode"
    db = fx.home / "xdg/data/opencode/opencode.db"
    before = (db.stat().st_mtime_ns, db.stat().st_size)
    fx.json("-a", "opencode")
    after = (db.stat().st_mtime_ns, db.stat().st_size)
    sidecars = [p.name for p in db.parent.iterdir()
                if p.name.startswith("opencode.db-")]
    return expect(
        before == after and not sidecars,
        f"database changed {before} -> {after} or left sidecars {sidecars}",
    )


@check("opencode-missing-db-diagnostic", "opencode-feature-off-no-sessions")
def _(fx, ctx):
    bare = Fixture(ctx["root"], ctx["binary"], QA_NO_OPENCODE=True)
    result = bare.run("--json", "--verbose", "-a", "opencode")
    payload = json.loads(result.stdout) if result.stdout.strip() else {"sessions": []}
    return expect(
        not payload["sessions"] and "opencode_root_unavailable" in result.stderr,
        f"sessions {payload['sessions']}, stderr {result.stderr[:160]!r}",
    )


@check("opencode-feature-off-diagnostic-name")
def _(fx, ctx):
    # A category only counts as emitted when it appears as a string literal;
    # a doc comment naming it is exactly what this check is looking for, so
    # scanning raw text would let the prose vouch for itself.
    emitted, documented = set(), set()
    for path in (ctx["root"] / "src").rglob("*.rs"):
        for line in path.read_text().splitlines():
            if line.lstrip().startswith("//"):
                documented.update(re.findall(r"opencode_[a-z_]+", line))
            else:
                emitted.update(re.findall(r'"(opencode_[a-z_]+)"', line))
    for line in (ctx["root"] / "README.md").read_text().splitlines():
        documented.update(re.findall(r"opencode_[a-z_]+", line))
    phantom = documented - emitted
    return expect(
        not phantom,
        f"documented but never emitted: {sorted(phantom)}; emitted: {sorted(emitted)}",
    )


@check("opencode-feature-off-vs-no-data")
def _(fx, ctx):
    """Feature-off with data must not read identically to feature-on without."""
    if not ctx["opencode_feature"]:
        return "SKIPPED: needs a --features opencode binary to compare against"
    plain = ctx["root"] / "target/debug/resume-noopencode"
    if not plain.is_file():
        return "SKIPPED: build a default-feature binary at target/debug/resume-noopencode"
    populated_off = Fixture(ctx["root"], plain)
    empty_on = Fixture(ctx["root"], ctx["binary"], QA_NO_OPENCODE=True)
    a = populated_off.run("--json", "--verbose", "-a", "opencode").stderr
    b = empty_on.run("--json", "--verbose", "-a", "opencode").stderr
    a = re.sub(r"/[^\s]*tmp\.\w+", "<home>", a)
    b = re.sub(r"/[^\s]*tmp\.\w+", "<home>", b)
    return expect(
        a != b,
        "a build lacking --features opencode is indistinguishable from having no "
        f"OpenCode data; both print {a.strip()!r}",
    )


@check("opencode-missing-session-table")
def _(fx, ctx):
    if not ctx["opencode_feature"]:
        return "SKIPPED: binary built without --features opencode"
    db = fx.home / "xdg/data/opencode/opencode.db"
    db.unlink()
    subprocess.run(["sqlite3", str(db), "create table unrelated (x int);"], check=True)
    result = fx.run("--json", "--verbose")
    payload = json.loads(result.stdout) if result.stdout.strip() else None
    if payload is None:
        return f"no JSON on stdout; exit {result.returncode}"
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(
        result.returncode == 0 and "opencode" not in agents,
        f"exit {result.returncode}, agents {agents}, stderr {result.stderr[:160]!r}",
    )


@check("opencode-effective-root-xdg")
def _(fx, ctx):
    """The default fixture already places the database under XDG_DATA_HOME."""
    if not ctx["opencode_feature"]:
        return "SKIPPED: binary built without --features opencode"
    db = fx.home / "xdg/data/opencode/opencode.db"
    if not db.is_file():
        return f"fixture did not create {db}"
    _, payload = fx.json("-a", "opencode")
    return expect(
        any(s["agent"] == "opencode" for s in payload["sessions"]),
        "database under XDG_DATA_HOME/opencode was not discovered",
    )


# ------------------------------------------------------------------- setup
@check("setup-noninteractive-requires-setup")
def _(fx, ctx):
    bare = Fixture(ctx["root"], ctx["binary"], QA_NO_SETTINGS=True)
    result = bare.run("--list", stdin="")
    return expect(
        result.returncode != 0 and "resume setup" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:200]!r}",
    )


@check("setup-save-permissions")
def _(fx, ctx):
    path = fx.home / ".resume/settings.json"
    mode = path.stat().st_mode & 0o777
    return expect(mode == 0o600, f"settings.json mode is {mode:o}, want 600")


@check("setup-atomic-save")
def _(fx, ctx):
    """A completed run must leave no temporary file behind."""
    fx.run("--list")
    leftovers = [p.name for p in (fx.home / ".resume").iterdir()
                 if p.name.startswith(".settings-")]
    return expect(not leftovers, f"temporary files left behind: {leftovers}")


@check("setup-rejects-bad-settings")
def _(fx, ctx):
    path = fx.home / ".resume/settings.json"
    original = path.read_text()
    failures = []
    for body in ("not json",
                 '{"schema_version":0,"agents":[],"known_agents":[]}',
                 '{"schema_version":1,"agents":["unknown"],"known_agents":[]}'):
        path.write_text(body)
        result = fx.run("--list")
        if result.returncode == 0:
            failures.append(f"{body[:40]!r} was accepted")
    path.write_text(original)
    return expect(not failures, "; ".join(failures))


@check("setup-new-agent-notified")
def _(fx, ctx):
    path = fx.home / ".resume/settings.json"
    settings = json.loads(path.read_text())
    settings["known_agents"] = [a for a in settings["known_agents"] if a != "opencode"]
    settings["agents"] = ["pi"]
    path.write_text(json.dumps(settings))
    first = fx.run("--list")
    reloaded = json.loads(path.read_text())
    second = fx.run("--list")
    problems = []
    if "opencode" not in first.stderr:
        problems.append(f"new agent not reported; stderr {first.stderr[:160]!r}")
    if reloaded["agents"] != ["pi"]:
        problems.append(f"selection changed to {reloaded['agents']}")
    if "opencode" not in reloaded["known_agents"]:
        problems.append("known_agents was not updated")
    if "opencode" in second.stderr:
        problems.append("new agent reported twice")
    return expect(not problems, "; ".join(problems))


@check("setup-preserves-unknown-fields")
def _(fx, ctx):
    path = fx.home / ".resume/settings.json"
    settings = json.loads(path.read_text())
    settings["future_field"] = {"kept": True}
    settings["known_agents"] = [a for a in settings["known_agents"] if a != "opencode"]
    path.write_text(json.dumps(settings))
    fx.run("--list")
    reloaded = json.loads(path.read_text())
    return expect(
        reloaded.get("future_field") == {"kept": True},
        f"unknown field dropped; file now holds keys {sorted(reloaded)}",
    )


@check("setup-roundtrip")
def _(fx, ctx):
    path = fx.home / ".resume/settings.json"
    settings = json.loads(path.read_text())
    settings["agents"] = ["pi"]
    path.write_text(json.dumps(settings))
    result, payload = fx.json()
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(
        agents <= {"pi"},
        f"saved selection ['pi'] was not honoured; discovered {agents}",
    )


@check("setup-uses-home-not-xdg")
def _(fx, ctx):
    home_path = fx.home / ".resume/settings.json"
    xdg_path = fx.home / "xdg/config/resume/settings.json"
    return expect(
        home_path.is_file() and not xdg_path.is_file(),
        f"settings at HOME={home_path.is_file()} XDG={xdg_path.is_file()}",
    )


# ------------------------------------------------------------------- cmux
@check("cmux-noop-without-env")
def _(fx, ctx):
    """No cmux variables set: resume must not probe for or invoke cmux."""
    result = fx.run("--list")
    log = fx.cmux_log()
    return expect(
        result.returncode == 0 and not any(l.startswith("cmux ") for l in log),
        f"exit {result.returncode}, cmux invocations {[l for l in log if l.startswith('cmux ')]}",
    )


# ----------------------------------------------------------------- session
@check("session-status-supported-ready")
def _(fx, ctx):
    result, payload = fx.json()
    bad = [s for s in payload["sessions"]
           if s["support"] != "Supported" or s["activity"] == "Active"]
    listing = fx.run("--list").stdout
    words = [w for w in ("Supported", "DiscoverOnly", "Unavailable", "Ready")
             if w in listing]
    return expect(
        not bad and not words,
        f"non-ready sessions {bad}; --list leaks status words {words}",
    )


@check("session-status-supported-active")
def _(fx, ctx):
    # The fixture sets RESUME_DISABLE_PROC_PROBE=1 precisely so QA never
    # depends on whatever agents happen to be running on the host, so the
    # Active half of this row is not reachable from here by construction.
    return ("SKIPPED: Active requires a live agent process holding the session; "
            "the fixture disables the proc probe")


@check("session-status-discover-only")
def _(fx, ctx):
    """Filename UUID and embedded sessionId disagree -> DiscoverOnly."""
    other = "22222222-2222-2222-2222-222222222222"
    path = fx.home / f".claude/projects/ws/{other}.jsonl"
    path.write_text(json.dumps({
        "type": "user", "sessionId": "33333333-3333-3333-3333-333333333333",
        "cwd": str(fx.workspace), "message": {"content": "disagreeing title"},
    }) + "\n")
    _, payload = fx.json()
    found = [s for s in payload["sessions"]
             if s["id"] == "33333333-3333-3333-3333-333333333333"]
    return expect(
        found and found[0]["support"] == "DiscoverOnly",
        f"identity disagreement did not yield DiscoverOnly: {found}",
    )


@check("session-status-unsupported")
def _(fx, ctx):
    """`Unsupported` must be unreachable: declaration and tests only."""
    live = []
    for path in (ctx["root"] / "src").rglob("*.rs"):
        text = path.read_text()
        head = text.split("#[cfg(test)]")[0]
        for n, line in enumerate(head.splitlines(), 1):
            if "SupportStatus::Unsupported" in line:
                live.append(f"{path.relative_to(ctx['root'])}:{n}")
    return expect(not live, f"production code can produce Unsupported at {live}")


@check("session-status-unavailable")
def _(fx, ctx):
    """A recorded workspace that no longer exists reads as Unavailable."""
    gone = fx.workspace / "gone"
    gone.mkdir()
    d = fx.home / ".pi/agent/sessions/gone"
    d.mkdir(parents=True)
    (d / "gone.jsonl").write_text(json.dumps({
        "type": "session", "version": 3, "id": "gone-id",
        "timestamp": 1700000000, "cwd": str(gone),
    }) + "\n")
    shutil.rmtree(gone)
    _, payload = fx.json("-D", "1")
    found = [s for s in payload["sessions"] if s["id"] == "gone-id"]
    listing = fx.run("-D", "1", "--list").stdout
    return expect(
        found and all(s["support"] == "Unavailable" for s in found)
        and "UNAVAILABLE" not in listing.upper(),
        f"support {[s['support'] for s in found]}; --list said {listing!r}",
    )


@check("session-activity-unknown-default")
def _(fx, ctx):
    _, payload = fx.json()
    bad = [(s["agent"], s["activity"]) for s in payload["sessions"]
           if s["activity"] == "Inactive"]
    return expect(
        not bad,
        f"absence of evidence was reported as Inactive rather than Unknown: {bad}",
    )


@check("session-sort-order", "output-json-sorted-sessions")
def _(fx, ctx):
    _, first = fx.json()
    _, second = fx.json()
    a = [(s["agent"], s["id"]) for s in first["sessions"]]
    b = [(s["agent"], s["id"]) for s in second["sessions"]]
    return expect(a == b, f"order is not stable across runs:\n  {a}\n  {b}")


# ------------------------------------------------------------- diagnostics
def _bad_claude_transcripts(fx, count, names=None):
    """Transcripts that trigger claude_no_session_id: Claude-shaped, but with
    neither an embedded sessionId nor a cwd, so identity is unrecoverable.

    Each lands directly under a workspace-key directory, because discovery
    enumerates only the direct `.jsonl` children of those and would not see a
    transcript nested any deeper.
    """
    paths = []
    for i in range(count):
        name = names[i] if names else f"broken{i}"
        d = fx.home / ".claude/projects" / name
        d.mkdir(parents=True, exist_ok=True)
        p = d / f"4444444{i}-1111-1111-1111-111111111111.jsonl"
        p.write_text(json.dumps(
            {"type": "user", "message": {"content": "no id, no cwd"}}) + "\n")
        paths.append(p)
    return paths


@check("diag-non-verbose-category-aggregation")
def _(fx, ctx):
    _bad_claude_transcripts(fx, 3)
    lines = [l for l in fx.run("--list").stderr.splitlines()
             if "claude_no_session_id" in l]
    return expect(
        lines == ["resume: claude_no_session_id: 3"],
        f"expected one aggregated line, got {lines}",
    )


@check("diag-verbose-per-path-preserved")
def _(fx, ctx):
    paths = _bad_claude_transcripts(fx, 2)
    # A third transcript in the same directory as the first, so two distinct
    # directories are involved but one of them carries two occurrences.
    dup = paths[0].with_name("44444440-1111-1111-1111-111111111112.jsonl")
    dup.write_text(paths[0].read_text())
    lines = [l for l in fx.run("--verbose", "--list").stderr.splitlines()
             if "claude_no_session_id" in l]
    return expect(
        len(lines) == 3 and all(": 1" in l for l in lines),
        f"verbose mode collapsed distinct paths: {lines}",
    )


@check("diag-verbose-redaction")
def _(fx, ctx):
    # A URL cannot occur in a path (`https://` needs two slashes, and a path
    # component cannot contain one) and chains never carry user text, so the
    # `git@host:` remote form is the only sensitive token reachable from a
    # real diagnostic. redact_text handles both through the same code path.
    _bad_claude_transcripts(fx, 1, names=["git@secret.example:api"])
    stderr = fx.run("--verbose", "--list").stderr
    return expect(
        "[redacted-remote]" in stderr and "secret.example" not in stderr,
        f"verbose diagnostics leaked the remote: {stderr[:300]!r}",
    )


@check("diag-no-message-body-leak")
def _(fx, ctx):
    """Chains must be fixed literals; a formatted chain could carry user text."""
    offenders = []
    for path in (ctx["root"] / "src").rglob("*.rs"):
        text = path.read_text()
        head = text.split("#[cfg(test)]")[0]
        for n, line in enumerate(head.splitlines(), 1):
            if "verbose_chain" not in line or "None" in line:
                continue
            if "format!" in line or "{}" in line:
                offenders.append(f"{path.relative_to(ctx['root'])}:{n}")
    return expect(
        not offenders,
        f"verbose_chain is built by interpolation at {offenders}",
    )


# ------------------------------------------------------------------ output
@check("output-list-zero-sessions")
def _(fx, ctx):
    for pattern in (".pi", ".claude", ".codex", ".omp"):
        for p in (fx.home / pattern).rglob("*.jsonl"):
            p.unlink()
    db = fx.home / "xdg/data/opencode/opencode.db"
    if db.is_file():
        db.unlink()
    result = fx.run("--list")
    return expect(
        result.returncode == 0 and result.stdout == "No Sessions found in Scope.\n",
        f"exit {result.returncode}, stdout {result.stdout!r}",
    )


@check("output-list-all-integrations-fail")
def _(fx, ctx):
    # Built without the opencode database, so that integration fails too and
    # every one of the five is genuinely broken rather than merely empty.
    broken = Fixture(ctx["root"], ctx["binary"], QA_NO_OPENCODE=True)
    roots = [broken.home / p for p in (".pi", ".claude", ".codex", ".omp")]
    for r in roots:
        os.chmod(r, 0o000)
    try:
        result = broken.run("--list")
    finally:
        for r in roots:
            os.chmod(r, 0o755)
        shutil.rmtree(broken.home, ignore_errors=True)
    categories = re.findall(r"resume: (\w+):", result.stderr)
    agentish = [c for c in categories
                if any(c.startswith(a) for a in ("pi_", "claude_", "codex_", "omp_"))]
    return expect(
        result.returncode == 1 and agentish,
        f"exit {result.returncode} (want 1), diagnostics {categories}",
    )


@check("output-list-one-integration-fails-others-continue")
def _(fx, ctx):
    os.chmod(fx.home / ".codex", 0o000)
    try:
        result = fx.run("--list")
        _, payload = fx.json()
    finally:
        os.chmod(fx.home / ".codex", 0o755)
    agents = {s["agent"] for s in (payload or {"sessions": []})["sessions"]}
    return expect(
        result.returncode == 0 and {"pi", "claude", "omp"} <= agents
        and "codex" not in agents and "codex" in result.stderr,
        f"exit {result.returncode}, agents {agents}, stderr {result.stderr[:200]!r}",
    )


@check("output-json-schema-envelope")
def _(fx, ctx):
    result, payload = fx.json()
    return expect(
        set(payload) == {"schemaVersion", "sessions", "errors"}
        and payload["schemaVersion"] == 1,
        f"top-level keys {sorted(payload)}, schemaVersion {payload.get('schemaVersion')!r}",
    )


@check("output-json-session-fields")
def _(fx, ctx):
    want = ["agent", "profile", "id", "title", "workspace",
            "support", "activity", "risk"]
    _, payload = fx.json()
    problems = []
    for s in payload["sessions"]:
        if list(s) != want:
            problems.append(f"{s.get('id')}: keys {list(s)}")
        for field in ("support", "activity", "risk"):
            if not isinstance(s.get(field), str):
                problems.append(f"{s.get('id')}: {field} is {s.get(field)!r}")
    return expect(not problems, "; ".join(problems[:5]))


@check("output-json-error-fields")
def _(fx, ctx):
    _bad_claude_transcripts(fx, 1, names=["git@secret.example:api"])
    result = fx.run("--json", "--verbose")
    payload = json.loads(result.stdout)
    bad = [e for e in payload["errors"] if set(e) != {"category", "count"}]
    return expect(
        not bad and "secret.example" not in result.stdout,
        f"error objects carry extra fields {bad}, or stdout leaked a path",
    )


@check("output-json-no-message-bodies")
def _(fx, ctx):
    # The title is a deterministic summary of this input, so its leading words
    # legitimately appear; what must never appear is the body itself.
    body = "line one of a long body\nline two mentions hunter2\n" + "x" * 400
    path = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    path.write_text(
        json.dumps({"type": "session", "version": 3, "id": "pi-id",
                    "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n"
        + json.dumps({"type": "message",
                      "message": {"role": "user", "content": body}}) + "\n"
    )
    result, payload = fx.json()
    session = next((s for s in payload["sessions"] if s["id"] == "pi-id"), None)
    title = (session or {}).get("title") or ""
    return expect(
        "x" * 60 not in result.stdout and "\n" not in title and len(title) <= 80,
        f"stdout carries the body verbatim, or title is {title!r} ({len(title)} chars)",
    )


@check("output-json-stdout-only-schema")
def _(fx, ctx):
    result = fx.run("--json", "--verbose")
    try:
        json.loads(result.stdout)
    except ValueError as error:
        return f"stdout is not parseable JSON ({error}); stdout {result.stdout[:200]!r}"
    return expect(
        result.stderr.strip(),
        "no diagnostics reached stderr, so this run does not prove separation",
    )


@check("errors-unified-catalog-e3003-unsupported-resume")
def _(fx, ctx):
    return ("SKIPPED: reaching the resume path requires selecting a row in the "
            "picker; belongs to the PTY harness")


# -------------------------------------------------------------------- docs
@check("doccheck-resume-env-vars")
def _(fx, ctx):
    src = ctx["root"] / "src"
    found = set()
    for path in src.rglob("*.rs"):
        text = path.read_text()
        # Strip the trailing #[cfg(test)] module so test-only helpers do not
        # count as production reads.
        head = text.split("#[cfg(test)]")[0]
        found.update(re.findall(r'"(RESUME_[A-Z_]+)"', head))
    spec = (ctx["root"] / "docs/product-design.md").read_text()
    undocumented = {name for name in found if name not in spec}
    claims_none = "No custom `RESUME_*` environment variables" in spec
    return expect(
        not (claims_none and found),
        f"docs/product-design.md claims no custom RESUME_* variables, but "
        f"production code reads {sorted(found)}",
    )


@check("doccheck-settings-json-documented")
def _(fx, ctx):
    spec = (ctx["root"] / "docs/product-design.md").read_text()
    missing = [t for t in ("settings.json", "resume setup") if t not in spec]
    return expect(not missing, f"docs/product-design.md never mentions {missing}")


@check("doccheck-picker-tab-keys-documented")
def _(fx, ctx):
    picker = (ctx["root"] / "src/picker.rs").read_text()
    nav = picker.split("fn classify_nav")[1].split("\n}")[0]
    bound = set(re.findall(r"SkimKey::(\w+(?:\('\w'\))?)", nav))
    spec = (ctx["root"] / "docs/product-design.md").read_text()
    # Matched with a leading boundary so bare `Left` is not vouched for by
    # `Alt+Left`, which is what makes the undocumented plain-arrow bindings
    # invisible to a substring search.
    names = {
        "Alt('p')": r"Alt\+P", "Alt('n')": r"Alt\+N",
        "AltLeft": r"Alt\+Left", "AltRight": r"Alt\+Right",
        "Left": r"(?<![+\w])Left", "Right": r"(?<![+\w])Right",
        "Tab": r"(?<![-\w])Tab", "BackTab": r"Shift\+Tab",
    }
    missing = sorted(
        k for k in bound
        if k in names and not re.search(names[k], spec)
    )
    return expect(not missing, f"bound but undocumented in the spec: {missing}")


# --------------------------------------------------------------------- main
def build_context(root):
    binary = root / "target/debug/resume"
    if not binary.is_file():
        sys.exit("build the binary first: cargo build --locked")
    probe = subprocess.run([str(binary), "--version"], capture_output=True, text=True)
    if probe.returncode != 0:
        sys.exit(f"{binary} is not runnable: {probe.stderr}")
    manifest = (root / "Cargo.toml").read_text()
    agents = re.search(r'SUPPORTED_AGENTS: \[&str; \d+\] = \[([^\]]+)\]',
                       (root / "src/cli.rs").read_text())
    return {
        "root": root,
        "binary": binary,
        "agents": re.findall(r'"([a-z]+)"', agents.group(1)) if agents else [],
        "opencode_feature": has_opencode_feature(root, binary),
        "manifest": manifest,
    }


def has_opencode_feature(root, binary):
    """Detect the feature by behaviour, not by build flags: a populated
    database that yields a session can only happen with rusqlite linked."""
    try:
        fx = Fixture(root, binary)
    except subprocess.CalledProcessError:
        return False
    result = fx.run("--json", "-a", "opencode")
    if not result.stdout.strip():
        return False
    payload = json.loads(result.stdout)
    return any(s["agent"] == "opencode" for s in payload["sessions"])


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    write = "--write" in sys.argv[1:]
    root = pathlib.Path(args[0] if args else ".").resolve()
    ctx = build_context(root)
    print(f"binary: {ctx['binary']}")
    print(f"opencode feature linked: {ctx['opencode_feature']}")
    print(f"supported agents: {ctx['agents']}\n")

    results = {}
    for fid, fn in CHECKS.items():
        fx = Fixture(root, ctx["binary"])
        try:
            problem = fn(fx, ctx)
        except Exception as error:  # a check that cannot run is Blocked, not Pass
            problem = f"check raised {type(error).__name__}: {error}"
            results[fid] = ("Blocked", problem)
            continue
        finally:
            shutil.rmtree(fx.home, ignore_errors=True)
        if problem is None:
            results[fid] = ("Pass", "")
        elif problem.startswith("SKIPPED"):
            results[fid] = ("Blocked", problem)
        else:
            results[fid] = ("Fail", problem)

    width = max(len(f) for f in results)
    for fid, (status, note) in sorted(results.items(), key=lambda kv: kv[1][0]):
        mark = {"Pass": "ok  ", "Fail": "FAIL", "Blocked": "skip"}[status]
        print(f"{mark} {fid:<{width}}  {note}")
    print("\n" + ", ".join(f"{k}={v}" for k, v in
                           sorted(Counter(s for s, _ in results.values()).items())))

    if write:
        write_back(root, results)
    return 1 if any(s == "Fail" for s, _ in results.values()) else 0


def write_back(root, results):
    import csv
    path = root / "docs/qa/feature-inventory.csv"
    with path.open(newline="") as fh:
        reader = csv.DictReader(fh)
        cols = reader.fieldnames
        rows = list(reader)
    for r in rows:
        if r["feature_id"] not in results:
            continue
        status, note = results[r["feature_id"]]
        r["status"] = status
        if not note:
            continue
        # Re-running the suite must not stack identical notes: keep the newest
        # first and preserve only genuinely different prior observations.
        prior = [s.strip() for s in r["error_notes"].split("||") if s.strip()]
        r["error_notes"] = " || ".join([note] + [p for p in prior if p != note])
    with path.open("w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=cols)
        w.writeheader()
        w.writerows(rows)
    print(f"\nwrote {len(results)} results into {path.relative_to(root)}")


if __name__ == "__main__":
    sys.exit(main())
