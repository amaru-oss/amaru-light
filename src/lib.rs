#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! Legacy job and delivery-loop runtime. The modules below predate the
//! project lint configuration and are exempt until the v2 daemon replaces them.

#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod brain;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod config;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod delivery;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod engine;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod github;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod gitops;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod joblog;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod policy;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod providers;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod runtime;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod server;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod sources;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod storage;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
pub mod workspace;

pub mod settings;
