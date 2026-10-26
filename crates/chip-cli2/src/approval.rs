//! Who may authorize a plan, and how.
//!
//! cli2 decides whether a plan needs approval (its policy, from the plan's effects). When it
//! does, the backend asks an [`Approver`] and then approves **the plan's full digest**, which is
//! what binds the approval to exactly the plan that was shown. A model is never an approver: the
//! backend has no path by which model output becomes an approval.
//!
//! The default is [`DenyAll`]: nothing that needs approval runs unless the host opted in to a
//! way of asking a person.

use std::io::{BufRead, Write};
use std::sync::Mutex;

/// What the person (or policy) is shown. Taken from cli2's recorded plan, not from the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReview {
    /// The discovered task, as cli2 names it.
    pub task: String,
    pub plan_id: String,
    /// The plan's full digest: what an approval is bound to.
    pub digest: String,
    /// The commands the plan will run, as displayed by cli2.
    pub commands: Vec<String>,
    pub working_directory: String,
    /// `confirm` or `explicit`.
    pub approval_level: String,
    pub reasons: Vec<String>,
    /// Effects as cli2 assessed them, e.g. `unknown (none)`.
    pub effects: String,
    pub risk: String,
}

impl PlanReview {
    /// The short form of the digest a person types to approve (the first 12 hex digits).
    pub fn short_digest(&self) -> String {
        self.digest
            .strip_prefix("sha256:")
            .unwrap_or(&self.digest)
            .chars()
            .take(12)
            .collect()
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("cli2 needs your approval to run a project task:\n");
        s.push_str(&format!("  task         {}\n", self.task));
        for c in &self.commands {
            s.push_str(&format!("  command      {c}\n"));
        }
        s.push_str(&format!("  directory    {}\n", self.working_directory));
        s.push_str(&format!("  effects      {}\n", self.effects));
        s.push_str(&format!(
            "  approval     {} ({})\n",
            self.approval_level,
            self.reasons.join("; ")
        ));
        s.push_str(&format!("  risk         {}\n", self.risk));
        s.push_str(&format!("  plan         {}\n", self.plan_id));
        s.push_str(&format!("  digest       {}\n", self.digest));
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Approve exactly the reviewed plan (its full digest).
    Approve,
    /// Do not run it. The reason is reported to the model and recorded.
    Deny(String),
}

pub trait Approver: Send + Sync {
    fn review(&self, plan: &PlanReview) -> Decision;
}

/// Approves nothing. The default.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAll;

impl Approver for DenyAll {
    fn review(&self, _plan: &PlanReview) -> Decision {
        Decision::Deny(
            "approval is required and this run has no way to ask a person (use --cli2-approval stdin)"
                .into(),
        )
    }
}

/// Asks a person on the terminal: the plan is printed to stderr and a line is read from stdin.
/// Only the plan's short digest, typed exactly, approves; anything else, or end of input,
/// declines. Typing the digest of the plan just shown is what ties the decision to that plan.
pub struct StdioApprover {
    // One question at a time, whatever calls in.
    lock: Mutex<()>,
}

impl StdioApprover {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }
}

impl Default for StdioApprover {
    fn default() -> Self {
        Self::new()
    }
}

impl Approver for StdioApprover {
    fn review(&self, plan: &PlanReview) -> Decision {
        let _one_at_a_time = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{}", plan.render());
        let _ = writeln!(
            err,
            "To approve exactly this plan, type its digest ({}) and press Enter; anything else declines:",
            plan.short_digest()
        );
        let _ = err.flush();
        drop(err);
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == plan.short_digest() => Decision::Approve,
            Ok(0) | Err(_) => Decision::Deny("no answer was given (end of input)".into()),
            Ok(_) => Decision::Deny("the digest was not typed; declined".into()),
        }
    }
}
