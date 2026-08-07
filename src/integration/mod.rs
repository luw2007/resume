//! Agent integrations.
//!
//! Each integration is an independent capability to discover, preview, and
//! resume Sessions persisted by one specific coding agent. The support of one
//! agent neither requires nor implies support of another. Shared code (JSONL
//! streaming, text normalization, message/summary helpers) lives in the
//! crate root; integrations only interpret records.

pub mod claude;
