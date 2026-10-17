//! Rust-hosted conformance run of the two WebAssembly session modules (known issue G).
//!
//! The test builds the real `fx_host.wasm` and `chip_wasm_host.wasm` from this workspace
//! (`wasm32-unknown-unknown`, release, into `target/wasm-session-conformance`), instantiates each
//! under `wasmi` with an empty linker, and drives it only through its exported ABI (the surface
//! expanded from `fx-wasm-abi`): `*_abi_version`, `*_protocol_version`, `*_alloc`, `*_free`,
//! `*_session_new`, `*_session_free`, `*_begin`, `*_command`, `*_output_ptr` and `memory`.
//!
//! The test is the host. It answers FX's provider-port calls with a scripted provider, Chip's
//! inference requests with FX (the `fx-host` module itself, or the native FX executor over a
//! scripted port, as `tests/session.rs` does), and Chip's capability executions deterministically.
//! Every exchange is then replayed against the native `Session` of the same crate, and the
//! responses must be byte-identical: the ABI adds nothing and loses nothing.
//!
//! If the modules cannot be built (for example the wasm32 target is missing), every test here
//! fails with the build output. Nothing is skipped.
//!
//! Not covered: WASI, Wasmtime, Node/V8, or any in-Wasm provider bridge. The provider is always
//! played by this test.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use fx_core::{Limits, NoHost, encode_report, execute, parse_request};
use fx_single_attempt::testing::{FakePort, failure};
use fx_single_attempt::{Observed, PortFailureKind, SingleAttemptCapability};
use serde_json::{Map, Value, json};
use wasmi::{Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

const FX_PACKAGE: &str = "fx-host";
const CHIP_PACKAGE: &str = "chip-wasm-host";

/// `fx_wasm_abi::ERR_HANDLE`, as the module returns it.
const ERR_HANDLE: i32 = -1;

const ABI_FUNCTIONS: [&str; 9] = [
    "abi_version",
    "protocol_version",
    "alloc",
    "free",
    "session_new",
    "session_free",
    "begin",
    "command",
    "output_ptr",
];

// ---------------------------------------------------------------------------------------------
// Building the modules
// ---------------------------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Modules {
    fx: Vec<u8>,
    chip: Vec<u8>,
}

/// Builds one package for `wasm32-unknown-unknown` (release) into a private target directory, so
/// the outer cargo invocation is never involved, and returns the module bytes. Each package is
/// built on its own, as CI builds it, so feature unification matches the shipped artifact.
fn build(package: &str, target_dir: &Path) -> Vec<u8> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(workspace_root())
        .env("CARGO_TARGET_DIR", target_dir)
        .args([
            "build",
            "--locked",
            "--offline",
            "-p",
            package,
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .output()
        .unwrap_or_else(|e| panic!("cargo could not be started to build {package}: {e}"));
    assert!(
        output.status.success(),
        "building the {package} Wasm module failed (is the target installed? `rustup target add \
         wasm32-unknown-unknown`); the wasmi conformance run cannot proceed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact = target_dir
        .join("wasm32-unknown-unknown/release")
        .join(format!("{}.wasm", package.replace('-', "_")));
    std::fs::read(&artifact)
        .unwrap_or_else(|e| panic!("the built module {} is missing: {e}", artifact.display()))
}

fn modules() -> &'static Modules {
    static MODULES: OnceLock<Modules> = OnceLock::new();
    MODULES.get_or_init(|| {
        let target_dir = workspace_root().join("target/wasm-session-conformance");
        Modules {
            fx: build(FX_PACKAGE, &target_dir),
            chip: build(CHIP_PACKAGE, &target_dir),
        }
    })
}

/// The section ids of a module, read from its binary (the same walk as CI's no-imports check).
fn section_ids(wasm: &[u8]) -> Vec<u8> {
    assert_eq!(&wasm[..8], b"\0asm\x01\0\0\0", "a wasm module, version 1");
    let mut ids = Vec::new();
    let mut i = 8;
    while i < wasm.len() {
        let id = wasm[i];
        i += 1;
        let (mut size, mut shift) = (0usize, 0);
        loop {
            let byte = wasm[i];
            i += 1;
            size |= usize::from(byte & 0x7f) << shift;
            shift += 7;
            if byte < 0x80 {
                break;
            }
        }
        ids.push(id);
        i += size;
    }
    ids
}

// ---------------------------------------------------------------------------------------------
// The ABI, driven through wasmi
// ---------------------------------------------------------------------------------------------

/// One module instance and one session handle in it, driven only through the exports.
struct WasmSession {
    store: Store<()>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    free: TypedFunc<(i32, i32), ()>,
    session_free: TypedFunc<i32, ()>,
    begin: TypedFunc<(i32, i32, i32), i32>,
    command: TypedFunc<(i32, i32, i32), i32>,
    output_ptr: TypedFunc<i32, i32>,
    abi_version: u32,
    protocol_version: u32,
    handle: i32,
}

fn compile(wasm: &[u8]) -> (Engine, Module) {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).expect("the module validates under wasmi");
    (engine, module)
}

impl WasmSession {
    /// Instantiates `wasm` with no imports at all and opens a session with `flags`.
    fn open(wasm: &[u8], prefix: &str, flags: u32) -> WasmSession {
        let (engine, module) = compile(wasm);
        let mut store = Store::new(&engine, ());
        // An empty linker: any import would make instantiation fail.
        let instance: Instance = Linker::<()>::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .expect("the module instantiates with no host imports");
        let memory = instance
            .get_memory(&store, "memory")
            .expect("the module exports its memory");
        macro_rules! export {
            ($name:literal, $params:ty, $results:ty) => {
                instance
                    .get_typed_func::<$params, $results>(&store, &format!("{prefix}_{}", $name))
                    .unwrap_or_else(|e| panic!("export {prefix}_{}: {e}", $name))
            };
        }
        let abi_version = export!("abi_version", (), i32);
        let protocol_version = export!("protocol_version", (), i32);
        let session_new = export!("session_new", i32, i32);
        let mut session = WasmSession {
            alloc: export!("alloc", i32, i32),
            free: export!("free", (i32, i32), ()),
            session_free: export!("session_free", i32, ()),
            begin: export!("begin", (i32, i32, i32), i32),
            command: export!("command", (i32, i32, i32), i32),
            output_ptr: export!("output_ptr", i32, i32),
            abi_version: abi_version.call(&mut store, ()).expect("abi_version") as u32,
            protocol_version: protocol_version
                .call(&mut store, ())
                .expect("protocol_version") as u32,
            handle: 0,
            memory,
            store,
        };
        session.handle = session_new
            .call(&mut session.store, flags as i32)
            .expect("session_new does not trap");
        assert_ne!(session.handle, 0, "session_new returned a null handle");
        session
    }

    /// Copies `input` into a fresh module buffer, runs `begin` or `command` on `handle`, and
    /// returns the raw `i32` result plus the response bytes when it is a length. The buffer is
    /// released afterwards. A trap fails the test: the ABI must never trap on host input.
    fn call_raw(&mut self, is_begin: bool, handle: i32, input: &[u8]) -> Result<Vec<u8>, i32> {
        let len = i32::try_from(input.len()).expect("input fits in i32");
        let ptr = self.alloc.call(&mut self.store, len).expect("alloc");
        self.memory
            .write(&mut self.store, ptr as u32 as usize, input)
            .expect("the allocated buffer lies in memory");
        let function = if is_begin { self.begin } else { self.command };
        let result = function
            .call(&mut self.store, (handle, ptr, len))
            .unwrap_or_else(|e| panic!("the module trapped on host input: {e}"));
        self.free
            .call(&mut self.store, (ptr, len))
            .expect("free does not trap");
        if result < 0 {
            return Err(result);
        }
        let out = self
            .output_ptr
            .call(&mut self.store, handle)
            .expect("output_ptr");
        assert_ne!(out, 0, "a successful call leaves a readable output");
        let mut response = vec![0u8; result as usize];
        self.memory
            .read(&self.store, out as u32 as usize, &mut response)
            .expect("the output lies in memory");
        Ok(response)
    }

    fn output_ptr_of(&mut self, handle: i32) -> i32 {
        self.output_ptr
            .call(&mut self.store, handle)
            .expect("output_ptr does not trap")
    }

    /// A module buffer of `len` zero bytes: memory the module owns that is not a session.
    fn zeroed_buffer(&mut self, len: usize) -> i32 {
        let ptr = self.alloc.call(&mut self.store, len as i32).expect("alloc");
        self.memory
            .write(&mut self.store, ptr as u32 as usize, &vec![0u8; len])
            .expect("in memory");
        ptr
    }
}

impl Drop for WasmSession {
    fn drop(&mut self) {
        if self.handle != 0 && !std::thread::panicking() {
            self.session_free
                .call(&mut self.store, self.handle)
                .expect("session_free does not trap");
        }
    }
}

/// A host-driven JSON session: the Wasm one and the two native ones.
trait Driver {
    fn begin(&mut self, input: &[u8]) -> Vec<u8>;
    fn command(&mut self, input: &[u8]) -> Vec<u8>;
}

impl Driver for WasmSession {
    fn begin(&mut self, input: &[u8]) -> Vec<u8> {
        let handle = self.handle;
        self.call_raw(true, handle, input)
            .unwrap_or_else(|code| panic!("begin returned error code {code} on a live handle"))
    }

    fn command(&mut self, input: &[u8]) -> Vec<u8> {
        let handle = self.handle;
        self.call_raw(false, handle, input)
            .unwrap_or_else(|code| panic!("command returned error code {code} on a live handle"))
    }
}

impl Driver for fx_host::Session {
    fn begin(&mut self, input: &[u8]) -> Vec<u8> {
        fx_host::Session::begin(self, input)
    }

    fn command(&mut self, input: &[u8]) -> Vec<u8> {
        fx_host::Session::command(self, input)
    }
}

impl Driver for chip_wasm_host::Session {
    fn begin(&mut self, input: &[u8]) -> Vec<u8> {
        chip_wasm_host::Session::begin(self, input)
    }

    fn command(&mut self, input: &[u8]) -> Vec<u8> {
        chip_wasm_host::Session::command(self, input)
    }
}

/// Every message sent to a session and every response, in order: (is `begin`, input, output).
#[derive(Default)]
struct Transcript {
    exchanges: Vec<(bool, Vec<u8>, Vec<u8>)>,
}

/// Wraps a driver and records the exchange so it can be replayed against another one.
struct Recorder<D> {
    inner: D,
    transcript: Transcript,
}

impl<D: Driver> Recorder<D> {
    fn new(inner: D) -> Self {
        Recorder {
            inner,
            transcript: Transcript::default(),
        }
    }

    fn begin(&mut self, document: &Value) -> Value {
        self.begin_bytes(document.to_string().into_bytes())
    }

    fn begin_bytes(&mut self, input: Vec<u8>) -> Value {
        let output = self.inner.begin(&input);
        self.record(true, input, output)
    }

    fn command(&mut self, document: &Value) -> Value {
        self.command_bytes(document.to_string().into_bytes())
    }

    fn command_bytes(&mut self, input: Vec<u8>) -> Value {
        let output = self.inner.command(&input);
        self.record(false, input, output)
    }

    fn record(&mut self, is_begin: bool, input: Vec<u8>, output: Vec<u8>) -> Value {
        let value = serde_json::from_slice(&output).unwrap_or_else(|_| {
            panic!(
                "every response is JSON: {}",
                String::from_utf8_lossy(&output)
            )
        });
        self.transcript.exchanges.push((is_begin, input, output));
        value
    }
}

/// Replays `transcript` against `native` and requires byte-identical responses.
fn assert_native_agrees(label: &str, transcript: &Transcript, mut native: impl Driver) {
    assert!(!transcript.exchanges.is_empty());
    for (index, (is_begin, input, wasm_output)) in transcript.exchanges.iter().enumerate() {
        let native_output = if *is_begin {
            native.begin(input)
        } else {
            native.command(input)
        };
        assert_eq!(
            String::from_utf8_lossy(wasm_output),
            String::from_utf8_lossy(&native_output),
            "{label}: exchange {index}: the Wasm module and the native session disagree"
        );
    }
}

fn fx_wasm(flags: u32) -> WasmSession {
    WasmSession::open(&modules().fx, "fx", flags)
}

fn chip_wasm() -> WasmSession {
    WasmSession::open(&modules().chip, "chip", 0)
}

// ---------------------------------------------------------------------------------------------
// Module shape: no imports, exactly the shared ABI, matching versions
// ---------------------------------------------------------------------------------------------

fn assert_module_shape(wasm: &[u8], prefix: &str, abi: u32, protocol: u32) {
    let ids = section_ids(wasm);
    assert!(
        !ids.contains(&2),
        "{prefix}: the module has an import section: {ids:?}"
    );
    let (_, module) = compile(wasm);
    let imports: Vec<String> = module
        .imports()
        .map(|i| format!("{}::{}", i.module(), i.name()))
        .collect();
    assert_eq!(imports, Vec::<String>::new(), "{prefix}: imports");
    let exports: BTreeSet<String> = module.exports().map(|e| e.name().to_owned()).collect();
    let expected: BTreeSet<String> = ABI_FUNCTIONS
        .iter()
        .map(|f| format!("{prefix}_{f}"))
        .chain(["memory".to_owned()])
        .collect();
    assert_eq!(exports, expected, "{prefix}: exports are exactly the ABI");
    let session = WasmSession::open(wasm, prefix, 0);
    assert_eq!(
        (session.abi_version, session.protocol_version),
        (abi, protocol),
        "{prefix}: versions"
    );
}

#[test]
fn fx_host_module_imports_nothing_and_exports_exactly_the_shared_abi() {
    assert_module_shape(
        &modules().fx,
        "fx",
        fx_host::ABI_VERSION,
        fx_host::PROTOCOL_VERSION,
    );
}

#[test]
fn chip_wasm_host_module_imports_nothing_and_exports_exactly_the_shared_abi() {
    assert_module_shape(
        &modules().chip,
        "chip",
        chip_wasm_host::ABI_VERSION,
        chip_wasm_host::PROTOCOL_VERSION,
    );
}

// ---------------------------------------------------------------------------------------------
// fx-host
// ---------------------------------------------------------------------------------------------

const FX_REQUEST: &str = r#"{"v":1,"model":"example/model-1","messages":[{"role":"user","parts":[{"type":"text","text":"say hi"}]}],"correlation":{"request_id":"00112233445566778899aabbccddeeff"}}"#;

fn fx_cmd(value: Value) -> Value {
    let mut object = value.as_object().expect("a command object").clone();
    object.insert("v".into(), fx_host::PROTOCOL_VERSION.into());
    Value::Object(object)
}

/// What the scripted provider does with one FX attempt.
#[derive(Clone, Copy)]
enum Provider<'a> {
    /// Accepts the call and streams `text` in two chunks, then a provider end marker.
    Answers(&'a str),
    /// The invocation fails after the request may have left (`provider_error`, `invoked`).
    FailsAfterSending,
}

/// Plays the host side of one fx-host attempt: invokes the scripted provider when asked, feeds
/// its output, steps until `done`, and returns the final response.
fn drive_fx<D: Driver>(fx: &mut Recorder<D>, request: &[u8], provider: Provider<'_>) -> Value {
    let begin = fx.begin_bytes(request.to_vec());
    assert_eq!(begin["status"], "need_invoke", "{begin}");
    match provider {
        Provider::Answers(text) => {
            let ready = fx.command(&fx_cmd(json!({"cmd": "invoke_result", "ok": true})));
            assert_eq!(ready["status"], "ready", "{ready}");
            let idle = fx.command(&fx_cmd(json!({"cmd": "step"})));
            assert_eq!(idle["status"], "need_input", "{idle}");
            let (head, tail) = text.split_at(text.len() / 2);
            fx.command(&fx_cmd(json!({
                "cmd": "feed", "observed": "output_observed", "ended": false,
                "items": [{"type": "text", "text": head}],
            })));
            let more = fx.command(&fx_cmd(json!({"cmd": "step"})));
            assert_eq!(more["status"], "need_input", "{more}");
            fx.command(&fx_cmd(json!({
                "cmd": "feed", "observed": "output_observed", "ended": true,
                "items": [
                    {"type": "text", "text": tail},
                    {"type": "done", "terminal": "provider_marker", "finish_reason": "stop",
                     "usage": {"input_tokens": 12, "output_tokens": 3}},
                ],
            })));
        }
        Provider::FailsAfterSending => {
            let ready = fx.command(&fx_cmd(json!({
                "cmd": "invoke_result", "ok": false,
                "failure": {"kind": "provider_error", "observed": "invoked", "message": "upstream 500", "status": 500},
            })));
            assert_eq!(ready["status"], "ready", "{ready}");
        }
    }
    let done = fx.command(&fx_cmd(json!({"cmd": "step"})));
    assert_eq!(done["status"], "done", "{done}");
    done
}

#[test]
fn fx_host_round_trip_completes_with_confirmed_delivery_and_matches_native() {
    let mut fx = Recorder::new(fx_wasm(0));
    let done = drive_fx(
        &mut fx,
        FX_REQUEST.as_bytes(),
        Provider::Answers("hello world"),
    );

    let report = &done["report"];
    assert_eq!(
        report["outcome"],
        json!({
            "type": "completed",
            "content": "hello world",
            "content_truncated": false,
            "finish_reason": "stop",
            "metadata": {"generation_id": null, "model_reported": null, "provider_timestamp_ms": null},
            "proposals": [],
            "provider_records": [],
            "reasoning_bytes": 0,
            "usage": {
                "input_tokens": 12, "output_tokens": 3, "cache_read_tokens": null,
                "cache_write_tokens": null, "reasoning_tokens": null, "cost": null,
            },
        }),
        "{done}"
    );
    assert_eq!(report["disposition"], "complete");
    assert_eq!(report["diagnostics"]["delivery"], "confirmed_response");
    assert_eq!(
        report["diagnostics"]["request_id"],
        "00112233445566778899aabbccddeeff"
    );
    assert_eq!(report["diagnostics"]["starts"], 1);
    assert_eq!(done["abort"], false);
    let events: Vec<&str> = done["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(events, ["text_delta", "usage", "finish"]);

    // After the end the module repeats the report and ignores late input, as natively.
    let again = fx.command(&fx_cmd(json!({"cmd": "step"})));
    assert_eq!(again, done);
    let late = fx.command(&fx_cmd(json!({"cmd": "cancel"})));
    assert_eq!(late["ignored"], true);

    assert_native_agrees(
        "fx-host round trip",
        &fx.transcript,
        fx_host::Session::new(fx_host::SupportFlags(0)),
    );
}

#[test]
fn fx_host_provider_failure_keeps_possibly_sent_and_matches_native() {
    let mut fx = Recorder::new(fx_wasm(0));
    let done = drive_fx(&mut fx, FX_REQUEST.as_bytes(), Provider::FailsAfterSending);
    let outcome = &done["report"]["outcome"];
    assert_eq!(outcome["type"], "failed", "{done}");
    assert_eq!(outcome["origin"], "capability");
    assert_eq!(outcome["delivery"], "possibly_sent");
    assert_eq!(outcome["status_hint"], 500);
    assert_eq!(done["report"]["diagnostics"]["delivery"], "possibly_sent");
    assert_eq!(done["report"]["diagnostics"]["starts"], 1);
    assert_native_agrees(
        "fx-host provider failure",
        &fx.transcript,
        fx_host::Session::new(fx_host::SupportFlags(0)),
    );
}

#[test]
fn fx_host_malformed_input_is_an_error_response_never_a_trap() {
    let mut fx = Recorder::new(fx_wasm(0));

    // A request that does not decode ends the attempt as invalid, without asking to invoke.
    let begin = fx.begin_bytes(b"{\"v\":1,".to_vec());
    assert_eq!(begin["status"], "done", "{begin}");
    assert_eq!(begin["report"]["outcome"]["type"], "failed");
    assert_eq!(begin["report"]["outcome"]["kind"], "invalid_request");
    assert_eq!(begin["report"]["outcome"]["delivery"], "definitely_unsent");
    assert_eq!(begin["report"]["diagnostics"]["starts"], 0);
    assert_native_agrees(
        "fx-host undecodable request",
        &fx.transcript,
        fx_host::Session::new(fx_host::SupportFlags(0)),
    );

    // Malformed commands are error responses and leave a live session usable.
    let mut fx = Recorder::new(fx_wasm(0));
    let begin = fx.begin_bytes(FX_REQUEST.as_bytes().to_vec());
    assert_eq!(begin["status"], "need_invoke");
    for (raw, code) in [
        (&b"{not json"[..], "bad_command"),
        (&b""[..], "bad_command"),
        (&b"[1,2]"[..], "bad_command"),
        (&br#"{"v":2,"cmd":"step"}"#[..], "unsupported_version"),
        (&br#"{"v":1,"cmd":"launch"}"#[..], "bad_command"),
        (&br#"{"v":1,"cmd":"step"}"#[..], "bad_state"),
    ] {
        let response = fx.command_bytes(raw.to_vec());
        assert_eq!(response["status"], "error", "{response}");
        assert_eq!(
            response["error"]["code"],
            code,
            "{}",
            String::from_utf8_lossy(raw)
        );
    }
    let ready = fx.command(&fx_cmd(json!({"cmd": "invoke_result", "ok": true})));
    assert_eq!(ready["status"], "ready", "the session survived the errors");
    assert_native_agrees(
        "fx-host malformed commands",
        &fx.transcript,
        fx_host::Session::new(fx_host::SupportFlags(0)),
    );

    assert_bad_handles_are_rejected(&mut fx.inner, FX_REQUEST.as_bytes());
}

/// A null or foreign handle is refused with `ERR_HANDLE` by every entry point; no trap.
fn assert_bad_handles_are_rejected(session: &mut WasmSession, input: &[u8]) {
    let foreign = session.zeroed_buffer(64);
    for handle in [0, foreign] {
        assert_eq!(session.call_raw(true, handle, input), Err(ERR_HANDLE));
        assert_eq!(session.call_raw(false, handle, input), Err(ERR_HANDLE));
        assert_eq!(session.output_ptr_of(handle), 0);
    }
}

// The fixtures in crates/fx-host/tests/fixtures/conformance are host transcripts with an
// expected response subset for each step. fx-host's own test runs them natively; here they run
// against the Wasm module, and every response must also equal the native one byte for byte.

/// Whether `actual` satisfies `expected`: objects by key subset, arrays and scalars exactly.
/// `null` matches an absent key. Keys starting with `$` are runner directives (the native
/// runner checks them; byte equality with native covers them here).
fn check(expected: &Value, actual: &Value, path: &str, errors: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(want), Value::Object(have)) => {
            for (key, value) in want.iter().filter(|(key, _)| !key.starts_with('$')) {
                match have.get(key) {
                    Some(found) => check(value, found, &format!("{path}.{key}"), errors),
                    None if value.is_null() => {}
                    None => errors.push(format!("{path}.{key}: missing")),
                }
            }
        }
        (Value::Array(want), Value::Array(have)) if want.len() == have.len() => {
            for (i, (w, h)) in want.iter().zip(have).enumerate() {
                check(w, h, &format!("{path}[{i}]"), errors);
            }
        }
        (w, h) if w == h => {}
        (w, h) => errors.push(format!("{path}: expected {w}, got {h}")),
    }
}

#[test]
fn every_fx_host_conformance_fixture_passes_under_wasmi_and_matches_native() {
    let dir = workspace_root().join("crates/fx-host/tests/fixtures/conformance");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    let mut errors = Vec::new();
    let mut ran = 0;
    for path in &paths {
        let case: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        if !case["targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "wasm")
        {
            continue;
        }
        ran += 1;
        let name = case["name"].as_str().unwrap();
        let flags = case["support"].as_u64().unwrap_or(0) as u32;
        let request: Vec<u8> = match case["request_raw"].as_str() {
            Some(raw) => raw.as_bytes().to_vec(),
            None => serde_json::to_vec(&case["request"]).unwrap(),
        };
        let mut wasm = fx_wasm(flags);
        let mut native = fx_host::Session::new(fx_host::SupportFlags(flags));
        let mut exchange = |label: &str, input: &[u8], is_begin: bool, expected: &Value| {
            let (wasm_out, native_out) = if is_begin {
                (wasm.begin(input), Driver::begin(&mut native, input))
            } else {
                (wasm.command(input), Driver::command(&mut native, input))
            };
            if wasm_out != native_out {
                errors.push(format!(
                    "{name} {label}: wasm {} != native {}",
                    String::from_utf8_lossy(&wasm_out),
                    String::from_utf8_lossy(&native_out)
                ));
            }
            match serde_json::from_slice::<Value>(&wasm_out) {
                Ok(actual) => check(expected, &actual, &format!("{name} {label}"), &mut errors),
                Err(_) => errors.push(format!("{name} {label}: the response is not JSON")),
            }
        };
        exchange("begin", &request, true, &case["begin"]);
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let command: Vec<u8> = match step["send_raw"].as_str() {
                Some(raw) => raw.as_bytes().to_vec(),
                None => {
                    let mut object: Map<String, Value> = step["send"].as_object().unwrap().clone();
                    object.insert("v".into(), 1.into());
                    serde_json::to_vec(&Value::Object(object)).unwrap()
                }
            };
            exchange(&format!("step {i}"), &command, false, &step["expect"]);
        }
    }
    assert!(ran >= 20, "only {ran} fixtures target wasm");
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

// ---------------------------------------------------------------------------------------------
// chip-wasm-host
// ---------------------------------------------------------------------------------------------

const REQUEST_SELFTEST: &str =
    r#"{"decision":"request_capability","capability":"compute.selftest"}"#;
const COMPLETE: &str = r#"{"decision":"complete","summary":"the self test passed"}"#;

/// The same work document as `tests/session.rs`.
fn begin_document() -> Value {
    json!({
        "v": 1,
        "work_id": "w",
        "goal": "Run the self test and report the result",
        "model": "model-x",
        "request_seed": "6368697000000000",
        "options_policy": "drop_unsupported",
        "capabilities": [{
            "id": "compute.selftest",
            "name": "selftest",
            "description": "runs the self test",
            "inputs": [{"name": "n", "required": false}]
        }]
    })
}

/// What the native FX executor over a scripted port returns, as the report a host sends
/// (the same helper as `tests/session.rs`).
fn native_fx_report(request: &Value, attempt: FakePort) -> Value {
    let request = parse_request(request.to_string().as_bytes(), &Limits::default())
        .expect("the session builds an FX request that FX itself accepts");
    let mut capability = SingleAttemptCapability::new(attempt);
    let report = execute(&mut capability, &request, &Limits::default(), &mut NoHost);
    serde_json::from_slice(&encode_report(&report)).unwrap()
}

/// Runs Chip's inference request through the fx-host *Wasm module* with a scripted provider,
/// checks that the FX module agrees with native fx-host, and returns the report envelope Chip
/// reads.
fn wasm_fx_report(request: &Value, provider: Provider<'_>) -> Value {
    let mut fx = Recorder::new(fx_wasm(0));
    let done = drive_fx(&mut fx, request.to_string().as_bytes(), provider);
    assert_native_agrees(
        "fx-host serving chip",
        &fx.transcript,
        fx_host::Session::new(fx_host::SupportFlags(0)),
    );
    json!({"v": fx_host::PROTOCOL_VERSION, "report": done["report"]})
}

#[test]
fn a_multi_step_work_runs_in_the_chip_module_with_fx_in_the_fx_module_and_matches_native() {
    let mut chip = Recorder::new(chip_wasm());

    let first = chip.begin(&begin_document());
    assert_eq!(first["status"], "need_inference", "{first}");
    assert_eq!(first["request_id"].as_str().unwrap().len(), 32);
    assert_eq!(
        first["request"]["correlation"]["request_id"], first["request_id"],
        "the FX request carries the id Chip issued"
    );

    // The host answers the inference by running Chip's FX request through the FX module.
    let report = wasm_fx_report(&first["request"], Provider::Answers(REQUEST_SELFTEST));
    assert_eq!(
        report["report"]["diagnostics"]["delivery"],
        "confirmed_response"
    );
    assert_eq!(
        report["report"]["diagnostics"]["request_id"],
        first["request_id"]
    );
    let second = chip.command(&json!({"cmd": "inference_result", "report": report}));
    assert_eq!(second["status"], "need_execution", "{second}");
    assert_eq!(second["execution"]["capability"], "compute.selftest");
    assert!(
        second["execution"]["id"]
            .as_str()
            .unwrap()
            .starts_with("w-exec-")
    );

    // The host's capability answers deterministically.
    let third = chip.command(&json!({
        "cmd": "execution_result", "status": "success",
        "output": "self test passed", "receipt_id": "sha256:r",
    }));
    assert_eq!(third["status"], "need_inference", "{third}");
    assert_ne!(third["request_id"], first["request_id"]);
    assert!(
        third["request"]["messages"]
            .to_string()
            .contains("self test passed"),
        "the observation reaches the next inference"
    );

    let report = wasm_fx_report(&third["request"], Provider::Answers(COMPLETE));
    let done = chip.command(&json!({"cmd": "inference_result", "report": report}));
    assert_eq!(done["status"], "done", "{done}");
    let result = &done["result"];
    assert_eq!(result["state"], "completed");
    assert_eq!(result["summary"], "the self test passed");
    assert_eq!(result["measurement"]["model_calls"], 2);
    assert_eq!(result["measurement"]["executions"], 1);
    assert_eq!(result["trajectory_problems"], json!([]));

    let late = chip.command(&json!({"cmd": "cancel"}));
    assert_eq!(late, json!({"status": "done", "ignored": true}));

    assert_native_agrees(
        "chip multi-step work",
        &chip.transcript,
        chip_wasm_host::Session::new(),
    );
}

#[test]
fn the_same_work_answered_by_the_native_fx_executor_matches_native() {
    // As tests/session.rs: inference answered by fx-core over a scripted port, natively.
    let mut chip = Recorder::new(chip_wasm());
    let first = chip.begin(&begin_document());
    let second = chip.command(&json!({
        "cmd": "inference_result",
        "report": native_fx_report(&first["request"], FakePort::text(&[REQUEST_SELFTEST])),
    }));
    assert_eq!(second["status"], "need_execution", "{second}");
    let third = chip.command(&json!({
        "cmd": "execution_result", "status": "failure", "output": "boom",
    }));
    assert_eq!(
        third["status"], "need_inference",
        "a failed capability is an observation"
    );
    assert!(third["request"]["messages"].to_string().contains("boom"));
    let done = chip.command(&json!({
        "cmd": "inference_result",
        "report": native_fx_report(&third["request"], FakePort::text(&[COMPLETE])),
    }));
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["result"]["measurement"]["executions"], 1);
    assert_eq!(done["result"]["measurement"]["model_calls"], 2);
    assert_native_agrees(
        "chip work over native FX",
        &chip.transcript,
        chip_wasm_host::Session::new(),
    );
}

fn assert_fx_failure_ends_the_work(chip: &mut Recorder<WasmSession>, done: &Value) {
    assert_eq!(
        done["status"], "done",
        "an ambiguous attempt is not repeated: {done}"
    );
    let result = &done["result"];
    assert_eq!(result["state"], "failed");
    assert_eq!(result["delivery"], "possibly_sent", "{done}");
    let reason = result["reason"].as_str().unwrap();
    assert!(
        reason.contains("possibly_sent") && reason.contains("not retried"),
        "{reason}"
    );
    assert_eq!(
        result["measurement"]["model_calls"], 1,
        "no second inference"
    );
    assert_eq!(result["measurement"]["executions"], 0);
    // Nothing more is asked of the host.
    let late = chip.command(&json!({"cmd": "cancel"}));
    assert_eq!(late, json!({"status": "done", "ignored": true}));
}

#[test]
fn an_fx_failure_ends_the_work_with_its_delivery_and_asks_for_no_second_inference() {
    // Mirrors the native test of the same name: the native FX executor reports the failure.
    let mut chip = Recorder::new(chip_wasm());
    let first = chip.begin(&begin_document());
    let mut port = FakePort::new(Vec::new());
    port.invoke = Err(failure(PortFailureKind::ProviderError, Observed::Invoked));
    let done = chip.command(&json!({
        "cmd": "inference_result", "report": native_fx_report(&first["request"], port),
    }));
    assert_fx_failure_ends_the_work(&mut chip, &done);
    assert_native_agrees(
        "chip fx failure (native FX)",
        &chip.transcript,
        chip_wasm_host::Session::new(),
    );

    // The same failure produced by the FX Wasm module and carried into the Chip Wasm module.
    let mut chip = Recorder::new(chip_wasm());
    let first = chip.begin(&begin_document());
    let report = wasm_fx_report(&first["request"], Provider::FailsAfterSending);
    assert_eq!(report["report"]["outcome"]["delivery"], "possibly_sent");
    let done = chip.command(&json!({"cmd": "inference_result", "report": report}));
    assert_fx_failure_ends_the_work(&mut chip, &done);
    assert_native_agrees(
        "chip fx failure (FX module)",
        &chip.transcript,
        chip_wasm_host::Session::new(),
    );
}

#[test]
fn chip_malformed_input_is_an_error_response_never_a_trap() {
    let mut chip = Recorder::new(chip_wasm());
    let not_begun = chip.command_bytes(br#"{"cmd":"cancel"}"#.to_vec());
    assert_eq!(not_begun["error"]["code"], "not_begun");
    for (raw, code) in [
        (&b"{not json"[..], "invalid_document"),
        (&b""[..], "invalid_document"),
        (&br#"{"v":2}"#[..], "unsupported_version"),
        (
            &br#"{"v":1,"work_id":"w","goal":"g","model":"m"}"#[..],
            "invalid_document",
        ),
    ] {
        let response = chip.begin_bytes(raw.to_vec());
        assert_eq!(response["status"], "error", "{response}");
        assert_eq!(
            response["error"]["code"],
            code,
            "{}",
            String::from_utf8_lossy(raw)
        );
    }

    let first = chip.begin(&begin_document());
    assert_eq!(
        first["status"], "need_inference",
        "a good begin works after rejected ones"
    );
    for (raw, code) in [
        (&b"{not json"[..], "invalid_command"),
        (&br#"{"cmd":"fly"}"#[..], "unknown_command"),
        (
            &br#"{"cmd":"execution_result","status":"success"}"#[..],
            "unexpected_command",
        ),
        (&br#"{"cmd":"inference_result"}"#[..], "invalid_command"),
        (&br#"{"cmd":"inference_result","report":{"v":9}}"#[..], ""),
    ] {
        let response = chip.command_bytes(raw.to_vec());
        if code.is_empty() {
            // An unreadable FX report is not an error of the protocol: Chip fails the model call
            // closed and ends the work, without a second inference.
            assert_eq!(response["status"], "done", "{response}");
            assert_eq!(response["result"]["state"], "failed");
            assert_eq!(response["result"]["measurement"]["model_calls"], 1);
        } else {
            assert_eq!(response["status"], "error", "{response}");
            assert_eq!(
                response["error"]["code"],
                code,
                "{}",
                String::from_utf8_lossy(raw)
            );
        }
    }
    assert_native_agrees(
        "chip malformed input",
        &chip.transcript,
        chip_wasm_host::Session::new(),
    );

    assert_bad_handles_are_rejected(&mut chip.inner, begin_document().to_string().as_bytes());
}

#[test]
fn a_host_error_crosses_the_boundary_as_a_failed_model_call() {
    let mut chip = Recorder::new(chip_wasm());
    chip.begin(&begin_document());
    let done = chip.command(&json!({"cmd": "inference_result", "host_error": "the bridge failed"}));
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["result"]["state"], "failed");
    assert_eq!(done["result"]["measurement"]["model_calls"], 1);
    assert_native_agrees(
        "chip host error",
        &chip.transcript,
        chip_wasm_host::Session::new(),
    );
}
