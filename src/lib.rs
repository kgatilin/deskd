//! deskd library — agent orchestration runtime.
//!
//! Public API surface: `domain` (pure types), `ports` (trait interfaces),
//! `infra` (implementations), `config` (workspace/user config).
//! Application internals live in `app`.

pub mod app;
pub mod config;
pub mod domain;
pub mod infra;
pub mod ports;
