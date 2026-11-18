//! Starts a command so that it dies with its parent: process groups, parent-death signals, job objects.

pub mod unix;
pub mod windows;
