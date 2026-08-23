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
import contextlib
import csv
import fcntl
import json
import os
import pathlib
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
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


def _detached(fx, *args, env=None, timeout=30):
    """Run the binary in a session of its own, where /dev/tty cannot open.

    A pipe is not enough: the code that wants a terminal opens `/dev/tty`
    rather than looking at its own descriptors, so it still finds the
    developer's terminal behind a redirect. Returns (exit code, output).
    """
    detach = (
        "import os,sys\n"
        "os.setsid()\n"                      # a new session has no /dev/tty
        "out = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)\n"
        "os.dup2(out, 1); os.dup2(out, 2)\n"
        "os.dup2(os.open(os.devnull, os.O_RDONLY), 0)\n"
        "os.execve(sys.argv[1], [sys.argv[1], *sys.argv[3:]], os.environ)\n"
    )
    with tempfile.TemporaryDirectory() as tmp:
        out = pathlib.Path(tmp) / "out"
        result = subprocess.run(
            [sys.executable, "-c", detach, str(fx.binary), str(out), *args],
            capture_output=True, text=True, env=env if env is not None else fx.env(),
            cwd=str(fx.workspace), timeout=timeout)
        text = out.read_text() if out.is_file() else result.stderr
    return result.returncode, text


ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[]P][^\x07\x1b]*(?:\x07|\x1b\\)?|\x1b[()][0-9A-B]|\x1b[=><]")


CSI = re.compile(r"\x1b\[([0-9;?]*)([@-~])")
OSC = re.compile(r"\x1b[]P][^\x07\x1b]*(?:\x07|\x1b\\)?")


def render(raw, rows, cols):
    """Replay cursor-addressed output onto a character grid.

    Stripping escape sequences is enough to look for a message, but not to
    assert on the picker: tuikit paints by jumping the cursor, so the columns
    of a row arrive out of order and interleaved with the rows around them.
    Only the subset tuikit actually emits is interpreted — absolute and
    relative cursor moves, the two erase commands, and the C0 controls.
    """
    grid = [[" "] * cols for _ in range(rows)]
    row = col = 0
    i, n = 0, len(raw)
    while i < n:
        ch = raw[i]
        if ch == "\x1b":
            m = CSI.match(raw, i)
            if m:
                nums = [int(x) for x in re.findall(r"\d+", m.group(1))]
                final = m.group(2)
                first = nums[0] if nums else 0
                if final in "Hf":
                    row = (nums[0] - 1) if nums else 0
                    col = (nums[1] - 1) if len(nums) > 1 else 0
                elif final == "A":
                    row -= max(1, first)
                elif final == "B":
                    row += max(1, first)
                elif final == "C":
                    col += max(1, first)
                elif final == "D":
                    col -= max(1, first)
                elif final == "G":
                    col = max(1, first) - 1
                elif final == "J":
                    blank = [" "] * cols
                    if first == 2:
                        grid = [list(blank) for _ in range(rows)]
                    elif first == 1:
                        for r in range(row):
                            grid[r] = list(blank)
                        grid[row][:col + 1] = [" "] * (col + 1)
                    else:
                        grid[row][col:] = [" "] * (cols - col)
                        for r in range(row + 1, rows):
                            grid[r] = list(blank)
                elif final == "K":
                    if first == 1:
                        grid[row][:col + 1] = [" "] * (col + 1)
                    elif first == 2:
                        grid[row] = [" "] * cols
                    else:
                        grid[row][col:] = [" "] * (cols - col)
                i = m.end()
            elif OSC.match(raw, i):
                i = OSC.match(raw, i).end()
            else:  # \x1b(B, \x1b=, \x1b>, \x1bM, ...
                i += 3 if i + 1 < n and raw[i + 1] in "()#" else 2
        elif ch == "\r":
            col, i = 0, i + 1
        elif ch == "\n":
            row, col, i = row + 1, 0, i + 1
            if row >= rows:
                grid.pop(0)
                grid.append([" "] * cols)
                row = rows - 1
        elif ch == "\b":
            col, i = col - 1, i + 1
        elif ch == "\t":
            col, i = (col // 8 + 1) * 8, i + 1
        elif ch >= " ":
            if col >= cols:
                row, col = row + 1, 0
            if row >= rows:
                grid.pop(0)
                grid.append([" "] * cols)
                row = rows - 1
            grid[max(0, row)][col] = ch
            col, i = col + 1, i + 1
        else:
            i += 1
        # col may sit one past the margin: that is the pending-wrap state a
        # real terminal keeps, and the next printable character wraps.
        row, col = max(0, min(row, rows - 1)), max(0, min(col, cols))
    return ["".join(line) for line in grid]


class PtyTimeout(Exception):
    """A pattern never appeared, or the binary never exited, in time."""


class Pty:
    """The real binary on a controlling terminal.

    A pseudoterminal is the only way to reach the code paths that read
    `/dev/tty` directly (first-run setup) or take over the screen (the
    picker), so those rows cannot be checked through a pipe.
    """

    def __init__(self, fx, *args, env=None, cwd=None, rows=30, cols=120,
                 term="xterm-256color", stdin=None):
        env = dict(fx.env() if env is None else env)
        env["TERM"] = term
        cwd = str(cwd or fx.workspace)
        argv = [str(fx.binary), *args]
        pid, fd = os.forkpty()
        if pid == 0:
            try:
                os.chdir(cwd)
                if stdin is not None:  # keystrokes still arrive over /dev/tty
                    os.dup2(os.open(str(stdin), os.O_RDONLY), 0)
                os.execve(argv[0], argv, env)
            except BaseException:
                os._exit(127)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.pid, self.fd, self.raw, self.status = pid, fd, "", None
        self.rows, self.cols = rows, cols

    @property
    def screen(self):
        """Output with escape sequences removed, for substring matching."""
        return ANSI.sub("", self.raw).replace("\r\n", "\n")

    @property
    def lines(self):
        """The visible grid, for asserting on layout rather than content."""
        return [line.rstrip() for line in render(self.raw, self.rows, self.cols)]

    @property
    def wrapped(self):
        """The grid as one string, padding intact, so a row that ran past the
        right margin can still be measured across the wrap."""
        return "".join(render(self.raw, self.rows, self.cols))

    def find(self, pattern):
        """Every visible line matching `pattern`, in top-to-bottom order."""
        return [line for line in self.lines if re.search(pattern, line)]

    def _drain(self, timeout):
        ready, _, _ = select.select([self.fd], [], [], timeout)
        if not ready:
            return False
        try:
            chunk = os.read(self.fd, 65536)
        except OSError:  # the child exited and closed the slave side
            return None
        if not chunk:
            return None
        self.raw += chunk.decode("utf-8", "replace")
        return True

    def expect(self, pattern, timeout=15):
        """Read until `pattern` (regex) matches the accumulated screen."""
        deadline = time.monotonic() + timeout
        while True:
            if re.search(pattern, self.screen, re.MULTILINE):
                return self.screen
            if time.monotonic() > deadline:
                raise PtyTimeout(f"never saw {pattern!r}; screen so far:\n"
                                 f"{self.screen[-1500:]}")
            if self._drain(min(0.2, max(0.0, deadline - time.monotonic()))) is None:
                if re.search(pattern, self.screen, re.MULTILINE):
                    return self.screen
                raise PtyTimeout(f"exited before {pattern!r}; screen:\n"
                                 f"{self.screen[-1500:]}")

    def send(self, data, settle=0.35):
        os.write(self.fd, data.encode() if isinstance(data, str) else data)
        deadline = time.monotonic() + settle
        while time.monotonic() < deadline:
            if self._drain(deadline - time.monotonic()) is None:
                break
        return self

    def wait(self, timeout=15):
        """Drain to EOF and return the exit code (128+signal when killed)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._drain(min(0.2, deadline - time.monotonic())) is None:
                break
        else:
            os.kill(self.pid, signal.SIGKILL)
            raise PtyTimeout(f"did not exit within {timeout}s; screen:\n"
                             f"{self.screen[-1500:]}")
        _, status = os.waitpid(self.pid, 0)
        self.status = (os.WEXITSTATUS(status) if os.WIFEXITED(status)
                       else 128 + os.WTERMSIG(status))
        return self.status

    def alive(self):
        """True while the child has neither exited nor been reaped."""
        if self.status is not None:
            return False
        return os.waitpid(self.pid, os.WNOHANG) == (0, 0)

    def close(self):
        """Kill the child and reap it, draining the master meanwhile.

        A process blocked writing into a full pty buffer stays unreapable
        until someone reads that buffer, so a plain `waitpid` here can wait
        forever on a picker that painted more than we bothered to read.
        """
        if self.status is None:
            try:
                os.kill(self.pid, signal.SIGKILL)
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if os.waitpid(self.pid, os.WNOHANG) != (0, 0):
                        break
                    self._drain(0.1)
            except OSError:
                pass
        os.close(self.fd)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


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


# ------------------------------------------------------------------- codex
def _rollout(fx, session_id, cwd, title="codex title", *, root=".codex",
             kind="sessions", day="2026/01/01", name=None, extra=()):
    """One rollout-*.jsonl under the date-partitioned Codex layout."""
    d = fx.home / root / kind / day
    d.mkdir(parents=True, exist_ok=True)
    path = d / (name or f"rollout-{session_id}.jsonl")
    records = [
        {"type": "session_meta", "payload": {
            "id": session_id, "cwd": str(cwd),
            "timestamp": "2026-01-01T00:00:00Z"}},
        {"type": "event_msg", "payload": {
            "type": "user_message", "message": {"role": "user", "content": title}}},
    ]
    records.extend(extra)
    path.write_text("".join(json.dumps(r) + "\n" for r in records))
    return path


def _codex(fx, *args, env=None, cwd=None, binary=None):
    result = (fx.run_env("--json", "-a", "codex", *args, env=env, cwd=cwd)
              if binary is None else subprocess.run(
                  [str(binary), "--json", "-a", "codex", *args],
                  capture_output=True, text=True,
                  env=env if env is not None else fx.env(),
                  cwd=str(cwd or fx.workspace)))
    payload = json.loads(result.stdout) if result.stdout.strip() else {"sessions": []}
    return result, {s["id"]: s for s in payload["sessions"] if s["agent"] == "codex"}


@check("codex-root-resolution")
def _(fx, ctx):
    other = fx.home / "custom-codex"
    _rollout(fx, "custom-root-id", fx.workspace, root="custom-codex")
    _, sessions = _codex(fx, env=fx.env(CODEX_HOME=str(other)))
    return expect(
        set(sessions) == {"custom-root-id"},
        f"CODEX_HOME was not honoured: {set(sessions)}",
    )


@check("codex-active-and-archived-roots")
def _(fx, ctx):
    _rollout(fx, "active-id", fx.workspace, kind="sessions")
    _rollout(fx, "archived-id", fx.workspace, kind="archived_sessions")
    _, sessions = _codex(fx)
    return expect(
        {"active-id", "archived-id"} <= set(sessions),
        f"both rollout roots must be scanned: {set(sessions)}",
    )


@check("codex-identity-and-workspace-authoritative")
def _(fx, ctx):
    elsewhere = fx.home / "codex-decoy"
    elsewhere.mkdir()
    _rollout(fx, "true-id", fx.workspace, name="rollout-unrelated-filename.jsonl",
             day="2026/02/02")
    # A payload.session_id that disagrees must be ignored in favour of payload.id.
    path = fx.home / ".codex/sessions/2026/02/02/rollout-unrelated-filename.jsonl"
    records = [json.loads(line) for line in path.read_text().splitlines()]
    records[0]["payload"]["session_id"] = "decoy-session-id"
    records[0]["payload"]["workspace_roots"] = [str(elsewhere)]
    path.write_text("".join(json.dumps(r) + "\n" for r in records))
    _, sessions = _codex(fx)
    found = sessions.get("true-id", {})
    return expect(
        "decoy-session-id" not in sessions and found
        and os.path.realpath(found.get("workspace", ""))
        == os.path.realpath(fx.workspace),
        f"identity/workspace did not come from session_meta.payload: {sessions}",
    )


@check("codex-user-message-dedup")
def _(fx, ctx):
    text = "the very same sentence"
    _rollout(fx, "dedup-id", fx.workspace, title=text, extra=[
        {"type": "response_item", "payload": {
            "type": "message", "role": "user", "content": text}}])
    _, sessions = _codex(fx)
    title = sessions.get("dedup-id", {}).get("title") or ""
    return expect(
        title.count(text) == 1,
        f"the two representations of one message were not deduped: {title!r}",
    )


@check("codex-import-badge-safe")
def _(fx, ctx):
    secret = "/private/origin/path/and-remote"
    _rollout(fx, "import-id", fx.workspace, extra=[
        {"type": "event_msg", "payload": {
            "type": "foreign_session_import",
            "foreign_session_import": {
                "source_kind": "claude", "origin_cwd": secret,
                "origin_remote": "git@example.invalid:secret/repo.git"}}}])
    result, sessions = _codex(fx)
    blob = result.stdout + result.stderr + fx.run("--list", "-a", "codex").stdout
    return expect(
        secret not in blob and "example.invalid" not in blob,
        f"an import payload leaked a path or remote into output: "
        f"{[l for l in blob.splitlines() if 'origin' in l or 'invalid' in l]}",
    )


@check("codex-per-file-error-isolation")
def _(fx, ctx):
    for i in range(3):
        _rollout(fx, f"valid-{i}", fx.workspace)
    corrupt = fx.home / ".codex/sessions/2026/01/01/rollout-corrupt.jsonl"
    corrupt.write_bytes(b"\x00\x01 not jsonl at all\n" * 20)
    result, sessions = _codex(fx, "--verbose")
    return expect(
        {f"valid-{i}" for i in range(3)} <= set(sessions)
        and "codex" in result.stderr,
        f"one corrupt rollout must not stop the others: discovered "
        f"{set(sessions)}, stderr {result.stderr[:200]!r}",
    )


@check("codex-root-unavailable-diagnostic")
def _(fx, ctx):
    _rollout(fx, "hidden-id", fx.workspace)
    sessions_dir = fx.home / ".codex/sessions"
    os.chmod(sessions_dir, 0o000)
    try:
        blocked = fx.run("--json", "--verbose", "-a", "codex")
    finally:
        os.chmod(sessions_dir, 0o755)
    missing = fx.run_env("--json", "--verbose", "-a", "codex",
                         env=fx.env(CODEX_HOME=str(fx.home / "no-such-codex")))
    reported = "codex_root_unavailable" in (blocked.stdout + blocked.stderr)
    return expect(
        reported and blocked.returncode == 1
        and "codex_root_unavailable" not in (missing.stdout + missing.stderr)
        and missing.returncode == 0,
        f"unreadable: exit {blocked.returncode} reported={reported}; "
        f"missing: exit {missing.returncode}, "
        f"stderr {missing.stderr[:150]!r}",
    )


@check("codex-proc-probe-disable-guard")
def _(fx, ctx):
    marker = fx.home / "lsof-ran"
    lsof = fx.home / "bin/lsof"
    lsof.write_text(f"#!/bin/sh\ntouch {marker}\nexit 0\n")
    os.chmod(lsof, 0o755)
    _rollout(fx, "probe-id", fx.workspace)
    _, sessions = _codex(fx)  # the fixture sets RESUME_DISABLE_PROC_PROBE=1
    return expect(
        not marker.exists()
        and {s["activity"] for s in sessions.values()} == {"Unknown"},
        f"lsof ran: {marker.exists()}; activities "
        f"{ {s['activity'] for s in sessions.values()} }",
    )


@check("codex-active-detection")
def _(fx, ctx):
    """The probe is one fixed-argv lsof run whose malformed records are
    isolated per PID. With the guard cleared, a replaying fake lsof must be
    executed exactly once however many sessions are queried."""
    base = len(_codex(fx)[1])  # the shared fixture already ships one rollout
    calls = fx.home / "lsof-calls"
    lsof = fx.home / "bin/lsof"
    lsof.write_text(f"#!/bin/sh\necho \"$*\" >>{calls}\n"
                    f"printf 'p-not-a-pid\\nfbad\\np4242\\n'\nexit 0\n")
    os.chmod(lsof, 0o755)
    for i in range(6):
        _rollout(fx, f"probe-{i}", fx.workspace)
    result, sessions = _codex(
        fx, env=fx.env(RESUME_DISABLE_PROC_PROBE=None))
    ran = calls.read_text().splitlines() if calls.is_file() else []
    return expect(
        len(ran) == 1 and len(sessions) == base + 6 and result.returncode == 0,
        f"lsof ran {len(ran)} time(s) for {len(sessions)} sessions "
        f"(want 1 probe, {base + 6} sessions), exit {result.returncode}",
    )


@check("codex-discovery-parallel-scan", "codex-parallel-scan-bounded")
def _(fx, ctx):
    """Output must not depend on whether the parallel path was taken, so the
    check compares a corpus above PARALLEL_THRESHOLD against one below it."""
    order = lambda result: [s["id"] for s in json.loads(result.stdout)["sessions"]
                            if s["agent"] == "codex"]
    base = len(_codex(fx)[1])  # the shared fixture already ships one rollout
    for i in range(40):
        _rollout(fx, f"many-{i:02d}", fx.workspace, day=f"2026/03/{i % 28 + 1:02d}")
    first, many = _codex(fx)
    small = Fixture(ctx["root"], ctx["binary"])
    try:
        small_base = len(_codex(small)[1])
        for i in range(3):
            _rollout(small, f"few-{i}", small.workspace)
        _, few = _codex(small)
    finally:
        shutil.rmtree(small.home, ignore_errors=True)
    # Re-run the parallel corpus: worker scheduling must not reorder output.
    (fx.home / "xdg/cache/resume/codex-discovery-v1.json").unlink(missing_ok=True)
    again, _ = _codex(fx)
    return expect(
        len(many) == base + 40 and len(few) == small_base + 3
        and order(first) == order(again) and order(first),
        f"parallel corpus yielded {len(many)} of {base + 40}, sequential "
        f"{len(few)} of {small_base + 3}, and the order was "
        f"{'reproducible' if order(first) == order(again) else 'not reproducible'}",
    )


def _cache_file(fx):
    path = fx.home / "xdg/cache/resume/codex-discovery-v1.json"
    return json.loads(path.read_text()) if path.is_file() else None


@check("codex-cache-location")
def _(fx, ctx):
    _rollout(fx, "cached-id", fx.workspace)
    fx.run("--json", "-a", "codex")
    stray = list((fx.home / ".cache").rglob("codex-discovery-v1.json")) \
        if (fx.home / ".cache").is_dir() else []
    return expect(
        _cache_file(fx) is not None and not stray,
        f"cache under XDG_CACHE_HOME: {_cache_file(fx) is not None}; "
        f"stray copies under HOME: {stray}",
    )


@check("codex-discovery-cache", "codex-cache-hit-skips-parse")
def _(fx, ctx):
    for i in range(20):
        _rollout(fx, f"warm-{i:02d}", fx.workspace)
    cold = fx.run("--json", "-a", "codex")
    entries = len((_cache_file(fx) or {}).get("entries", {}))
    rollouts = len(list((fx.home / ".codex").rglob("rollout-*.jsonl")))
    warm = fx.run("--json", "-a", "codex")
    # Content change must invalidate the entry, not the (unchanged) mtime.
    path = fx.home / ".codex/sessions/2026/01/01/rollout-warm-00.jsonl"
    records = [json.loads(l) for l in path.read_text().splitlines()]
    records[1]["payload"]["message"]["content"] = "edited after caching"
    path.write_text("".join(json.dumps(r) + "\n" for r in records))
    edited = json.loads(fx.run("--json", "-a", "codex").stdout)
    title = next(s["title"] for s in edited["sessions"] if s["id"] == "warm-00")
    (fx.home / "xdg/cache/resume/codex-discovery-v1.json").unlink()
    fresh = fx.run("--json", "-a", "codex")
    return expect(
        cold.stdout == warm.stdout and entries == rollouts
        and title == "edited after caching"
        and json.loads(fresh.stdout)["sessions"] == edited["sessions"],
        f"cold==warm: {cold.stdout == warm.stdout}; entries {entries} "
        f"(want {rollouts}, one per rollout on disk); "
        f"edited title {title!r}; cache-less run matched: "
        f"{json.loads(fresh.stdout)['sessions'] == edited['sessions']}",
    )


@check("codex-cache-degrades-silently")
def _(fx, ctx):
    _rollout(fx, "degrade-id", fx.workspace)
    cold = fx.run("--json", "-a", "codex")
    path = fx.home / "xdg/cache/resume/codex-discovery-v1.json"
    problems = []
    for label, content in (("truncated", ""), ("garbage", "}{ not json"),
                           ("wrong version", json.dumps(
                               {"version": 9999, "entries": {}}))):
        path.write_text(content)
        again = fx.run("--json", "-a", "codex")
        # Compared against the cold run, not against empty: the fixture always
        # emits git_scope_discovery_failed, which is unrelated to the cache.
        if (again.stdout, again.stderr, again.returncode) \
                != (cold.stdout, cold.stderr, cold.returncode):
            problems.append(f"{label}: exit {again.returncode}, "
                            f"stderr {again.stderr[:120]!r}")
    return expect(not problems, "; ".join(problems))


@check("codex-cache-prunes-orphans")
def _(fx, ctx):
    kept = _rollout(fx, "kept-id", fx.workspace)
    doomed = _rollout(fx, "doomed-id", fx.workspace)
    fx.run("--json", "-a", "codex")
    before = set((_cache_file(fx) or {}).get("entries", {}))
    doomed.unlink()
    fx.run("--json", "-a", "codex")
    after = set((_cache_file(fx) or {}).get("entries", {}))
    return expect(
        any(str(doomed) in k for k in before)
        and not any(str(doomed) in k for k in after)
        and any(str(kept) in k for k in after),
        f"orphan pruning: before {len(before)} entries, after {len(after)}; "
        f"deleted rollout still present: "
        f"{[k for k in after if str(doomed) in k]}",
    )


@check("codex-cache-keeps-other-roots")
def _(fx, ctx):
    _rollout(fx, "root-a-id", fx.workspace, root="codex-a")
    _rollout(fx, "root-b-id", fx.workspace, root="codex-b")
    a = fx.env(CODEX_HOME=str(fx.home / "codex-a"))
    b = fx.env(CODEX_HOME=str(fx.home / "codex-b"))
    fx.run_env("--json", "-a", "codex", env=a)
    fx.run_env("--json", "-a", "codex", env=b)
    entries = set((_cache_file(fx) or {}).get("entries", {}))
    return expect(
        any("codex-a" in k for k in entries) and any("codex-b" in k for k in entries),
        f"scanning root B pruned root A's entries: {sorted(entries)}",
    )


@check("codex-scope-gated-not-cached")
def _(fx, ctx):
    deep = fx.workspace / "deep"
    deep.mkdir()
    _rollout(fx, "deep-id", deep)
    narrow, _ = _codex(fx)
    broad, wide = _codex(fx, "-D", "all")
    return expect(
        "deep-id" not in json.loads(narrow.stdout).get("sessions", [{}])[0].get("id", "")
        and "deep-id" in wide,
        f"a scope-gated rejection was cached as 'no session': the broader "
        f"run found {set(wide)}",
    )


def _sqlite_binary(ctx):
    """Build (once) a binary with the optional codex-sqlite feature. The
    default binary deliberately has no SQLite linked, so the enrichment rows
    cannot be checked with it."""
    if "sqlite_binary" in ctx:
        return ctx["sqlite_binary"]
    target = ctx["root"] / "target/qa-codex-sqlite"
    build = subprocess.run(
        ["cargo", "build", "--locked", "--features", "codex-sqlite",
         "--target-dir", str(target)],
        cwd=ctx["root"], capture_output=True, text=True)
    ctx["sqlite_binary"] = (target / "debug/resume") if build.returncode == 0 else None
    ctx["sqlite_build"] = build.stderr[-400:]
    return ctx["sqlite_binary"]


def _state_db(fx, rows, name="state_5.sqlite", root=".codex", with_path=False):
    """A Codex state DB holding the columns the enrichment query reads.

    `with_path` adds `path` (one of the rollout-path column names
    sqlite.rs detect_schema recognises); rows then carry a trailing rollout
    path. It matters for the precedence rows: sqlite.rs match_row only reaches
    the cwd-disagreement check through a path match, because an id match whose
    cwd disagrees is rejected outright as an id collision."""
    path = fx.home / root / name
    script = [
        "create table session (id text primary key, cwd text, title text,"
        " updated_at integer, archived integer"
        + (", path text" if with_path else "") + ");"]
    for row in rows:
        script.append(
            "insert into session values ("
            + ", ".join("null" if v is None else
                        (str(v) if isinstance(v, int) else f"'{v}'")
                        for v in row) + ");")
    subprocess.run(["sqlite3", str(path)], input="\n".join(script),
                   text=True, check=True, capture_output=True)
    return path


@check("codex-sqlite-enrichment-off-by-default")
def _(fx, ctx):
    tree = subprocess.run(["cargo", "tree", "--locked", "-e", "normal",
                           "--prefix", "none"],
                          cwd=ctx["root"], capture_output=True, text=True)
    linked = subprocess.run(["otool", "-L", str(ctx["binary"])],
                            capture_output=True, text=True)
    return expect(
        "rusqlite" not in tree.stdout and "sqlite" not in linked.stdout.lower(),
        f"a default build links SQLite: cargo tree mentions rusqlite "
        f"{'rusqlite' in tree.stdout}; otool lines "
        f"{[l.strip() for l in linked.stdout.splitlines() if 'sqlite' in l.lower()]}",
    )


@check("codex-sqlite-readonly-open")
def _(fx, ctx):
    if _sqlite_binary(ctx) is None:
        return f"the codex-sqlite build failed: {ctx['sqlite_build']}"
    result = subprocess.run(
        ["cargo", "test", "--locked", "--features", "codex-sqlite", "--lib",
         "--quiet", "--", "integration::codex::sqlite::tests"],
        cwd=ctx["root"], capture_output=True, text=True)
    return expect(
        result.returncode == 0 and "0 passed" not in result.stdout,
        f"the assert_readonly-backed suite did not pass: "
        f"{(result.stdout + result.stderr)[-400:]}",
    )


@check("codex-sqlite-db-absent-degrades-silently")
def _(fx, ctx):
    binary = _sqlite_binary(ctx)
    if binary is None:
        return f"the codex-sqlite build failed: {ctx['sqlite_build']}"
    _rollout(fx, "no-db-id", fx.workspace)
    plain = fx.run("--json", "--verbose", "-a", "codex")
    with_feature = subprocess.run(
        [str(binary), "--json", "--verbose", "-a", "codex"],
        capture_output=True, text=True, env=fx.env(), cwd=str(fx.workspace))
    return expect(
        (with_feature.stdout, with_feature.stderr, with_feature.returncode)
        == (plain.stdout, plain.stderr, plain.returncode),
        f"an absent state DB changed the output: exit {with_feature.returncode} "
        f"vs {plain.returncode}; stderr {with_feature.stderr[:200]!r} vs "
        f"{plain.stderr[:200]!r}",
    )


@check("codex-sqlite-db-corrupt-degrades-silently")
def _(fx, ctx):
    binary = _sqlite_binary(ctx)
    if binary is None:
        return f"the codex-sqlite build failed: {ctx['sqlite_build']}"
    _rollout(fx, "corrupt-db-id", fx.workspace)
    plain = fx.run("--json", "-a", "codex")
    (fx.home / ".codex/state_5.sqlite").write_bytes(b"definitely not a database\n")
    degraded = subprocess.run(
        [str(binary), "--json", "--verbose", "-a", "codex"],
        capture_output=True, text=True, env=fx.env(), cwd=str(fx.workspace))
    return expect(
        degraded.returncode == 0
        and json.loads(degraded.stdout)["sessions"]
        == json.loads(plain.stdout)["sessions"],
        f"a corrupt state DB did not degrade to the JSONL result: exit "
        f"{degraded.returncode}, stderr {degraded.stderr[:200]!r}",
    )


@check("codex-sqlite-enrichment-title-activity")
def _(fx, ctx):
    binary = _sqlite_binary(ctx)
    if binary is None:
        return f"the codex-sqlite build failed: {ctx['sqlite_build']}"
    # One rollout with a derivable title, one with none at all.
    _rollout(fx, "has-title-id", fx.workspace, title="from the transcript")
    silent = fx.home / ".codex/sessions/2026/01/01/rollout-silent-id.jsonl"
    silent.write_text(json.dumps({"type": "session_meta", "payload": {
        "id": "silent-id", "cwd": str(fx.workspace),
        "timestamp": "2026-01-01T00:00:00Z"}}) + "\n")
    _state_db(fx, [("has-title-id", str(fx.workspace), "db title", 1700000000, 0),
                   ("silent-id", str(fx.workspace), "db title only", 1700000000, 0)])
    _, sessions = _codex(fx, binary=binary)
    return expect(
        sessions.get("has-title-id", {}).get("title") == "from the transcript"
        and sessions.get("silent-id", {}).get("title") == "db title only",
        f"enrichment is fallback-only: got "
        f"{ {k: v.get('title') for k, v in sessions.items()} }",
    )


@check("codex-sqlite-precedence-conflict-diagnostic")
def _(fx, ctx):
    binary = _sqlite_binary(ctx)
    if binary is None:
        return f"the codex-sqlite build failed: {ctx['sqlite_build']}"
    elsewhere = fx.home / "sqlite-decoy"
    elsewhere.mkdir()
    silent = fx.home / ".codex/sessions/2026/01/01/rollout-conflict-id.jsonl"
    silent.parent.mkdir(parents=True, exist_ok=True)
    silent.write_text(json.dumps({"type": "session_meta", "payload": {
        "id": "conflict-id", "cwd": str(fx.workspace),
        "timestamp": "2026-01-01T00:00:00Z"}}) + "\n")
    _state_db(fx, [("conflict-id", str(elsewhere), "db title", 1700000000, 0,
                    str(silent))], with_path=True)
    result, sessions = _codex(fx, "--verbose", binary=binary)
    found = sessions.get("conflict-id", {})
    return expect(
        os.path.realpath(found.get("workspace", "")) == os.path.realpath(fx.workspace)
        and found.get("title") != "db title"
        and "codex_sqlite_workspace_mismatch" in (result.stdout + result.stderr),
        f"session {found}; a cwd disagreement must leave the JSONL identity "
        f"untouched and report codex_sqlite_workspace_mismatch; stderr "
        f"{result.stderr[:250]!r}",
    )


@check("codex-active-resume-risk", "codex-resume-spec-exact",
       "codex-resume-env-preservation-provenance-based")
def _(fx, ctx):
    return "SKIPPED: requires a resume handoff; PTY harness"


@check("codex-background-discovery", "codex-sole-agent-synchronous",
       "codex-results-merge-on-navigation", "codex-progress-after-picker")
def _(fx, ctx):
    return "SKIPPED: requires the picker; PTY harness"


# --------------------------------------------------------------------- omp
def _omp(fx, root, key, name, records):
    """One OMP transcript under a grouped workspace-key directory."""
    d = fx.home / root / key
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{name}.jsonl").write_text(
        "".join(json.dumps(r) + "\n" for r in records))
    return d / f"{name}.jsonl"


def _omp_header(session_id, cwd, title=None, timestamp=1700000000):
    header = {"type": "session", "version": 3, "id": session_id,
              "timestamp": timestamp, "cwd": str(cwd)}
    if title is not None:
        header["title"] = title
    return header


def _omp_key(fx, workspace):
    """OMP's encoded, home-relative grouping directory name."""
    rel = os.path.relpath(str(workspace), str(fx.home))
    return "-" + re.sub(r"[^A-Za-z0-9]", "-", rel)


def _omp_ids(fx, *args, env=None):
    result = fx.run_env("--json", "-a", "omp", *args, env=env)
    payload = json.loads(result.stdout) if result.stdout.strip() else {"sessions": []}
    return result, {s["id"]: s for s in payload["sessions"] if s["agent"] == "omp"}


@check("omp-base-root-resolution")
def _(fx, ctx):
    custom = fx.home / "custom-omp"
    _omp(fx, "custom-omp/agent", _omp_key(fx, fx.workspace), "base",
         [_omp_header("custom-config-id", fx.workspace, "custom config root")])
    # PI_CODING_AGENT_DIR would win for the default profile, so clear it to
    # observe the config root alone.
    _, sessions = _omp_ids(fx, env=fx.env(PI_CONFIG_DIR=str(custom),
                                          PI_CODING_AGENT_DIR=None,
                                          XDG_DATA_HOME=None))
    return expect(
        set(sessions) == {"custom-config-id"},
        f"PI_CONFIG_DIR was not used as the OMP config root: {set(sessions)}",
    )


@check("omp-profile-selection-precedence")
def _(fx, ctx):
    """`discover_omp` force-resolves the unprofiled Default and enumerates
    every `profiles/<name>` directory regardless of which profile the env
    selected, so the discovered set is identical either way and the
    precedence order itself is only observable in `select_profile`."""
    flag = fx.run("--json", "-a", "omp", "--profile", "x")
    for profile in ("work", "personal"):
        _omp(fx, f".omp/profiles/{profile}/agent", _omp_key(fx, fx.workspace),
             profile, [_omp_header(f"{profile}-id", fx.workspace, profile)])
    _, both = _omp_ids(fx, env=fx.env(OMP_PROFILE="work", PI_PROFILE="personal"))
    _, neither = _omp_ids(fx)
    designated = _cargo_tests(ctx, *[
        f"integration::omp::tests::roots::{n}" for n in (
            "profile_flag_beats_omp_and_pi_profile_env",
            "omp_profile_beats_pi_profile",
            "pi_profile_selected_when_no_omp_profile_or_flag",
        )])
    return designated or expect(
        flag.returncode == 2 and set(both) == set(neither)
        and {"work-id", "personal-id"} <= set(both),
        f"--profile exit {flag.returncode} (want 2, no such argument exists); "
        f"with env {set(both)} vs without {set(neither)} — every profile is "
        f"discovered either way",
    )


@check("omp-all-profiles-discovered")
def _(fx, ctx):
    key = _omp_key(fx, fx.workspace)
    _omp(fx, ".omp/agent", key, "default",
         [_omp_header("default-profile-id", fx.workspace, "default")])
    for profile in ("work", "personal"):
        _omp(fx, f".omp/profiles/{profile}/agent", key, profile,
             [_omp_header(f"{profile}-id", fx.workspace, profile)])
    # PI_CODING_AGENT_DIR would redirect the default profile at Pi's root.
    env = fx.env(PI_CODING_AGENT_DIR=None, XDG_DATA_HOME=None)
    result, sessions = _omp_ids(fx, env=env)
    listing = fx.run_env("--list", "-a", "omp", env=env).stdout
    want = {"default-profile-id", "work-id", "personal-id"}
    profiles = re.findall(r"omp\[([a-z]+)\]", listing)
    return expect(
        want <= set(sessions) and {"work", "personal"} <= set(profiles),
        f"discovered {set(sessions)} (want {want}); "
        f"profile tags in --list: {sorted(set(profiles))}",
    )


@check("omp-named-profile-ignores-pi-coding-agent-dir")
def _(fx, ctx):
    _omp(fx, ".omp/profiles/work/agent", _omp_key(fx, fx.workspace), "work",
         [_omp_header("work-id", fx.workspace, "work profile")])
    # The fixture's PI_CODING_AGENT_DIR points at Pi's root, which holds no
    # OMP fixture of its own; only the Default profile may read from it.
    _, sessions = _omp_ids(fx, env=fx.env(OMP_PROFILE="work"))
    work = sessions.get("work-id", {})
    borrowed = [i for i, s in sessions.items() if s.get("profile") == "work"]
    return expect(
        work.get("profile") == "work" and borrowed == ["work-id"],
        f"the 'work' profile reported {work.get('profile')!r} and claimed "
        f"{borrowed}; it must read only <config-root>/profiles/work/agent, "
        f"never PI_CODING_AGENT_DIR",
    )


@check("omp-default-profile-agent-root-honors-pi-coding-agent-dir")
def _(fx, ctx):
    """The fixture deliberately sets PI_CODING_AGENT_DIR at Pi's root, so
    unprofiled OMP must mirror the Pi fixture rather than read ~/.omp/agent."""
    _, sessions = _omp_ids(fx)
    _, payload = fx.json("-a", "pi")
    pi = {s["id"]: s["title"] for s in payload["sessions"]}
    return expect(
        "pi-id" in sessions and "omp-id" not in sessions
        and sessions["pi-id"]["title"] == pi["pi-id"],
        f"unprofiled OMP reported {set(sessions)}; with PI_CODING_AGENT_DIR set "
        f"it must read Pi's root, not <config-root>/agent",
    )


@check("omp-title-sidecar-before-header")
def _(fx, ctx):
    _omp(fx, ".pi/agent", _omp_key(fx, fx.workspace), "sidecar", [
        {"type": "title", "v": 1, "title": "my title"},
        _omp_header("sidecar-id", fx.workspace)])
    _, sessions = _omp_ids(fx)
    return expect(
        sessions.get("sidecar-id", {}).get("title") == "my title",
        f"a title sidecar preceding the header was lost: "
        f"{sessions.get('sidecar-id')}",
    )


@check("omp-title-change-latest-wins")
def _(fx, ctx):
    _omp(fx, ".pi/agent", _omp_key(fx, fx.workspace), "renamed", [
        _omp_header("renamed-id", fx.workspace, "original header title"),
        {"type": "title_change", "title": "renamed later"},
        {"type": "title_change", "title": "   "}])
    _, sessions = _omp_ids(fx)
    return expect(
        sessions.get("renamed-id", {}).get("title") == "renamed later",
        f"latest non-empty title did not win: {sessions.get('renamed-id')}",
    )


@check("omp-attribution-filters-injected-messages")
def _(fx, ctx):
    _omp(fx, ".pi/agent", _omp_key(fx, fx.workspace), "attributed", [
        _omp_header("attributed-id", fx.workspace),
        {"type": "message", "attribution": {"source": "agent"},
         "message": {"role": "user", "content": "injected by the agent"}},
        {"type": "message",
         "message": {"role": "user", "content": "typed by the human"}}])
    _, sessions = _omp_ids(fx)
    title = sessions.get("attributed-id", {}).get("title") or ""
    return expect(
        "injected by the agent" not in title and "typed by the human" in title,
        f"title is {title!r}; an agent-attributed message must not contribute",
    )


@check("omp-import-badge-safe")
def _(fx, ctx):
    origin = "0123456789abcdef-full-origin-identifier"
    secret_cwd = "/private/import/source/path"
    _omp(fx, ".pi/agent", _omp_key(fx, fx.workspace), "imported", [
        _omp_header("imported-id", fx.workspace, "carried over"),
        {"type": "custom", "foreign_session_import": {
            "source_kind": "claude", "origin_id": origin,
            "origin_cwd": secret_cwd}}])
    _, sessions = _omp_ids(fx)
    title = sessions.get("imported-id", {}).get("title") or ""
    return expect(
        "imported from claude" in title and "origin:01234567" in title
        and origin not in title and secret_cwd not in title,
        f"badge title is {title!r}; want the source kind and an 8-character id "
        f"prefix only, never the full id or the origin cwd",
    )


@check("omp-header-cwd-scope-filtering")
def _(fx, ctx):
    """The grouping directory name is a prefilter; the header cwd decides."""
    outside = fx.home / "outside-omp"
    outside.mkdir()
    key = _omp_key(fx, fx.workspace)
    _omp(fx, ".pi/agent", key, "in-scope",
         [_omp_header("omp-in-id", fx.workspace)])
    # Same in-scope directory name, but a header cwd pointing elsewhere.
    _omp(fx, ".pi/agent", key, "lying-key",
         [_omp_header("omp-lying-id", outside)])
    _, sessions = _omp_ids(fx)
    return expect(
        "omp-in-id" in sessions and "omp-lying-id" not in sessions,
        f"scope followed the storage directory rather than the header cwd: "
        f"{set(sessions)}",
    )


@check("omp-home-relative-prefilter")
def _(fx, ctx):
    outside = fx.home / "outside-omp"
    outside.mkdir()
    _omp(fx, ".pi/agent", _omp_key(fx, fx.workspace), "inside",
         [_omp_header("omp-inside-id", fx.workspace)])
    _omp(fx, ".pi/agent", _omp_key(fx, outside), "outside",
         [_omp_header("omp-outside-id", outside)])
    _, sessions = _omp_ids(fx)
    return expect(
        "omp-inside-id" in sessions and "omp-outside-id" not in sessions,
        f"the encoded-directory prefilter kept out-of-scope workspaces: "
        f"{set(sessions)}",
    )


@check("omp-hidden-dot-workspace")
def _(fx, ctx):
    hidden = fx.home / ".hidden/project"
    hidden.mkdir(parents=True)
    _omp(fx, ".pi/agent", _omp_key(fx, hidden), "hidden",
         [_omp_header("hidden-id", hidden)])
    result, sessions = _omp_ids(fx, env=None)
    from_hidden = fx.run_env("--json", "-a", "omp", cwd=hidden)
    ids = {s["id"] for s in json.loads(from_hidden.stdout)["sessions"]}
    return expect(
        "hidden-id" in ids,
        f"a workspace under a hidden dot directory was pruned by the "
        f"lossy-key prefilter: {ids}",
    )


def _cargo_tests(ctx, *names):
    """Run named library tests. Rows whose how_to_test designates a Rust
    regression test are verified by running exactly that test."""
    failed = []
    for name in names:
        result = subprocess.run(
            ["cargo", "test", "--all-features", "--lib", "--quiet", "--",
             "--exact", name],
            cwd=ctx["root"], capture_output=True, text=True)
        if result.returncode != 0 or "1 passed" not in result.stdout:
            failed.append(f"{name}: {result.stdout.strip()[-300:]}")
    return expect(not failed, "designated regression test(s) did not pass: "
                              + " || ".join(failed))


@check("omp-activity-triple-correlation", "omp-active-staleness-gate-m3-fix")
def _(fx, ctx):
    return _cargo_tests(
        ctx,
        "integration::omp::tests::activity::correlate_live_rejects_breadcrumb_older_than_recycled_tty_process",
        "integration::omp::tests::activity::missing_process_start_time_uses_twelve_hour_freshness_fallback",
        "integration::omp::tests::activity::correlate_live_requires_process_tty_breadcrumb_and_existing_transcript",
        "integration::omp::tests::activity::activity_active_only_with_live_process_tty_and_matching_breadcrumb",
    )


@check("omp-breadcrumb-directory-xdg-state-resolution")
def _(fx, ctx):
    """XDG_STATE_HOME is only consulted when the path already exists, so the
    binary must report Unknown either way; the resolution itself is only
    observable through the designated unit tests."""
    xdg = fx.home / "xdg/state/omp/terminal-sessions"
    xdg.mkdir(parents=True)
    _, sessions = _omp_ids(fx)
    activities = {s["activity"] for s in sessions.values()}
    designated = _cargo_tests(
        ctx, *[f"integration::omp::tests::activity::{n}" for n in (
            "breadcrumb_directory_uses_xdg_only_for_native_default_agent_roots",
            "breadcrumb_directory_requires_exact_profile_xdg_path",
        )])
    return designated or expect(
        activities <= {"Unknown"},
        f"an empty breadcrumb directory produced {activities}",
    )


@check("omp-resume-spec-default-profile", "omp-resume-spec-named-profile",
       "omp-resume-env-config-dir-propagated-only-when-overridden")
def _(fx, ctx):
    return "SKIPPED: requires a resume handoff; PTY harness"


# ---------------------------------------------------------------- opencode
@check("opencode-resume-exact-session", "opencode-resume-cwd-is-workspace")
def _(fx, ctx):
    return "SKIPPED: requires a resume handoff; PTY harness"


# ------------------------------------------------------------------- setup
SETUP_PROMPT = r"Selection \(for example 1,3; `all`; or `none`\): "


def _settings(fx):
    path = fx.home / ".resume/settings.json"
    return json.loads(path.read_text()) if path.is_file() else None


def _first_run(fx):
    """Return the fixture to its pre-setup state: no persisted selection."""
    path = fx.home / ".resume/settings.json"
    if path.is_file():
        path.unlink()
    return fx


def _answer_setup(fx, answer, *args):
    """Drive one setup dialogue to completion and return (exit code, screen)."""
    _first_run(fx)
    with Pty(fx, *(args or ("setup",))) as pty:
        pty.expect(SETUP_PROMPT)
        pty.send(f"{answer}\r")
        return pty.wait(), pty.screen


@check("setup-first-run-prompt")
def _(fx, ctx):
    """Bare `resume` takes the picker path, which prompts. `--list`/`--json`
    deliberately refuse instead (setup-required-for-list-json)."""
    _first_run(fx)
    with Pty(fx) as pty:
        screen = pty.expect(SETUP_PROMPT)
        pty.send("none\r")
        pty.wait()
    numbered = re.findall(r"^\s*(\d+)\. (\S+)", screen, re.MULTILINE)
    return expect(
        [a for _, a in numbered] == ctx["agents"]
        and [int(n) for n, _ in numbered] == list(range(1, len(ctx["agents"]) + 1))
        and screen.index("Choose agents to scan:") < screen.index("Selection"),
        f"numbered list {numbered} does not match SUPPORTED_AGENTS "
        f"{ctx['agents']} in order, or did not precede the prompt",
    )


@check("setup-selection-numbers")
def _(fx, ctx):
    _answer_setup(fx, "2, 1, 2")
    saved = _settings(fx)
    want = [ctx["agents"][1], ctx["agents"][0]]
    return expect(
        saved and saved["agents"] == want,
        f"`2, 1, 2` saved {saved and saved['agents']}; want {want} — "
        f"whitespace ignored, repeats deduped, order preserved",
    )


@check("setup-selection-all-and-none")
def _(fx, ctx):
    _answer_setup(fx, "ALL")
    every = _settings(fx)
    code, _ = _answer_setup(fx, "None")
    none = _settings(fx)
    return expect(
        every and every["agents"] == ctx["agents"]
        and none is not None and none["agents"] == [] and code == 0,
        f"`ALL` saved {every and every['agents']} (want {ctx['agents']}); "
        f"`None` saved {none and none['agents']} (want []) with exit {code}",
    )


@check("setup-selection-rejects-invalid")
def _(fx, ctx):
    problems = []
    for answer in ("", "x", "0", "99"):
        code, screen = _answer_setup(fx, answer)
        grammar = all(form in screen for form in
                      ("comma-separated numbers", "`all`", "`none`"))
        written = _settings(fx) is not None
        if code == 0 or not grammar or written:
            problems.append(f"{answer!r}: exit {code}, grammar named {grammar}, "
                            f"settings written {written}")
    return expect(not problems, "; ".join(problems))


@check("setup-no-terminal-error")
def _(fx, ctx):
    code, text = _detached(fx, "setup")
    return expect(
        code != 0 and "no controlling terminal" in text
        and "resume setup" in text and "interactive terminal" in text,
        f"exit {code}, output {text[:250]!r}",
    )


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


# ------------------------------------------------------------------- picker
# Everything below drives the tabbed picker through a real pseudoterminal:
# these rows are unreachable through a pipe, because Skim opens /dev/tty
# itself and the layout only exists once something paints it.

ROW_HEADER = r"^\s*UPDATED\s+AGENT\[PROFILE\]\s+TITLE\s+BRANCH\s*$"
PAGE = r"PAGE (\d+)/(\d+)"

KEYS = {
    "ctrl-o": "\x0f", "ctrl-r": "\x12", "ctrl-c": "\x03", "esc": "\x1b",
    "enter": "\r", "tab": "\t", "shift-tab": "\x1b[Z",
    "left": "\x1b[D", "right": "\x1b[C",
    "alt-left": "\x1b[1;3D", "alt-right": "\x1b[1;3C",
    "alt-p": "\x1bp", "alt-n": "\x1bn",
}


def _picker(fx, *args, cols=120, rows=30, env=None, stdin=None, timeout=25):
    """Open the picker and wait until it has painted its header line."""
    pty = Pty(fx, *args, cols=cols, rows=rows, env=env, stdin=stdin)
    try:
        pty.expect(PAGE, timeout=timeout)
    except BaseException:
        pty.close()
        raise
    return pty


def _key(pty, name, settle=1.0):
    return pty.send(KEYS[name], settle=settle)


def _status(pty):
    """Tab names, active tab, page numbers and hint, read off the header."""
    line = next((l for l in pty.lines if re.search(PAGE, l)), None)
    if line is None:
        return None
    m = re.search(PAGE, line)
    tokens = re.sub(r"\([^)]*\)", "", line[:m.start()]).split()
    return {
        "tabs": [t.strip("[]") for t in tokens],
        "tab": next((t.strip("[]") for t in tokens if t.startswith("[")), None),
        "page": int(m.group(1)),
        "pages": int(m.group(2)),
        "hint": line[m.start():].strip(),
        "line": line.strip(),
    }


def _rows(pty):
    """The candidate rows, which sit above the column header."""
    lines = pty.lines
    end = next((i for i, l in enumerate(lines) if re.match(ROW_HEADER, l)), 0)
    return [l for l in lines[:end] if l.strip()]


def _count(pty):
    """How many candidates the current view holds, from Skim's own counter.

    Reading the rows is not the same thing: a page of 50 cannot fit on a
    30-line terminal, and the counter is what the user sees for the total.
    """
    for line in pty.lines:
        m = re.match(r"^\s*(\d+)/(\d+)\b", line)
        if m:
            return int(m.group(2))
    return None


def _settled(pty, predicate, timeout=20, poll=0.2):
    """Poll the terminal until `predicate` is truthy, then return its value.

    The picker paints its header before the first candidate arrives and grows
    a Codex tab whenever the background scan lands, so anything read straight
    after `expect` can be a half-drawn screen.
    """
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value or time.monotonic() > deadline:
            return value
        pty._drain(poll)


def _stable(pty, read, timeout=20, hold=0.8, poll=0.2):
    """Read until the value has been non-empty and unchanged for `hold`.

    Skim streams a page's candidates in rather than starting with all of
    them, so both the empty screen it opens with and the first counter it
    prints are partial answers, not the total.
    """
    deadline = time.monotonic() + timeout
    value, since = None, time.monotonic()
    while True:
        current = read()
        if current != value:
            value, since = current, time.monotonic()
        elif value and time.monotonic() - since >= hold:
            return value
        if time.monotonic() > deadline:
            return value
        pty._drain(poll)


def _await_tab(pty, name, timeout=30):
    """Wait for a background agent's tab, redrawing until it appears.

    `run_tabbed_picker` re-reads the candidate snapshot once per navigation
    and never mid-render, so a tab that lands behind the user's back only
    shows up on the next redraw. Right-then-Left is the cheapest redraw that
    always works: Alt+N is only bound away from the newest page, and both
    steps reset to page 0, which is where a freshly opened tab already is.
    """
    deadline = time.monotonic() + timeout
    while True:
        if name in (_status(pty) or {"tabs": []})["tabs"]:
            return True
        if time.monotonic() > deadline:
            return False
        _key(pty, "right", settle=0.4)
        _key(pty, "left", settle=0.4)


def _pi_corpus(fx, count, *, prefix="bulk", base=1_700_000_000):
    """`count` pi sessions with strictly increasing modification times, so the
    rank order they must appear in is known independently of the picker.

    The session the shared fixture ships is removed first: it was written just
    now, so it would sort newest and shift every count by one.
    """
    shutil.rmtree(fx.home / ".pi/agent/sessions/ws", ignore_errors=True)
    made = []
    for i in range(count):
        name = f"{prefix}{i:03d}"
        _pi_session(fx, name, f"{name}-id", fx.workspace, title=f"title {i:03d}",
                    timestamp=base + i)
        path = fx.home / ".pi/agent/sessions" / name / f"{name}.jsonl"
        os.utime(path, (base + i, base + i))
        made.append(f"{name}-id")
    return made


@check("cli-default-picker", "picker-tabs-per-agent")
def _(fx, ctx):
    """Bare `resume` opens the picker; tab 0 is All and every agent holding a
    session gets exactly one tab of its own."""
    with _picker(fx) as pty:
        # Codex is discovered in the background, so wait for its tab to land.
        _await_tab(pty, "codex")
        status = _status(pty)
        _key(pty, "ctrl-c")
        pty.wait()
    agents = {s["agent"] for s in json.loads(fx.run("--json").stdout)["sessions"]}
    return expect(
        status["tabs"][0] == "All" and status["tab"] == "All"
        and sorted(status["tabs"][1:]) == sorted(agents),
        f"tabs {status['tabs']} (active {status['tab']}) for agents "
        f"{sorted(agents)}",
    )


@check("picker-preview-hidden-default", "picker-ctrl-o-toggle",
       "picker-preview-dual-section")
def _(fx, ctx):
    """One Ctrl+O sequence settles the default visibility, the toggle, and the
    two sections the production preview always renders."""
    with _picker(fx) as pty:
        before = "\n".join(pty.lines)
        _key(pty, "ctrl-o")
        shown = pty.lines
        _key(pty, "ctrl-o")
        after = "\n".join(pty.lines)
        _key(pty, "ctrl-c")
        pty.wait()
    text = "\n".join(shown)
    return expect(
        "# normalized" not in before and "# normalized" not in after
        and "# normalized (terminal-safe)" in text
        and "# raw (still terminal-safe, unfiltered)" in text,
        f"hidden by default: {'# normalized' not in before}; hidden again: "
        f"{'# normalized' not in after}; sections while shown: "
        f"{[l.strip() for l in shown if '#' in l]}",
    )


@check("picker-preview-position-auto", "picker-preview-toggle")
def _(fx, ctx):
    """Auto puts the pane on the right only at 100 columns or more; below that
    it goes to the bottom, which shows up as the section starting at column 0
    rather than beside the rows."""
    offsets = {}
    for cols in (120, 80):
        with _picker(fx, cols=cols) as pty:
            _key(pty, "ctrl-o")
            line = next((l for l in pty.lines
                         if "# normalized (terminal-safe)" in l), None)
            offsets[cols] = None if line is None else line.index("#")
            _key(pty, "ctrl-c")
            pty.wait()
    return expect(
        offsets[120] is not None and offsets[120] > 0 and offsets[80] == 0,
        f"the preview section began at column {offsets} by terminal width "
        f"(a right pane starts beside the rows, a bottom pane at column 0)",
    )


PREVIEW_MARKER = "# normalized (terminal-safe)"


def _preview_column(fx, cols=120, settle=4.0):
    """Where the preview section begins, or None when no pane is showing.

    A right-hand pane starts beside the rows; a bottom pane starts at column
    0. That offset is the only way to read the layout back off the screen.
    A visible pane squeezes the header, so `PAGE n/m` can be off the right
    edge and cannot be the readiness signal here.
    """
    pty = Pty(fx, cols=cols)
    try:
        pty.expect(r"UPDATED\s+AGENT")
        deadline, line = time.monotonic() + settle, None
        while time.monotonic() < deadline:
            pty._drain(0.3)
            line = next((l for l in pty.lines if PREVIEW_MARKER in l), None)
            if line is not None:
                pty._drain(1.0)   # let the pane finish painting before measuring
                line = next((l for l in pty.lines if PREVIEW_MARKER in l), line)
                break
        return None if line is None else line.index("#")
    finally:
        pty.close()


@check("config-preview-field")
def _(fx, ctx):
    """`preview = "visible"` opens the pane with the picker; with no setting
    the pane stays hidden until Ctrl+O asks for it."""
    config = fx.home / "xdg/config/resume/config.toml"
    default = _preview_column(fx)
    _write_config(config, 'preview = "visible"\n')
    configured = _preview_column(fx)
    _write_config(config, 'preview = "hidden"\n')
    explicit_hidden = _preview_column(fx)
    return expect(
        default is None and explicit_hidden is None and configured is not None,
        f"preview section column with no setting {default}, with "
        f'preview = "visible" {configured}, with preview = "hidden" '
        f"{explicit_hidden} (None means no pane on screen)",
    )


@check("config-preview-position-field")
def _(fx, ctx):
    """The position setting overrides the width heuristic in both
    directions: bottom stays bottom on a wide terminal, right stays right on
    a narrow one."""
    config = fx.home / "xdg/config/resume/config.toml"
    _write_config(config, 'preview = "visible"\npreview_position = "bottom"\n')
    wide_bottom = _preview_column(fx, cols=120)
    _write_config(config, 'preview = "visible"\npreview_position = "right"\n')
    narrow_right = _preview_column(fx, cols=80)
    return expect(
        wide_bottom == 0 and narrow_right is not None and narrow_right > 0,
        f'preview_position = "bottom" on 120 columns began at {wide_bottom} '
        f'(want 0); preview_position = "right" on 80 columns began at '
        f"{narrow_right} (want a column beside the rows)",
    )


@check("picker-ctrl-r-noop", "picker-ctrl-r-ignored")
def _(fx, ctx):
    """Ctrl+R is bound to `ignore`, so it must not reload, exit, or disturb
    the query, tab, or page."""
    with _picker(fx) as pty:
        _key(pty, "tab")
        pty.send("title", settle=1.0)
        before = (_status(pty), _rows(pty), pty.lines[-1])
        _key(pty, "ctrl-r")
        after = (_status(pty), _rows(pty), pty.lines[-1])
        alive = pty.alive()
        _key(pty, "ctrl-c")
        pty.wait()
    return expect(
        alive and before == after,
        f"still running: {alive}; before {before[0]} {before[2]!r}; "
        f"after {after[0]} {after[2]!r}",
    )


@check("picker-esc-cancel")
def _(fx, ctx):
    with _picker(fx) as pty:
        _key(pty, "esc", settle=0.8)
        code = pty.wait()
    return expect(code == 0, f"Esc exited {code}, want 0")


@check("picker-ctrl-c-interrupt", "exitcode-130-interrupt")
def _(fx, ctx):
    with _picker(fx) as pty:
        _key(pty, "ctrl-c", settle=0.8)
        code = pty.wait()
    return expect(code == 130, f"Ctrl+C exited {code}, want 130")


@check("picker-terminal-too-small")
def _(fx, ctx):
    pty = Pty(fx, rows=8, cols=40)
    try:
        code = pty.wait()
        screen = pty.screen
    finally:
        pty.close()
    return expect(
        code == 2 and "terminal too small" in screen and "minimum 60x10" in screen,
        f"exit {code} (want 2), screen {screen.strip()[-200:]!r}",
    )


@check("picker-redirected-stdin-still-works")
def _(fx, ctx):
    """Skim reads keystrokes from /dev/tty, so a redirected stdin must not
    stop the picker opening or responding."""
    with _picker(fx, stdin="/dev/null") as pty:
        rows = _settled(pty, lambda: _rows(pty))
        _key(pty, "tab")
        switched = _status(pty)["tab"]
        _key(pty, "ctrl-c")
        code = pty.wait()
    return expect(
        rows and switched != "All" and code == 130,
        f"rows {len(rows)}, tab after Tab {switched!r}, exit {code}",
    )


@check("picker-tab-switch-keys", "picker-tabbed-view-tabs-and-pages")
def _(fx, ctx):
    """All six switch keys move one tab, wrapping at both ends."""
    with _picker(fx) as pty:
        _await_tab(pty, "codex")
        tabs = _status(pty)["tabs"]
        forward, backward = [], []
        for _ in range(len(tabs)):
            _key(pty, "tab")
            forward.append(_status(pty)["tab"])
        for _ in range(len(tabs)):
            _key(pty, "shift-tab")
            backward.append(_status(pty)["tab"])
        singles = {}
        for name in ("right", "alt-right", "left", "alt-left"):
            was = _status(pty)["tab"]
            _key(pty, name)
            singles[name] = (was, _status(pty)["tab"])
        _key(pty, "ctrl-c")
        pty.wait()
    step = lambda pair, delta: (tabs.index(pair[1])
                                == (tabs.index(pair[0]) + delta) % len(tabs))
    return expect(
        forward == tabs[1:] + tabs[:1]
        and backward == list(reversed(tabs))[:-1] + [tabs[0]]
        and all(step(singles[k], +1) for k in ("right", "alt-right"))
        and all(step(singles[k], -1) for k in ("left", "alt-left")),
        f"tabs {tabs}; Tab cycle {forward}; Shift+Tab cycle {backward}; "
        f"single steps {singles}",
    )


@check("picker-paging-keys", "picker-newest-page-is-full")
def _(fx, ctx):
    """Page 1 always holds the newest PAGE_SIZE rows; the short page is the
    oldest one, and both ends clamp rather than wrap."""
    size = int(re.search(r"PAGE_SIZE: usize = (\d+)",
                         (ctx["root"] / "src/picker.rs").read_text()).group(1))
    _pi_corpus(fx, size + 3)
    with _picker(fx, "--agent", "pi") as pty:
        newest = _stable(pty, lambda: _count(pty))
        first = (_status(pty), newest)
        _key(pty, "alt-n")  # already newest: a no-op redraw
        clamped_newest = _status(pty)["page"]
        _key(pty, "alt-p")
        oldest = _stable(pty, lambda: _count(pty))
        second = (_status(pty), oldest)
        _key(pty, "alt-p")  # already oldest
        clamped_oldest = _status(pty)["page"]
        _key(pty, "ctrl-c")
        pty.wait()
    return expect(
        first[0]["page"] == 1 and first[0]["pages"] == 2 and first[1] == size
        and clamped_newest == 1 and second[0]["page"] == 2 and second[1] == 3
        and clamped_oldest == 2,
        f"page 1 held {first[1]} of {size} candidates ({first[0]['line']!r}); "
        f"page 2 held {second[1]} of 3; Alt+N on the newest page gave page "
        f"{clamped_newest}, Alt+P on the oldest gave page {clamped_oldest}",
    )


@check("picker-page-clamped-on-shrink")
def _(fx, ctx):
    """Paging deep into a large tab and then switching to a single-page tab
    must land on that tab's last page, not past its end."""
    size = int(re.search(r"PAGE_SIZE: usize = (\d+)",
                         (ctx["root"] / "src/picker.rs").read_text()).group(1))
    _pi_corpus(fx, size + 3)
    with _picker(fx) as pty:
        _key(pty, "alt-p")
        deep = _status(pty)
        while _status(pty)["tab"] != "claude":
            _key(pty, "tab")
        small = _status(pty)
        rows = _rows(pty)
        _key(pty, "ctrl-c")
        pty.wait()
    return expect(
        deep["page"] == 2 and small["pages"] == 1 and small["page"] == 1 and rows,
        f"left the All tab on page {deep['page']}/{deep['pages']}, arrived at "
        f"{small['tab']} page {small['page']}/{small['pages']} showing "
        f"{len(rows)} rows",
    )


@check("picker-rank-ordering")
def _(fx, ctx):
    """Rows ascend by rank, so the most recently active session is the bottom
    row of the newest page, and the order is stable across runs."""
    _pi_corpus(fx, 6)
    orders = []
    for _ in range(2):
        with _picker(fx, "--agent", "pi") as pty:
            rows = _settled(pty, lambda: _rows(pty) if len(_rows(pty)) == 6 else None)
            orders.append([l.split()[-2] for l in rows or []])
            _key(pty, "ctrl-c")
            pty.wait()
    listed = [l.split()[-2] for l in fx.run("--list", "--agent", "pi").stdout.splitlines()
              if l.strip()]
    return expect(
        orders[0] == orders[1] and orders[0] == listed[::-1]
        and orders[0][-1] == "005",
        f"picker order {orders[0]}; rerun {orders[1]}; --list order {listed} "
        f"(the picker ascends, so it is --list reversed and the newest "
        f"fixture session, 'title 005', is the bottom row)",
    )


@check("picker-tabbed-view-waits-for-discovery", "codex-background-discovery",
       "codex-progress-after-picker", "codex-results-merge-on-navigation")
def _(fx, ctx):
    """The picker opens on the synchronous agents while Codex is still
    scanning, says so in its header, merges Codex's rows when they land, and
    only prints Codex's progress line once the screen is its own again."""
    for i in range(300):
        _rollout(fx, f"slow-{i:03d}", fx.workspace, day=f"2026/03/{i % 28 + 1:02d}")
    with _picker(fx) as pty:
        opening = _status(pty)
        _await_tab(pty, "codex", timeout=60)
        landed = _status(pty)
        while _status(pty)["tab"] != "codex":
            _key(pty, "tab")
        merged = len(_rows(pty))
        during = pty.screen
        _key(pty, "esc", settle=0.8)
        pty.wait()
        after = pty.screen
    return expect(
        "still scanning" in opening["line"] and "codex" not in opening["tabs"]
        and "codex" in landed["tabs"] and "still scanning" not in landed["line"]
        and merged > 0
        and "codex scanned" not in during and "codex scanned" in after,
        f"header on open {opening['line']!r}; after Codex landed "
        f"{landed['line']!r}; codex tab showed {merged} rows; progress line "
        f"printed before the picker closed: {'codex scanned' in during}",
    )


@check("picker-background-wait-message", "codex-sole-agent-synchronous")
def _(fx, ctx):
    """With Codex the only configured agent there is nothing to show while it
    scans, so it is discovered synchronously and no waiting line is printed."""
    with _picker(fx, "--agent", "codex") as pty:
        opened = pty.screen
        status = _status(pty)
        _key(pty, "ctrl-c")
        pty.wait()
    return expect(
        "waiting for codex" not in opened and status["tabs"] == ["All", "codex"]
        and "still scanning" not in status["line"],
        f"screen {opened.strip()[:200]!r}; header {status['line']!r}",
    )


@check("picker-tab-tracked-by-name")
def _(fx, ctx):
    """A tab arriving behind the user's back must not shift the selection: the
    current tab is tracked by agent name, not by index."""
    for i in range(300):
        _rollout(fx, f"slow-{i:03d}", fx.workspace, day=f"2026/03/{i % 28 + 1:02d}")
    with _picker(fx) as pty:
        before = _status(pty)
        _key(pty, "tab")
        chosen = _status(pty)["tab"]
        _await_tab(pty, "codex", timeout=60)
        _key(pty, "ctrl-o")   # a redraw that is not a tab switch
        after = _status(pty)
        _key(pty, "ctrl-c")
        pty.wait()
    return expect(
        "codex" not in before["tabs"] and "codex" in after["tabs"]
        and after["tab"] == chosen,
        f"selected {chosen!r} before Codex landed; tabs went {before['tabs']} "
        f"-> {after['tabs']} and the selection ended on {after['tab']!r}",
    )


LEADING = 10 + 1 + 18 + 1  # the updated and agent columns, plus their spaces


def _title_width(text, marker="unknown    pi "):
    """Recover the title column width from a rendered row: the branch column
    starts one space after it, at a fixed offset from the row's start."""
    start = text.index(marker)
    return text.index("no-branch", start) - start - LEADING - 1


@check("picker-row-format")
def _(fx, ctx):
    """Four columns, with the branch always starting at the same place, and
    the picker header naming them."""
    with _picker(fx) as pty:
        header = [l.strip() for l in pty.lines if re.match(ROW_HEADER, l)]
        _key(pty, "ctrl-c")
        pty.wait()
    _, text = _detached(fx, "--list")
    listed = [l for l in text.splitlines() if "no-branch" in l]
    starts = {l.index("no-branch") for l in listed}
    shapes = {(l[:10].strip() != "", l[10] == " ", l[LEADING - 1] == " ") for l in listed}
    return expect(
        header == ["UPDATED  AGENT[PROFILE]  TITLE  BRANCH"]
        and len(starts) == 1 and shapes == {(True, True, True)},
        f"picker header {header}; branch column {starts} (want one shared "
        f"offset across {len(listed)} rows); column shapes {shapes}",
    )


@check("picker-title-column-width-adaptive")
def _(fx, ctx):
    """With no terminal to measure, the title column is the documented
    default; with one, it follows the width, clamped at both ends."""
    _, piped = _detached(fx, "--list")
    widths = {}
    for cols in (200, 120, 70):
        pty = Pty(fx, "--list", cols=cols)
        try:
            pty.wait()
            widths[cols] = _title_width(pty.wrapped)
        finally:
            pty.close()
    return expect(
        _title_width(piped) == 48 and widths == {200: 60, 120: 60, 70: 40},
        f"detached title width {_title_width(piped)} (want the documented 48); "
        f"by terminal width {widths} (want 60, 60, 40: clamped at "
        f"TITLE_WIDTH_MAX twice, then width-derived)",
    )


@check("picker-row-branch-placeholder")
def _(fx, ctx):
    """A git workspace shows its branch; anything else shows `no-branch`."""
    if GIT is None:
        return "SKIPPED: git is not installed"
    # Both live under the workspace, so `-D all` brings them into Scope.
    repo = fx.workspace / "on-a-branch"
    repo.mkdir()
    _init_repo(repo)
    _git(repo, "checkout", "-q", "-b", "qa-branch")
    _pi_session(fx, "branched", "branched-id", repo, title="lives-in-a-repo")
    plain = fx.workspace / "not-a-repo"
    plain.mkdir()
    _pi_session(fx, "plain", "plain-id", plain, title="lives-anywhere-else")
    result = fx.run_env("--list", "--agent", "pi", "-D", "all", env=_with_git(fx))
    listed = [l.split() for l in result.stdout.splitlines() if l.strip()]
    branches = {row[-2]: row[-1] for row in listed}
    return expect(
        branches.get("lives-in-a-repo") == "qa-branch"
        and branches.get("lives-anywhere-else") == "no-branch",
        f"title -> branch column: {branches}",
    )


@check("picker-selection-identity-stable")
def _(fx, ctx):
    """Filtering reorders what is on screen; Enter must still resume the row
    that was highlighted, identified by an opaque key rather than a position."""
    made = _pi_corpus(fx, 6)
    with _picker(fx, "--agent", "pi") as pty:
        pty.send("003", settle=1.0)
        rows = _rows(pty)
        _key(pty, "enter", settle=2.0)
        pty.wait()
    launched = fx.cmux_log()
    # pi resumes by transcript path, so the launched argv carries the session
    # directory name rather than the native id.
    return expect(
        len(rows) == 1 and "title 003" in rows[0]
        and any("bulk003/bulk003.jsonl" in line for line in launched),
        f"filtered rows {rows}; the fake agent was invoked as {launched}",
    )


# ------------------------------------------------------------------- launch
CONFIRM = r"Continue\? \[y/N\]"


def _resume(fx, query=None, *args, answer=None, before_enter=None, env=None,
            timeout=25, wait=20):
    """Drive one resume through the picker and return (exit code, screen).

    There is no non-interactive resume path, so every launch row goes this
    way: filter to a single row, press Enter, and answer the confirmation
    prompt if one is printed. `before_enter` runs with the row already on
    screen, which is where a revalidation race has to be staged.
    """
    with _picker(fx, *args, env=env, timeout=timeout) as pty:
        if query:
            pty.send(query, settle=1.0)
        _settled(pty, lambda: _rows(pty))
        if before_enter is not None:
            before_enter(pty)
        _key(pty, "enter", settle=1.0)
        if answer is not None:
            pty.expect(CONFIRM, timeout=15)
            pty.send(answer + "\r", settle=0.5)
        code = pty.wait(timeout=wait)
    return code, pty.screen


def _launched(fx):
    """(argv, cwd) for every fake agent the fixture executed, in order."""
    calls = []
    for line in fx.cmux_log():
        if line.startswith("agent "):
            calls.append([line[len("agent "):].strip(), None])
        elif line.startswith("pwd ") and calls:
            calls[-1][1] = line[len("pwd "):].strip()
    return [tuple(call) for call in calls]


def _real(path):
    """The path as the launched process reports it: `pwd` and cmux both
    resolve symlinks, and a fixture HOME under /var is one on macOS."""
    return str(pathlib.Path(path).resolve())


def _dump_env(fx, agent):
    """Reinstall a fake agent so it also writes its environment to a file.

    The fixture PATH holds nothing but the fake agents, so `env` has to be
    put there too -- reading the child's own environment is the only way to
    see what `exec` actually applied.
    """
    shutil.copy(shutil.which("env") or "/usr/bin/env", fx.home / "bin/env")
    path = fx.home / "bin" / agent
    dump = fx.home / f"{agent}.env"
    path.write_text(
        "#!/bin/sh\n"
        f'printf "agent {agent} %s\\n" "$*" >>"{fx.home}/cmux.log"\n'
        f'printf "pwd %s\\n" "$(pwd)" >>"{fx.home}/cmux.log"\n'
        f'env >"{dump}"\nexit 0\n'
    )
    path.chmod(0o755)
    return dump


def _read_env(dump):
    if not dump.is_file():
        return None
    return dict(line.split("=", 1) for line in dump.read_text().splitlines()
                if "=" in line)


@check("pi-resume-spec-exact", "launch-exec-native-argv")
def _(fx, ctx):
    """Resume execs the agent with exactly the documented argv, in the
    Session's recorded workspace -- and `exec` really does replace the
    process image, so the agent inherits resume's own PID rather than
    running as its child."""
    pid_file = fx.home / "pi.pid"
    agent = fx.home / "bin/pi"
    agent.write_text(
        "#!/bin/sh\n"
        f'printf "agent pi %s\\n" "$*" >>"{fx.home}/cmux.log"\n'
        f'printf "pwd %s\\n" "$(pwd)" >>"{fx.home}/cmux.log"\n'
        f'printf "%s" "$$" >"{pid_file}"\nexit 0\n')
    agent.chmod(0o755)
    transcript = fx.home / ".pi/agent/sessions/ws/pi.jsonl"
    with _picker(fx, "--agent", "pi") as pty:
        _settled(pty, lambda: _rows(pty))
        _key(pty, "enter", settle=1.0)
        code = pty.wait()
        resume_pid = pty.pid
    launched_pid = int(pid_file.read_text()) if pid_file.is_file() else None
    return expect(
        _launched(fx) == [(f"pi --session {transcript}", _real(fx.workspace))]
        and launched_pid == resume_pid,
        f"exit {code}; launched {_launched(fx)}; want "
        f"pi --session {transcript} in {_real(fx.workspace)}; agent pid "
        f"{launched_pid} vs resume pid {resume_pid}",
    )


@check("claude-resume-spec-exact", "claude-resume-env-preservation",
       "launch-exec-env-preservation")
def _(fx, ctx):
    """Claude resumes by id and carries its nondefault root along: every
    `spec.env` pair is applied to the process `exec` replaces."""
    dump = _dump_env(fx, "claude")
    uuid = "11111111-1111-1111-1111-111111111111"
    code, screen = _resume(fx, None, "--agent", "claude")
    child = _read_env(dump) or {}
    return expect(
        _launched(fx) == [(f"claude --resume {uuid}", _real(fx.workspace))]
        and child.get("CLAUDE_CONFIG_DIR") == str(fx.home / ".claude"),
        f"exit {code}; launched {_launched(fx)}; child CLAUDE_CONFIG_DIR "
        f"{child.get('CLAUDE_CONFIG_DIR')!r}; tail {screen[-200:]!r}",
    )


@check("codex-resume-spec-exact")
def _(fx, ctx):
    """Codex resumes as `-C <workspace> resume <id>`."""
    code, screen = _resume(fx, None, "--agent", "codex")
    want = f"codex -C {_real(fx.workspace)} resume codex-id"
    return expect(
        _launched(fx) == [(want, _real(fx.workspace))],
        f"exit {code}; launched {_launched(fx)}; want {want!r}; "
        f"tail {screen[-200:]!r}",
    )


@check("codex-resume-env-preservation-provenance-based")
def _(fx, ctx):
    """The override is the root discovery actually walked, canonicalized when
    it was walked -- not a re-read of CODEX_HOME. Reaching a root through a
    symlink makes the two answers differ, which is the only way to tell them
    apart from outside: the parent's variable says `custom-link`, and only a
    provenance-derived override can name the directory behind it.

    The row's own recipe -- change CODEX_HOME between selection and resume --
    is not runnable end to end: discovery and resume are one process, and
    nothing outside it can rewrite that process's environment.
    """
    dump = _dump_env(fx, "codex")
    custom = fx.home / "custom-codex"
    _rollout(fx, "custom-id", fx.workspace, root="custom-codex")
    custom_link = fx.home / "custom-link"
    custom_link.symlink_to(custom)
    _resume(fx, None, "--agent", "codex", env=fx.env(CODEX_HOME=str(custom_link)))
    nondefault = (_read_env(dump) or {}).get("CODEX_HOME")

    # The same indirection onto the default root: recognised as default, so
    # the child keeps the parent's string untouched rather than gaining one.
    default_link = fx.home / "codex-link"
    default_link.symlink_to(fx.home / ".codex")
    _resume(fx, None, "--agent", "codex",
            env=fx.env(CODEX_HOME=str(default_link)))
    via_link = (_read_env(dump) or {}).get("CODEX_HOME")

    _resume(fx, None, "--agent", "codex", env=fx.env(CODEX_HOME=None))
    unset = (_read_env(dump) or {}).get("CODEX_HOME")
    return expect(
        nondefault == _real(custom) and via_link == str(default_link)
        and unset is None,
        f"nondefault root via {custom_link} gave child CODEX_HOME "
        f"{nondefault!r} (want {_real(custom)}); default root via "
        f"{default_link} gave {via_link!r} (want the parent's own string); "
        f"unset gave {unset!r}",
    )


def _row(ctx, feature_id):
    with open(ctx["root"] / "docs/qa/feature-inventory.csv", newline="") as f:
        for row in csv.DictReader(f):
            if row["feature_id"] == feature_id:
                return row
    return {}


@check("doccheck-omp-vs-claude-codex-env-propagation-asymmetry")
def _(fx, ctx):
    """The asymmetry this row was opened for is closed: all three
    integrations propagate their root variable only for a nondefault root,
    so an unset variable must reach the child unset in every case."""
    dumps = {a: _dump_env(fx, a) for a in ("claude", "codex", "omp")}
    seen = {}
    for agent, var in (("claude", "CLAUDE_CONFIG_DIR"), ("codex", "CODEX_HOME"),
                       ("omp", "PI_CONFIG_DIR")):
        _resume(fx, None, "--agent", agent, env=fx.env(**{var: None}))
        seen[var] = (_read_env(dumps[agent]) or {}).get(var)
    stale = "PI_CONFIG_DIR is present in its environment" in _row(
        ctx, "doccheck-omp-vs-claude-codex-env-propagation-asymmetry")["how_to_test"]
    return expect(
        not any(seen.values()) and not stale,
        f"child environments with the variable unset: {seen}"
        + ("; this row's how_to_test still describes the pre-fix behaviour "
           "(PI_CONFIG_DIR present for a never-set default root), which "
           "contradicts its own expected_behaviour" if stale else ""),
    )


@check("omp-resume-spec-default-profile", "omp-resume-spec-named-profile",
       "omp-resume-env-config-dir-propagated-only-when-overridden")
def _(fx, ctx):
    """OMP names its profile on the command line only when one is selected,
    and carries PI_CONFIG_DIR only when that variable was set."""
    dump = _dump_env(fx, "omp")
    # Distinct, non-overlapping titles: Skim filters fuzzily, so two titles
    # that share letters in order would both survive the query.
    _pi_session(fx, "zebra", "zebra-id", fx.workspace, title="zebra")
    _omp(fx, ".omp/profiles/work/agent", _omp_key(fx, fx.workspace), "work",
         [_omp_header("wombat-id", fx.workspace, title="wombat")])
    # The unprofiled root follows PI_CODING_AGENT_DIR when it is set, so the
    # default-profile Sessions here are the pi fixture's -- see the caveat at
    # the top of fixtures.sh.
    code, screen = _resume(fx, "zebra", "--agent", "omp", "-U", "all")
    default_calls = _launched(fx)
    (fx.home / "cmux.log").unlink()
    _resume(fx, "wombat", "--agent", "omp", "-U", "all")
    named_calls = _launched(fx)
    # No PI_CONFIG_DIR at all: the plain ~/.omp default was used, so nothing
    # may be injected. With the variable set, injection and plain inheritance
    # produce the same string, so only the unset direction is observable.
    (fx.home / "cmux.log").unlink()
    _resume(fx, "zebra", "--agent", "omp", "-U", "all",
            env=fx.env(PI_CONFIG_DIR=None))
    unset = _read_env(dump) or {}
    failed = _cargo_tests(
        ctx,
        "integration::omp::tests::resume::resume_spec_omits_default_config_root_env",
        "integration::omp::tests::resume::resume_spec_preserves_explicit_config_root_env",
    )
    return expect(
        default_calls == [("omp --resume zebra-id", _real(fx.workspace))]
        and named_calls == [("omp --profile work --resume wombat-id",
                             _real(fx.workspace))]
        and "PI_CONFIG_DIR" not in unset and failed is None,
        f"exit {code}; default profile {default_calls}; named profile "
        f"{named_calls}; PI_CONFIG_DIR with the variable unset="
        f"{unset.get('PI_CONFIG_DIR')!r}; designated tests: {failed}; "
        f"tail {screen[-200:]!r}",
    )


@check("opencode-resume-exact-session", "opencode-resume-cwd-is-workspace")
def _(fx, ctx):
    """OpenCode resumes by id, in the directory the database recorded --
    which is not the directory the picker was started from."""
    if not ctx["opencode_feature"]:
        return "SKIPPED: this binary was built without the opencode feature"
    elsewhere = fx.workspace / "elsewhere"
    elsewhere.mkdir()
    db = fx.home / "xdg/data/opencode/opencode.db"
    subprocess.run(["sqlite3", str(db),
                    "insert into session values "
                    f"('other-id', '{elsewhere}', 'other title', 1600000000000);"],
                   check=True, capture_output=True)
    code, screen = _resume(fx, "other title", "--agent", "opencode", "-D", "all")
    return expect(
        _launched(fx) == [("opencode --session other-id", _real(elsewhere))],
        f"exit {code}; launched {_launched(fx)}; want opencode --session "
        f"other-id in {_real(elsewhere)}; tail {screen[-200:]!r}",
    )


@check("launch-confirm-prompt-format", "launch-confirm-refusal-exit0",
       "cli-confirm-always-flag", "launch-confirm-always-forces-prompt")
def _(fx, ctx):
    """--confirm-always turns a no-risk Session into a prompted one; the
    prompt refuses by default and only `y`/`yes` proceeds."""
    code, screen = _resume(fx, None, "--agent", "pi", "--confirm-always",
                           answer="")
    refused = (code, _launched(fx))
    lines = [l for l in screen.splitlines() if l.strip()][-3:]
    (fx.home / "cmux.log").write_text("")
    accepted_code, _ = _resume(fx, None, "--agent", "pi", "--confirm-always",
                               answer="y")
    return expect(
        refused == (0, [])
        and lines[0] == f'Resume "pi-id" in {fx.workspace}?'
        and lines[1] == "Risk: confirmation requested"
        and lines[2].startswith("Continue? [y/N]")
        and accepted_code == 0 and len(_launched(fx)) == 1,
        f"pressing Enter gave exit {refused[0]} and launched {refused[1]}; "
        f"prompt {lines}; answering y gave exit {accepted_code} and launched "
        f"{_launched(fx)}",
    )


@check("config-confirm-always-field")
def _(fx, ctx):
    """The same prompt is reachable from config.toml, with no flag."""
    _write_config(fx.home / "xdg/config/resume/config.toml",
                  "confirm_always = true\n")
    code, screen = _resume(fx, None, "--agent", "pi", answer="y")
    return expect(
        "Risk: confirmation requested" in screen and code == 0
        and len(_launched(fx)) == 1,
        f"exit {code}; launched {_launched(fx)}; tail {screen[-300:]!r}",
    )


@check("launch-risk-broad-workspace", "cli-no-confirm-flag")
def _(fx, ctx):
    """A Session whose workspace is $HOME is risky, and risk confirmation is
    mandatory: --no-confirm suppresses ordinary prompts, never a risk one."""
    _pi_session(fx, "broad", "broad-id", fx.home, title="broad title")
    code, screen = _resume(fx, "broad", "--agent", "pi", "--no-confirm",
                           "-U", "all", answer="y")
    prompted = "Risk: Workspace is broad" in screen
    (fx.home / "cmux.log").write_text("")
    quiet_code, quiet = _resume(fx, "pi title", "--agent", "pi", "--no-confirm",
                                "-U", "all")
    return expect(
        prompted and code == 0 and len(_launched(fx)) == 1
        and "Continue?" not in quiet and quiet_code == 0,
        f"broad workspace: exit {code}, prompt seen {prompted}; normal-risk "
        f"session under --no-confirm: exit {quiet_code}, prompted "
        f"{'Continue?' in quiet}; tail {screen[-300:]!r}",
    )


@check("launch-risk-workspace-changed", "launch-risk-conflicting-metadata")
def _(fx, ctx):
    """Two RiskStatus values exist and are rendered, but no discovery path
    constructs either: they are reachable only from test code."""
    sources = [p for p in (ctx["root"] / "src").rglob("*.rs")]
    built = {"WorkspaceChanged": [], "ConflictingMetadata": []}
    for path in sources:
        text = path.read_text()
        # Ignore the enum itself, the renderer, and every #[cfg(test)] block's
        # file: what matters is a *discovery* path assigning the value.
        if path.name in ("session.rs", "launch.rs") or "/tests/" in str(path):
            continue
        for name in built:
            if f"RiskStatus::{name}" in text:
                built[name].append(str(path.relative_to(ctx["root"])))
    rendered = (ctx["root"] / "src/launch.rs").read_text()
    return expect(
        not built["WorkspaceChanged"] and not built["ConflictingMetadata"]
        and '"Workspace changed"' in rendered and '"metadata conflicts"' in rendered,
        f"discovery paths constructing these risks: {built}",
    )


@check("launch-revalidate-cli-unavailable", "launch-revalidate-before-confirm")
def _(fx, ctx):
    """Revalidation runs before the confirmation prompt, so a CLI that
    disappears while the picker is open aborts without ever asking."""
    def remove_cli(pty):
        (fx.home / "bin/pi").unlink()

    code, screen = _resume(fx, None, "--agent", "pi", "--confirm-always",
                           before_enter=remove_cli)
    return expect(
        code == 1 and "agent CLI is no longer available" in screen
        and "Continue?" not in screen and not _launched(fx),
        f"exit {code}; launched {_launched(fx)}; tail {screen[-300:]!r}",
    )


@check("launch-revalidate-transcript-changed")
def _(fx, ctx):
    """The transcript's identity is captured at selection time and rechecked
    before exec."""
    transcript = fx.home / ".pi/agent/sessions/ws/pi.jsonl"

    def rewrite(pty):
        transcript.write_text(transcript.read_text() + json.dumps(
            {"type": "message", "message": {"role": "user", "content": "more"}}) + "\n")

    code, screen = _resume(fx, None, "--agent", "pi", before_enter=rewrite)
    return expect(
        code == 1 and "transcript identity changed after selection" in screen
        and not _launched(fx),
        f"exit {code}; launched {_launched(fx)}; tail {screen[-300:]!r}",
    )


@check("launch-revalidate-workspace-unavailable")
def _(fx, ctx):
    """A workspace that disappears between selection and exec aborts."""
    workspace = fx.workspace / "doomed"
    workspace.mkdir()
    _pi_session(fx, "doomed", "doomed-id", workspace, title="doomed title")
    code, screen = _resume(fx, "doomed", "--agent", "pi", "-D", "all",
                           before_enter=lambda pty: workspace.rmdir())
    return expect(
        code == 1 and "Workspace is no longer available" in screen
        and not _launched(fx),
        f"exit {code}; launched {_launched(fx)}; tail {screen[-300:]!r}",
    )


@check("launch-revalidate-workspace-changed")
def _(fx, ctx):
    """Same path, new inode: the workspace was replaced, not kept."""
    workspace = fx.workspace / "swapped"
    workspace.mkdir()
    _pi_session(fx, "swapped", "swapped-id", workspace, title="swapped title")

    def replace(pty):
        workspace.rmdir()
        workspace.mkdir()

    code, screen = _resume(fx, "swapped", "--agent", "pi", "-D", "all",
                           before_enter=replace)
    return expect(
        code == 1 and "Workspace was replaced after selection" in screen
        and not _launched(fx),
        f"exit {code}; launched {_launched(fx)}; tail {screen[-300:]!r}",
    )


@check("launch-revalidate-unsupported",
       "errors-unified-catalog-e3003-unsupported-resume")
def _(fx, ctx):
    """A DiscoverOnly Session is selectable but not resumable: the E3003
    block prints and exit is 2, without reaching exec."""
    other = "22222222-2222-2222-2222-222222222222"
    _claude(fx, "ws", f"{other}.jsonl", [{
        "type": "user", "sessionId": "33333333-3333-3333-3333-333333333333",
        "cwd": str(fx.workspace), "message": {"content": "disagreeing title"}}])
    code, screen = _resume(fx, "disagreeing", "--agent", "claude")
    return expect(
        code == 2 and "ERROR [E3003]" in screen
        and "selected Session is unavailable" in screen and not _launched(fx),
        f"exit {code}; launched {_launched(fx)}; tail {screen[-400:]!r}",
    )


@check("claude-missing-workspace-blocks-resume-spec")
def _(fx, ctx):
    """A Claude transcript with no cwd cannot produce a ResumeSpec. The
    Session is still listed -- hiding it would be a silent loss -- but as
    Unavailable with no workspace, and selecting it is refused."""
    uuid = "44444444-4444-4444-4444-444444444444"
    _claude(fx, "ws", f"{uuid}.jsonl",
            [{"type": "user", "sessionId": uuid, "message": {"content": "no cwd"}}])
    result = fx.run("--json", "--agent", "claude")
    payload = json.loads(result.stdout)
    found = next((s for s in payload["sessions"] if s["id"] == uuid), None)
    listed = fx.run("--list", "--agent", "claude")
    code, screen = _resume(fx, "no cwd", "--agent", "claude")
    return expect(
        found and found["workspace"] is None and found["support"] == "Unavailable"
        and any(e["category"] == "claude_missing_workspace"
                for e in payload["errors"])
        and "claude_missing_workspace" in listed.stderr
        and code == 2 and "ERROR [E3003]" in screen and not _launched(fx),
        f"session {found}; errors {[e['category'] for e in payload['errors']]}; "
        f"--list stderr {listed.stderr.strip()!r}; resume exit {code}; "
        f"launched {_launched(fx)}; tail {screen[-300:]!r}",
    )


@contextlib.contextmanager
def _live_codex(fx):
    """Hold the fixture's rollout open from a process the probe will match,
    and yield the environment that lets the probe run.

    `lsof -c codex` matches the kernel's command name, which comes from the
    executable file rather than argv, so the holder has to be a copy under a
    codex* name; `tail -f` is the shortest thing that only holds a file open.
    lsof itself is looked up on PATH, and the fixture's PATH holds only what
    is put there.
    """
    shutil.copy(shutil.which("lsof"), fx.home / "bin/lsof")
    holder = fx.home / "bin/codex-hold"
    shutil.copy(shutil.which("tail") or "/usr/bin/tail", holder)
    rollout = fx.home / ".codex/sessions/2026/01/01/rollout-test.jsonl"
    live = subprocess.Popen([str(holder), "-f", str(rollout)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        yield fx.env(RESUME_DISABLE_PROC_PROBE=None)
    finally:
        live.kill()
        live.wait()


def _active_codex(fx, env):
    """The held Session as --json describes it, or None if lsof saw nothing."""
    payload = json.loads(fx.run_env("--json", "--agent", "codex", env=env).stdout)
    found = next((s for s in payload["sessions"] if s["id"] == "codex-id"), None)
    return found if found and str(found["activity"]).startswith("Active") else None


@check("launch-risk-active", "codex-active-resume-risk")
def _(fx, ctx):
    """A Codex rollout held open by a live `codex*` process is Active, which
    is a risk: the prompt fires even under --no-confirm."""
    if shutil.which("lsof") is None:
        return "SKIPPED: the activity probe needs lsof, which is not installed"
    with _live_codex(fx) as env:
        if _active_codex(fx, env) is None:
            return ("SKIPPED: lsof did not report the held rollout; the probe "
                    "cannot be exercised in this environment")
        code, screen = _resume(fx, None, "--agent", "codex", "--no-confirm",
                               env=env, answer="y")
    return expect(
        "Risk: Session is Active" in screen and code == 0
        and len(_launched(fx)) == 1,
        f"exit {code}; launched {_launched(fx)}; tail {screen[-300:]!r}",
    )


@check("session-status-supported-active")
def _(fx, ctx):
    """A Supported+Active Session carries no inline status text in --list;
    --json is where the two fields are readable."""
    if shutil.which("lsof") is None:
        return "SKIPPED: the activity probe needs lsof, which is not installed"
    with _live_codex(fx) as env:
        found = _active_codex(fx, env)
        if found is None:
            return ("SKIPPED: lsof did not report the held rollout; the probe "
                    "cannot be exercised in this environment")
        listed = fx.run_env("--list", "--agent", "codex", env=env)
    row = listed.stdout.strip()
    return expect(
        found["support"] == "Supported" and found["activity"] == "Active"
        and "status_label" not in (ctx["root"] / "src/app.rs").read_text()
        and row.endswith("no-branch") and "Active" not in row,
        f"--json activity {found['activity']!r} (want the bare label "
        f'"Active"); support {found["support"]!r}; --list row {row!r}',
    )


@check("exitcode-130-interrupt")
def _(fx, ctx):
    """130 is reserved for the interactive interrupt: Esc, a declined
    confirmation and a usage error all use their own codes."""
    with _picker(fx) as pty:
        _key(pty, "ctrl-c")
        interrupted = pty.wait()
    with _picker(fx) as pty:
        _key(pty, "esc")
        cancelled = pty.wait()
    declined, _ = _resume(fx, None, "--agent", "pi", "--confirm-always",
                          answer="n")
    usage = fx.run("--list", "--agent", "nope").returncode
    return expect(
        interrupted == 130 and cancelled != 130 and declined != 130
        and usage != 130,
        f"Ctrl+C {interrupted}, Esc {cancelled}, declined confirmation "
        f"{declined}, usage error {usage}",
    )


@check("picker-no-controlling-terminal")
def _(fx, ctx):
    """Without a terminal the picker cannot run, so it must say so and point
    at the two non-interactive modes."""
    code, output = _detached(fx)
    return expect(
        code == 2 and "controlling terminal" in output
        and "--list" in output and "--json" in output,
        f"exit {code}; output {output.strip()[:300]!r}",
    )


@check("m2-fix-non-verbose-list-prints-diagnostics")
def _(fx, ctx):
    """Diagnostics are not a --verbose feature: the count line prints on a
    plain --list too."""
    _rollout(fx, "corrupt", fx.workspace, name="rollout-corrupt.jsonl")
    (fx.home / ".codex/sessions/2026/01/01/rollout-corrupt.jsonl").write_text(
        "{not json\n")
    result = fx.run("--list", "--agent", "codex")
    return expect(
        re.search(r"codex_invalid_session: \d+", result.stderr),
        f"--list stderr {result.stderr.strip()!r}",
    )


@check("m5-fix-json-errors-aggregated")
def _(fx, ctx):
    """One diagnostic entry per category, carrying the count -- and the same
    count stderr reports for that run."""
    for i in range(3):
        path = _rollout(fx, f"bad-{i}", fx.workspace, name=f"rollout-bad-{i}.jsonl")
        path.write_text("{not json\n")
    result = fx.run("--json", "--agent", "codex")
    payload = json.loads(result.stdout)
    entries = [e for e in payload["errors"] if e["category"] == "codex_invalid_session"]
    stderr_count = re.search(r"codex_invalid_session: (\d+)", result.stderr)
    return expect(
        len(entries) == 1 and entries[0]["count"] == 3
        and stderr_count and int(stderr_count.group(1)) == 3,
        f"json errors {payload['errors']}; stderr {result.stderr.strip()!r}",
    )


# --------------------------------------------------------------------- cmux
CMUX_W, CMUX_S = "11111111-2222-3333-4444-555555555555", "aaaa-bbbb-cccc"


@contextlib.contextmanager
def _cmux_fixture(ctx):
    """A fixture with a scripted `cmux` on PATH and a Session whose workspace
    is a subdirectory, so origin and handoff target genuinely differ."""
    fx = Fixture(ctx["root"], ctx["binary"], QA_FAKE_CMUX=True)
    try:
        fx.target = fx.workspace / "target"
        fx.target.mkdir()
        _pi_session(fx, "handoff", "handoff-id", fx.target, title="handoff title")
        yield fx
    finally:
        shutil.rmtree(fx.home, ignore_errors=True)


def _cmux_reply(fx, name, body, status=None):
    path = fx.home / "cmux-replies" / name
    path.parent.mkdir(exist_ok=True)
    path.write_text(body)
    if status is not None:
        (path.parent / f"{name}.status").write_text(str(status))


def _cmux_list(directory, workspace=CMUX_W):
    return json.dumps({"workspaces": [
        {"id": workspace, "current_directory": _real(directory)}]})


def _cmux_ready(fx, **overrides):
    """Wire the scripted cmux for a handoff that should succeed, and return
    the environment that makes resume attempt one."""
    _cmux_reply(fx, "identify", json.dumps({
        "caller": {"workspace_id": CMUX_W, "surface_id": CMUX_S},
        "app_cli_path": str(fx.home / "bin/cmux")}))
    _cmux_reply(fx, "workspace.1", _cmux_list(fx.workspace))   # pre-state
    _cmux_reply(fx, "workspace.2", _cmux_list(fx.target))      # read-back
    ids = {"CMUX_WORKSPACE_ID": CMUX_W, "CMUX_SURFACE_ID": CMUX_S}
    return fx.env(**{**ids, **overrides})


def _cmux_resume(fx, env):
    code, screen = _resume(fx, "handoff", "--agent", "pi", "-D", "all", env=env)
    return code, screen, fx.cmux_log()


@check("cmux-noop-without-env")
def _(fx, ctx):
    """No cmux identifiers means no handoff at all -- not even a PATH probe."""
    with _cmux_fixture(ctx) as cfx:
        _cmux_ready(cfx)
        code, screen, log = _cmux_resume(cfx, cfx.env())
    return expect(
        code == 0 and not any(l.startswith("cmux ") for l in log)
        and len(_launched_lines(log)) == 1,
        f"exit {code}; cmux log {log}; tail {screen[-200:]!r}",
    )


def _launched_lines(log):
    return [l for l in log if l.startswith("agent ")]


@check("cmux-handoff-precedes-exec")
def _(fx, ctx):
    """Every cmux call happens before the agent is executed -- exec replaces
    the process, so anything after it could never run."""
    with _cmux_fixture(ctx) as cfx:
        env = _cmux_ready(cfx)
        code, screen, log = _cmux_resume(cfx, env)
    verbs = [l for l in log if l.startswith("cmux ")]
    agents = [i for i, l in enumerate(log) if l.startswith("agent ")]
    return expect(
        code == 0 and len(agents) == 1
        and all(i < agents[0] for i, l in enumerate(log) if l.startswith("cmux "))
        and len(verbs) == 4,
        f"exit {code}; log {log}; tail {screen[-300:]!r}",
    )


@check("cmux-incomplete-env-fails-closed")
def _(fx, ctx):
    """One identifier without the other is never enough to infer provenance."""
    with _cmux_fixture(ctx) as cfx:
        env = _cmux_ready(cfx)
        env.pop("CMUX_SURFACE_ID")
        code, screen, log = _cmux_resume(cfx, env)
        empty = _cmux_ready(cfx, CMUX_SURFACE_ID="")
        (cfx.home / "cmux.log").write_text("")
        blank_code, blank_screen, blank_log = _cmux_resume(cfx, empty)
    return expect(
        code == 1 and "incomplete cmux provenance: missing ID" in screen
        and not _launched_lines(log)
        and blank_code == 1 and "incomplete cmux provenance: empty ID" in blank_screen
        and not _launched_lines(blank_log),
        f"missing: exit {code} tail {screen[-200:]!r}; empty: exit "
        f"{blank_code} tail {blank_screen[-200:]!r}",
    )


@check("cmux-cli-unavailable-fails-closed")
def _(fx, ctx):
    """Identifiers present but no cmux binary: refuse, do not resume anyway."""
    with _cmux_fixture(ctx) as cfx:
        env = _cmux_ready(cfx)
        (cfx.home / "bin/cmux").unlink()
        code, screen, log = _cmux_resume(cfx, env)
    return expect(
        code == 1 and "cmux CLI unavailable" in screen
        and not _launched_lines(log),
        f"exit {code}; log {log}; tail {screen[-300:]!r}",
    )


@check("cmux-caller-identity-checked")
def _(fx, ctx):
    """The identify step is not decoration: a caller that disagrees, a
    non-zero status and unparseable output each abort with their own
    message."""
    cases = {}
    for name, body, status in (
        ("mismatch", json.dumps({
            "caller": {"workspace_id": "someone-else", "surface_id": CMUX_S},
            "app_cli_path": "/bin/sh"}), None),
        ("status", "", 3),
        ("json", "not json at all", None),
        ("nopath", json.dumps({
            "caller": {"workspace_id": CMUX_W, "surface_id": CMUX_S},
            "app_cli_path": ""}), None),
    ):
        with _cmux_fixture(ctx) as cfx:
            env = _cmux_ready(cfx)
            _cmux_reply(cfx, "identify", body, status=status)
            code, screen, log = _cmux_resume(cfx, env)
            cases[name] = (code, screen[-300:], bool(_launched_lines(log)))
    return expect(
        cases["mismatch"][0] == 1 and "cmux caller mismatch" in cases["mismatch"][1]
        and cases["status"][0] == 1 and "cmux identify failed" in cases["status"][1]
        and cases["json"][0] == 1
        and "invalid cmux identify response" in cases["json"][1]
        and cases["nopath"][0] == 1
        and "cmux CLI path unavailable" in cases["nopath"][1]
        and not any(launched for _, _, launched in cases.values()),
        f"{cases}",
    )


@check("cmux-workspace-must-be-unique")
def _(fx, ctx):
    """Zero or several matching workspaces is not a workspace to hand off."""
    cases = {}
    for name, body in (
        ("none", json.dumps({"workspaces": []})),
        ("two", json.dumps({"workspaces": [
            {"id": CMUX_W, "current_directory": "/tmp"},
            {"id": CMUX_W, "current_directory": "/var"}]})),
        ("shape", json.dumps({"unexpected": []})),
    ):
        with _cmux_fixture(ctx) as cfx:
            env = _cmux_ready(cfx)
            _cmux_reply(cfx, "workspace.1", body)
            code, screen, log = _cmux_resume(cfx, env)
            cases[name] = (code, screen[-300:], bool(_launched_lines(log)))
    return expect(
        cases["none"][0] == 1
        and "workspace is not unique (0 matches)" in cases["none"][1]
        and cases["two"][0] == 1
        and "workspace is not unique (2 matches)" in cases["two"][1]
        and cases["shape"][0] == 1
        and "invalid cmux workspace list response" in cases["shape"][1]
        and not any(launched for _, _, launched in cases.values()),
        f"{cases}",
    )


@check("cmux-pre-state-checked")
def _(fx, ctx):
    """If the workspace is not where resume was invoked, someone else moved
    it: abort rather than clobber the newer state."""
    with _cmux_fixture(ctx) as cfx:
        env = _cmux_ready(cfx)
        _cmux_reply(cfx, "workspace.1", _cmux_list(cfx.home))
        code, screen, log = _cmux_resume(cfx, env)
    return expect(
        code == 1 and "caller workspace directory mismatch" in screen
        and not _launched_lines(log),
        f"exit {code}; log {log}; tail {screen[-300:]!r}",
    )


@check("cmux-readback-verified")
def _(fx, ctx):
    """The report is verified, not assumed: a read-back that still shows the
    old directory aborts, as does a report that failed outright."""
    with _cmux_fixture(ctx) as cfx:
        env = _cmux_ready(cfx)
        _cmux_reply(cfx, "workspace.2", _cmux_list(cfx.workspace))
        stale_code, stale_screen, stale_log = _cmux_resume(cfx, env)
    with _cmux_fixture(ctx) as cfx:
        env = _cmux_ready(cfx)
        _cmux_reply(cfx, "rpc", "boom", status=4)
        code, screen, log = _cmux_resume(cfx, env)
    return expect(
        stale_code == 1 and "read-back mismatch" in stale_screen
        and not _launched_lines(stale_log)
        and code == 1 and "cmux workspace report failed" in screen
        and not _launched_lines(log),
        f"stale read-back: exit {stale_code} tail {stale_screen[-250:]!r}; "
        f"failed report: exit {code} tail {screen[-250:]!r}",
    )


@check("cmux-target-encoding-checked")
def _(fx, ctx):
    """A target that cannot be resolved aborts. The non-UTF-8 branch is not
    reachable through discovery: every recorded workspace arrives as a JSON
    string, so `spec.cwd` is always valid UTF-8."""
    with _cmux_fixture(ctx) as cfx:
        env = _cmux_ready(cfx)
        # Revalidation has already run by the time the prompt appears, so
        # removing the directory here reaches the handoff's canonicalize.
        _write_config(cfx.home / "xdg/config/resume/config.toml",
                      "confirm_always = true\n")
        with _picker(cfx, "--agent", "pi", "-D", "all", env=env) as pty:
            pty.send("handoff", settle=1.0)
            _settled(pty, lambda: _rows(pty))
            _key(pty, "enter", settle=1.0)
            pty.expect(CONFIRM, timeout=15)
            cfx.target.rmdir()
            pty.send("y\r", settle=0.5)
            code = pty.wait()
        screen, log = pty.screen, cfx.cmux_log()
    non_utf8 = [line for line in
                (ctx["root"] / "src/launch.rs").read_text().splitlines()
                if "NonUtf8Target" in line]
    return expect(
        code == 1 and "target Workspace unavailable" in screen
        and not _launched_lines(log) and len(non_utf8) >= 3,
        f"exit {code}; log {log}; tail {screen[-300:]!r}",
    )


@check("cmux-every-variant-fails-closed")
def _(fx, ctx):
    """`handoff_then_exec` reaches `exec` only on Ok(()), so no variant can
    resume anyway. Confirmed structurally plus by the variants driven above."""
    text = (ctx["root"] / "src/launch.rs").read_text()
    variants = re.search(r"enum CmuxHandoffError \{(.*?)\n\}", text, re.S)
    names = re.findall(r"^\s{4}(\w+)", variants.group(1), re.M)
    ordering = re.search(
        r"fn handoff_then_exec.*?\{(.*?)\n\}", text, re.S).group(1)
    failed = _cargo_tests(
        ctx, "launch::tests::handoff_then_exec_short_circuits_and_orders")
    return expect(
        len(names) == 18
        and ordering.index("Err(error)") < ordering.index("Ok(()) =>")
        and "HandoffThenExecError::Exec(exec(spec))" in ordering
        and failed is None,
        f"{len(names)} variants {names}; handoff_then_exec body "
        f"{ordering.strip()!r}; ordering test: {failed}",
    )


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

    only = [a.split("=", 1)[1] for a in sys.argv[1:] if a.startswith("--only=")]
    results = {}
    for fid, fn in CHECKS.items():
        if only and not any(o in fid for o in only):
            continue
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
        if status == "Pass":
            # A passing row keeps no findings: a note left over from an
            # earlier run would read as an open defect.
            r["error_notes"] = ""
            continue
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
