//! The macro's exports behave as the calling convention says, on the native target, so the
//! raw-pointer code is exercised by `cargo test` and not only by a wasm host.
#![allow(unsafe_code)]

use fx_wasm_abi::{ERR_HANDLE, JsonSession};

struct Echo {
    flags: u32,
}

impl JsonSession for Echo {
    fn begin(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = format!("begin:{}:", self.flags).into_bytes();
        out.extend_from_slice(input);
        out
    }
    fn command(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = b"command:".to_vec();
        out.extend_from_slice(input);
        out
    }
}

mod abi {
    #![allow(unsafe_code, missing_docs)]
    use super::Echo;
    fx_wasm_abi::export_json_session_abi! {
        session: Echo,
        new: |flags| Echo { flags },
        abi_version: 7,
        protocol_version: 9,
        magic: 0x4543_484F,
        exports: {
            abi_version: t_abi_version,
            protocol_version: t_protocol_version,
            alloc: t_alloc,
            free: t_free,
            session_new: t_session_new,
            session_free: t_session_free,
            begin: t_begin,
            command: t_command,
            output_ptr: t_output_ptr,
        },
    }
}

fn response(handle: *mut fx_wasm_abi::Handle<Echo>, length: i32) -> Vec<u8> {
    assert!(length >= 0, "error code {length}");
    let pointer = unsafe { abi::t_output_ptr(handle) };
    assert!(!pointer.is_null());
    unsafe { std::slice::from_raw_parts(pointer, length as usize) }.to_vec()
}

#[test]
fn versions_are_reported() {
    assert_eq!(abi::t_abi_version(), 7);
    assert_eq!(abi::t_protocol_version(), 9);
}

#[test]
fn a_session_answers_begin_and_command_and_is_freed() {
    let handle = abi::t_session_new(5);
    assert!(!handle.is_null());
    let input = b"hello";
    let length = unsafe { abi::t_begin(handle, input.as_ptr(), input.len() as u32) };
    assert_eq!(response(handle, length), b"begin:5:hello");
    let length = unsafe { abi::t_command(handle, input.as_ptr(), 0) };
    assert_eq!(
        response(handle, length),
        b"command:",
        "a zero length reads nothing"
    );
    unsafe { abi::t_session_free(handle) };
}

#[test]
fn a_null_handle_is_an_error_not_a_crash() {
    let input = b"x";
    let code = unsafe { abi::t_command(std::ptr::null_mut(), input.as_ptr(), 1) };
    assert_eq!(code, ERR_HANDLE);
    assert!(unsafe { abi::t_output_ptr(std::ptr::null_mut()) }.is_null());
    unsafe { abi::t_session_free(std::ptr::null_mut()) };
}

#[test]
fn the_allocator_pair_round_trips() {
    let pointer = abi::t_alloc(64);
    assert!(!pointer.is_null());
    unsafe { abi::t_free(pointer, 64) };
    unsafe { abi::t_free(std::ptr::null_mut(), 0) };
}
