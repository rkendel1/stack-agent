//! [`Session`]: one FX attempt driven by a host, as a message protocol.
//!
//! The host owns the provider call and the event loop. The session owns the FX
//! executor: it validates the request, decides when the provider may be invoked,
//! normalizes the provider's results, enforces bounds, and produces the one report.
//! Everything crosses the boundary as bytes, so the same session runs natively (the
//! Rust tests) and inside a WebAssembly instance (the Node host) with no difference.
//!
//! ```text
//! host                                   session
//!  |  begin(request bytes)  ----------->  decode + validate (fx-core)
//!  |  <------ need_invoke | done          invalid request: done, provider never invoked
//!  |  [host invokes the provider]
//!  |  invoke_result(ok | failure) ------>
//!  |  step ----------------------------->  fx_core::Execution::step ...
//!  |  <------ need_input | done           events forwarded so far, or the report
//!  |  feed(items) / feed_failure ------->  [host awaits the provider, then feeds]
//!  |  step ... until done
//!  |  cancel ------------------------->   honored at the next step
//! ```
//!
//! Cancellation honesty: a cancel that arrives before the host invoked the provider
//! ends the attempt as `Cancelled { DefinitelyUnsent, NotStarted }`. Once the host
//! reports an invocation, a cancel is applied after the executor started, so the
//! outcome keeps the delivery observed so far and never claims the provider call did
//! not happen.

use fx_core::{
    Event, Execution, ExecutionReport, Flow, Host, Limits, Step, encode_event, encode_report,
    encode_request, execute_document, parse_request,
};
use fx_single_attempt::{
    Observed, PortFailure, PortPoll, SingleAttemptCapability, first_unsupported,
};
use serde_json::{Map, Value, json};

use crate::port::HostPort;
use crate::protocol::{CommandDoc, PROTOCOL_VERSION, SupportFlags};

/// Upper bound on one host command, in bytes. Bounds the memory a session will copy
/// from the host; the executor separately bounds every event and the whole attempt.
pub const MAX_COMMAND_BYTES: usize = 64 * 1024 * 1024;

type Cap = SingleAttemptCapability<HostPort>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    New,
    NeedInvoke,
    Running,
    Done,
}

#[derive(Default)]
struct SessionHost {
    cancel: bool,
    provider_invoked: bool,
    started: bool,
    events: Vec<Event>,
}

impl Host for SessionHost {
    fn cancel_requested(&mut self) -> bool {
        // Before the executor started the capability, a cancel only counts if the host
        // has not invoked the provider; otherwise it must take the normal cancel path
        // so the outcome does not claim the provider was never called.
        self.cancel && (self.started || !self.provider_invoked)
    }

    fn on_event(&mut self, event: &Event) -> Flow {
        self.events.push(event.clone());
        Flow::Continue
    }
}

/// One FX attempt driven through the host protocol.
pub struct Session {
    capability: Cap,
    host: SessionHost,
    execution: Option<Execution<Cap>>,
    state: State,
    limits: Limits,
    final_response: Option<Vec<u8>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .finish()
    }
}

fn error(code: &str, message: &str) -> Vec<u8> {
    json!({"v": PROTOCOL_VERSION, "status": "error", "error": {"code": code, "message": message}})
        .to_string()
        .into_bytes()
}

impl Session {
    /// Creates a session whose provider path can carry the features in `support`.
    pub fn new(support: SupportFlags) -> Self {
        Session {
            capability: SingleAttemptCapability::new(HostPort::new(support.support())),
            host: SessionHost::default(),
            execution: None,
            state: State::New,
            limits: Limits::DEFAULT,
            final_response: None,
        }
    }

    /// Whether the attempt has ended and its report was produced.
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Starts the attempt from a raw FX request document (the exact bytes; strict
    /// decoding depends on them). Responds `need_invoke` with the canonical request,
    /// or `done` when the request is invalid or cannot be carried, in which case the
    /// provider is never invoked.
    pub fn begin(&mut self, request: &[u8]) -> Vec<u8> {
        if self.state != State::New {
            return error("bad_state", "begin was already called");
        }
        let parsed = match parse_request(request, &self.limits) {
            Ok(parsed) => parsed,
            Err(_) => {
                // Decode and validation failures become an InvalidRequest report without
                // touching the capability.
                let report =
                    execute_document(&mut self.capability, request, &self.limits, &mut self.host);
                return self.finish(report);
            }
        };
        self.execution = Some(Execution::new(&parsed, &self.limits));
        if first_unsupported(&parsed, self.capability.port().support()).is_some() {
            // The adapter refuses before invoking; the host never needs to.
            self.state = State::Running;
            return self.run();
        }
        self.state = State::NeedInvoke;
        let canonical: Value =
            serde_json::from_slice(&encode_request(&parsed)).unwrap_or(Value::Null);
        let timeout_ms = parsed
            .budget
            .map(|b| u64::try_from(b.0.as_millis()).unwrap_or(u64::MAX));
        json!({
            "v": PROTOCOL_VERSION,
            "status": "need_invoke",
            "request": canonical,
            "timeout_ms": timeout_ms,
        })
        .to_string()
        .into_bytes()
    }

    /// Applies one host command document and returns the response document.
    pub fn command(&mut self, command: &[u8]) -> Vec<u8> {
        if command.len() > MAX_COMMAND_BYTES {
            return error("too_large", "the command exceeds the size limit");
        }
        let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(command) else {
            return error("bad_command", "the command is not a JSON object");
        };
        if object.remove("v") != Some(Value::from(PROTOCOL_VERSION)) {
            return error(
                "unsupported_version",
                "the command version is not supported",
            );
        }
        let Ok(command) = serde_json::from_value::<CommandDoc>(Value::Object(object)) else {
            return error(
                "bad_command",
                "the command is not valid for this protocol version",
            );
        };
        match command {
            CommandDoc::InvokeResult { ok, failure } => self.invoke_result(ok, failure),
            CommandDoc::Feed {
                items,
                observed,
                ended,
            } => {
                let poll = PortPoll {
                    items: items.into_iter().map(Into::into).collect(),
                    observed: observed.into(),
                    ended,
                };
                self.feed(Ok(poll))
            }
            CommandDoc::FeedFailure { failure } => self.feed(Err(failure.into())),
            CommandDoc::Cancel { transport_closed } => self.cancel(transport_closed),
            CommandDoc::Step {} => self.step(),
        }
    }

    fn invoke_result(&mut self, ok: bool, failure: Option<crate::protocol::FailureDoc>) -> Vec<u8> {
        if self.state != State::NeedInvoke {
            return error("bad_state", "no provider invocation is awaited");
        }
        let result: Result<(), PortFailure> = match (ok, failure) {
            (true, None) => Ok(()),
            (false, Some(failure)) => Err(failure.into()),
            _ => {
                return error(
                    "bad_command",
                    "ok requires no failure and a failure requires ok=false",
                );
            }
        };
        self.host.provider_invoked = match &result {
            Ok(()) => true,
            Err(failure) => failure.observed != Observed::NotInvoked,
        };
        self.capability.port_mut().invoke_result = Some(result);
        self.state = State::Running;
        json!({"v": PROTOCOL_VERSION, "status": "ready"})
            .to_string()
            .into_bytes()
    }

    fn feed(&mut self, poll: Result<PortPoll, PortFailure>) -> Vec<u8> {
        match self.state {
            State::Running => {
                self.capability.port_mut().feed(poll);
                json!({"v": PROTOCOL_VERSION, "status": "ok"})
                    .to_string()
                    .into_bytes()
            }
            // A chunk that arrives after the attempt ended is dropped, never replayed.
            State::Done => self.ignored(),
            _ => error("bad_state", "the provider has not been invoked"),
        }
    }

    fn cancel(&mut self, transport_closed: bool) -> Vec<u8> {
        match self.state {
            State::Done => self.ignored(),
            State::New => error("bad_state", "begin was not called"),
            _ => {
                self.host.cancel = true;
                self.capability.port_mut().transport_closed |= transport_closed;
                json!({"v": PROTOCOL_VERSION, "status": "ok"})
                    .to_string()
                    .into_bytes()
            }
        }
    }

    fn step(&mut self) -> Vec<u8> {
        match self.state {
            State::New => error("bad_state", "begin was not called"),
            State::Done => self.final_response.clone().unwrap_or_default(),
            // Stepping before the host reported an invocation is allowed only when the
            // host chose not to invoke because it saw a cancel.
            State::NeedInvoke if !self.host.cancel => {
                error("bad_state", "invoke_result is required before step")
            }
            State::NeedInvoke | State::Running => {
                self.state = State::Running;
                self.run()
            }
        }
    }

    fn ignored(&self) -> Vec<u8> {
        json!({"v": PROTOCOL_VERSION, "status": "done", "ignored": true})
            .to_string()
            .into_bytes()
    }

    /// Steps the executor until it needs host input or ends.
    fn run(&mut self) -> Vec<u8> {
        loop {
            let step = match self.execution.as_mut() {
                Some(execution) => execution.step(&mut self.capability, &mut self.host),
                None => return error("bad_state", "begin was not called"),
            };
            match step {
                Step::Progress => self.host.started = true,
                Step::Idle => {
                    self.host.started = true;
                    return json!({
                        "v": PROTOCOL_VERSION,
                        "status": "need_input",
                        "events": self.take_events(),
                    })
                    .to_string()
                    .into_bytes();
                }
                Step::Done(report) => return self.finish(*report),
                Step::Finished => return self.final_response.clone().unwrap_or_default(),
            }
        }
    }

    fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.host.events)
            .iter()
            .filter_map(|event| serde_json::from_slice::<Value>(&encode_event(event)).ok())
            .filter_map(|envelope| match envelope {
                Value::Object(mut map) => map.remove("event"),
                _ => None,
            })
            .collect()
    }

    fn finish(&mut self, report: ExecutionReport) -> Vec<u8> {
        self.state = State::Done;
        let mut response = Map::new();
        response.insert("v".into(), PROTOCOL_VERSION.into());
        response.insert("status".into(), "done".into());
        response.insert("events".into(), Value::Array(self.take_events()));
        let report_doc = serde_json::from_slice::<Value>(&encode_report(&report))
            .ok()
            .and_then(|mut envelope| envelope.get_mut("report").map(Value::take))
            .unwrap_or(Value::Null);
        response.insert("report".into(), report_doc);
        // Tells the host to abort the provider call: the executor cancelled the capability.
        response.insert("abort".into(), self.capability.port().abort_called.into());
        let bytes = Value::Object(response).to_string().into_bytes();
        self.final_response = Some(bytes.clone());
        bytes
    }

    /// Call counts on the provider port, for tests and diagnostics. Content-free.
    pub fn port_counts(&self) -> PortCounts {
        let port = self.capability.port();
        PortCounts {
            invokes: port.invokes,
            polls: port.polls,
            aborts: u32::from(port.abort_called),
            releases: port.releases,
        }
    }
}

/// How the executor used the host-fed provider port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortCounts {
    /// `invoke` calls (the executor's single start).
    pub invokes: u32,
    /// `poll` calls.
    pub polls: u64,
    /// `abort` calls.
    pub aborts: u32,
    /// `release` calls.
    pub releases: u32,
}
