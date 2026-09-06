//! Everything that talks to FreeToken: its HTTP control plane, its CLI, and the
//! processes ft-man spawns from it.

pub mod api;
pub mod locate;
pub mod proc;
pub mod types;

pub use api::Client;
pub use locate::Freetoken;
pub use proc::{Engine, EngineEvent, EngineState, Job, JobEvent};
