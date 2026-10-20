//! Phase-level execution attribution for the sustained workload.
//!
//! PR33 named query execution as the sustained-workload bottleneck. PR34
//! removed whole-collection materialization and PR35 removed the full predicate
//! scan, taking the workload from 274.1 to 1,665.9 ops/sec on one runner. PR36
//! asks what is left, and that question cannot be answered from throughput:
//! an aggregate rate says how fast the system is, never where the time went.
//!
//! So this module records **where the time went**, as elapsed nanoseconds and a
//! call count per execution phase. Three properties keep it honest:
//!
//! 1. **The boundaries are real.** Every phase brackets an actual span of
//!    execution — the wait for the state lock, the index lookup, the record
//!    fetch, the predicate, the sort, the log append. Nothing here is a
//!    proportion inferred from wall-clock time, and a phase that cannot be
//!    bracketed cleanly is simply absent rather than estimated.
//! 2. **Phases nest, and the report says so.** `STATE_LOCK_HOLD` contains
//!    `RECORD_MUTATION`, `INDEX_MAINTENANCE` and `PERSISTENCE`; `INDEXED_QUERY`
//!    contains `INDEX_LOOKUP`, `CANDIDATE_RETRIEVAL` and `PREDICATE_EVALUATION`.
//!    Summing every phase would double-count, which is why [`Phase::parent`]
//!    exists and why the analysis distinguishes exclusive time from nested time.
//! 3. **It is off unless asked for.** Disabled, each instrumented site costs one
//!    relaxed atomic load and a predictable branch; no clock is read and no
//!    counter is touched. It is enabled by `FELTDB_WORKLOAD_DIAGNOSTICS=1`, is
//!    test instrumentation rather than a product surface, and reports counts and
//!    durations only — never a record, a field, a value or an id.
//!
//! Instrumentation that changes what it measures is not a measurement, so the
//! overhead of *enabled* diagnostics is itself measured, and the headline
//! production figures are taken with diagnostics off.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// One bracketed span of execution.
///
/// Ordered so that a report reads top-down: request, then query phases, then
/// mutation phases. The discriminant is the counter index, so adding a phase
/// means adding it here and to [`Phase::ALL`] and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum Phase {
    /// Waiting to acquire the single state lock. Pure contention: no work is
    /// done here, which is what makes it the one phase whose time is always
    /// wasted rather than merely spent.
    StateLockWait = 0,
    /// Holding the state lock. Contains every phase below that touches state,
    /// and bounds how long other operations can be made to wait.
    StateLockHold,
    /// A bounded query answered from the equality index, end to end.
    IndexedQuery,
    /// Turning `(field, value)` into candidate record ids inside the index.
    IndexLookup,
    /// Fetching authoritative records for candidate ids.
    CandidateRetrieval,
    /// Evaluating the query's conjunction against a record, on either execution.
    PredicateEvaluation,
    /// A bounded query answered by scanning the collection, end to end.
    ScanQuery,
    /// Sorting the matching set into the query's requested order.
    Ordering,
    /// Cloning matches and injecting `recordId` to build the visible result.
    ResultMaterialization,
    /// Writing or removing a record in the authoritative map.
    RecordMutation,
    /// Maintaining the derived equality index alongside that record change.
    IndexMaintenance,
    /// Appending to the durable log, including any fsync it performs.
    Persistence,
    /// Transaction bookkeeping around a commit: preconditions, operation
    /// construction, revision and dedup accounting. Excludes the log append.
    TransactionCommit,
    /// A whole HTTP handler, measured inside the server between extraction and
    /// response. The gap between this and the client's own timing is transport,
    /// decode and encode.
    HttpHandler,
    /// Authenticating and authorizing a request: key lookup, signature and
    /// capability checks. Nested inside `HttpHandler`, and separated from it
    /// because "the request costs something outside state" is not an answer —
    /// which part of the request is.
    Authorization,
    /// The request's whole dealing with the security audit subsystem: building
    /// the event, submitting it to the audit writer, and waiting for the
    /// writer to accept it. Nested inside `HttpHandler` and deliberately not
    /// folded into `Persistence`, which is the *state* log — these are two
    /// different durable writes with two different reasons to exist, and
    /// attributing them together would hide which one costs what.
    ///
    /// Since PR38 this span no longer contains a durability barrier. The
    /// barrier happens on the audit writer's own threads, in
    /// `AuditGroupSync`, after the request has already been released.
    AuditWrite,
    /// Handing the event to the audit writer's bounded queue. Fails fast when
    /// the queue is at capacity rather than blocking the request.
    AuditSubmit,
    /// Waiting for the audit writer to append the submitted event to the
    /// stream — the acceptance boundary, and the only part of the audit
    /// pipeline a request waits for.
    ///
    /// This is the audit path's analogue of `StateLockWait`, and it is where
    /// the single-writer discipline shows up as time: a request waits behind
    /// the appends in front of it, and behind whatever the writer is doing
    /// when it arrives.
    AuditAcceptWait,
    /// Opening the audit file, on the writer thread. Since PR38 the writer
    /// keeps the handle, so this happens once per stream rather than once per
    /// event; it is still measured, because "once" is a claim.
    AuditOpen,
    /// Serializing one event into the writer's reusable buffer and issuing the
    /// single `write(2)` that appends it. On the writer thread.
    AuditAppend,
    /// `File::flush` on the audit handle, inside a durability group. A
    /// `std::fs::File` holds no userspace buffer, so this is measured rather
    /// than assumed to be free.
    AuditFlush,
    /// `File::sync_data` on the audit handle: the durability barrier itself,
    /// and the one phase that asks the storage stack for a guarantee rather
    /// than for a write. On the syncer thread, off the request path.
    AuditFsync,
    /// One whole durability group: the flush and the barrier that make a
    /// bounded set of already-accepted events durable. On the syncer thread.
    AuditGroupSync,
    /// Closing the audit file handle at shutdown. Measured because it is a
    /// syscall, and because the point of the writer owning the handle is that
    /// this no longer happens per event.
    AuditClose,
    /// The body of a request handler, from the first line of the handler
    /// function to its return.
    ///
    /// It exists to bracket what a middleware cannot. `HttpHandler` wraps the
    /// whole route, so it includes axum's extraction — where the request JSON is
    /// decoded — and the response conversion, where the response JSON is
    /// encoded. Neither can be timed from inside a handler or from a middleware
    /// alone. The difference `HttpHandler - HandlerBody - Authorization -
    /// AuditWrite` is therefore routing plus decode plus encode, measured as a
    /// difference of two spans rather than asserted.
    HandlerBody,
    /// Resolving a managed request's application contract: the revision, the
    /// runtime inventory it is validated against, and the resolved runtime.
    /// Every `/v1` request pays it, so it must not depend on unrelated state.
    ContractResolution,
    /// Cloning one state namespace's rows for a state-contract read or
    /// transaction. Proportional to that application's stored rows, not to
    /// other applications'.
    NamespaceSnapshot,
}

impl Phase {
    pub const ALL: [Phase; 27] = [
        Phase::StateLockWait,
        Phase::StateLockHold,
        Phase::IndexedQuery,
        Phase::IndexLookup,
        Phase::CandidateRetrieval,
        Phase::PredicateEvaluation,
        Phase::ScanQuery,
        Phase::Ordering,
        Phase::ResultMaterialization,
        Phase::RecordMutation,
        Phase::IndexMaintenance,
        Phase::Persistence,
        Phase::TransactionCommit,
        Phase::HttpHandler,
        Phase::Authorization,
        Phase::AuditWrite,
        Phase::AuditSubmit,
        Phase::AuditAcceptWait,
        Phase::AuditOpen,
        Phase::AuditAppend,
        Phase::AuditFlush,
        Phase::AuditFsync,
        Phase::AuditGroupSync,
        Phase::AuditClose,
        Phase::HandlerBody,
        Phase::ContractResolution,
        Phase::NamespaceSnapshot,
    ];

    /// The stable name this phase is reported under.
    pub fn name(self) -> &'static str {
        match self {
            Phase::StateLockWait => "state_lock_wait",
            Phase::StateLockHold => "state_lock_hold",
            Phase::IndexedQuery => "indexed_query",
            Phase::IndexLookup => "index_lookup",
            Phase::CandidateRetrieval => "candidate_retrieval",
            Phase::PredicateEvaluation => "predicate_evaluation",
            Phase::ScanQuery => "scan_query",
            Phase::Ordering => "ordering",
            Phase::ResultMaterialization => "result_materialization",
            Phase::RecordMutation => "record_mutation",
            Phase::IndexMaintenance => "index_maintenance",
            Phase::Persistence => "persistence",
            Phase::TransactionCommit => "transaction_commit",
            Phase::HttpHandler => "http_handler",
            Phase::Authorization => "authorization",
            Phase::AuditWrite => "audit_write",
            Phase::AuditSubmit => "audit_submit",
            Phase::AuditAcceptWait => "audit_accept_wait",
            Phase::AuditOpen => "audit_open",
            Phase::AuditAppend => "audit_append",
            Phase::AuditFlush => "audit_flush",
            Phase::AuditFsync => "audit_fsync",
            Phase::AuditGroupSync => "audit_group_sync",
            Phase::AuditClose => "audit_close",
            Phase::HandlerBody => "handler_body",
            Phase::ContractResolution => "contract_resolution",
            Phase::NamespaceSnapshot => "namespace_snapshot",
        }
    }

    /// The phase whose span contains this one, when there is one.
    ///
    /// This is what stops a reader from adding the columns up. A phase's time is
    /// already counted inside its parent, so only sibling phases at one level
    /// can be compared, and only exclusive time can be summed.
    pub fn parent(self) -> Option<Phase> {
        match self {
            Phase::StateLockWait | Phase::StateLockHold | Phase::HttpHandler => None,
            Phase::IndexedQuery | Phase::ScanQuery => Some(Phase::StateLockHold),
            Phase::IndexLookup | Phase::CandidateRetrieval => Some(Phase::IndexedQuery),
            // The predicate runs on both executions, so its parent is the lock
            // rather than either query phase.
            Phase::PredicateEvaluation => Some(Phase::StateLockHold),
            Phase::Authorization
            | Phase::AuditWrite
            | Phase::HandlerBody
            | Phase::ContractResolution
            | Phase::NamespaceSnapshot => Some(Phase::HttpHandler),
            Phase::Ordering | Phase::ResultMaterialization => Some(Phase::HandlerBody),
            Phase::RecordMutation | Phase::IndexMaintenance | Phase::Persistence => {
                Some(Phase::StateLockHold)
            }
            Phase::TransactionCommit => Some(Phase::StateLockHold),
            // What the request pays decomposes into the submit and the wait for
            // acceptance, and nothing else: since PR38 no other audit phase
            // happens on the request's thread.
            Phase::AuditSubmit | Phase::AuditAcceptWait => Some(Phase::AuditWrite),
            // The flush and the barrier are the two halves of one durability
            // group.
            Phase::AuditFlush | Phase::AuditFsync => Some(Phase::AuditGroupSync),
            // Deliberately parentless. `AuditOpen`, `AuditAppend`,
            // `AuditGroupSync` and `AuditClose` run on the audit writer's own
            // threads, concurrently with the requests that produced them.
            // Nesting them under `AuditWrite` or `HttpHandler` would let a
            // reader compute a share of a span they do not run inside, and
            // that share could exceed one hundred per cent.
            Phase::AuditOpen | Phase::AuditAppend | Phase::AuditGroupSync | Phase::AuditClose => {
                None
            }
        }
    }
}

const PHASES: usize = Phase::ALL.len();

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);
static CALLS: [AtomicU64; PHASES] = [ZERO; PHASES];
static NANOS: [AtomicU64; PHASES] = [ZERO; PHASES];
static ENABLED: AtomicU64 = AtomicU64::new(UNRESOLVED);

const UNRESOLVED: u64 = 0;
const OFF: u64 = 1;
const ON: u64 = 2;

/// Is phase attribution recording?
///
/// Resolved once from `FELTDB_WORKLOAD_DIAGNOSTICS`, then a relaxed load. The
/// environment is read at most once per process, so a hot path never touches the
/// environment and never allocates.
#[inline]
pub fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        ON => true,
        OFF => false,
        _ => {
            let resolved = if std::env::var("FELTDB_WORKLOAD_DIAGNOSTICS").as_deref() == Ok("1") {
                ON
            } else {
                OFF
            };
            ENABLED.store(resolved, Ordering::Relaxed);
            resolved == ON
        }
    }
}

/// Turn recording on or off for the current process, for tests that need to
/// measure the instrumentation itself rather than through it.
pub fn set_enabled(on: bool) {
    ENABLED.store(if on { ON } else { OFF }, Ordering::Relaxed);
}

/// An open span. Dropping it records the elapsed time against its phase.
///
/// Constructed only when recording is on, so the disabled path allocates
/// nothing and never reads the clock.
#[derive(Debug)]
pub struct Span {
    phase: Phase,
    began: Instant,
}

impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let index = self.phase as usize;
        CALLS[index].fetch_add(1, Ordering::Relaxed);
        NANOS[index].fetch_add(elapsed, Ordering::Relaxed);
    }
}

/// Open a span, or `None` when recording is off.
///
/// The `Option` is the whole disabled-path cost: no clock read, no atomic
/// write, and a branch the predictor gets right every time.
#[inline]
pub fn span(phase: Phase) -> Option<Span> {
    enabled().then(|| Span {
        phase,
        began: Instant::now(),
    })
}

/// Record a duration measured elsewhere, such as a lock wait that has to be
/// timed across an acquisition rather than around a closure.
#[inline]
pub fn record(phase: Phase, nanos: u64) {
    if !enabled() {
        return;
    }
    let index = phase as usize;
    CALLS[index].fetch_add(1, Ordering::Relaxed);
    NANOS[index].fetch_add(nanos, Ordering::Relaxed);
}

/// Time one closure into a phase and return its value.
#[inline]
pub fn timed<T>(phase: Phase, body: impl FnOnce() -> T) -> T {
    let _span = span(phase);
    body()
}

/// What one phase accumulated: how often, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseCounter {
    pub phase: Phase,
    pub calls: u64,
    pub nanos: u64,
}

impl PhaseCounter {
    pub fn mean_nanos(&self) -> u64 {
        self.nanos.checked_div(self.calls).unwrap_or(0)
    }
}

/// Read every phase counter. Monotonic since process start or the last
/// [`reset`], so a caller compares deltas across a measured window.
pub fn counters() -> Vec<PhaseCounter> {
    Phase::ALL
        .iter()
        .map(|&phase| PhaseCounter {
            phase,
            calls: CALLS[phase as usize].load(Ordering::Relaxed),
            nanos: NANOS[phase as usize].load(Ordering::Relaxed),
        })
        .collect()
}

/// Zero every counter, so a phase of a measurement starts from a clean base.
pub fn reset() {
    for index in 0..PHASES {
        CALLS[index].store(0, Ordering::Relaxed);
        NANOS[index].store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Counters are process-global; a counter assertion is only meaningful
    /// while no other test in this module is recording.
    fn exclusive() -> MutexGuard<'static, ()> {
        static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
        SERIAL
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn counter(phase: Phase) -> PhaseCounter {
        counters()
            .into_iter()
            .find(|counter| counter.phase == phase)
            .expect("every phase is reported")
    }

    #[test]
    fn a_disabled_span_records_nothing() {
        let _serial = exclusive();
        set_enabled(false);
        reset();

        for _ in 0..1_000 {
            let _span = span(Phase::Ordering);
        }

        assert_eq!(counter(Phase::Ordering).calls, 0);
        assert!(
            span(Phase::Ordering).is_none(),
            "a disabled span is not constructed, so it reads no clock",
        );
    }

    #[test]
    fn an_enabled_span_records_calls_and_elapsed_time() {
        let _serial = exclusive();
        set_enabled(true);
        reset();

        for _ in 0..8 {
            timed(Phase::IndexLookup, || std::hint::black_box(0u64));
        }
        let observed = counter(Phase::IndexLookup);
        assert_eq!(observed.calls, 8);
        assert!(observed.nanos > 0, "a span of real work takes real time");

        set_enabled(false);
    }

    #[test]
    fn every_phase_is_reported_and_nesting_is_declared() {
        let _serial = exclusive();
        let reported: Vec<&str> = counters()
            .into_iter()
            .map(|counter| counter.phase.name())
            .collect();
        assert_eq!(reported.len(), Phase::ALL.len());
        // A phase must not be its own ancestor, or a report could not
        // distinguish exclusive time from nested time.
        for phase in Phase::ALL {
            let mut ancestor = phase.parent();
            let mut depth = 0;
            while let Some(current) = ancestor {
                assert_ne!(current, phase, "{} is its own ancestor", phase.name());
                depth += 1;
                assert!(depth < PHASES, "phase nesting must terminate");
                ancestor = current.parent();
            }
        }
    }
}
