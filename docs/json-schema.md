# JSON output schema (v1)

Run `resume --json` to write one compact JSON document to stdout. Discovery diagnostics are written separately to stderr. The JSON document contains Session metadata and aggregate errors only; it never contains Session message bodies.

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

- `profile`, `title`, and `workspace` are JSON `null` when unavailable.
- `support`, `activity`, and `risk` are the Rust debug-form strings currently emitted by the v1 serializer. In particular, an active value can include its observed timestamp inside the string; consumers must not assume these fields are lower-case enums.
- Paths and native IDs are converted to display strings for JSON. The native launch boundary retains OS-native path/argument values separately.
- `errors` entries expose only a redacted category and aggregate count. Verbose paths/chains remain diagnostics on stderr and do not enter JSON.
- Session arrays are deterministically sorted after non-interactive discovery completes. This does not imply an exact global visible order in the asynchronously loaded interactive picker.
