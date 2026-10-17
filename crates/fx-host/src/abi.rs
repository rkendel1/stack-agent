//! The WebAssembly ABI: the shared export surface (`fx-wasm-abi`) instantiated for
//! [`Session`].
//!
//! This file compiles for `wasm32` only and is the only place in this crate that opts in
//! to unsafe code. It contains no raw-pointer code of its own: the exports are expanded
//! from the one implementation in `fx-wasm-abi`, which Chip's host session shares. The
//! module it produces imports nothing, and every export validates its handle before use.
//!
//! Export names are unchanged from the first version of this ABI
//! (`fx_abi_version`, `fx_protocol_version`, `fx_alloc`, `fx_free`, `fx_session_new`,
//! `fx_session_free`, `fx_begin`, `fx_command`, `fx_output_ptr`); `fx_session_new` takes a
//! [`SupportFlags`] bit set. See `fx_wasm_abi` for the calling convention.
#![allow(unsafe_code)]

use crate::{ABI_VERSION, PROTOCOL_VERSION, Session, SupportFlags};

fx_wasm_abi::export_json_session_abi! {
    session: Session,
    new: |flags| Session::new(SupportFlags(flags)),
    abi_version: ABI_VERSION,
    protocol_version: PROTOCOL_VERSION,
    magic: 0x4658_5353,
    exports: {
        abi_version: fx_abi_version,
        protocol_version: fx_protocol_version,
        alloc: fx_alloc,
        free: fx_free,
        session_new: fx_session_new,
        session_free: fx_session_free,
        begin: fx_begin,
        command: fx_command,
        output_ptr: fx_output_ptr,
    },
}
