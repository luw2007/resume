#!/bin/sh
# Builds an isolated $HOME holding one session fixture per integration, matching
# the exact formats `tests/step9_app.rs::fixtures()` uses (the same fixtures the
# automated integration suite exercises). Intended for manual/subagent QA passes
# against a real compiled `resume` binary, never for production use.
#
# Usage:
#   FIXTURE_HOME=$(docs/qa/fixtures.sh)
#   "$FIXTURE_HOME/run" --json
#
# The generated `run` wrapper applies the full environment isolation and cds
# into the fixture workspace, so every QA step is a one-liner rather than a
# twelve-variable incantation that can drift between rows. Override the binary
# with RESUME_BIN; it defaults to ./target/debug/resume resolved at build time.
#
# Prints the fixture HOME path to stdout on success; nothing else.
#
# Knobs (set when invoking this script, not the wrapper):
#   QA_NO_SETTINGS=1   omit ~/.resume/settings.json, so the first-run setup and
#                      `SetupRequired` paths can be exercised
#   QA_FAKE_CMUX=1     put a scriptable fake `cmux` on PATH (see below)
#   QA_NO_OPENCODE=1   omit opencode.db, for the "root unavailable" path
#
# The fake cmux appends every invocation to "$FIXTURE_HOME/cmux.log" and reads
# its canned stdout from "$FIXTURE_HOME/cmux-replies/<subcommand>" (missing file
# means empty stdout, exit 0). Write "<file>.status" next to it to force a
# non-zero exit. The same log receives the fake agents' invocations, so ordering
# between handoff and exec is directly observable in one file.
#
# CAVEAT (confirmed intentional, not a bug — see
# omp-default-profile-agent-root-honors-pi-coding-agent-dir in
# feature-inventory.csv): because both PI_CODING_AGENT_DIR and the .omp
# fixture are set, OMP's *unprofiled default* agent root resolves to
# PI_CODING_AGENT_DIR first (src/integration/omp/roots.rs agent_root,
# Default branch), so the "omp" entry in --json output will mirror the Pi
# fixture's id/title, and .omp/agent/omp.jsonl below is never read via the
# default profile. To exercise OMP's own dedicated data, use a named
# profile (OMP_PROFILE=<name>, sessions under
# "$FIXTURE_HOME/.omp/profiles/<name>/agent/"), which deliberately ignores
# PI_CODING_AGENT_DIR.
set -eu

REPO="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
RESUME_BIN="${RESUME_BIN:-$REPO/target/debug/resume}"

HOME_DIR="$(mktemp -d)"
WS="$HOME_DIR/workspace"
mkdir -p "$WS"

# Fake agent executables so `--list`/`--json` report every integration as
# "Agent unavailable: false" (present on PATH) rather than Unavailable. Each
# logs its own invocation so a resume handoff is observable without exec'ing a
# real agent.
mkdir -p "$HOME_DIR/bin"
for agent in pi claude codex omp opencode; do
  cat >"$HOME_DIR/bin/$agent" <<EOF
#!/bin/sh
printf 'agent %s %s\n' "$agent" "\$*" >>"$HOME_DIR/cmux.log"
printf 'pwd %s\n' "\$(pwd)" >>"$HOME_DIR/cmux.log"
exit 0
EOF
  chmod 755 "$HOME_DIR/bin/$agent"
done

if [ "${QA_FAKE_CMUX:-0}" = 1 ]; then
  mkdir -p "$HOME_DIR/cmux-replies"
  # The wrapper puts only this directory on PATH, so the reply reader below
  # has to bring its own `cat` along.
  cp "$(command -v cat)" "$HOME_DIR/bin/cat"
  # Replies are keyed by verb ($1) and may be numbered -- workspace.1,
  # workspace.2 -- so the two `workspace list` calls that bracket a handoff
  # can answer differently, which is what a read-back check needs. An
  # unnumbered file answers every call to that verb.
  cat >"$HOME_DIR/bin/cmux" <<EOF
#!/bin/sh
printf 'cmux %s\n' "\$*" >>"$HOME_DIR/cmux.log"
dir="$HOME_DIR/cmux-replies"
verb="\${1:-none}"
n=\$(( \$(cat "\$dir/\$verb.count" 2>/dev/null || echo 0) + 1 ))
printf '%s' "\$n" >"\$dir/\$verb.count"
reply="\$dir/\$verb.\$n"
[ -f "\$reply" ] || reply="\$dir/\$verb"
[ -f "\$reply" ] && cat "\$reply"
[ -f "\$reply.status" ] && exit "\$(cat "\$reply.status")"
exit 0
EOF
  chmod 755 "$HOME_DIR/bin/cmux"
fi

for d in config data state cache; do
  mkdir -p "$HOME_DIR/xdg/$d"
done

# Agent selection. Without this file `--list`/`--json` fail with SetupRequired
# (src/settings.rs load_or_require_setup) and the picker would prompt, so the
# default fixture ships one and QA_NO_SETTINGS opts out to test those paths.
if [ "${QA_NO_SETTINGS:-0}" != 1 ]; then
  mkdir -p "$HOME_DIR/.resume"
  cat >"$HOME_DIR/.resume/settings.json" <<'EOF'
{
  "schema_version": 1,
  "agents": ["pi", "claude", "codex", "omp", "opencode"],
  "known_agents": ["pi", "claude", "codex", "omp", "opencode"]
}
EOF
  chmod 600 "$HOME_DIR/.resume/settings.json"
fi

# pi
mkdir -p "$HOME_DIR/.pi/agent/sessions/ws"
cat >"$HOME_DIR/.pi/agent/sessions/ws/pi.jsonl" <<EOF
{"type":"session","version":3,"id":"pi-id","timestamp":1700000000,"cwd":"$WS"}
{"type":"message","message":{"role":"user","content":"pi title"}}
EOF

# claude
CID="11111111-1111-1111-1111-111111111111"
mkdir -p "$HOME_DIR/.claude/projects/ws"
cat >"$HOME_DIR/.claude/projects/ws/$CID.jsonl" <<EOF
{"type":"user","sessionId":"$CID","cwd":"$WS","message":{"content":"claude title"}}
EOF

# codex
mkdir -p "$HOME_DIR/.codex/sessions/2026/01/01"
cat >"$HOME_DIR/.codex/sessions/2026/01/01/rollout-test.jsonl" <<EOF
{"type":"session_meta","payload":{"id":"codex-id","cwd":"$WS","timestamp":"2026-01-01T00:00:00Z"}}
{"type":"event_msg","payload":{"type":"user_message","message":{"role":"user","content":"codex title"}}}
EOF

# omp
mkdir -p "$HOME_DIR/.omp/agent"
cat >"$HOME_DIR/.omp/agent/omp.jsonl" <<EOF
{"type":"title","v":1,"title":"omp title"}
{"type":"session","version":3,"id":"omp-id","timestamp":1700000000,"cwd":"$WS"}
EOF

# opencode: SQLite only, under XDG_DATA_HOME (see integration/opencode/roots.rs).
# Columns match the discovery query exactly; time_updated is Unix milliseconds.
if [ "${QA_NO_OPENCODE:-0}" != 1 ]; then
  mkdir -p "$HOME_DIR/xdg/data/opencode"
  sqlite3 "$HOME_DIR/xdg/data/opencode/opencode.db" <<EOF
create table session (id text primary key, directory text, title text, time_updated integer);
insert into session values ('opencode-id', '$WS', 'opencode title', 1700000000000);
EOF
fi

# One-liner wrapper: full isolation, fixture workspace as cwd, argv forwarded.
cat >"$HOME_DIR/run" <<EOF
#!/bin/sh
set -eu
cd "$WS"
HOME="$HOME_DIR" \\
PATH="$HOME_DIR/bin" \\
TERM="\${TERM:-dumb}" \\
RESUME_DISABLE_PROC_PROBE=1 \\
XDG_CONFIG_HOME="$HOME_DIR/xdg/config" \\
XDG_DATA_HOME="$HOME_DIR/xdg/data" \\
XDG_STATE_HOME="$HOME_DIR/xdg/state" \\
XDG_CACHE_HOME="$HOME_DIR/xdg/cache" \\
PI_CODING_AGENT_DIR="$HOME_DIR/.pi/agent" \\
PI_CONFIG_DIR="$HOME_DIR/.omp" \\
CLAUDE_CONFIG_DIR="$HOME_DIR/.claude" \\
CODEX_HOME="$HOME_DIR/.codex" \\
exec "$RESUME_BIN" "\$@"
EOF
chmod 755 "$HOME_DIR/run"

printf '%s\n' "$HOME_DIR"
