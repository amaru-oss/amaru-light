#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! Amaru Light: a personal daemon that turns GitHub issues into pull requests
//! by driving agent CLIs, then follows up on reviews.

pub mod actions;
pub mod agent;
pub mod app;
pub mod checks;
pub mod cleanup;
pub mod contract;
pub mod db;
pub mod forge;
pub mod install;
pub mod lifecycle;
pub mod login;
pub mod process;
pub mod prompts;
pub mod scheduler;
pub mod settings;
pub mod setup;
#[cfg(test)]
mod testing;
pub mod ticket;
pub mod web;
pub mod workflow;
pub mod worktree;
