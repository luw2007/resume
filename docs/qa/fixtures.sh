#!/bin/sh
# Builds an isolated $HOME with one pi/claude/codex/omp session fixture each,
# matching the exact format `tests/step9_app.rs::fixtures()` uses (the same
# fixtures the automated integration suite exercises). Intended for manual/
# subagent QA passes against a real compiled `resume` binary, never for
# production use.
#
# Usage:
#   FIXTURE_HOME=$(docs/qa/fixtures.sh)
#   cd "$FIXTURE_HOME/workspace"
#   export HOME="$FIXTURE_HOME" PATH="$FIXTURE_HOME/bin" TERM=dumb \
#     RESUME_DISABLE_PROC_PROBE=1 \
#     XDG_CONFIG_HOME="$FIXTURE_HOME/xdg/config" \
#     XDG_DATA_HOME="$FIXTURE_HOME/xdg/data" \
#     XDG_STATE_HOME="$FIXTURE_HOME/xdg/state" \
#     XDG_CACHE_HOME="$FIXTURE_HOME/xdg/cache" \
#     PI_CODING_AGENT_DIR="$FIXTURE_HOME/.pi/agent" \
#     PI_CONFIG_DIR="$FIXTURE_HOME/.omp" \
#     CLAUDE_CONFIG_DIR="$FIXTURE_HOME/.claude" \
#     CODEX_HOME="$FIXTURE_HOME/.codex"
#   /path/to/target/debug/resume --json
#
# Prints the fixture HOME path to stdout on success; nothing else.
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

HOME_DIR="$(mktemp -d)"
WS="$HOME_DIR/workspace"
mkdir -p "$WS"

# Fake agent executables so `--list`/`--json` report every integration as
# "Agent unavailable: false" (present on PATH) rather than Unavailable.
mkdir -p "$HOME_DIR/bin"
for agent in pi claude codex omp; do
  printf '#!/bin/sh\nexit 0\n' >"$HOME_DIR/bin/$agent"
  chmod 755 "$HOME_DIR/bin/$agent"
done

for d in config data state cache; do
  mkdir -p "$HOME_DIR/xdg/$d"
done

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

printf '%s\n' "$HOME_DIR"
