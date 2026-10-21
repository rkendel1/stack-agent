//! A deterministic stuck guard: a predicate over the work's own trajectory.
//!
//! It reads the observations the loop recorded and where each came from (both already kept, in
//! step, by the loop), compares the last few, and says whether the work is repeating itself without
//! learning anything. It never reads a model's words, executes nothing, asks nothing and keeps no
//! state, so it can be evaluated at every iteration and gives the same answer for the same
//! trajectory, including one restored from durable records.
//!
//! A *pair* is a request and what it observed. Two pairs are equal when the requests are the same
//! capability with the same complete typed inputs ([`ObservationOrigin::invocation`]) and the
//! observations are the same reality: same kind, status and output. Execution ids, receipts and
//! runtime evidence differ by construction and are never compared.

use crate::{ExecutionStatus, Observation, ObservationOrigin};

/// When the guard fires. Both thresholds are counts of consecutive pairs at the end of the
/// trajectory. The guard reads at most the larger of the two, however long the trajectory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StuckThresholds {
    /// The last this-many pairs are all the same pair. Below 2 it would fire on a single request,
    /// so it is held to at least 2.
    pub repeated: usize,
    /// The last this-many pairs alternate between two different pairs (`X Y X Y`). An odd value is
    /// read as the even number above it, and a value below 4 is held to 4: two pairs alternating
    /// once is not a cycle.
    pub alternating: usize,
}

impl Default for StuckThresholds {
    fn default() -> Self {
        Self {
            repeated: 3,
            alternating: 4,
        }
    }
}

impl StuckThresholds {
    fn repeated(&self) -> usize {
        self.repeated.max(2)
    }

    fn alternating(&self) -> usize {
        let n = self.alternating.max(4);
        n + n % 2
    }

    /// The most pairs one evaluation reads.
    pub fn window(&self) -> usize {
        self.repeated().max(self.alternating())
    }
}

/// What the trajectory is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StuckPattern {
    /// The same request returned the same observation this many times in a row.
    Repeated { pairs: usize },
    /// Two different requests, each returning the same observation every time, alternated for this
    /// many pairs.
    Alternating { pairs: usize },
}

type Pair<'a> = (&'a str, &'a Observation);

fn same(a: Pair<'_>, b: Pair<'_>) -> bool {
    a.0 == b.0 && a.1.kind == b.1.kind && a.1.status == b.1.status && a.1.output == b.1.output
}

/// Whether the end of the trajectory is a stuck pattern. `origins` and `observations` are the
/// parallel vectors the loop keeps; only their common length is read.
pub fn detect_stuck(
    origins: &[ObservationOrigin],
    observations: &[Observation],
    thresholds: &StuckThresholds,
) -> Option<StuckPattern> {
    let n = origins.len().min(observations.len());
    let take = thresholds.window().min(n);
    let tail: Vec<Pair<'_>> = (n - take..n)
        .map(|i| (origins[i].invocation.as_str(), &observations[i]))
        .collect();
    let repeated = thresholds.repeated();
    if tail.len() >= repeated {
        let last = tail[tail.len() - 1];
        if tail[tail.len() - repeated..].iter().all(|p| same(*p, last)) {
            return Some(StuckPattern::Repeated { pairs: repeated });
        }
    }
    let alternating = thresholds.alternating();
    if tail.len() >= alternating {
        let t = &tail[tail.len() - alternating..];
        let cycle = !same(t[0], t[1]) && (2..t.len()).all(|k| same(t[k], t[k - 2]));
        if cycle {
            return Some(StuckPattern::Alternating { pairs: alternating });
        }
    }
    None
}

impl StuckPattern {
    /// The reason a work ended on this pattern gives. It begins [`REASON_PREFIX`], which is how a
    /// caller tells this stop from any other `Blocked`.
    ///
    /// `goal_met` says the goal's required observations were already satisfied: the work is then
    /// not completed only because completion needs a decision and none was made, and the reason
    /// says so rather than leave it to be read as a work that learned nothing.
    pub fn reason(&self, origins: &[ObservationOrigin], goal_met: bool) -> String {
        let name = |back: usize| {
            origins
                .len()
                .checked_sub(back)
                .and_then(|i| origins.get(i))
                .map_or("?", |o| o.capability.as_str())
        };
        let ending = if goal_met {
            "The goal's required observations were already satisfied, but the work is completed only by a completion decision and none was made, so it was not completed. The work stopped here: no further capability was executed and the model was not asked. Everything recorded before it is kept."
        } else {
            "Nothing new was learned, so the work stopped here: no further capability was executed and the model was not asked. Everything recorded before it is kept."
        };
        match self {
            StuckPattern::Repeated { pairs } => format!(
                "{REASON_PREFIX}{} was requested {pairs} times in a row with the same inputs and returned the same observation each time. {ending}",
                name(1)
            ),
            StuckPattern::Alternating { pairs } => format!(
                "{REASON_PREFIX}{} and {} alternated for {pairs} requests, each with the same inputs and the same observation every time. {ending}",
                name(2),
                name(1)
            ),
        }
    }
}

impl StuckPattern {
    /// [`reason`](Self::reason) followed by what the stop tells whoever reads it about getting
    /// unstuck. Only what the trajectory establishes is said: whether the repeated requests failed
    /// or succeeded, and that the guard itself retried nothing. No output text and no request input
    /// is quoted, because the reason is recorded durably and observations are kept there only as
    /// sizes and hashes. The guard's decision is unchanged: this text is produced after it.
    pub fn reason_with_recovery(
        &self,
        origins: &[ObservationOrigin],
        observations: &[Observation],
        goal_met: bool,
    ) -> String {
        let window = match self {
            StuckPattern::Repeated { pairs } | StuckPattern::Alternating { pairs } => *pairs,
        };
        let tail = &observations[observations.len().saturating_sub(window)..];
        let all =
            |status: ExecutionStatus| !tail.is_empty() && tail.iter().all(|o| o.status == status);
        let advice = if all(ExecutionStatus::Failure) {
            "Recovery: every one of those requests failed, and each failure was the same, so the same request cannot succeed. The failure observations recorded for them state what the runtime knows about the cause; a different request is needed, with inputs taken from what the observations so far show, or a different capability."
        } else if all(ExecutionStatus::Success) {
            "Recovery: every one of those requests succeeded and returned the same observation, so asking again cannot add anything. The work needs a different request, or a completion decision based on the observations recorded so far."
        } else {
            "Recovery: the same observations keep coming back, so asking again cannot add anything. A different request is needed."
        };
        format!(
            "{} {advice} This stop did not retry, change a request or ask the model again.",
            self.reason(origins, goal_met)
        )
    }
}

/// How the reason of a work the guard stopped begins.
pub const REASON_PREFIX: &str = "stuck: ";
