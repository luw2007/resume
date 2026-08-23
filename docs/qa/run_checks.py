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
        self.binary = binary
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

    def env(self, **overrides):
        """The wrapper's own environment, read back from the generated script
        so a Python-side copy cannot drift from fixtures.sh."""
        env = {}
        for line in (self.home / "run").read_text().splitlines():
            m = re.match(r'^([A-Z_]+)=(.*?) *\\$', line)
            if m:
                env[m.group(1)] = m.group(2).strip('"')
        env["TERM"] = "dumb"
        for key, value in overrides.items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        return env

    def run_env(self, *args, env=None, cwd=None):
        """Drive the binary with a modified environment, bypassing the wrapper."""
        return subprocess.run(
            [str(self.binary), *args], capture_output=True, text=True,
            env=env if env is not None else self.env(),
            cwd=str(cwd or self.workspace),
        )

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


def _pi_session(fx, name, session_id, cwd, title="t", timestamp=1700000000):
    """A pi transcript recording `cwd` as its workspace, for scope tests."""
    d = fx.home / ".pi/agent/sessions" / name
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{name}.jsonl").write_text(
        json.dumps({"type": "session", "version": 3, "id": session_id,
                    "timestamp": timestamp, "cwd": str(cwd)}) + "\n"
        + json.dumps({"type": "message",
                      "message": {"role": "user", "content": title}}) + "\n"
    )


def _ids(payload, agent="pi"):
    return {s["id"] for s in payload["sessions"] if s["agent"] == agent}


@check("cli-default-picker")
def _(fx, ctx):
    return "SKIPPED: opening the picker is interactive; belongs to the PTY harness"


@check("cli-directory-arg")
def _(fx, ctx):
    other = fx.home / "other"
    other.mkdir()
    _pi_session(fx, "other", "other-id", other)
    _, payload = fx.json(str(other))
    missing = fx.run(str(fx.home / "nonexistent"), "--list")
    return expect(
        _ids(payload) == {"other-id"} and missing.returncode == 2,
        f"scoped ids {_ids(payload)} (want {{'other-id'}}); "
        f"missing-directory exit {missing.returncode} (want 2)",
    )


@check("cli-up-flag")
def _(fx, ctx):
    parent, deep = fx.home, fx.workspace / "deep"
    deep.mkdir()
    _pi_session(fx, "parent", "parent-id", parent)
    _pi_session(fx, "deep", "deep-id", deep)
    _, without = fx.json()
    _, with_up = fx.json("-U", "1")
    return expect(
        _ids(without) == {"pi-id"} and _ids(with_up) == {"pi-id", "parent-id"},
        f"default {_ids(without)} (want {{'pi-id'}}); "
        f"-U 1 {_ids(with_up)} (want pi-id + parent-id, no descendant)",
    )


@check("cli-up-all")
def _(fx, ctx):
    sibling = fx.home / "sibling"
    sibling.mkdir()
    _pi_session(fx, "parent", "parent-id", fx.home)
    _pi_session(fx, "sibling", "sibling-id", sibling)
    _, payload = fx.json("-U", "all")
    ids = _ids(payload)
    return expect(
        {"pi-id", "parent-id"} <= ids and "sibling-id" not in ids,
        f"-U all returned {ids}",
    )


@check("cli-down-flag")
def _(fx, ctx):
    one = fx.workspace / "one"
    three = one / "two/three"
    three.mkdir(parents=True)
    _pi_session(fx, "one", "depth1-id", one)
    _pi_session(fx, "three", "depth3-id", three)
    _, payload = fx.json("-D", "2")
    ids = _ids(payload)
    return expect(
        {"pi-id", "depth1-id"} <= ids and "depth3-id" not in ids,
        f"-D 2 returned {ids}; depth 3 must be excluded",
    )


@check("cli-down-all")
def _(fx, ctx):
    deep = fx.workspace / "a/b/c/d"
    deep.mkdir(parents=True)
    _pi_session(fx, "deep", "deep-id", deep)
    _, payload = fx.json("-D", "all")
    return expect("deep-id" in _ids(payload), f"-D all returned {_ids(payload)}")


@check("cli-agent-flag-repeat-replaces")
def _(fx, ctx):
    _, payload = fx.json("-a", "pi", "-a", "codex")
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(agents == {"pi", "codex"}, f"agents {agents}")


@check("cli-agent-case-insensitive")
def _(fx, ctx):
    _, lower = fx.json("-a", "pi")
    _, upper = fx.json("-a", "PI")
    return expect(lower == upper, "`-a PI` and `-a pi` produced different output")


@check("cli-list-flag", "exitcode-0-success")
def _(fx, ctx):
    first = fx.run("--list")
    second = fx.run("--list")
    escapes = "\x1b[" in first.stdout
    return expect(
        first.returncode == 0 and first.stdout == second.stdout and not escapes,
        f"exit {first.returncode}, stable={first.stdout == second.stdout}, "
        f"terminal escapes present={escapes}",
    )


@check("cli-json-flag")
def _(fx, ctx):
    result = fx.run("--json")
    try:
        payload = json.loads(result.stdout)
    except ValueError as error:
        return f"stdout is not one JSON document: {error}"
    return expect(payload.get("schemaVersion") == 1,
                  f"schemaVersion {payload.get('schemaVersion')!r}")


@check("cli-list-json-both-run-discovery")
def _(fx, ctx):
    listing = fx.run("--list", "-a", "pi").stdout
    _, payload = fx.json("-a", "pi")
    titles = [s["title"] for s in payload["sessions"]]
    return expect(
        payload["sessions"] and all(t in listing for t in titles if t),
        f"--list is missing sessions the JSON reports: {titles}",
    )


@check("cli-verbose-flag")
def _(fx, ctx):
    _bad_claude_transcripts(fx, 1)
    plain = fx.run("--list").stderr
    verbose = fx.run("--verbose", "--list").stderr
    return expect(
        "claude_no_session_id" in plain and len(verbose) > len(plain)
        and "/" in verbose,
        f"plain {plain!r} vs verbose {verbose!r}",
    )


@check("cli-config-flag")
def _(fx, ctx):
    path = fx.home / "elsewhere.toml"
    path.write_text('agents = ["pi"]\n')
    _, payload = fx.json("--config", str(path))
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(agents == {"pi"}, f"--config at a nonstandard path gave {agents}")


@check("cli-confirm-always-flag", "cli-no-confirm-flag", "config-confirm-always-field")
def _(fx, ctx):
    return ("SKIPPED: the confirmation prompt only appears after selecting a row; "
            "belongs to the PTY harness")


@check("cli-confirm-conflict")
def _(fx, ctx):
    result = fx.run("--confirm-always", "--no-confirm", "--list")
    return expect(
        result.returncode == 2 and "cannot be used with" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:120]!r}",
    )


@check("cli-since-duration", "since-fallback-mtime")
def _(fx, ctx):
    """A transcript with no in-band time falls back to file mtime."""
    _pi_session(fx, "stale", "stale-id", fx.workspace)
    stale = fx.home / ".pi/agent/sessions/stale/stale.jsonl"
    old = 1_600_000_000
    os.utime(stale, (old, old))
    _, recent = fx.json("--since", "10m")
    _, everything = fx.json("--since", "all")
    return expect(
        "stale-id" not in _ids(recent) and "stale-id" in _ids(everything),
        f"--since 10m gave {_ids(recent)}; --since all gave {_ids(everything)}",
    )


@check("cli-since-date")
def _(fx, ctx):
    old = 1_600_000_000  # 2020-09, before the cutoff below
    _pi_session(fx, "stale", "stale-id", fx.workspace, timestamp=old)
    stale = fx.home / ".pi/agent/sessions/stale/stale.jsonl"
    os.utime(stale, (old, old))
    _, payload = fx.json("--since", "2021-01-01")
    bad_date = fx.run("--since", "2026-13-40", "--list")
    return expect(
        "stale-id" not in _ids(payload) and bad_date.returncode == 2,
        f"cutoff kept {_ids(payload)}; invalid date exit {bad_date.returncode}",
    )


@check("cli-since-all")
def _(fx, ctx):
    _, absent = fx.json()
    _, everything = fx.json("--since", "all")
    return expect(absent == everything,
                  "`--since all` differs from omitting --since")


@check("cli-config-subcommand-example", "config-example-matches-schema")
def _(fx, ctx):
    result = fx.run("config", "example")
    path = fx.home / "example.toml"
    path.write_text(result.stdout)
    loaded = fx.run("--config", str(path), "--list")
    readme = (ctx["root"] / "README.md").read_text()
    fields = re.findall(r"^(\w+) = ", result.stdout, re.M)
    undocumented = [f for f in fields if f not in readme]
    # The example is what users copy, so an agent missing from its `agents`
    # list is silently dropped the moment the file is adopted.
    listed = set(re.findall(r'"([a-z]+)"', re.search(
        r"^agents = \[([^\]]*)\]", result.stdout, re.M).group(1)))
    missing = sorted(set(ctx["agents"]) - listed)
    return expect(
        result.returncode == 0 and loaded.returncode == 0
        and not undocumented and not missing,
        f"example exit {result.returncode}, reload exit {loaded.returncode}, "
        f"fields absent from README: {undocumented}, "
        f"supported agents absent from the example `agents` list: {missing}",
    )


@check("cli-completions-bash", "cli-completions-zsh", "cli-completions-fish",
       "cli-completions-precede-startup")
def _(fx, ctx):
    problems = []
    for shell, marker in (("bash", "complete"), ("zsh", "_resume"),
                          ("fish", "complete -c resume")):
        # An empty environment: completions must precede any config or HOME
        # lookup, so no variable may be required to reach this output.
        result = fx.run_env("completions", shell, env={})
        if result.returncode != 0 or marker not in result.stdout:
            problems.append(f"{shell}: exit {result.returncode}, "
                            f"marker present {marker in result.stdout}")
    return expect(not problems, "; ".join(problems))


@check("cli-man-flag")
def _(fx, ctx):
    bare = fx.run_env("--man", env={})
    conflicts = [fx.run("--man", "--json"), fx.run("--man", str(fx.home))]
    return expect(
        bare.returncode == 0 and "RESUME(1)" in bare.stdout
        and all(c.returncode == 2 for c in conflicts),
        f"--man exit {bare.returncode}, conflict exits "
        f"{[c.returncode for c in conflicts]}",
    )


@check("cli-help-three-layers")
def _(fx, ctx):
    short = fx.run("-h").stdout
    long = fx.run("--help").stdout
    man = fx.run("--man").stdout
    return expect(
        len(short) < len(long) < len(man)
        and "--man" in short and "COMMON ERRORS" in long,
        f"lengths {len(short)}/{len(long)}/{len(man)}; "
        f"-h points at --man: {'--man' in short}; "
        f"--help has COMMON ERRORS: {'COMMON ERRORS' in long}",
    )


@check("cli-error-catalog-mechanics")
def _(fx, ctx):
    bad_config = fx.home / "bad.toml"
    bad_config.write_text("mystery = true\n")
    # E1001 and E1003 reach the user only as their catalog `parser_hint`,
    # because clap owns the wording of a value-parser rejection. Read the
    # hints out of the catalog so this check cannot drift from it.
    catalog = (ctx["root"] / "src/errors.rs").read_text()
    hints = dict(zip(
        re.findall(r'code: "(E\d+)"', catalog),
        [m or None for m in re.findall(
            r'parser_hint: (?:Some\(\s*"([^"]+)"|None)', catalog)],
    ))
    cases = [
        (["-U", "1", "-D", "2", "--list"], "cannot be used with", False),
        (["--since", "yesterday", "--list"], hints["E1001"], False),
        (["-U", "-1", "--list"], hints["E1003"], False),
        (["--config", str(bad_config), "--list"], "E1004", True),
        (["--list", "--confirm-always", "--no-confirm"], "cannot be used with", False),
    ]
    problems = []
    for args, marker, want_block in cases:
        result = fx.run(*args)
        text = result.stderr
        if result.returncode != 2:
            problems.append(f"{args}: exit {result.returncode}")
        if marker not in text:
            problems.append(f"{args}: stderr lacks {marker!r}: {text[:100]!r}")
        if want_block is False and re.search(r"ERROR \[E\d+\]", text):
            problems.append(f"{args}: unexpected E-code block: {text[:80]!r}")
        if want_block is True and ("Trigger:" not in text or "Fix:" not in text):
            problems.append(f"{args}: not the four-line block: {text[:120]!r}")
    return expect(not problems, "; ".join(problems))


@check("exitcode-1-error")
def _(fx, ctx):
    broken = Fixture(ctx["root"], ctx["binary"], QA_NO_OPENCODE=True)
    settings = broken.home / ".resume/settings.json"
    settings.write_text(json.dumps({"schema_version": 1, "agents": ["opencode"],
                                    "known_agents": ["pi", "claude", "codex",
                                                     "omp", "opencode"]}))
    result = broken.run("--list")
    shutil.rmtree(broken.home, ignore_errors=True)
    return expect(result.returncode == 1,
                  f"the only selected integration failed but exit was "
                  f"{result.returncode}, want 1")


@check("exitcode-2-usage")
def _(fx, ctx):
    bad_config = fx.home / "bad.toml"
    bad_config.write_text("mystery = true\n")
    cases = [["--list", "--agent", "bogus"],
             [str(fx.home / "nonexistent"), "--list"],
             ["--config", str(bad_config), "--list"]]
    codes = [fx.run(*c).returncode for c in cases]
    return expect(codes == [2, 2, 2], f"exit codes {codes} for {cases}")


@check("exitcode-130-interrupt")
def _(fx, ctx):
    return "SKIPPED: Ctrl+C in the picker is interactive; belongs to the PTY harness"


# ------------------------------------------------------------------ config
def _write_config(path, body):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)


@check("config-precedence-explicit", "config-no-merge")
def _(fx, ctx):
    _write_config(fx.home / "xdg/config/resume/config.toml",
                  'agents = ["claude"]\nsince = "1m"\n')
    _write_config(fx.home / ".config/resume/config.toml", 'agents = ["codex"]\n')
    explicit = fx.home / "explicit.toml"
    _write_config(explicit, 'agents = ["pi"]\n')
    _, payload = fx.json("--config", str(explicit))
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(
        agents == {"pi"},
        f"explicit config gave {agents}; lower-precedence files must not merge "
        f"(a merged `since = \"1m\"` would also have emptied the result)",
    )


@check("config-precedence-xdg")
def _(fx, ctx):
    _write_config(fx.home / "xdg/config/resume/config.toml", 'agents = ["claude"]\n')
    _write_config(fx.home / ".config/resume/config.toml", 'agents = ["codex"]\n')
    _, payload = fx.json()
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(agents == {"claude"}, f"XDG config lost precedence: {agents}")


@check("config-precedence-home")
def _(fx, ctx):
    _write_config(fx.home / ".config/resume/config.toml", 'agents = ["codex"]\n')
    result = fx.run_env("--json", env=fx.env(XDG_CONFIG_HOME=None))
    payload = json.loads(result.stdout)
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(agents == {"codex"},
                  f"with no XDG_CONFIG_HOME, ~/.config config gave {agents}")


@check("config-unknown-field-rejected", "config-parse-error-reports-line-and-field")
def _(fx, ctx):
    path = fx.home / "unknown.toml"
    _write_config(path, "mystery = true\n")
    result = fx.run("--config", str(path), "--list")
    return expect(
        result.returncode == 2 and "line 1" in result.stderr
        and "mystery" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:200]!r}",
    )


@check("config-invalid-value-rejected")
def _(fx, ctx):
    path = fx.home / "invalid.toml"
    _write_config(path, "preview_position = 'left'\n")
    result = fx.run("--config", str(path), "--list")
    return expect(
        result.returncode == 2 and "preview_position" in result.stderr,
        f"exit {result.returncode}, stderr {result.stderr[:200]!r}",
    )


@check("config-since-field")
def _(fx, ctx):
    _pi_session(fx, "stale", "stale-id", fx.workspace)
    stale = fx.home / ".pi/agent/sessions/stale/stale.jsonl"
    os.utime(stale, (1_600_000_000, 1_600_000_000))
    path = fx.home / "since.toml"
    _write_config(path, 'since = "7d"\n')
    _, filtered = fx.json("--config", str(path))
    _, overridden = fx.json("--config", str(path), "--since", "all")
    return expect(
        "stale-id" not in _ids(filtered) and "stale-id" in _ids(overridden),
        f"config cutoff kept {_ids(filtered)}; --since all gave {_ids(overridden)}",
    )


@check("config-documented-fields-load")
def _(fx, ctx):
    path = fx.home / "full.toml"
    _write_config(path, 'agents = ["pi"]\nsince = "all"\nconfirm_always = true\n'
                        'preview = "visible"\npreview_position = "bottom"\n'
                        'verbose = true\n')
    _bad_claude_transcripts(fx, 1)
    result = fx.run("--config", str(path), "--list")
    _, payload = fx.json("--config", str(path))
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(
        result.returncode == 0 and agents == {"pi"},
        f"exit {result.returncode}, agents {agents} (want just pi)",
    )


@check("config-agents-default")
def _(fx, ctx):
    _, payload = fx.json()
    agents = {s["agent"] for s in payload["sessions"]}
    return expect(
        {"pi", "claude", "codex", "omp"} <= agents,
        f"with no config file, discovery covered only {agents}",
    )


@check("config-verbose-field")
def _(fx, ctx):
    _bad_claude_transcripts(fx, 1)
    path = fx.home / "verbose.toml"
    _write_config(path, "verbose = true\n")
    quiet = fx.run("--list").stderr
    loud = fx.run("--config", str(path), "--list").stderr
    return expect(
        len(loud) > len(quiet) and "/" in loud,
        f"config verbose had no effect: {quiet!r} vs {loud!r}",
    )


@check("config-preview-field", "config-preview-position-field")
def _(fx, ctx):
    return ("SKIPPED: the preview pane only exists inside the picker; "
            "belongs to the PTY harness")


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


# ---------------------------------------------------------------------- pi
@check("pi-root-resolution-env")
def _(fx, ctx):
    other = fx.home / "custom-pi"
    (other / "sessions/ws").mkdir(parents=True)
    (other / "sessions/ws/custom.jsonl").write_text(json.dumps({
        "type": "session", "version": 3, "id": "custom-root-id",
        "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n")
    result = fx.run_env("--json", "-a", "pi",
                        env=fx.env(PI_CODING_AGENT_DIR=str(other)))
    ids = _ids(json.loads(result.stdout))
    return expect(
        ids == {"custom-root-id"},
        f"PI_CODING_AGENT_DIR was not honoured: {ids}",
    )


@check("pi-session-dir-precedence")
def _(fx, ctx):
    sessions = fx.home / "custom-sessions/ws"
    sessions.mkdir(parents=True)
    (sessions / "custom.jsonl").write_text(json.dumps({
        "type": "session", "version": 3, "id": "custom-sessions-id",
        "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n")
    result = fx.run_env("--json", "-a", "pi", env=fx.env(
        PI_CODING_AGENT_SESSION_DIR=str(sessions.parent)))
    ids = _ids(json.loads(result.stdout))
    return expect(
        ids == {"custom-sessions-id"},
        f"PI_CODING_AGENT_SESSION_DIR did not override the default "
        f"<agent-root>/sessions: {ids}",
    )


@check("pi-title-precedence")
def _(fx, ctx):
    named = fx.home / ".pi/agent/sessions/named"
    named.mkdir(parents=True)
    (named / "named.jsonl").write_text(
        json.dumps({"type": "session", "version": 3, "id": "named-id",
                    "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n"
        + json.dumps({"type": "session_info", "name": "explicit name"}) + "\n"
        + json.dumps({"type": "message",
                      "message": {"role": "user", "content": "ignored body"}}) + "\n")
    _, payload = fx.json("-a", "pi")
    titles = {s["id"]: s["title"] for s in payload["sessions"]}
    return expect(
        titles.get("named-id") == "explicit name"
        and titles.get("pi-id") == "pi title",
        f"titles {titles}; session_info.name must win, otherwise a summary of "
        f"the first user message",
    )


@check("pi-activity-positive-evidence-only")
def _(fx, ctx):
    """Without matching control evidence the answer is Unknown, never a
    guess of Inactive."""
    _, payload = fx.json("-a", "pi")
    activities = {s["activity"] for s in payload["sessions"]}
    return expect(
        activities == {"Unknown"},
        f"absent evidence produced {activities} rather than Unknown only",
    )


@check("pi-broad-workspace-risk")
def _(fx, ctx):
    home_session = fx.home / ".pi/agent/sessions/athome"
    home_session.mkdir(parents=True)
    (home_session / "athome.jsonl").write_text(json.dumps({
        "type": "session", "version": 3, "id": "athome-id",
        "timestamp": 1700000000, "cwd": str(fx.home)}) + "\n")
    result = fx.run_env("--json", "-a", "pi", cwd=fx.home)
    found = [s for s in json.loads(result.stdout)["sessions"]
             if s["id"] == "athome-id"]
    return expect(
        found and found[0]["risk"] == "BroadWorkspace",
        f"a session whose workspace is $HOME reported risk "
        f"{[s['risk'] for s in found]}",
    )


@check("pi-header-cwd-scope-filtering")
def _(fx, ctx):
    """Grouping directory names carry no meaning; only the header cwd does."""
    shared = fx.home / ".pi/agent/sessions/ws"
    elsewhere = fx.home / "elsewhere"
    elsewhere.mkdir()
    (shared / "outside.jsonl").write_text(json.dumps({
        "type": "session", "version": 3, "id": "outside-id",
        "timestamp": 1700000000, "cwd": str(elsewhere)}) + "\n")
    _, payload = fx.json("-a", "pi")
    ids = _ids(payload)
    return expect(
        ids == {"pi-id"},
        f"a session sharing the grouping directory but recording a different "
        f"cwd was kept in scope: {ids}",
    )


@check("pi-header-version-compat")
def _(fx, ctx):
    for version in (1, 2, 3):
        d = fx.home / f".pi/agent/sessions/v{version}"
        d.mkdir(parents=True)
        (d / f"v{version}.jsonl").write_text(json.dumps({
            "type": "session", "version": version, "id": f"v{version}-id",
            "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n")
    _, payload = fx.json("-a", "pi")
    ids = _ids(payload)
    missing = [f"v{v}-id" for v in (1, 2, 3) if f"v{v}-id" not in ids]
    return expect(not missing, f"header versions not discovered: {missing}")


@check("pi-activity-time-fallback")
def _(fx, ctx):
    """Message time beats header time; with neither, file mtime decides."""
    old, recent = 1_600_000_000, int(subprocess.run(
        ["date", "+%s"], capture_output=True, text=True).stdout.strip()) - 60
    fresh_message = fx.home / ".pi/agent/sessions/fresh"
    fresh_message.mkdir(parents=True)
    (fresh_message / "fresh.jsonl").write_text(
        json.dumps({"type": "session", "version": 3, "id": "fresh-id",
                    "timestamp": old, "cwd": str(fx.workspace)}) + "\n"
        + json.dumps({"type": "message", "timestamp": recent,
                      "message": {"role": "user", "content": "recent"}}) + "\n")
    no_time = fx.home / ".pi/agent/sessions/notime"
    no_time.mkdir(parents=True)
    path = no_time / "notime.jsonl"
    path.write_text(json.dumps({"type": "session", "version": 3,
                                "id": "notime-id", "cwd": str(fx.workspace)}) + "\n")
    os.utime(path, (old, old))
    _, payload = fx.json("-a", "pi", "--since", "10m")
    ids = _ids(payload)
    return expect(
        "fresh-id" in ids and "notime-id" not in ids,
        f"--since 10m kept {ids}; the message-timestamped session must survive "
        f"its older header, and the mtime-only one must be filtered out",
    )


@check("pi-dedupe-canonical-locator")
def _(fx, ctx):
    real = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    (real.parent / "link-a.jsonl").symlink_to(real)
    (real.parent / "link-b.jsonl").symlink_to(real)
    _, same_root = fx.json("-a", "pi")
    other_root = fx.home / "second-root"
    (other_root / "sessions/ws").mkdir(parents=True)
    shutil.copy(real, other_root / "sessions/ws/pi.jsonl")
    result = fx.run_env("--json", "-a", "pi", env=fx.env(
        PI_CODING_AGENT_SESSION_DIR=str(other_root / "sessions")))
    return expect(
        len([s for s in same_root["sessions"] if s["id"] == "pi-id"]) == 1
        and "pi-id" in _ids(json.loads(result.stdout)),
        f"symlinks to one transcript yielded "
        f"{len([s for s in same_root['sessions'] if s['id'] == 'pi-id'])} sessions",
    )


@check("pi-malformed-record-tolerance")
def _(fx, ctx):
    path = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    path.write_text(
        json.dumps({"type": "session", "version": 3, "id": "pi-id",
                    "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n"
        + "{not json at all\n"
        + json.dumps({"type": "message",
                      "message": {"role": "user", "content": "survivor"}}) + "\n")
    _, payload = fx.json("-a", "pi")
    found = [s for s in payload["sessions"] if s["id"] == "pi-id"]
    return expect(
        found and found[0]["title"] == "survivor",
        f"a malformed middle record lost the session or its title: {found}",
    )


@check("pi-resume-spec-exact")
def _(fx, ctx):
    return "SKIPPED: observing the agent's argv requires a resume; PTY harness"


# ------------------------------------------------------------------ claude
def _claude(fx, key, name, records):
    d = fx.home / ".claude/projects" / key
    d.mkdir(parents=True, exist_ok=True)
    (d / name).write_text("".join(json.dumps(r) + "\n" for r in records))
    return d / name


@check("claude-root-resolution")
def _(fx, ctx):
    other = fx.home / "custom-claude"
    (other / "projects/ws").mkdir(parents=True)
    uuid = "55555555-5555-5555-5555-555555555555"
    (other / f"projects/ws/{uuid}.jsonl").write_text(json.dumps({
        "type": "user", "sessionId": uuid, "cwd": str(fx.workspace),
        "message": {"content": "custom root"}}) + "\n")
    result = fx.run_env("--json", "-a", "claude",
                        env=fx.env(CLAUDE_CONFIG_DIR=str(other)))
    ids = _ids(json.loads(result.stdout), "claude")
    return expect(ids == {uuid}, f"CLAUDE_CONFIG_DIR was not honoured: {ids}")


@check("claude-identity-agreement-supported")
def _(fx, ctx):
    uuid = "66666666-6666-6666-6666-666666666666"
    _claude(fx, "agree", f"{uuid.upper()}.jsonl", [
        {"type": "user", "sessionId": uuid, "cwd": str(fx.workspace),
         "message": {"content": "agreeing"}}])
    _, payload = fx.json("-a", "claude")
    found = [s for s in payload["sessions"] if s["id"].lower() == uuid]
    return expect(
        found and found[0]["support"] == "Supported",
        f"case-insensitive UUID agreement did not yield Supported: {found}",
    )


@check("claude-identity-disagreement-discover-only-or-skip")
def _(fx, ctx):
    with_cwd = "77777777-7777-7777-7777-777777777777"
    _claude(fx, "disagree", f"{with_cwd}.jsonl", [
        {"type": "user", "sessionId": "aaaaaaaa-7777-7777-7777-777777777777",
         "cwd": str(fx.workspace), "message": {"content": "kept"}}])
    _claude(fx, "disagree-nocwd", "88888888-8888-8888-8888-888888888888.jsonl", [
        {"type": "user", "sessionId": "bbbbbbbb-8888-8888-8888-888888888888",
         "message": {"content": "dropped"}}])
    result = fx.run("--json", "--verbose", "-a", "claude")
    payload = json.loads(result.stdout)
    kept = [s for s in payload["sessions"]
            if s["id"] == "aaaaaaaa-7777-7777-7777-777777777777"]
    ids = _ids(payload, "claude")
    return expect(
        kept and kept[0]["support"] == "DiscoverOnly"
        and "bbbbbbbb-8888-8888-8888-888888888888" not in ids
        and "claude_identity_disagreement" in result.stderr,
        f"kept {kept}; ids {ids}; stderr {result.stderr[:200]!r}",
    )


@check("claude-no-session-id-handling")
def _(fx, ctx):
    stem = "99999999-9999-9999-9999-999999999999"
    _claude(fx, "nosid", f"{stem}.jsonl", [
        {"type": "user", "cwd": str(fx.workspace),
         "message": {"content": "unconfirmed"}}])
    _bad_claude_transcripts(fx, 1)
    result = fx.run("--json", "--verbose", "-a", "claude")
    payload = json.loads(result.stdout)
    found = [s for s in payload["sessions"] if s["id"] == stem]
    return expect(
        found and found[0]["support"] == "DiscoverOnly"
        and "claude_no_session_id" in result.stderr,
        f"filename-only identity gave {found}; stderr {result.stderr[:200]!r}",
    )


@check("claude-weak-identity-diagnostic")
def _(fx, ctx):
    _claude(fx, "weak", "notes.jsonl", [
        {"type": "user", "cwd": str(fx.workspace),
         "message": {"content": "weak identity"}}])
    stderr = fx.run("--verbose", "--list", "-a", "claude").stderr
    return expect(
        "claude_weak_identity" in stderr,
        f"a non-UUID filename produced no weak-identity diagnostic: "
        f"{stderr[:250]!r}",
    )


@check("claude-title-precedence")
def _(fx, ctx):
    cases = {
        "aaaa1111-1111-1111-1111-111111111111": (
            {"agent-name": "explicit agent", "ai-title": "ai chosen"},
            "explicit agent"),
        "bbbb1111-1111-1111-1111-111111111111": (
            {"ai-title": "ai chosen"}, "ai chosen"),
        "cccc1111-1111-1111-1111-111111111111": ({}, "summary body"),
    }
    for uuid, (extra, _want) in cases.items():
        record = {"type": "user", "sessionId": uuid, "cwd": str(fx.workspace),
                  "message": {"content": "summary body"}}
        record.update(extra)
        _claude(fx, f"title-{uuid[:4]}", f"{uuid}.jsonl", [record])
    _, payload = fx.json("-a", "claude")
    titles = {s["id"]: s["title"] for s in payload["sessions"]}
    wrong = {u: (titles.get(u), want) for u, (_, want) in cases.items()
             if titles.get(u) != want}
    return expect(not wrong, f"title precedence broken, got/want: {wrong}")


@check("claude-cwd-authoritative-not-directory-name")
def _(fx, ctx):
    uuid = "dddd1111-1111-1111-1111-111111111111"
    # A key that encodes no path at all. It must not start with '-', which is
    # Claude's own path encoding and the only form the scope prefilter prunes
    # (src/integration/claude/discover.rs collect_candidates).
    _claude(fx, "not.an.encoded.path", f"{uuid}.jsonl", [
        {"type": "user", "sessionId": uuid, "cwd": str(fx.workspace),
         "message": {"content": "cwd wins"}}])
    _, payload = fx.json("-a", "claude")
    found = [s for s in payload["sessions"] if s["id"] == uuid]
    return expect(
        found and found[0]["workspace"]
        and os.path.realpath(found[0]["workspace"])
        == os.path.realpath(fx.workspace),
        f"workspace came from somewhere other than the event cwd: {found}",
    )


@check("claude-encoded-key-prefilter")
def _(fx, ctx):
    encoded = "-" + str(fx.home / "far-away").replace("/", "-")
    pruned = "4b4b1111-1111-1111-1111-111111111111"
    kept = "5b5b1111-1111-1111-1111-111111111111"
    # Both transcripts record an in-scope cwd; only the directory names differ,
    # so any difference in the result is the prefilter's doing.
    _claude(fx, encoded, f"{pruned}.jsonl", [
        {"type": "user", "sessionId": pruned, "cwd": str(fx.workspace),
         "message": {"content": "pruned before reading"}}])
    _claude(fx, "arbitrary_key", f"{kept}.jsonl", [
        {"type": "user", "sessionId": kept, "cwd": str(fx.workspace),
         "message": {"content": "read normally"}}])
    _, payload = fx.json("-a", "claude")
    ids = _ids(payload, "claude")
    return expect(
        pruned not in ids and kept in ids,
        f"discovered {ids}; the '-'-encoded out-of-scope key must be pruned "
        f"unread and the arbitrary key must not be",
    )


@check("claude-subagent-transcripts-excluded")
def _(fx, ctx):
    top = "eeee1111-1111-1111-1111-111111111111"
    nested = "ffff1111-1111-1111-1111-111111111111"
    _claude(fx, "withsub", f"{top}.jsonl", [
        {"type": "user", "sessionId": top, "cwd": str(fx.workspace),
         "message": {"content": "top level"}}])
    _claude(fx, "withsub/subagents", f"{nested}.jsonl", [
        {"type": "user", "sessionId": nested, "cwd": str(fx.workspace),
         "message": {"content": "subagent"}}])
    _, payload = fx.json("-a", "claude")
    ids = _ids(payload, "claude")
    return expect(
        top in ids and nested not in ids,
        f"subagent transcript leaked into discovery: {ids}",
    )


@check("claude-tool-result-and-injected-exclusion")
def _(fx, ctx):
    uuid = "1a1a1111-1111-1111-1111-111111111111"
    _claude(fx, "toolonly", f"{uuid}.jsonl", [
        {"type": "user", "sessionId": uuid, "cwd": str(fx.workspace),
         "message": {"content": [{"type": "tool_result", "content": "tool noise"}]}},
        {"type": "user", "sessionId": uuid, "isMeta": True,
         "message": {"content": "meta noise"}},
        {"type": "user", "sessionId": uuid,
         "message": {"content": "real human input"}}])
    _, payload = fx.json("-a", "claude")
    found = [s for s in payload["sessions"] if s["id"] == uuid]
    title = (found[0]["title"] or "") if found else ""
    return expect(
        "tool noise" not in title and "meta noise" not in title
        and "real human input" in title,
        f"title is {title!r}; tool output and isMeta records must not "
        f"contribute",
    )


@check("claude-discovery-read-only-guarantee")
def _(fx, ctx):
    root = fx.home / ".claude"
    before = _snapshot(root)
    fx.run("--list", "-a", "claude")
    fx.run("--json", "-a", "claude")
    return expect(_snapshot(root) == before,
                  "the Claude config root changed during discovery")


@check("claude-truncated-malformed-diagnostics")
def _(fx, ctx):
    truncated = "2a2a1111-1111-1111-1111-111111111111"
    path = _claude(fx, "trunc", f"{truncated}.jsonl", [
        {"type": "user", "sessionId": truncated, "cwd": str(fx.workspace),
         "message": {"content": "kept despite truncation"}}])
    with path.open("a") as fh:
        fh.write('{"type":"user","sessionId":"2a2a')
    malformed = "3a3a1111-1111-1111-1111-111111111111"
    mpath = _claude(fx, "malformed", f"{malformed}.jsonl", [
        {"type": "user", "sessionId": malformed, "cwd": str(fx.workspace),
         "message": {"content": "first"}}])
    mpath.write_text(mpath.read_text() + "{ not json }\n" + json.dumps(
        {"type": "user", "sessionId": malformed,
         "message": {"content": "second"}}) + "\n")
    result = fx.run("--json", "--verbose", "-a", "claude")
    ids = _ids(json.loads(result.stdout), "claude")
    missing = [c for c in ("claude_truncated", "claude_malformed")
               if c not in result.stderr]
    return expect(
        not missing and {truncated, malformed} <= ids,
        f"categories absent: {missing}; discovered {ids}",
    )


@check("claude-root-unavailable-diagnostic")
def _(fx, ctx):
    projects = fx.home / ".claude/projects"
    os.chmod(projects, 0o000)
    try:
        result = fx.run("--verbose", "--list", "-a", "claude")
    finally:
        os.chmod(projects, 0o755)
    return expect(
        "claude_root_unavailable" in result.stderr,
        f"an unreadable projects/ directory produced no diagnostic; "
        f"exit {result.returncode}, stderr {result.stderr[:200]!r}",
    )


@check("claude-resume-spec-exact", "claude-resume-env-preservation",
       "claude-missing-workspace-blocks-resume-spec")
def _(fx, ctx):
    return "SKIPPED: requires a resume handoff; PTY harness"


# ------------------------------------------------------------------- scope
GIT = shutil.which("git")


def _with_git(fx, **overrides):
    """The fixture environment with a real `git` reachable.

    The isolated PATH deliberately holds only the fake agents, so every other
    check sees the no-git fallback; scope checks that need a repository opt
    back in explicitly rather than the fixture leaking git everywhere.
    """
    env = fx.env(**overrides)
    env["PATH"] = f"{env['PATH']}:{os.path.dirname(GIT)}"
    return env


def _git(cwd, *args):
    subprocess.run([GIT, "-C", str(cwd), *args], capture_output=True, check=True)


def _init_repo(path):
    subprocess.run([GIT, "init", "-q", str(path)], capture_output=True, check=True)
    _git(path, "config", "user.email", "qa@example.invalid")
    _git(path, "config", "user.name", "qa")
    (path / "seed").write_text("seed\n")
    _git(path, "add", "seed")
    _git(path, "commit", "-qm", "seed")


@check("scope-default-git")
def _(fx, ctx):
    if not GIT:
        return "SKIPPED: git is not installed"
    _init_repo(fx.workspace)
    linked = fx.home / "linked"
    _git(fx.workspace, "worktree", "add", "-q", str(linked), "-b", "qa-linked")
    _pi_session(fx, "linked", "linked-id", linked)
    nested = fx.workspace / "nested"
    nested.mkdir()
    _init_repo(nested)
    _pi_session(fx, "nested", "nested-id", nested)
    default = _ids(json.loads(fx.run_env("--json", env=_with_git(fx)).stdout))
    widened = _ids(json.loads(
        fx.run_env("--json", "--all-worktrees", env=_with_git(fx)).stdout))
    return expect(
        default == {"pi-id"} and {"pi-id", "linked-id"} <= widened
        and "nested-id" not in widened,
        f"default scope {default} (want just the current worktree), "
        f"--all-worktrees {widened} (want the linked worktree in, the nested "
        f"repository out)",
    )


@check("scope-default-nongit")
def _(fx, ctx):
    child = fx.workspace / "child"
    child.mkdir()
    _pi_session(fx, "child", "child-id", child)
    _, payload = fx.json()
    return expect(
        _ids(payload) == {"pi-id"},
        f"non-git default scope returned {_ids(payload)}; want the exact "
        f"directory only",
    )


@check("scope-git-failure-diagnostic",
       "doccheck-git-scope-failure-not-surfaced-as-diagnostic")
def _(fx, ctx):
    """With no git on PATH the scope falls back to the exact directory, and
    the failure is visible rather than silent."""
    child = fx.workspace / "child"
    child.mkdir()
    _pi_session(fx, "child", "child-id", child)
    plain = fx.run("--list")
    verbose = fx.run("--verbose", "--list")
    _, payload = fx.json()
    return expect(
        "resume: git_scope_discovery_failed: 1" in plain.stderr
        and len(verbose.stderr) > len(plain.stderr)
        and _ids(payload) == {"pi-id"},
        f"stderr {plain.stderr!r}; verbose adds nothing: "
        f"{verbose.stderr == plain.stderr}; scope {_ids(payload)}",
    )


@check("scope-directory-distance-zero")
def _(fx, ctx):
    child = fx.workspace / "child"
    child.mkdir()
    _pi_session(fx, "child", "child-id", child)
    _pi_session(fx, "parent", "parent-id", fx.home)
    _, up = fx.json("-U", "0")
    _, down = fx.json("-D", "0")
    return expect(
        _ids(up) == {"pi-id"} and _ids(down) == {"pi-id"},
        f"-U 0 gave {_ids(up)}, -D 0 gave {_ids(down)}; both want exactly pi-id",
    )


@check("scope-canonicalizes-symlinks")
def _(fx, ctx):
    link = fx.home / "link-to-workspace"
    link.symlink_to(fx.workspace)
    result = fx.run_env("--json", cwd=link)
    ids = _ids(json.loads(result.stdout))
    return expect(
        "pi-id" in ids,
        f"entering through a symlink lost the session recorded under the real "
        f"path: {ids}",
    )


@check("scope-missing-base-usage-error")
def _(fx, ctx):
    result = fx.run(str(fx.home / "does/not/exist"), "--list")
    return expect(
        result.returncode == 2 and result.stderr.strip(),
        f"exit {result.returncode}, stderr {result.stderr!r}",
    )


@check("scope-unavailable-workspace")
def _(fx, ctx):
    gone = fx.workspace / "gone"
    gone.mkdir()
    _pi_session(fx, "gone", "gone-id", gone)
    shutil.rmtree(gone)
    _, payload = fx.json("-D", "1")
    found = [s for s in payload["sessions"] if s["id"] == "gone-id"]
    return expect(
        found and all(s["support"] == "Unavailable" for s in found),
        f"deleted workspace reported as {[s['support'] for s in found]}",
    )


@check("scope-missing-workspace-matched-by-last-known-path")
def _(fx, ctx):
    gone = fx.workspace / "deleted"
    _pi_session(fx, "gone", "gone-id", gone)          # never created
    elsewhere = fx.home / "other/deleted"
    _pi_session(fx, "elsewhere", "elsewhere-id", elsewhere)
    _, payload = fx.json("-D", "all")
    ids = _ids(payload)
    return expect(
        "gone-id" in ids and "elsewhere-id" not in ids,
        f"--down all returned {ids}; a path under the base must still match "
        f"by its last known location, an unrelated one must not",
    )


@check("scope-contains-workspace-caches-git-common-dir")
def _(fx, ctx):
    """Repeated membership tests for one base must spawn git once."""
    if not GIT:
        return "SKIPPED: git is not installed"
    _init_repo(fx.workspace)
    spy_dir = fx.home / "gitspy"
    spy_dir.mkdir()
    log = fx.home / "git.log"
    spy = spy_dir / "git"
    spy.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >>"{log}"\nexec "{GIT}" "$@"\n')
    spy.chmod(0o755)
    env = fx.env()
    env["PATH"] = f"{spy_dir}:{env['PATH']}"

    def spawns(session_count):
        for i in range(session_count):
            _pi_session(fx, f"s{i}", f"s{i}-id", fx.workspace)
        log.write_text("")
        fx.run_env("--json", "-a", "pi", env=env)
        return len([l for l in log.read_text().splitlines()
                    if "--git-common-dir" in l and "--show-toplevel" not in l])

    few, many = spawns(4), spawns(16)
    return expect(
        few == many == 1,
        f"membership spawns git {few} times for 4 sessions and {many} for 16 "
        f"at the same canonical path; a memoized lookup must spawn once",
    )


# ------------------------------------------------------------------ safety
def _snapshot(root):
    out = {}
    for path in sorted(root.rglob("*")):
        try:
            st = path.lstat()
        except OSError:
            continue
        out[str(path.relative_to(root))] = (st.st_mode, st.st_size, st.st_mtime_ns)
    return out


@check("safety-no-write-mutation-during-discovery")
def _(fx, ctx):
    roots = [fx.home / p for p in (".pi", ".claude", ".codex", ".omp",
                                   "xdg/data/opencode")]
    before = {str(r): _snapshot(r) for r in roots if r.exists()}
    fx.run("--list")
    fx.run("--json")
    after = {str(r): _snapshot(r) for r in roots if r.exists()}
    changed = [r for r in before if before[r] != after[r]]
    return expect(not changed, f"discovery mutated {changed}")


@check("safety-preview-cache-in-memory-only")
def _(fx, ctx):
    """No preview code may open a file for writing, and a full run must leave
    the isolated HOME byte-identical outside the directories resume owns."""
    writes = []
    for path in (ctx["root"] / "src/preview").rglob("*.rs"):
        head = path.read_text().split("#[cfg(test)]")[0]
        for n, line in enumerate(head.splitlines(), 1):
            if re.search(r"fs::write|File::create|OpenOptions", line):
                writes.append(f"{path.relative_to(ctx['root'])}:{n}")
    before = _snapshot(fx.home)
    fx.run("--json")
    after = _snapshot(fx.home)
    # The Codex discovery cache is a documented, separate on-disk artifact;
    # anything else appearing would be preview content reaching the disk.
    added = [p for p in sorted(set(after) - set(before))
             if "codex-discovery" not in p and not p.endswith("xdg/cache/resume")]
    return expect(
        not writes and not added,
        f"preview writes files at {writes}; run created {added}",
    )


@check("safety-symlink-confinement")
def _(fx, ctx):
    outside = fx.home / "outside"
    outside.mkdir()
    smuggled = outside / "rollout-smuggled.jsonl"
    smuggled.write_text(
        json.dumps({"type": "session_meta", "payload": {
            "id": "smuggled-id", "cwd": str(fx.workspace),
            "timestamp": "2026-01-01T00:00:00Z"}}) + "\n")
    link = fx.home / ".codex/sessions/2026/01/01/rollout-link.jsonl"
    link.symlink_to(smuggled)
    _, payload = fx.json("-a", "codex")
    ids = {s["id"] for s in payload["sessions"]}
    return expect(
        "smuggled-id" not in ids,
        f"a symlink out of CODEX_HOME surfaced as a session: {ids}",
    )


@check("safety-attachments-never-base64")
def _(fx, ctx):
    blob = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5" * 40
    path = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    path.write_text(
        json.dumps({"type": "session", "version": 3, "id": "pi-id",
                    "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n"
        + json.dumps({"type": "message", "message": {
            "role": "user",
            "content": [{"type": "image", "data": blob},
                        {"type": "text", "text": "look at this"}]}}) + "\n"
    )
    result = fx.run("--json")
    listing = fx.run("--list")
    return expect(
        blob[:60] not in result.stdout and blob[:60] not in listing.stdout,
        "attachment payload reached the output",
    )


@check("safety-injection-collapses-known-wrappers-only")
def _(fx, ctx):
    text = "<root><child>value</child></root> <skill>injected</skill> tail"
    path = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    path.write_text(
        json.dumps({"type": "session", "version": 3, "id": "pi-id",
                    "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n"
        + json.dumps({"type": "message",
                      "message": {"role": "user", "content": text}}) + "\n"
    )
    _, payload = fx.json("-a", "pi")
    title = next(s["title"] for s in payload["sessions"] if s["id"] == "pi-id")
    # Collapsing keeps the inner text and drops only the wrapper tags, so
    # "injected" is expected to survive; "<skill>" must not.
    return expect(
        "<skill>" not in title and "</skill>" not in title
        and "injected" in title and "<root><child>value</child></root>" in title,
        f"title is {title!r}; the skill wrapper must collapse to its inner "
        f"text and the unknown XML must survive verbatim",
    )


@check("safety-terminal-control-stripping")
def _(fx, ctx):
    hostile = ("]8;;https://evil.exampleclick]8;;"
               "]0;pwned‮txet-desrever")
    path = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    path.write_text(
        json.dumps({"type": "session", "version": 3, "id": "pi-id",
                    "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n"
        + json.dumps({"type": "message",
                      "message": {"role": "user", "content": hostile}}) + "\n"
    )
    listing = subprocess.run([str(fx.home / "run"), "--list"], capture_output=True)
    payload = fx.run("--json").stdout
    leaked = [name for name, needle in
              (("ESC", b"\x1b"), ("BEL", b"\x07"), ("bidi", "‮".encode()))
              if needle in listing.stdout]
    return expect(
        not leaked and "‮" not in payload,
        f"--list stdout carries {leaked}; JSON keeps the bidi override: "
        f"{chr(0x202e) in payload}",
    )


@check("safety-jsonl-bounds-dos-protection")
def _(fx, ctx):
    """An oversized line must be refused, not buffered."""
    path = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    path.write_text(
        json.dumps({"type": "session", "version": 3, "id": "pi-id",
                    "timestamp": 1700000000, "cwd": str(fx.workspace)}) + "\n"
        + json.dumps({"type": "message", "message": {
            "role": "user", "content": "y" * (9 * 1024 * 1024)}}) + "\n"
    )
    result = fx.run("--verbose", "--json")
    return expect(
        result.returncode in (0, 1) and "y" * 200 not in result.stdout,
        f"exit {result.returncode}; a 9 MiB line reached the output",
    )


@check("safety-no-machine-wide-scan")
def _(fx, ctx):
    return ("SKIPPED: syscall tracing needs dtruss/fs_usage under sudo, which a "
            "QA run must not require; the read-only claim is covered by "
            "safety-no-write-mutation-during-discovery")


# -------------------------------------------------------------------- docs
@check("doccheck-branch-column-unpopulated")
def _(fx, ctx):
    if not GIT:
        return "SKIPPED: git is not installed"
    _init_repo(fx.workspace)
    _git(fx.workspace, "checkout", "-q", "-b", "qa-branch")
    listing = fx.run_env("--list", env=_with_git(fx))
    return expect(
        "qa-branch" in listing.stdout and " - " not in listing.stdout,
        f"BRANCH column shows no real branch name: {listing.stdout!r}",
    )


@check("doccheck-support-status-unsupported-unreachable")
def _(fx, ctx):
    context = ctx["root"] / "CONTEXT.md"
    if not context.is_file():
        return "CONTEXT.md no longer exists, so the row's citation is dangling"
    text = context.read_text()
    return expect(
        re.search(r"Unsupported is modeled for .{0,120}not currently assigned",
                  text, re.S),
        "CONTEXT.md no longer describes Unsupported as modeled but unassigned",
    )


@check("doccheck-risk-statuses-partially-unreachable")
def _(fx, ctx):
    live = []
    for path in (ctx["root"] / "src").rglob("*.rs"):
        head = path.read_text().split("#[cfg(test)]")[0]
        for n, line in enumerate(head.splitlines(), 1):
            for variant in ("WorkspaceChanged", "ConflictingMetadata"):
                if f"RiskStatus::{variant}" in line and "=>" not in line:
                    live.append(f"{path.relative_to(ctx['root'])}:{n} {variant}")
    return expect(
        not live,
        f"the row records these variants as unreachable, but production code "
        f"assigns them at {live}",
    )


@check("doccheck-config-example-not-round-trip-tested")
def _(fx, ctx):
    """The example is user-facing; a schema change must not be able to break
    it silently. This passes only once a test parses it back."""
    for name in ("src/cli.rs", "src/config.rs"):
        text = (ctx["root"] / name).read_text()
        tests = text.split("#[cfg(test)]", 1)
        if len(tests) > 1 and "config_example" in tests[1] and "from_str" in tests[1]:
            return None
    return ("no test parses `config example` output back into Config, so a "
            "field rename would break the documented example silently")


@check("doccheck-omp-vs-claude-codex-env-propagation-asymmetry")
def _(fx, ctx):
    return ("SKIPPED: observing the resumed agent's environment requires "
            "selecting a row; belongs to the PTY harness")


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
