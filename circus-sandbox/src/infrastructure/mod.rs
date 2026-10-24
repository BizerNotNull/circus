//! Docker deployment adapter. Not a public SDK surface.

mod engine;
pub(crate) use engine::{
    CreateSpec, Engine, EngineError, EngineErrorKind, Recovered, SocketKind, StagedBytes,
};
