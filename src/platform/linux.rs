//! Linux-specific integrations: sched_ext scheduler detection and proc-connector events.
mod events;
mod scheduler;

pub use events::{ProcEvent, start_event_stream};
pub use scheduler::{ScxScheduler, Strategy, detect};