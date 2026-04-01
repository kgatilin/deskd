//! deskd library — all modules exposed for the binary and integration tests.

pub mod domain;
pub mod infra;
pub mod ports;

pub mod acp;
pub mod adapters;
pub mod agent;
pub mod bus;
pub mod cli;
pub mod commands;
pub mod config;
pub mod context;
pub mod graph;
pub mod mcp;
pub mod message;
pub mod paths;
pub mod schedule;
pub mod serve;
pub mod statemachine;
pub mod task;
pub mod tasklog;
pub mod unified_inbox;
pub mod worker;
pub mod workflow;
