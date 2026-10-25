//! A single Codex task over the public Circus sandbox interface.
//!
//! The caller supplies configuration, input bytes and requested artifact paths.
//! [`AgentRunner::start`] returns an owned [`AgentTask`]: consume events while
//! waiting, use `cancel` to stop, and inspect `wait` for the authoritative result.
//! Success is published only after artifacts have been read and cleanup succeeds.
//! Dropping the handle requests cancellation; keep it and await `wait` when cleanup
//! must be observed. Runtime/process shutdown is not a durable cleanup guarantee.
//!
//! This crate is a caller of `circus-sandbox`; Codex protocol and model policy do
//! not belong to the sandbox core. It does not expose HTTP or a Paseo transport.

mod config;
mod events;
mod model;
mod process;
mod runner;

pub use config::{ModelConfig, RunnerConfig, TaskRequest};
pub use model::{
    CleanupHandle, ModelFailure, TaskError, TaskEvent, TaskFailure, TaskOutcome, TaskOutput,
    TaskPhase, ToolPhase,
};
pub use runner::{AgentRunner, AgentTask};

#[cfg(test)]
mod tests;
