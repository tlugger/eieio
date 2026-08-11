//! What a run says happened (ABI-SPEC §13.1).
//!
//! Three outcomes and not two. A scenario a host cannot express is [`Outcome::Skipped`] with
//! the reason, never a silent pass: the daemon implements no capability namespaces yet
//! (DAEMON §5.1), so half the suite is unreachable for it today, and a summary that counted
//! those as passes would report a coverage this platform does not have.

use core::fmt;

use crate::record::{Allocation, Disposition, HostFault};

/// Whether a scenario held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every expectation held.
    Passed,
    /// At least one did not. See [`Report::violations`].
    Failed,
    /// This host cannot run this scenario, for the reason given.
    Skipped(String),
}

/// One expectation that did not hold, or one host-side ABI violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Which step, or [`None`] for a whole-run expectation.
    pub step: Option<usize>,
    /// What went wrong, in the words a scenario author needs.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.step {
            Some(step) => write!(f, "step {step}: {}", self.detail),
            None => write!(f, "{}", self.detail),
        }
    }
}

/// One scenario, run against one host.
#[derive(Debug, Clone)]
pub struct Report {
    /// The scenario's name.
    pub scenario: String,
    /// The host's name, so a divergence names both sides.
    pub host: String,
    /// Whether it held.
    pub outcome: Outcome,
    /// Everything that did not, in the order it was found.
    pub violations: Vec<Violation>,
    /// Every inbound allocation the host made (ABI §9), recorded whether or not the scenario
    /// asked. See [`crate::record`] for what this can and cannot see.
    pub allocations: Vec<Allocation>,
    /// ABI §9 rules the *host* broke.
    pub host_faults: Vec<HostFault>,
    /// The guest's linear memory at the end of the run, in 64 KiB pages — §13.1's leak signal.
    pub memory_pages: u32,
}

impl Report {
    /// A scenario this host cannot run.
    pub fn skipped(scenario: &str, host: &str, reason: impl Into<String>) -> Report {
        Report {
            scenario: scenario.to_string(),
            host: host.to_string(),
            outcome: Outcome::Skipped(reason.into()),
            violations: Vec::new(),
            allocations: Vec::new(),
            host_faults: Vec::new(),
            memory_pages: 0,
        }
    }

    /// Whether nothing failed. A skipped scenario did not fail.
    pub fn ok(&self) -> bool {
        self.outcome != Outcome::Failed
    }

    /// How many allocations the guest declined with `0` (ABI §9.5).
    pub fn refused_allocations(&self) -> usize {
        self.count(Disposition::Refused)
    }

    /// How many pointers the host had to reject as misaligned (ABI §9.6).
    pub fn misaligned_allocations(&self) -> usize {
        self.count(Disposition::Misaligned)
    }

    fn count(&self, disposition: Disposition) -> usize {
        self.allocations
            .iter()
            .filter(|allocation| allocation.disposition == disposition)
            .count()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.outcome {
            Outcome::Passed => write!(f, "ok      {} [{}]", self.scenario, self.host),
            Outcome::Skipped(reason) => {
                write!(f, "skipped {} [{}]: {reason}", self.scenario, self.host)
            }
            Outcome::Failed => {
                writeln!(f, "FAILED  {} [{}]", self.scenario, self.host)?;
                for violation in &self.violations {
                    writeln!(f, "          {violation}")?;
                }
                Ok(())
            }
        }
    }
}

/// Every report from one suite run.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    /// The reports, in the order the scenarios were run.
    pub reports: Vec<Report>,
}

impl Summary {
    /// The scenarios that failed.
    pub fn failed(&self) -> impl Iterator<Item = &Report> {
        self.reports
            .iter()
            .filter(|report| report.outcome == Outcome::Failed)
    }

    /// The scenarios this host could not run, with their reasons.
    pub fn skipped(&self) -> impl Iterator<Item = &Report> {
        self.reports
            .iter()
            .filter(|report| matches!(report.outcome, Outcome::Skipped(_)))
    }

    /// The suite's verdict: `Ok` with a one-line count, `Err` with every failure spelled out.
    ///
    /// Separate from [`assert_ok`](Summary::assert_ok) because a panic is only the right
    /// answer inside a `#[test]`; `cargo eio test` reports the same verdict as an ordinary
    /// error (SDK §5.3). Both go through this, so what counts as a pass — and how a failure
    /// reads — cannot come to depend on who asked.
    ///
    /// Skipped scenarios are neither: they are the caller's to report, and every caller MUST
    /// (§13.1). Counting one as a pass would claim coverage the platform does not have.
    pub fn verdict(&self) -> Result<String, String> {
        let failed = self.failed().count();
        if failed == 0 {
            let ran = self.reports.len() - self.skipped().count();
            return Ok(format!("{ran} scenario(s) passed"));
        }
        let mut message = format!("{failed} conformance scenario(s) failed:\n");
        for report in self.failed() {
            message.push_str(&format!("{report}"));
        }
        Err(message)
    }

    /// Panics unless every scenario passed or was skipped, printing all of them.
    ///
    /// Skipped ones are printed too, always: a host silently covering less of the ABI than
    /// the suite describes is the failure this whole crate exists to make visible.
    #[track_caller]
    pub fn assert_ok(&self) {
        for report in self.skipped() {
            println!("{report}");
        }
        if let Err(message) = self.verdict() {
            panic!("{message}");
        }
    }
}
