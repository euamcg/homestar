#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_debug_implementations, missing_docs, rust_2018_idioms)]
#![deny(unreachable_pub)]

//! homestar-runtime is a determistic Wasm runtime and effectful workflow/job
//! system intended to be embedded inside or run alongside IPFS.
//!
//! You can find a more complete description [here].
//!
//!
//! Related crates/packages:
//!
//! - [homestar-core]
//! - [homestar-wasm]
//!
//! [here]: <https://github.com/ipvm-wg/spec>
//! [homestar-core]: homestar_core
//! [homestar-wasm]: homestar_wasm

pub mod channel;
pub mod cli;
pub mod daemon;
pub mod db;
mod event_handler;
mod logger;
pub mod network;
mod receipt;
pub mod runner;
mod scheduler;
mod settings;
mod tasks;
mod worker;
pub mod workflow;

/// Test utilities.
#[cfg(any(test, feature = "test-utils"))]
#[cfg_attr(docsrs, doc(cfg(feature = "test-utils")))]
pub mod test_utils;

pub use db::Db;
pub(crate) mod libp2p;
pub use logger::*;
pub(crate) mod metrics;
#[allow(unused_imports)]
pub(crate) use event_handler::EventHandler;
pub use receipt::{Receipt, RECEIPT_TAG, VERSION_KEY};
pub use runner::Runner;
pub(crate) use scheduler::TaskScheduler;
pub use settings::Settings;
pub(crate) use worker::Worker;
pub use workflow::WORKFLOW_TAG;
