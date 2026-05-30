//! Background worker: owns a tokio runtime and dispatches jobs to the engine.
//!
//! The worker is the *only* component that owns the async runtime. Both the
//! UI and the engine are wired together through channels created here.

pub mod dispatcher;
pub mod job;
pub mod preview;
pub mod progress;
pub mod runtime;

pub use dispatcher::Dispatcher;
pub use job::{Command, Event, JobId};
pub use runtime::WorkerHandle;
