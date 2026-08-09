//! Agent integrations.
//!
//! Each integration independently discovers, previews, and builds the resume
//! command for one agent's persisted Sessions. Integrations share only the
//! pure helpers under [`crate::preview::jsonl`], [`crate::preview::message`], [`crate::preview::text`],
//! and [`crate::preview::summary`]; there is no shared transcript schema. The support
//! of one agent neither requires nor implies support of another.
//!
//! v0.1.0 ships integrations for Codex, Claude Code, Pi, and OMP.

pub mod claude;
pub mod codex;
pub mod omp;
pub mod pi;
