//! The executor: the single safe door every AI action goes through.
//! Modules are declared by the task that creates each one (action, rules, log, worker, executor).

pub mod action;
pub mod rules;
pub mod log;
pub mod worker;
pub mod undo;
pub mod admin;
pub mod desktop;
pub mod atspi;
pub mod screen;
pub mod awake;
pub mod executor;
