//! The core: conversation front door, job loop, model connection, memory outside the chat.
//! Each task adds its own `pub mod` line.
pub mod cloud;
pub mod engine;
pub mod event;
pub mod find;
pub mod job;
pub mod learn;
pub mod machine;
pub mod model;
pub mod moves;
pub mod notes;
pub mod prompt;
pub mod schema;
pub mod service;
pub mod store;
// Not `#[cfg(test)]`: the integration tests are their own crates and need the fakes too. They
// ship inside the binaries, unused, which is harmless.
pub mod testing;
