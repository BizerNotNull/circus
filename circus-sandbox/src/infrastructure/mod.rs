//! Docker deployment adapter. Not a public SDK surface.

mod engine;
pub(crate) use engine::{
    AttachedExec, CreateSpec, Engine, EngineError, EngineErrorKind, OutputStream, Recovered,
    SocketKind, StagedBytes,
};
