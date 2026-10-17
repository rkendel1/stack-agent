//! A scripted [`ProviderPort`] for offline tests and for the R4 bridge conformance
//! suite. Test support, not a provider. It records non-sensitive call counts and
//! reports abort exactly as scripted.

use std::collections::VecDeque;

use fx_core::{AttemptBudget, Request};

use crate::port::{
    AbortReport, Observed, PortFailure, PortFailureKind, PortItem, PortPoll, PortSupport,
    PortUsage, ProviderPort, Terminal,
};

/// What the executor did to the port.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortObservations {
    /// `invoke` calls.
    pub invokes: u32,
    /// `poll` calls.
    pub polls: u32,
    /// `abort` calls.
    pub aborts: u32,
    /// `release` calls.
    pub releases: u32,
    /// The largest `max_items` requested.
    pub max_items_requested: usize,
    /// The budget received at `invoke`.
    pub budget: Option<AttemptBudget>,
}

/// A scripted port.
#[derive(Debug)]
pub struct FakePort {
    /// Features the port claims to carry. Defaults to everything.
    pub support: PortSupport,
    /// Result of `invoke`.
    pub invoke: Result<(), PortFailure>,
    /// Poll results, consumed in order. After the last, polls report an ended stream.
    pub polls: VecDeque<Result<PortPoll, PortFailure>>,
    /// What `abort` reports.
    pub abort: AbortReport,
    /// Observations.
    pub observations: PortObservations,
}

/// Handle for one fake call.
#[derive(Debug)]
pub struct FakeCall(());

/// A failure helper.
pub fn failure(kind: PortFailureKind, observed: Observed) -> PortFailure {
    PortFailure {
        kind,
        message: String::new(),
        observed,
        status: None,
        retry_after: None,
    }
}

impl FakePort {
    /// A port that carries every feature and replays `polls`.
    pub fn new(polls: Vec<Result<PortPoll, PortFailure>>) -> Self {
        FakePort {
            support: PortSupport {
                images: true,
                tool_schemas: true,
                tool_history: true,
                tool_choice: true,
                provider_tools: true,
                generation_options: true,
                response_format: true,
            },
            invoke: Ok(()),
            polls: polls.into(),
            abort: AbortReport {
                transport_closed: false,
            },
            observations: PortObservations::default(),
        }
    }

    /// A complete text answer: the deltas, then `done` with usage and finish `stop`.
    pub fn text(deltas: &[&str]) -> Self {
        let mut items: Vec<PortItem> = deltas
            .iter()
            .map(|d| PortItem::Text((*d).to_owned()))
            .collect();
        items.push(done(Some("stop"), Terminal::ProviderMarker));
        FakePort::new(vec![Ok(poll(items, Observed::OutputObserved, true))])
    }

    /// A port whose `invoke` fails.
    pub fn invoke_failure(failure: PortFailure) -> Self {
        let mut port = FakePort::new(Vec::new());
        port.invoke = Err(failure);
        port
    }
}

/// A poll result.
pub fn poll(items: Vec<PortItem>, observed: Observed, ended: bool) -> PortPoll {
    PortPoll {
        items,
        observed,
        ended,
    }
}

/// A `done` item with usage 12 in and 3 out.
pub fn done(finish_reason: Option<&str>, terminal: Terminal) -> PortItem {
    PortItem::Done {
        usage: Some(PortUsage {
            input_tokens: Some(12),
            output_tokens: Some(3),
            ..PortUsage::default()
        }),
        finish_reason: finish_reason.map(str::to_owned),
        model: None,
        terminal,
    }
}

impl ProviderPort for FakePort {
    type Call = FakeCall;

    fn support(&self) -> PortSupport {
        self.support
    }

    fn invoke(
        &mut self,
        _request: &Request,
        budget: Option<AttemptBudget>,
    ) -> Result<FakeCall, PortFailure> {
        self.observations.invokes += 1;
        self.observations.budget = budget;
        self.invoke.clone().map(|()| FakeCall(()))
    }

    fn poll(&mut self, _call: &mut FakeCall, max_items: usize) -> Result<PortPoll, PortFailure> {
        self.observations.polls += 1;
        self.observations.max_items_requested =
            self.observations.max_items_requested.max(max_items);
        self.polls
            .pop_front()
            .unwrap_or_else(|| Ok(poll(Vec::new(), Observed::Invoked, true)))
    }

    fn abort(&mut self, _call: &mut FakeCall) -> AbortReport {
        self.observations.aborts += 1;
        self.abort
    }

    fn release(&mut self, _call: FakeCall) {
        self.observations.releases += 1;
    }
}
