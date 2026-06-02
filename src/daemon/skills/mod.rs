//! Skills subsystem: install GitHub SKILL.md packages and mirror them to IDEs.
//!
//! Discovers SKILL.md packages on GitHub, stashes a canonical copy under
//! `<config>/conduit/skills/managed/<name>/`, and mirrors it into each agentic
//! IDE's skills dir (`~/.claude/skills`, etc.). `manager` holds the
//! `SkillManager` coordinator; `discover` does the GitHub IO; `sync` mirrors.

pub mod discover;
pub mod manager;
pub mod sync;

pub use manager::{SkillCandidate, SkillManager};
