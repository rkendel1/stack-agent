//! The host-fed provider port.
//!
//! The provider lives in the host (Easy-LLM in JavaScript). The host reports its
//! results to the session as commands, and this port replays them when the executor
//! polls. It does no I/O, blocks on nothing, and never invokes anything: an empty
//! queue is "nothing available yet", which the executor surfaces as an idle step.

use std::collections::VecDeque;

use fx_core::{AttemptBudget, Request};
use fx_single_attempt::{
    AbortReport, Observed, PortFailure, PortFailureKind, PortPoll, PortSupport, ProviderPort,
};

#[derive(Debug)]
pub(crate) struct HostPort {
    support: PortSupport,
    /// What the host reported when it invoked (or declined to invoke) the provider.
    pub(crate) invoke_result: Option<Result<(), PortFailure>>,
    queue: VecDeque<Result<PortPoll, PortFailure>>,
    observed: Observed,
    pub(crate) abort_called: bool,
    pub(crate) transport_closed: bool,
    pub(crate) invokes: u32,
    pub(crate) polls: u64,
    pub(crate) releases: u32,
}

impl HostPort {
    pub(crate) fn new(support: PortSupport) -> Self {
        HostPort {
            support,
            invoke_result: None,
            queue: VecDeque::new(),
            observed: Observed::NotInvoked,
            abort_called: false,
            transport_closed: false,
            invokes: 0,
            polls: 0,
            releases: 0,
        }
    }

    pub(crate) fn support(&self) -> PortSupport {
        self.support
    }

    pub(crate) fn feed(&mut self, poll: Result<PortPoll, PortFailure>) {
        self.queue.push_back(poll);
    }
}

impl ProviderPort for HostPort {
    type Call = ();

    fn support(&self) -> PortSupport {
        self.support
    }

    fn invoke(
        &mut self,
        _request: &Request,
        _budget: Option<AttemptBudget>,
    ) -> Result<(), PortFailure> {
        self.invokes += 1;
        match self.invoke_result.take() {
            Some(Ok(())) => {
                self.observed = self.observed.max(Observed::Invoked);
                Ok(())
            }
            Some(Err(failure)) => {
                self.observed = self.observed.max(failure.observed);
                Err(failure)
            }
            // The session only steps after the host reported a result, so this is a
            // protocol bug. Fail closed and claim nothing was sent.
            None => Err(PortFailure {
                kind: PortFailureKind::ProviderError,
                message: "the host reported no invocation result".to_owned(),
                observed: Observed::NotInvoked,
                status: None,
                retry_after: None,
            }),
        }
    }

    fn poll(&mut self, _call: &mut (), max_items: usize) -> Result<PortPoll, PortFailure> {
        self.polls += 1;
        match self.queue.pop_front() {
            None => Ok(PortPoll {
                items: Vec::new(),
                observed: self.observed,
                ended: false,
            }),
            Some(Err(failure)) => {
                self.observed = self.observed.max(failure.observed);
                Err(failure)
            }
            Some(Ok(mut poll)) => {
                self.observed = self.observed.max(poll.observed);
                if poll.items.len() > max_items {
                    // Hand over only what the executor asked for; keep the rest, and
                    // report the stream as ended only with the last of its items.
                    let rest = poll.items.split_off(max_items);
                    self.queue.push_front(Ok(PortPoll {
                        items: rest,
                        observed: poll.observed,
                        ended: poll.ended,
                    }));
                    poll.ended = false;
                }
                Ok(poll)
            }
        }
    }

    fn abort(&mut self, _call: &mut ()) -> AbortReport {
        self.abort_called = true;
        // Only the host can observe the transport closing, and it reports that
        // explicitly. Nothing is assumed here.
        AbortReport {
            transport_closed: self.transport_closed,
        }
    }

    fn release(&mut self, _call: ()) {
        self.releases += 1;
    }
}
