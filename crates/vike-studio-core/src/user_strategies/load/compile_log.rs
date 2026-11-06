//! The `compile.log` text one scan renders as: every pass and every failure, one stamped line each.
use vike_model::Severity;

use super::LoadReport;

/// Render one scan as the text a caller appends to `user_data/logs/compile.log`
/// (`crates/vike-paths/src/state_path.rs`'s `user_logs_dir`).
///
/// **Passes are logged too, not only failures.** That directory's own contract says "every
/// strategy load, pass and fail", and the reason is that a user debugging "why is my strategy not
/// in the list" needs to see the strategies that ARE — an absent name is the answer, and it is
/// only visible against the present ones.
///
/// `stamp` is a CALLER-SUPPLIED time string and this function reads no clock: a library that
/// stamps its own output cannot be tested for its output, and ambient time is the defect
/// `crates/vike-ops/tests/architecture/clock_pin.rs` ratchets down elsewhere in this tree. One stamp per LINE,
/// not per block, because the file is appended to once per app start and `grep <name>` must still
/// answer "when".
///
/// Ends with a newline, so appending twice cannot splice two runs onto one line.
pub fn render_compile_log(report: &LoadReport, stamp: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("{stamp} info  scanning {}\n", report.root.display()));
    for s in &report.strategies {
        let presets = match s.presets.len() {
            0 => String::new(),
            1 => format!(", 1 preset ({})", s.presets[0].name),
            n => format!(
                ", {n} presets ({})",
                s.presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        };
        // A built-in strategy has no file to name; saying so is the point, since the folder's
        // presets are all there is to see.
        let what = match s.entry() {
            Some(entry) => entry.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            None => "built-in".to_string(),
        };
        out.push_str(&format!("{stamp} ok    {} [{what}{presets}]\n", s.name));
    }
    for d in &report.diagnostics {
        let level = match d.severity() {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN ",
        };
        out.push_str(&format!("{stamp} {level} {d}\n"));
    }
    out.push_str(&format!(
        "{stamp} info  {} loaded, {} error(s), {} warning(s)\n",
        report.strategies.len(),
        report.error_count(),
        report.warning_count()
    ));
    out
}
