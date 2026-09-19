//! The executor: the one door every AI action goes through.
//! Modules are declared by the task that creates each one (action, log, worker, executor).

pub mod action;
pub mod log;
pub mod worker;
pub mod desktop;
pub mod atspi;
pub mod screen;
pub mod awake;
pub mod executor;
