//! Agent integrations.
//!
//! Each integration is an independent capability to discover, preview, and
//! resume Sessions persisted by one specific coding agent. Shared code (JSONL
//! reading, text normalization, message/summary helpers, Scope) lives in the
//! crate root; integrations interpret records but never impose a shared
//! transcript schema.

pub mod pi;
