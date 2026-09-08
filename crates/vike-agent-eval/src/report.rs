//! The verdict: one table line per case, one JSON document per run, and the transcript of anything
//! that failed written out beside it.
//!
//! ⚠ The report carries no credential and no environment sweep. It names the model, the case, the
//! checks and their evidence, and nothing else — a report is the artefact that gets pasted into an
//! issue, and the only way to be sure a key never lands in one is for the writer never to have it.

use serde_json::{Value, json};

use crate::grade::CheckOutcome;

/// What one case produced.
pub struct CaseVerdict {
    pub name: &'static str,
    pub skill: &'static str,
    pub pass: bool,
    /// Model turns (or scripted steps) spent. With a real model this is also the number of API
    /// calls the case cost.
    pub steps: usize,
    pub checks: Vec<CheckOutcome>,
    /// A harness-level failure — a node that never listened, a server that died — as opposed to a
    /// failed expectation. Kept apart because they mean different things: one is a finding about
    /// the agent, the other is a finding about the run.
    pub error: Option<String>,
    /// The recorded MCP lines, for the transcript file a failure gets.
    pub transcript: Value,
    pub final_text: String,
}

impl CaseVerdict {
    /// The single table line. `FAIL` first so a column of results reads down the left edge.
    pub fn row(&self) -> String {
        let failed = self.checks.iter().filter(|c| !c.pass).count();
        let detail = match &self.error {
            Some(e) => format!("harness error: {e}"),
            None if failed > 0 => format!("{failed} of {} checks failed", self.checks.len()),
            None => format!("{} checks", self.checks.len()),
        };
        format!(
            "  {:<4}  {:<28} {:<28} {:>2} step(s)  {detail}",
            if self.pass { "ok" } else { "FAIL" },
            self.name,
            self.skill,
            self.steps
        )
    }

    pub fn to_json(&self) -> Value {
        json!({
            "case": self.name,
            "skill": self.skill,
            "pass": self.pass,
            "steps": self.steps,
            "error": self.error,
            "final_text": self.final_text,
            "checks": self.checks.iter().map(|c| json!({
                "check": c.check,
                "pass": c.pass,
                "detail": c.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

/// One whole run.
pub struct Report {
    pub driver: String,
    pub cases: Vec<CaseVerdict>,
}

impl Report {
    pub fn failed(&self) -> usize {
        self.cases.iter().filter(|c| !c.pass).count()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "driver": self.driver,
            "cases": self.cases.iter().map(CaseVerdict::to_json).collect::<Vec<_>>(),
            "passed": self.cases.len() - self.failed(),
            "failed": self.failed(),
        })
    }

    /// The table, then the detail of every failing check — collected and printed at the END, the
    /// way `scripts/cli_mcp_smoke.sh` prints its own: the point is the whole picture, not the first
    /// thing that broke.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("=========================================================================\n");
        out.push_str(&format!(" agent eval — driver: {}\n", self.driver));
        out.push_str("=========================================================================\n");
        for c in &self.cases {
            out.push_str(&c.row());
            out.push('\n');
        }
        out.push_str("-------------------------------------------------------------------------\n");
        out.push_str(&format!("  {} case(s), {} failed\n", self.cases.len(), self.failed()));
        out.push_str("=========================================================================\n");
        if self.failed() > 0 {
            out.push_str("\nFAILURE DETAIL\n");
            for c in self.cases.iter().filter(|c| !c.pass) {
                out.push_str(&format!("\n  {}\n", c.name));
                if let Some(e) = &c.error {
                    out.push_str(&format!("    harness error: {e}\n"));
                }
                for check in c.checks.iter().filter(|k| !k.pass) {
                    out.push_str(&format!("    {} — {}\n", check.check, check.detail));
                }
                out.push_str(&format!("    final text: {}\n", c.final_text));
            }
        }
        out
    }
}
