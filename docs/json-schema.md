# JSON output schema (v1)

Run `resume --json` to write one compact JSON document to stdout. Discovery diagnostics are written separately to stderr. The JSON document contains Session metadata and aggregate errors only; it never contains a `messages` array or full/raw transcript content. The `title` metadata may intentionally contain a bounded, truncated summary excerpt derived from the first user message (or an explicit native title), so consumers should treat titles as potentially conversation-derived text.

The current serialization implemented in `src/app.rs` is exactly the envelope `{schemaVersion, sessions, errors}`. Unknown future fields should be ignored by consumers. `schemaVersion` changes when an incompatible representation is introduced.

## JSON Schema

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://github.com/luw2007/resume/schemas/output-v1.json",
  "title": "resume JSON output v1",
  "type": "object",
  "required": ["schemaVersion", "sessions", "errors"],
  "properties": {
    "schemaVersion": { "const": 1 },
    "sessions": {
      "type": "array",
      "items": { "$ref": "#/$defs/session" }
    },
    "errors": {
      "type": "array",
      "items": { "$ref": "#/$defs/error" }
    }
  },
  "additionalProperties": false,
  "$defs": {
    "session": {
      "type": "object",
      "required": [
        "agent",
        "profile",
        "id",
        "title",
        "workspace",
        "support",
        "activity",
        "risk"
      ],
      "properties": {
        "agent": { "type": "string" },
        "profile": { "type": ["string", "null"] },
        "id": { "type": "string" },
        "title": { "type": ["string", "null"] },
        "workspace": { "type": ["string", "null"] },
        "support": { "type": "string" },
        "activity": { "type": "string" },
        "risk": { "type": "string" }
      },
      "additionalProperties": false
    },
    "error": {
      "type": "object",
      "required": ["category", "count"],
      "properties": {
        "category": { "type": "string" },
        "count": { "type": "integer", "minimum": 0 }
      },
      "additionalProperties": false
    }
  }
}
```

## Serialization details

- `profile`, `title`, and `workspace` are JSON `null` when unavailable. A non-null `title` may be an explicit native title or a bounded, truncated summary excerpt of the first user message.
- `support` and `risk` use the Rust variant labels emitted by the v1 serializer. `activity` is exactly `Active`, `Inactive`, or `Unknown`; it does not include process details or an observation timestamp.
- Paths and native IDs are converted to display strings for JSON. The native launch boundary retains OS-native path/argument values separately.
- `errors` entries expose only a redacted category and aggregate count. Verbose paths/chains remain diagnostics on stderr and do not enter JSON.
- Session arrays are deterministically sorted after non-interactive discovery completes. This does not imply an exact global visible order in the asynchronously loaded interactive picker.

## Relationship graph output

`resume --tree --json` uses a separate graph envelope:
`{schemaVersion, sessions, related, relations, errors}`. Ordinary `--json` remains unchanged.

- Each `sessions` entry adds an opaque `nodeId` to the Session metadata above.
- `related` entries contain `id`, `agent`, and `kind` (`MissingSession` or `AgentExecution`). These nodes cannot be resumed.
- `relations` entries contain `parent`, `child`, `kind`, and `source`. Both endpoints refer to a Session `nodeId` or a related node `id`; native evidence determines the edge, never title, time, or workspace similarity.
- Session node identity includes the agent, effective root, profile, and native locator. IDs distinguish OS-native path bytes and escape separators; treat the complete ID as opaque rather than parsing its encoding or persisting assumptions about its spelling.
- Graph diagnostics use the same category aggregation as ordinary JSON and stderr. JSON escaping protects control characters; native launch arguments and paths remain separate from terminal-safe display text.

