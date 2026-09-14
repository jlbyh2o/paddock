//! Everything that talks to FreeToken: its HTTP control plane, its CLI, and the
//! processes ft-man spawns from it.

pub mod api;
pub mod checkout;
pub mod locate;
pub mod preflight;
pub mod proc;
pub mod types;

pub use api::Client;
pub use checkout::FtCheckout;
pub use locate::Freetoken;
pub use preflight::Outcome as Preflight;
pub use proc::{Engine, EngineEvent, EngineState, Job, JobEvent};
