//! Remote execution core, with user operations and process entry points as its public boundary.
#![deny(unreachable_pub)]
mod cli;
pub mod client;
mod clock;
mod config;
mod control;
mod crypto;
mod daemon;
pub mod error;
mod forwarding;
mod history;
mod membership;
mod net;
mod network;
mod process;
pub mod protocol;
mod relay;
pub mod runtime;
mod screenshot;
mod secure;
mod server;
mod service;
mod store;
mod streaming;
mod transfer;

mod database;
mod ipc;
mod pool;
mod session;

/// Internal fixtures for protocol, adversarial, storage and process integration tests.
/// These capabilities are deliberately excluded from the supported application API.
#[doc(hidden)]
pub mod testing;
