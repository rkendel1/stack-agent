//! # fx-host
//!
//! Drives the FX executor (`fx-core`) from a host that owns the provider call and the
//! event loop, through a small versioned message protocol.
//!
//! - [`Session`] is the safe, portable core: one attempt as a state machine over bytes.
//!   It validates through `fx-core`, runs `fx_core::Execution`, and reaches the provider
//!   only through `fx-single-attempt`'s `ProviderPort`, fed by the host.
//! - On `wasm32` the crate also exports a C ABI (`abi`) over a [`Session`], expanded from
//!   the one shared implementation in `fx-wasm-abi`. Its `abi` module is the only one that
//!   opts in to the ABI exports, and it imports nothing: no filesystem, process,
//!   environment, network, thread, clock or provider access exists in the module.
//!
//! The host implements the provider side (for Node, `host/node` over Easy-LLM's
//! `generateOnce` and `streamOnce`). Provider selection is the host's explicit input.
//! Nothing here routes, retries or falls back.
//!
//! See `docs/rebuild/HOST_BRIDGE.md`.

#[cfg(target_arch = "wasm32")]
mod abi;
mod port;
mod protocol;
mod session;

pub use protocol::{PROTOCOL_VERSION, SupportFlags};
pub use session::{MAX_COMMAND_BYTES, PortCounts, Session};

impl fx_wasm_abi::JsonSession for Session {
    fn begin(&mut self, input: &[u8]) -> Vec<u8> {
        Session::begin(self, input)
    }

    fn command(&mut self, input: &[u8]) -> Vec<u8> {
        Session::command(self, input)
    }
}

/// The WebAssembly ABI version exported as `fx_abi_version`. It moves with the
/// protocol: a host checks both before driving a module.
pub const ABI_VERSION: u32 = 1;
