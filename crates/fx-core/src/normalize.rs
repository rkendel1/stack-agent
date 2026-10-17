//! Normalization of capability events into the FX contract.
//!
//! [`Normalizer`] is a pure, step-driven state machine: it never calls the
//! capability, has no clock and allocates only within [`Limits`]. The synchronous
//! [`crate::executor::execute`] drives it, and a host-driven adapter (R4) can drive it
//! directly event by event.
//!
//! Rules (oracle references are to `src/gateway/client.zig` tests unless noted):
//!
//! - Every event is bounds-checked first. A field over its bound is a
//!   `ResourceLimit` violation; an empty identifier is a `ProtocolViolation`.
//! - `finish` is terminal and authoritative (I2). Anything after the first terminal
//!   event is dropped and counted as an anomaly, never forwarded (I1;
//!   `consumeSseStream ignores conflicting and late stream events without mutation`).
//! - Streamed tool input is display-only (I5). A final `tool_call` whose arguments
//!   are absent (empty) may borrow the arguments of an *ended* stream with the exact
//!   same id and name; an open stream, a different name, an empty or duplicate id
//!   never lend arguments (`... accepts authoritative final input before end`,
//!   `... rejects fallback from an open stream`,
//!   `... rejects a final tool name that conflicts with streamed identity`,
//!   `... malformed final identity borrows no streamed state`,
//!   `... duplicate final ids do not reuse finalized stream fallback`).
//! - Proposal arguments are always valid JSON after normalization: malformed or
//!   duplicate-key input is replaced by `{}` and flagged (I6;
//!   `... replaces malformed trailing or duplicate-key serialized final input with safe JSON`).
//! - Duplicate proposal ids are preserved and flagged for admission, never merged (I7).
//! - Provider tool records are accepted only for tools the request declared as
//!   provider-executed, and never for an id used by a client proposal (I8;
//!   `... rejects provider results for local calls`,
//!   `... rejects ambiguous provider result correlation`). The first final record
//!   wins; a preliminary record is replaced by a later one
//!   (`... replaces preliminary results and preserves the first final result`).
//! - Usage merges field by field; an absent field stays unknown, never zero.
//! - Metadata keeps the first value of each field; a conflicting later value is an
//!   anomaly (the oracle marks `generation_metadata_invalid`).
//! - Accumulated text is bounded by `max_completion_bytes`; deltas are still
//!   forwarded in full and the completion is marked truncated.

use crate::contract::{
    Completion, DeliveryCertainty, ErrorKind, Event, FinishReason, Limits, Metadata, PartialOutput,
    ProviderToolRecord, Request, ToolCallProposal, ToolExecution, Usage,
};
use crate::json_check;
use crate::validate::{ValidationCode, validate_event_bounds};

/// Integrity of a proposal's arguments after normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArgumentIntegrity {
    /// The arguments are the valid JSON the model produced.
    Valid,
    /// The arguments were malformed or absent and were replaced by `{}`.
    Replaced,
}

/// Where a proposal's arguments came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArgumentSource {
    /// The final `tool_call` event.
    Final,
    /// An ended streamed tool input with the exact same id and name.
    StreamedFallback,
    /// Neither: arguments were absent and no eligible stream existed.
    Missing,
}

/// Identity of a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProposalIdentity {
    /// A non-empty id not used by an earlier proposal.
    Valid,
    /// The id is empty.
    EmptyId,
    /// An earlier proposal used the same id.
    DuplicateId,
    /// A streamed input with this id announced a different tool name.
    ConflictingStreamedName,
}

/// What the executor observed about one proposal. Index-aligned with
/// `Completion.proposals`. Data for the consumer's admission decision; it grants
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProposalCheck {
    /// Identity.
    pub identity: ProposalIdentity,
    /// Argument integrity.
    pub integrity: ArgumentIntegrity,
    /// Argument source.
    pub source: ArgumentSource,
}

/// Why the normalizer rejected the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Violation {
    /// An event field exceeded its bound.
    OversizedEvent,
    /// An event identifier or name was empty.
    EmptyEventField,
    /// More than `max_events_per_attempt` events.
    TooManyEvents,
    /// More than `max_proposals` proposals.
    TooManyProposals,
    /// More than `max_proposals` streamed tool inputs.
    TooManyStreamedInputs,
    /// More than `max_proposals` provider tool records.
    TooManyProviderRecords,
    /// A provider record for a tool the request did not declare as provider-executed.
    ProviderRecordForUndeclaredTool,
    /// A provider record whose id collides with a client proposal.
    ProviderRecordIdCollision,
    /// A provider record whose input or result is not valid JSON.
    MalformedProviderRecord,
}

impl Violation {
    /// The failure kind this violation produces.
    pub fn kind(self) -> ErrorKind {
        match self {
            Violation::OversizedEvent
            | Violation::TooManyEvents
            | Violation::TooManyProposals
            | Violation::TooManyStreamedInputs
            | Violation::TooManyProviderRecords => ErrorKind::ResourceLimit,
            _ => ErrorKind::ProtocolViolation,
        }
    }
}

/// Result of pushing one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// The normalized event to forward to observers.
    Forward(Event),
    /// The event was an anomaly and is dropped (not forwarded).
    Dropped,
    /// The normalized terminal `finish` event; the stream is now complete.
    Finished(Event),
}

#[derive(Debug)]
struct StreamedInput {
    id: String,
    name: String,
    arguments: String,
    ended: bool,
    overflowed: bool,
    lent: bool,
}

/// Step-driven event normalizer for one attempt.
#[derive(Debug)]
pub struct Normalizer {
    /// Names of the provider-executed tools the request declared.
    provider_tools: Vec<String>,
    limits: Limits,
    finished: Option<FinishReason>,
    delivery: DeliveryCertainty,
    content: String,
    content_truncated: bool,
    text_bytes: u64,
    reasoning_bytes: u64,
    tool_input_bytes: u64,
    events: usize,
    anomalies: u32,
    streams: Vec<StreamedInput>,
    proposals: Vec<ToolCallProposal>,
    checks: Vec<ProposalCheck>,
    records: Vec<ProviderToolRecord>,
    usage: Option<Usage>,
    metadata: Metadata,
}

impl Normalizer {
    /// Creates a normalizer for an already validated request.
    pub fn new(request: &Request, limits: &Limits) -> Self {
        Normalizer {
            provider_tools: request
                .tools
                .iter()
                .filter(|t| t.execution == ToolExecution::Provider)
                .map(|t| t.name.clone())
                .collect(),
            limits: *limits,
            finished: None,
            delivery: DeliveryCertainty::DefinitelyUnsent,
            content: String::new(),
            content_truncated: false,
            text_bytes: 0,
            reasoning_bytes: 0,
            tool_input_bytes: 0,
            events: 0,
            anomalies: 0,
            streams: Vec::new(),
            proposals: Vec::new(),
            checks: Vec::new(),
            records: Vec::new(),
            usage: None,
            metadata: Metadata::default(),
        }
    }

    /// Records delivery certainty reported by the capability. Never moves backwards.
    pub fn observe_delivery(&mut self, observed: DeliveryCertainty) {
        self.delivery = self.delivery.advance(observed);
    }

    /// Current delivery certainty.
    pub fn delivery(&self) -> DeliveryCertainty {
        self.delivery
    }

    /// Whether a terminal `finish` has been accepted.
    pub fn is_finished(&self) -> bool {
        self.finished.is_some()
    }

    /// Number of dropped anomalous events.
    pub fn anomalies(&self) -> u32 {
        self.anomalies
    }

    /// Records that an event arrived after the stream was already decided.
    pub fn note_late_event(&mut self) {
        self.anomalies = self.anomalies.saturating_add(1);
    }

    /// Pushes one capability event.
    pub fn push(&mut self, event: Event) -> Result<Pushed, Violation> {
        if self.finished.is_some() {
            self.note_late_event();
            return Ok(Pushed::Dropped);
        }
        self.events += 1;
        if self.events > self.limits.max_events_per_attempt {
            return Err(Violation::TooManyEvents);
        }
        if let Err(error) = validate_event_bounds(&event, &self.limits) {
            return Err(match error.code {
                ValidationCode::EventFieldEmpty => Violation::EmptyEventField,
                _ => Violation::OversizedEvent,
            });
        }
        // Any event is evidence that the provider responded.
        self.observe_delivery(DeliveryCertainty::ConfirmedResponse);

        match event {
            Event::Metadata(m) => {
                self.merge_metadata(&m);
                Ok(Pushed::Forward(Event::Metadata(m)))
            }
            Event::TextDelta(text) => {
                if text.len() > self.limits.max_event_bytes {
                    return Err(Violation::OversizedEvent);
                }
                self.text_bytes = self.text_bytes.saturating_add(text.len() as u64);
                self.accumulate(&text);
                Ok(Pushed::Forward(Event::TextDelta(text)))
            }
            Event::ReasoningDelta(text) => {
                if text.len() > self.limits.max_event_bytes {
                    return Err(Violation::OversizedEvent);
                }
                self.reasoning_bytes = self.reasoning_bytes.saturating_add(text.len() as u64);
                Ok(Pushed::Forward(Event::ReasoningDelta(text)))
            }
            Event::ToolInputStart { id, name } => {
                if self.streams.iter().any(|s| s.id == id) {
                    self.anomalies += 1;
                    return Ok(Pushed::Dropped);
                }
                if self.streams.len() >= self.limits.max_proposals {
                    return Err(Violation::TooManyStreamedInputs);
                }
                self.streams.push(StreamedInput {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                    ended: false,
                    overflowed: false,
                    lent: false,
                });
                Ok(Pushed::Forward(Event::ToolInputStart { id, name }))
            }
            Event::ToolInputDelta { id, delta } => {
                let max = self.limits.max_tool_arguments_bytes;
                match self.streams.iter_mut().find(|s| s.id == id) {
                    Some(stream) if !stream.ended => {
                        if stream.arguments.len() + delta.len() > max {
                            stream.overflowed = true;
                        } else {
                            stream.arguments.push_str(&delta);
                        }
                        self.tool_input_bytes =
                            self.tool_input_bytes.saturating_add(delta.len() as u64);
                        Ok(Pushed::Forward(Event::ToolInputDelta { id, delta }))
                    }
                    _ => {
                        self.anomalies += 1;
                        Ok(Pushed::Dropped)
                    }
                }
            }
            Event::ToolInputEnd { id } => match self.streams.iter_mut().find(|s| s.id == id) {
                Some(stream) if !stream.ended => {
                    stream.ended = true;
                    Ok(Pushed::Forward(Event::ToolInputEnd { id }))
                }
                _ => {
                    self.anomalies += 1;
                    Ok(Pushed::Dropped)
                }
            },
            Event::ToolCall(proposal) => {
                if self.proposals.len() >= self.limits.max_proposals {
                    return Err(Violation::TooManyProposals);
                }
                if self.records.iter().any(|r| r.call_id == proposal.id) {
                    return Err(Violation::ProviderRecordIdCollision);
                }
                let (normalized, check) = self.normalize_proposal(proposal);
                self.proposals.push(normalized.clone());
                self.checks.push(check);
                Ok(Pushed::Forward(Event::ToolCall(normalized)))
            }
            Event::ProviderToolRecord(record) => {
                let declared = self.provider_tools.contains(&record.name);
                if !declared {
                    return Err(Violation::ProviderRecordForUndeclaredTool);
                }
                if self.proposals.iter().any(|p| p.id == record.call_id) {
                    return Err(Violation::ProviderRecordIdCollision);
                }
                let depth = self.limits.max_json_depth;
                if json_check::check(&record.input_json, depth).is_err()
                    || record
                        .result_json
                        .as_ref()
                        .is_some_and(|r| json_check::check(r, depth).is_err())
                {
                    return Err(Violation::MalformedProviderRecord);
                }
                match self
                    .records
                    .iter_mut()
                    .find(|r| r.call_id == record.call_id)
                {
                    Some(existing) if existing.is_final => {
                        // The first final result wins.
                        self.anomalies += 1;
                        Ok(Pushed::Dropped)
                    }
                    Some(existing) => {
                        *existing = record.clone();
                        Ok(Pushed::Forward(Event::ProviderToolRecord(record)))
                    }
                    None => {
                        if self.records.len() >= self.limits.max_proposals {
                            return Err(Violation::TooManyProviderRecords);
                        }
                        self.records.push(record.clone());
                        Ok(Pushed::Forward(Event::ProviderToolRecord(record)))
                    }
                }
            }
            Event::Usage(usage) => {
                self.merge_usage(&usage);
                Ok(Pushed::Forward(Event::Usage(usage)))
            }
            Event::Finish { reason } => {
                self.finished = Some(reason);
                Ok(Pushed::Finished(Event::Finish { reason }))
            }
        }
    }

    fn accumulate(&mut self, text: &str) {
        if self.content_truncated {
            return;
        }
        let room = self.limits.max_completion_bytes - self.content.len();
        if text.len() <= room {
            self.content.push_str(text);
            return;
        }
        let mut end = room;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.content.push_str(&text[..end]);
        self.content_truncated = true;
    }

    fn merge_metadata(&mut self, m: &Metadata) {
        fn keep<T: Clone + PartialEq>(slot: &mut Option<T>, new: &Option<T>, anomalies: &mut u32) {
            match (slot.as_ref(), new) {
                (None, Some(value)) => *slot = Some(value.clone()),
                (Some(old), Some(value)) if old != value => *anomalies += 1,
                _ => {}
            }
        }
        keep(
            &mut self.metadata.model_reported,
            &m.model_reported,
            &mut self.anomalies,
        );
        keep(
            &mut self.metadata.generation_id,
            &m.generation_id,
            &mut self.anomalies,
        );
        keep(
            &mut self.metadata.provider_timestamp_ms,
            &m.provider_timestamp_ms,
            &mut self.anomalies,
        );
    }

    fn merge_usage(&mut self, u: &Usage) {
        let current = self.usage.get_or_insert_with(Usage::default);
        fn set<T: Clone>(slot: &mut Option<T>, new: &Option<T>) {
            if let Some(value) = new {
                *slot = Some(value.clone());
            }
        }
        set(&mut current.input_tokens, &u.input_tokens);
        set(&mut current.output_tokens, &u.output_tokens);
        set(&mut current.cache_read_tokens, &u.cache_read_tokens);
        set(&mut current.cache_write_tokens, &u.cache_write_tokens);
        set(&mut current.reasoning_tokens, &u.reasoning_tokens);
        set(&mut current.cost, &u.cost);
    }

    fn normalize_proposal(
        &mut self,
        proposal: ToolCallProposal,
    ) -> (ToolCallProposal, ProposalCheck) {
        let identity = if proposal.id.is_empty() {
            ProposalIdentity::EmptyId
        } else if self.proposals.iter().any(|p| p.id == proposal.id) {
            ProposalIdentity::DuplicateId
        } else if self
            .streams
            .iter()
            .any(|s| s.id == proposal.id && s.name != proposal.name)
        {
            ProposalIdentity::ConflictingStreamedName
        } else {
            ProposalIdentity::Valid
        };
        let depth = self.limits.max_json_depth;
        let (arguments, integrity, source) = if !proposal.arguments_json.is_empty() {
            if json_check::check(&proposal.arguments_json, depth).is_ok() {
                (
                    proposal.arguments_json,
                    ArgumentIntegrity::Valid,
                    ArgumentSource::Final,
                )
            } else {
                (
                    "{}".to_owned(),
                    ArgumentIntegrity::Replaced,
                    ArgumentSource::Final,
                )
            }
        } else {
            let eligible = (identity == ProposalIdentity::Valid)
                .then(|| {
                    self.streams.iter_mut().find(|s| {
                        s.id == proposal.id
                            && s.name == proposal.name
                            && s.ended
                            && !s.overflowed
                            && !s.lent
                    })
                })
                .flatten();
            match eligible {
                Some(stream) => {
                    stream.lent = true;
                    if json_check::check(&stream.arguments, depth).is_ok() {
                        (
                            stream.arguments.clone(),
                            ArgumentIntegrity::Valid,
                            ArgumentSource::StreamedFallback,
                        )
                    } else {
                        (
                            "{}".to_owned(),
                            ArgumentIntegrity::Replaced,
                            ArgumentSource::StreamedFallback,
                        )
                    }
                }
                None => (
                    "{}".to_owned(),
                    ArgumentIntegrity::Replaced,
                    ArgumentSource::Missing,
                ),
            }
        };
        (
            ToolCallProposal {
                id: proposal.id,
                name: proposal.name,
                arguments_json: arguments,
            },
            ProposalCheck {
                identity,
                integrity,
                source,
            },
        )
    }

    /// Partial output accumulated so far, if any.
    pub fn partial(&self) -> Option<PartialOutput> {
        (!self.content.is_empty() || self.content_truncated).then(|| PartialOutput {
            content: self.content.clone(),
            content_truncated: self.content_truncated,
        })
    }

    /// Proposal checks, index-aligned with the proposals.
    pub fn checks(&self) -> &[ProposalCheck] {
        &self.checks
    }

    /// Byte counters for diagnostics: (text, reasoning, tool input).
    pub fn byte_counts(&self) -> (u64, u64, u64) {
        (self.text_bytes, self.reasoning_bytes, self.tool_input_bytes)
    }

    /// Number of events accepted so far.
    pub fn event_count(&self) -> usize {
        self.events
    }

    /// Builds the completion. Returns `None` unless a `finish` was accepted.
    pub fn completion(&self) -> Option<Completion> {
        let reason = self.finished?;
        Some(Completion {
            content: self.content.clone(),
            content_truncated: self.content_truncated,
            reasoning_bytes: self.reasoning_bytes,
            proposals: self.proposals.clone(),
            provider_records: self.records.clone(),
            finish_reason: reason,
            usage: self.usage.clone(),
            metadata: self.metadata.clone(),
        })
    }
}

/// How a completion should be read (oracle: `classifyProviderCompletion`,
/// src/core/shared/types.zig:1123-1139).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Disposition {
    /// A normal answer with no proposals.
    Complete,
    /// The model proposed client tool calls and finished for tool calls.
    ToolProposals,
    /// Output stopped at the token limit.
    LengthLimited,
    /// The provider reported an in-band error or a content filter.
    ProviderFailure,
    /// The finish reason contradicts the content: proposals with `stop`/`other`,
    /// or `tool_calls` with nothing proposed or executed.
    InvalidCompletion,
}

/// Classifies a completion. Pure; grants nothing.
pub fn classify(completion: &Completion) -> Disposition {
    match completion.finish_reason {
        FinishReason::ProviderError | FinishReason::ContentFilter => Disposition::ProviderFailure,
        FinishReason::Length => Disposition::LengthLimited,
        FinishReason::ToolCalls if !completion.proposals.is_empty() => Disposition::ToolProposals,
        FinishReason::ToolCalls if !completion.provider_records.is_empty() => Disposition::Complete,
        FinishReason::ToolCalls => Disposition::InvalidCompletion,
        FinishReason::Stop | FinishReason::Other if !completion.proposals.is_empty() => {
            Disposition::InvalidCompletion
        }
        FinishReason::Stop | FinishReason::Other => Disposition::Complete,
    }
}

/// Why a proposal set is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdmissionRejection {
    /// A proposal with a valid identity had no final arguments and no exact ended
    /// stream to fall back to (oracle: `invalid_tool_input` and
    /// `incomplete_streamed_input`). Its `"{}"` placeholder must never run.
    MissingArguments,
    /// A proposal's identity is empty, duplicated or conflicts with its stream.
    Identity(ProposalIdentity),
}

/// Why proposals cannot be admitted as a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AdmissionError {
    /// Index of the first offending proposal.
    pub index: usize,
    /// The problem.
    pub reason: AdmissionRejection,
}

/// Structural admission of proposals (oracle: `authoritativeToolAdmission`,
/// src/core/shared/types.zig:1264-1308). As in the oracle, missing arguments are
/// a completion-level failure checked first, then identities. It checks
/// structure only. Whether and how to execute a proposal is the consumer's
/// decision (Chip, under Attn's authority); FX never executes tools. Proposals
/// whose present-but-malformed arguments were replaced are admissible: the
/// consumer reports the malformed input back to the model, as the oracle does.
pub fn admit_proposals(checks: &[ProposalCheck]) -> Result<(), AdmissionError> {
    let missing = checks
        .iter()
        .position(|c| c.identity == ProposalIdentity::Valid && c.source == ArgumentSource::Missing);
    if let Some(index) = missing {
        return Err(AdmissionError {
            index,
            reason: AdmissionRejection::MissingArguments,
        });
    }
    match checks
        .iter()
        .enumerate()
        .find(|(_, c)| c.identity != ProposalIdentity::Valid)
    {
        Some((index, check)) => Err(AdmissionError {
            index,
            reason: AdmissionRejection::Identity(check.identity),
        }),
        None => Ok(()),
    }
}
