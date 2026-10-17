//! # fx-wasm-abi
//!
//! The single implementation of the C-compatible WebAssembly export surface for a
//! host-driven JSON session. A module defines a [`JsonSession`] and invokes
//! [`export_json_session_abi!`]; the macro expands the raw-pointer exports **in that
//! module's own `abi` file**, so the symbols belong to the module that ships.
//!
//! Why a macro in a crate instead of a copy: the raw-pointer code is the part of this
//! runtime that can corrupt memory, and it is the part a reviewer should read once. FX's
//! host session (`fx-host`) and Chip's host session use the same text, and the unsafe code
//! exists nowhere else. This crate itself contains no unsafe code; the macro body is the
//! only place the tokens appear, and `check-boundaries.sh` pins that.
//!
//! The module that expands the macro imports nothing: no filesystem, process,
//! environment, network, thread, clock or provider access exists in it. The wasm
//! instance is the isolation boundary: a host that passes a bad pointer can corrupt only
//! its own instance's memory. Every export validates its handle before use. A panic traps
//! the instance, and a host must discard a trapped instance.
//!
//! Calling convention (integers are `i32`/`u32`, pointers are linear-memory offsets):
//!
//! 1. The ABI and protocol versions must match the host's.
//! 2. `alloc(len)` returns a buffer the host fills; `free(ptr, len)` releases it.
//! 3. `session_new(flags)` returns a handle (null on failure); `session_free(handle)`
//!    releases it.
//! 4. `begin(handle, ptr, len)` and `command(handle, ptr, len)` each return the length of
//!    the JSON response, readable at `output_ptr(handle)` until the next call, or a
//!    negative error code ([`ERR_HANDLE`], [`ERR_SIZE`]).

/// A session driven by whole messages: bytes in, a JSON response out. Nothing else is
/// exposed to the host.
pub trait JsonSession {
    /// Starts the session from its first document. Returns the JSON response.
    fn begin(&mut self, input: &[u8]) -> Vec<u8>;
    /// Applies one command. Returns the JSON response.
    fn command(&mut self, input: &[u8]) -> Vec<u8>;
}

/// A negative return: the handle is null or not a session.
pub const ERR_HANDLE: i32 = -1;
/// A negative return: the response does not fit in an `i32` length.
pub const ERR_SIZE: i32 = -2;

/// A session plus its last response, owned by the instance. The magic number rejects a
/// stale or foreign pointer on a best-effort basis.
pub struct Handle<S> {
    /// Set to the module's magic while live; cleared on free.
    pub magic: u32,
    /// The session.
    pub session: S,
    /// The last response, kept readable until the next call.
    pub output: Vec<u8>,
}

/// Expands the export surface for `$session`.
///
/// Arguments (all required, named so a call site reads as a table):
///
/// - `session`: the type implementing [`JsonSession`].
/// - `new`: an expression of type `fn(u32) -> $session` called with the `flags` argument.
/// - `abi_version`, `protocol_version`: `u32` expressions returned to the host.
/// - `magic`: a `u32` constant unique to the module.
/// - the export names, as identifiers: this crate does not invent symbol names.
///
/// Must be invoked from a module compiled for `wasm32` only and declared with
/// `#![allow(unsafe_code)]` (the only such declaration in the module).
#[macro_export]
macro_rules! export_json_session_abi {
    (
        session: $session:ty,
        new: $new:expr,
        abi_version: $abi_version:expr,
        protocol_version: $protocol_version:expr,
        magic: $magic:expr,
        exports: {
            abi_version: $f_abi:ident,
            protocol_version: $f_proto:ident,
            alloc: $f_alloc:ident,
            free: $f_free:ident,
            session_new: $f_new:ident,
            session_free: $f_sfree:ident,
            begin: $f_begin:ident,
            command: $f_command:ident,
            output_ptr: $f_output:ident $(,)?
        } $(,)?
    ) => {
        /// The ABI version.
        #[unsafe(no_mangle)]
        pub extern "C" fn $f_abi() -> u32 {
            $abi_version
        }

        /// The host protocol version.
        #[unsafe(no_mangle)]
        pub extern "C" fn $f_proto() -> u32 {
            $protocol_version
        }

        /// Allocates `len` bytes in linear memory for the host to fill.
        #[unsafe(no_mangle)]
        pub extern "C" fn $f_alloc(len: u32) -> *mut u8 {
            let mut buffer = Vec::<u8>::with_capacity(len as usize);
            let pointer = buffer.as_mut_ptr();
            std::mem::forget(buffer);
            pointer
        }

        /// Releases a buffer from the allocator export.
        ///
        /// # Safety
        /// `ptr` and `len` must be exactly a pair returned by and given to the allocator
        /// export, and the buffer must not be used afterwards.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $f_free(ptr: *mut u8, len: u32) {
            if !ptr.is_null() {
                // SAFETY: the caller guarantees `ptr`/`len` came from the allocator
                // export, which leaked a `Vec<u8>` with capacity `len` and length 0.
                drop(unsafe { Vec::from_raw_parts(ptr, 0, len as usize) });
            }
        }

        /// Creates a session. `flags` is passed to the module's constructor.
        #[unsafe(no_mangle)]
        pub extern "C" fn $f_new(flags: u32) -> *mut $crate::Handle<$session> {
            let make: fn(u32) -> $session = $new;
            Box::into_raw(Box::new($crate::Handle {
                magic: $magic,
                session: make(flags),
                output: Vec::new(),
            }))
        }

        /// Releases a session.
        ///
        /// # Safety
        /// `handle` must be a value returned by the constructor export that has not been
        /// freed.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $f_sfree(handle: *mut $crate::Handle<$session>) {
            if handle.is_null() {
                return;
            }
            // SAFETY: the caller guarantees `handle` came from the constructor and is unfreed.
            let valid = unsafe { (*handle).magic == $magic };
            if valid {
                // SAFETY: as above; the magic check rejects a stale or foreign pointer on a
                // best-effort basis.
                let mut boxed = unsafe { Box::from_raw(handle) };
                boxed.magic = 0;
            }
        }

        /// Starts the session from its first document.
        ///
        /// # Safety
        /// `handle` must be a live session handle and `ptr`/`len` a readable range.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $f_begin(
            handle: *mut $crate::Handle<$session>,
            ptr: *const u8,
            len: u32,
        ) -> i32 {
            // SAFETY: forwarded from this function's contract.
            unsafe { abi_call(handle, ptr, len, <$session as $crate::JsonSession>::begin) }
        }

        /// Applies one JSON command.
        ///
        /// # Safety
        /// `handle` must be a live session handle and `ptr`/`len` a readable range.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $f_command(
            handle: *mut $crate::Handle<$session>,
            ptr: *const u8,
            len: u32,
        ) -> i32 {
            // SAFETY: forwarded from this function's contract.
            unsafe { abi_call(handle, ptr, len, <$session as $crate::JsonSession>::command) }
        }

        /// Runs one action against the handle's session and stores its response.
        ///
        /// # Safety
        /// `handle` must be a live session handle and `ptr`/`len` a readable range.
        unsafe fn abi_call(
            handle: *mut $crate::Handle<$session>,
            ptr: *const u8,
            len: u32,
            action: fn(&mut $session, &[u8]) -> Vec<u8>,
        ) -> i32 {
            if handle.is_null() {
                return $crate::ERR_HANDLE;
            }
            // SAFETY: the caller guarantees a live handle.
            let handle = unsafe { &mut *handle };
            if handle.magic != $magic {
                return $crate::ERR_HANDLE;
            }
            let input: &[u8] = if len == 0 || ptr.is_null() {
                &[]
            } else {
                // SAFETY: the caller guarantees `ptr..ptr+len` is readable.
                unsafe { std::slice::from_raw_parts(ptr, len as usize) }
            };
            handle.output = action(&mut handle.session, input);
            i32::try_from(handle.output.len()).unwrap_or($crate::ERR_SIZE)
        }

        /// The last response of `handle`.
        ///
        /// # Safety
        /// `handle` must be a live session handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $f_output(handle: *mut $crate::Handle<$session>) -> *const u8 {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: the caller guarantees a live handle.
            let handle = unsafe { &*handle };
            if handle.magic != $magic {
                return std::ptr::null();
            }
            handle.output.as_ptr()
        }
    };
}
