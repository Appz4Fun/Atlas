//! The stage every release goes through after it is saved: it waits for the
//! release to settle, rejects what no step can help, and runs ordered steps
//! with retries. Rules ported from newznab-tmux, see `rules.rs`.

pub mod rules;
pub mod schema;
