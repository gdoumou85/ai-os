//! The core: conversation front door, job loop, model connection, memory outside the chat.
//! Each task adds its own `pub mod` line.
pub mod engine;
pub mod event;
pub mod job;
pub mod model;
pub mod moves;
pub mod prompt;
pub mod schema;
pub mod snapshot;
pub mod store;
#[cfg(test)]
pub mod testing;
