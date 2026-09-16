//! The core: conversation front door, job loop, model connection, memory outside the chat.
//! Each task adds its own `pub mod` line.
pub mod model;
pub mod moves;
pub mod schema;
