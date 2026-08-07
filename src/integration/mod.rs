//! Agent integrations.
//!
//! Each integration independently discovers, previews, and builds the resume
//! command for one agent's persisted Sessions. Integrations share only the
//! pure helpers under [`crate::jsonl`], [`crate::message`], [`crate::text`],
//! and [`crate::summary`]; there is no shared transcript schema.
//!
//! v0.1.0 ships integrations for Codex, Claude Code, Pi, and OMP. This module
//! currently hosts the Codex JSONL integration (Step 6); the other integrations
//! are implemented in their own steps.

pub mod codex;
