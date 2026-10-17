//! The shared native provider: one worker, one request, polled through a channel.
//!
//! Every provider uses this code; only the [`Wire`] differs (request, headers, stream format).
//! One `invoke` starts one worker thread that makes one HTTP request and streams its response
//! into a channel; `poll` drains the channel with a short wait. There is no retry, no redirect
//! following and no second request.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use fx_core::{AttemptBudget, Request};
use fx_single_attempt::{
    AbortReport, Observed, PortFailure, PortFailureKind, PortItem, PortPoll, PortSupport,
    ProviderPort,
};

use crate::config::{AnthropicConfig, OpenAiConfig};
use crate::sse::Framer;
use crate::wire::{Step, Wire};

/// How long one `poll` waits for the worker before returning empty.
const POLL_WAIT: Duration = Duration::from_millis(25);
/// The most of an error body that is read and kept.
const ERROR_BODY_LIMIT: u64 = 8 * 1024;
const MESSAGE_LIMIT: usize = 512;

/// A native provider over wire format `W`. See [`OpenAiCompatible`] and [`Anthropic`].
pub struct Native<W: Wire> {
    wire: Arc<W>,
}

impl<W: Wire> std::fmt::Debug for Native<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Native { .. }")
    }
}

/// The native provider for OpenAI-compatible `/chat/completions` endpoints (streaming):
/// OpenAI, OpenRouter, LM Studio and others. Text conversations and an output token cap.
pub type OpenAiCompatible = Native<crate::openai::OpenAiWire>;

/// The native provider for the Anthropic Messages API (streaming). Text conversations and an
/// output token cap.
pub type Anthropic = Native<crate::anthropic::AnthropicWire>;

impl OpenAiCompatible {
    /// Creates the provider from host configuration.
    pub fn new(config: OpenAiConfig) -> Self {
        Self {
            wire: Arc::new(crate::openai::OpenAiWire { config }),
        }
    }
}

impl Anthropic {
    /// Creates the provider from host configuration.
    pub fn new(config: AnthropicConfig) -> Self {
        Self {
            wire: Arc::new(crate::anthropic::AnthropicWire { config }),
        }
    }
}

enum Message {
    /// Items decoded from the stream; the first message of this kind proves output arrived.
    Items(Vec<PortItem>),
    Failed(PortFailure),
    Ended,
}

/// Per-call state: the receiving end of the worker and what has been observed.
pub struct NativeCall {
    rx: Receiver<Message>,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    pending: VecDeque<PortItem>,
    failure: Option<PortFailure>,
    observed: Observed,
    ended: bool,
    aborted: bool,
}

impl std::fmt::Debug for NativeCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeCall { .. }")
    }
}

/// Former name of [`NativeCall`].
pub type OpenAiCall = NativeCall;

fn failure(kind: PortFailureKind, message: impl Into<String>, observed: Observed) -> PortFailure {
    PortFailure {
        kind,
        message: message.into(),
        observed,
        status: None,
        retry_after: None,
    }
}

impl<W: Wire> ProviderPort for Native<W> {
    type Call = NativeCall;

    fn support(&self) -> PortSupport {
        self.wire.support()
    }

    fn invoke(
        &mut self,
        request: &Request,
        budget: Option<AttemptBudget>,
    ) -> Result<NativeCall, PortFailure> {
        let body = self
            .wire
            .body(request)
            .map_err(|why| failure(PortFailureKind::Unsupported, why, Observed::NotInvoked))?;
        let deadline_after = budget.map_or(self.wire.settings().default_deadline, |b| b.0);
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            wire: Arc::clone(&self.wire),
            deadline: deadline_after,
            cancel: Arc::clone(&cancel),
            tx,
        };
        std::thread::Builder::new()
            .name("fx-native-provider".into())
            .spawn(move || worker.run(&body))
            .map_err(|_| {
                failure(
                    PortFailureKind::ProviderError,
                    "the worker thread could not start",
                    Observed::NotInvoked,
                )
            })?;
        Ok(NativeCall {
            rx,
            cancel,
            deadline: Instant::now() + deadline_after,
            pending: VecDeque::new(),
            failure: None,
            observed: Observed::Invoked,
            ended: false,
            aborted: false,
        })
    }

    fn poll(&mut self, call: &mut NativeCall, max_items: usize) -> Result<PortPoll, PortFailure> {
        if call.aborted {
            return Err(failure(
                PortFailureKind::Aborted,
                "the call was aborted",
                call.observed,
            ));
        }
        if call.pending.is_empty() && call.failure.is_none() && !call.ended {
            match call.rx.recv_timeout(POLL_WAIT) {
                Ok(message) => call.accept(message),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    call.failure = Some(failure(
                        PortFailureKind::ProviderError,
                        "the provider worker stopped unexpectedly",
                        call.observed,
                    ));
                }
            }
        }
        // Whatever else is already waiting, without blocking.
        while call.failure.is_none() && !call.ended && call.pending.len() < max_items {
            match call.rx.try_recv() {
                Ok(message) => call.accept(message),
                Err(_) => break,
            }
        }
        let take = max_items.min(call.pending.len());
        let items: Vec<PortItem> = call.pending.drain(..take).collect();
        if items.is_empty() && call.pending.is_empty() {
            if let Some(f) = call.failure.take() {
                return Err(f);
            }
            if !call.ended && Instant::now() >= call.deadline {
                call.cancel.store(true, Ordering::SeqCst);
                return Err(failure(
                    PortFailureKind::Timeout,
                    "the attempt budget elapsed",
                    call.observed,
                ));
            }
        }
        let ended = call.ended && call.pending.is_empty() && call.failure.is_none();
        Ok(PortPoll {
            items,
            observed: call.observed,
            ended,
        })
    }

    fn abort(&mut self, call: &mut NativeCall) -> AbortReport {
        call.aborted = true;
        call.cancel.store(true, Ordering::SeqCst);
        // A blocking read cannot be interrupted, so the closing of the transport is not
        // observed here. The worker stops at its next read or at its deadline.
        AbortReport {
            transport_closed: false,
        }
    }

    fn release(&mut self, call: NativeCall) {
        call.cancel.store(true, Ordering::SeqCst);
        drop(call);
    }
}

impl NativeCall {
    fn accept(&mut self, message: Message) {
        match message {
            Message::Items(items) => {
                self.observed = Observed::OutputObserved;
                self.pending.extend(items);
            }
            Message::Failed(f) => {
                if f.observed > self.observed {
                    self.observed = f.observed;
                }
                self.failure = Some(PortFailure {
                    observed: self.observed,
                    ..f
                });
            }
            Message::Ended => self.ended = true,
        }
    }
}

struct Worker<W: Wire> {
    wire: Arc<W>,
    deadline: Duration,
    cancel: Arc<AtomicBool>,
    tx: Sender<Message>,
}

impl<W: Wire> Worker<W> {
    fn run(self, body: &[u8]) {
        let result = self.exchange(body);
        let _ = self.tx.send(match result {
            Ok(()) => Message::Ended,
            Err(f) => Message::Failed(f),
        });
    }

    fn scrub(&self, text: &str) -> String {
        let mut text = match self.wire.secret() {
            Some(secret) => secret.scrub(text),
            None => text.to_owned(),
        };
        if let Some((_, password)) = self
            .wire
            .settings()
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.credentials.as_ref())
        {
            text = password.scrub(&text);
        }
        if text.len() > MESSAGE_LIMIT {
            let mut cut = MESSAGE_LIMIT;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        text
    }

    fn exchange(&self, body: &[u8]) -> Result<(), PortFailure> {
        let settings = self.wire.settings();
        let mut builder = ureq::Agent::config_builder();
        if let Some(roots) = &settings.root_certs {
            builder = builder.tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(roots.clone())
                    .build(),
            );
        }
        // The host's proxy, or none: never one from the environment.
        let proxy = match &settings.proxy {
            Some(proxy) => match proxy.build() {
                Ok(proxy) => Some(proxy),
                Err(error) => {
                    return Err(failure(
                        PortFailureKind::ProviderError,
                        error.0,
                        Observed::NotInvoked,
                    ));
                }
            },
            None => None,
        };
        let agent: ureq::Agent = builder
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(proxy)
            .timeout_connect(Some(settings.connect_timeout))
            .timeout_global(Some(self.deadline))
            .build()
            .into();
        let mut request = agent.post(&self.wire.url());
        for (name, value) in self.wire.headers() {
            request = request.header(name, value);
        }
        let mut response = match request.send(body) {
            Ok(response) => response,
            Err(error) => return Err(self.transport_failure(&error)),
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let mut text = String::new();
            let _ = response
                .body_mut()
                .as_reader()
                .take(ERROR_BODY_LIMIT)
                .read_to_string(&mut text);
            let detail = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.pointer("/error/message")?.as_str().map(str::to_owned))
                .unwrap_or(text);
            return Err(PortFailure {
                kind: PortFailureKind::ProviderError,
                message: self.scrub(&format!("HTTP {status}: {}", detail.trim())),
                observed: Observed::Invoked,
                status: Some(status),
                retry_after,
            });
        }
        let is_stream = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("text/event-stream")
            });
        if !is_stream {
            return Err(failure(
                PortFailureKind::MalformedOutput,
                "the response is not an event stream",
                Observed::Invoked,
            ));
        }
        self.stream(response.body_mut().as_reader())
    }

    /// Every failure after `invoke` returned is at least `Invoked`: the call had started, and
    /// a connect failure seen later cannot prove nothing left the process (a send can fail
    /// part-way), so it is never reported as `NotInvoked`.
    fn transport_failure(&self, error: &ureq::Error) -> PortFailure {
        let kind = match error {
            ureq::Error::Timeout(_) => PortFailureKind::Timeout,
            _ => PortFailureKind::ProviderError,
        };
        failure(kind, self.scrub(&error.to_string()), Observed::Invoked)
    }

    fn stream(&self, mut reader: impl Read) -> Result<(), PortFailure> {
        let mut framer = Framer::default();
        let mut decoder = self.wire.decoder();
        let mut buf = [0u8; 4096];
        let mut seen_output = false;
        let observed = |seen: bool| {
            if seen {
                Observed::OutputObserved
            } else {
                Observed::Invoked
            }
        };
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Ok(());
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => {
                    // A complete 2xx response was received, so the provider answered even if
                    // it said nothing before it stopped.
                    return Err(failure(
                        PortFailureKind::IncompleteStream,
                        "the stream ended without its terminal marker",
                        Observed::OutputObserved,
                    ));
                }
                Ok(n) => n,
                Err(error) => {
                    let timed_out = error.to_string().to_ascii_lowercase().contains("timeout")
                        || error.kind() == std::io::ErrorKind::TimedOut;
                    // A broken connection is an error the transport raised, not a stream that
                    // ended; the text already delivered is kept by the executor either way.
                    let kind = if timed_out {
                        PortFailureKind::Timeout
                    } else {
                        PortFailureKind::ProviderError
                    };
                    return Err(failure(
                        kind,
                        self.scrub(&error.to_string()),
                        observed(seen_output),
                    ));
                }
            };
            let frames = framer.push(&buf[..n]).map_err(|why| {
                failure(PortFailureKind::MalformedOutput, why, observed(seen_output))
            })?;
            for frame in frames {
                match decoder.feed(frame.event.as_deref(), &frame.data) {
                    Ok(Step::Items(items)) => {
                        seen_output = true;
                        // The first event always reports arrival, even when it carries no text.
                        let _ = self.tx.send(Message::Items(items));
                    }
                    Ok(Step::Done(done)) => {
                        let _ = self.tx.send(Message::Items(vec![done]));
                        return Ok(());
                    }
                    Ok(Step::ProviderError(message)) => {
                        return Err(failure(
                            PortFailureKind::ProviderError,
                            self.scrub(&message),
                            observed(seen_output),
                        ));
                    }
                    Err(why) => {
                        return Err(failure(
                            PortFailureKind::MalformedOutput,
                            why,
                            observed(seen_output),
                        ));
                    }
                }
            }
        }
    }
}
