//! The REPAIR plan for one series' manifest — what a rebuild would recover, and what it would lose.
//!
//! [`super::DataFusionHist::rebuild_series_manifest`] has existed since the index became a cache
//! rather than ground truth, and until now **no binary called it**: `git grep` found tests and doc
//! comments, so `super::manifest::read_manifest`'s own refusal named a repair an operator had no
//! command for. This module is the half that was missing — not the rebuild itself (which was
//! correct), but the VERDICT an operator has to read before and after one.
//!
//! # Why a rebuild needs a plan at all, when `rm` needs one because it deletes
//!
//! A rebuild does not delete rows, and the shallow reading is therefore that it is safe and needs
//! no rehearsal. Three facts say otherwise, and all three are counts rather than opinions:
//!
//! * **It can succeed and lose the idempotency log.** A part sealed before `COMMIT_KEYS_META`
//!   existed (or appended with `commit_key: None`) carries no keys, so the rebuilt index has
//!   nothing to refuse an already-applied append with. `RebuildReport::parts_without_keys` is that
//!   count, and the consequence of ignoring it is DUPLICATED rows on the next backfill — which is
//!   the shape of damage nobody notices for weeks.
//! * **It can succeed and leave rows unreachable.** A part whose footer will not open, or which
//!   carries no `ts` statistics, is deliberately NOT admitted — admitting it with a fabricated
//!   range would let the read path prune away a file that really overlaps the query.
//!   `RebuildReport::parts_unreadable` is that count, and a non-zero one is data loss to
//!   investigate rather than a warning to note.
//! * **It DROPS [`super::manifest::Manifest::orphan_commits`]**, which no `RebuildReport` field
//!   can see: the rebuild derives keys from part footers and an orphan is by definition a key no
//!   part carries. The live box's largest series had 13. So the plan reads the base for that count
//!   itself and reports it beside the five.
//!
//! None of those is an error. Every one of them is a decision, which is exactly the shape that
//! needs a rehearsal in front of it — the same argument `crate::removal`'s plan makes for a delete,
//! reached from the other direction.
//!
//! # ⚠ The DELTA LOG: not refused, not folded first, and neither was available
//!
//! Since manifest v3 a series' index is a base file plus an append-only `_manifest.delta`, so a
//! repair owes an answer about the log. Three were possible and two are unreachable:
//!
//! * **Refuse while the log is non-empty?** That refuses the HEADLINE case. A surviving log with no
//!   base is the exact state `super::manifest::read_manifest` rejects and names this repair for.
//! * **Fold the log in first?** A fold needs the folded manifest, which needs `read_manifest`,
//!   which refuses that same state. "Fold first" cannot run where the repair is most needed.
//! * **Rebuild from the parts and CLEAR the log** — what
//!   [`super::DataFusionHist::rebuild_series_manifest`] does, through `manifest::fold_base`. The
//!   clear is mandatory rather than tidy: a surviving frame the new base does not carry would
//!   replay over the repair and re-add every file the rebuild had just decided to skip. The version
//!   bump (`max(base, last frame) + 1`) is what makes the crash between the publish and the clear
//!   safe — every surviving frame's version is then below the base's, so replay skips all of them.
//!
//! **Nothing is lost by not replaying**, and that is a fact about what a frame carries rather than
//! a hope: a frame's `files_add`/`files_rm` name parts that are already fsynced on disk, so the
//! rebuild re-derives them from the directory; and nothing in this tree writes a non-empty
//! `keys_add`/`keys_rm`. What IS lost is [`RepairPlan::orphan_commits`], which the plan counts and
//! reports for exactly that reason.
//!
//! ⚠ The one hole is narrow and is REPORTED rather than assumed away: see
//! [`RepairPlan::delta_error`].
//!
//! # ⚠ The plan is LOCK-FREE and writes nothing, and that is load-bearing
//!
//! [`super::DataFusionHist::plan_series_manifest_rebuild`] takes NO series lock. It re-runs the same
//! pure `super::manifest::rebuild_manifest` pass the write performs, so the numbers it prints are
//! the numbers the write would print, and it costs a live writer exactly nothing — where the WRITE
//! holds the series lock across every footer read and can therefore cost a live `RecorderSink` its
//! buffer. A rehearsal that stalled the recorder in order to tell you what a rebuild would do would
//! be the wrong tool for the question it answers.
//!
//! The price of running lock-free is that a concurrent commit can land between the plan and the
//! write, so the two can disagree by a part. That is the same gap `crate::removal`'s plan/execute
//! pair has and it is bounded the same way: the write re-derives everything under the lock, so the
//! plan is a forecast and never an input to the write.

use serde::Serialize;

use super::manifest::RebuildReport;
use crate::series::SeriesId;

/// What a rebuild of ONE series would do, computed without writing anything or taking any lock.
///
/// Plain data, `Serialize` for the verb's `--json` document. Every field is a fact an operator acts
/// on; nothing here is derived for rendering alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepairPlan {
    /// The series this plan is about, as the selector NAMED it — not as the store enumerated it.
    ///
    /// ⚠ The distinction is the whole reason this verb takes a named selector rather than offering
    /// an `--all`: `super::DataFusionHist::list_series` finds leaves by the presence of
    /// `_manifest.json`, so the headline failure — a base deleted out from under a surviving delta
    /// log — is INVISIBLE to every enumeration in this crate, to `inventory()`, and therefore to
    /// `vike-cli data list`. A repair reachable only through enumeration could not reach the state
    /// `read_manifest`'s own error text sends an operator here for.
    pub id: SeriesId,
    /// The series leaf directory, as a string so the document needs no path escaping rules.
    pub series_dir: String,
    /// `false` when that directory does not exist. A caller must REFUSE on this rather than
    /// rebuilding: `rebuild_manifest` treats an absent directory as an empty series (which is right
    /// for a series that legitimately holds no parts yet), so a mistyped selector would otherwise
    /// publish an empty manifest at a path nothing had ever written — minting a phantom series
    /// `list_series` then enumerates forever.
    pub leaf_present: bool,
    /// Is `_manifest.json` there? `false` with [`RepairPlan::delta_frames`] non-zero is the state
    /// `super::manifest::read_manifest` refuses outright, and the one this verb exists for.
    pub base_present: bool,
    /// The base's version, `0` when there is none or it could not be read. The rebuild publishes
    /// ABOVE `max(base, last frame)`, never at 1.
    pub base_version: u64,
    /// How many parts the CURRENT index names, or `None` when the index cannot be read at all.
    pub current_parts: Option<usize>,
    /// Rows the CURRENT index names (manifest fold, no Parquet scan), or `None` as above.
    pub current_rows: Option<u64>,
    /// `read_manifest`'s refusal VERBATIM when the series does not read today — usually the exact
    /// sentence that sent the operator to this verb. `None` when the series reads fine, which is a
    /// legitimate reason to run a rehearsal (to see whether it WOULD lose anything) and a reason to
    /// think twice before the write.
    pub current_error: Option<String>,
    /// Frames in the delta log; `None` when the log itself could not be read.
    pub delta_frames: Option<usize>,
    /// Why the log could not be read. ⚠ This is the narrow hole in the version guard: a frame that
    /// passes its CRC and fails to PARSE makes `delta_read_with_end` error, which
    /// `super::manifest::last_published_version` swallows to `0` — so a rebuild of a base-less
    /// series would publish at version 2 beneath frames at a far higher version. The clear that
    /// follows removes them, so the state is right; it is only a `delta_clear` that ALSO fails
    /// which leaves those frames to replay over the repair. Reported here so the decision is the
    /// operator's rather than a silent one.
    pub delta_error: Option<String>,
    /// Commit keys the base records that NO part carries — DROPPED by a rebuild, and counted by no
    /// `RebuildReport` field. See this module's doc.
    pub orphan_commits: usize,
    /// What the rebuild WOULD recover: the same pass the write runs, over the same directory.
    pub report: RebuildReport,
    /// Rows the REBUILT index would name. Compare with [`RepairPlan::current_rows`]: a rebuild that
    /// recovers FEWER rows than the current index names is the shape worth stopping on.
    pub rebuilt_rows: u64,
}

impl RepairPlan {
    /// Every parquet file the rebuild opened the footer of — the size of the critical section the
    /// WRITE holds the series lock across.
    ///
    /// It is printed because it is the only honest measure of what a live writer would pay: the
    /// rebuild reads each of these footers with the lock HELD, and a `RecorderSink` whose flush
    /// spins that budget out discards its buffer. A number the operator can compare against their
    /// own store beats a sentence saying "this may take a while".
    pub fn parts_seen(&self) -> usize {
        let r = &self.report;
        r.parts_recovered + r.parts_superseded + r.parts_unreadable + r.parts_unpublished_merge
    }

    /// `true` when this rebuild would cost nothing but the index it replaces.
    ///
    /// The three lossy shapes, and nothing else: keys that will not come back
    /// (`parts_without_keys`), rows that will not come back (`parts_unreadable`), and orphan keys
    /// the derivation cannot see ([`RepairPlan::orphan_commits`]).
    ///
    /// ⚠ `parts_superseded` and `parts_unpublished_merge` are deliberately NOT here. Both are the
    /// mechanism working — a superseded fragment is double-counting PREVENTED, and a skipped
    /// `_tmp-merge-…` output has its rows in the inputs, which are indexed. They are reported
    /// loudly all the same (see [`RepairPlan::notes`]), because each is also evidence about the
    /// store: one says a compaction crashed here, the other says a compaction may be running in
    /// another process RIGHT NOW.
    pub fn is_lossless(&self) -> bool {
        self.report.parts_without_keys == 0
            && self.report.parts_unreadable == 0
            && self.orphan_commits == 0
    }

    /// The LOSSES, one sentence each, every one ending in what to do about it.
    ///
    /// Empty when [`RepairPlan::is_lossless`]. A caller that prints these and exits zero has
    /// handed an operator a clean-looking exit over a store that just lost its idempotency log,
    /// which is the failure this list exists to make impossible to miss.
    pub fn losses(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.report.parts_without_keys > 0 {
            out.push(format!(
                "LOSSY: {} of {} recovered parts carry NO commit keys, so the rebuilt index has \
                 nothing to refuse an already-applied append with. Re-running a backfill over a \
                 window these parts cover will RE-ADMIT it and DUPLICATE rows. Next: bound every \
                 later backfill to a window you know is missing (`vike-cli data list --gaps` names \
                 them), or accept that this series' ingest is no longer idempotent.",
                self.report.parts_without_keys, self.report.parts_recovered
            ));
        }
        if self.report.parts_unreadable > 0 {
            out.push(format!(
                "LOSSY: {} part(s) could not be read — an unopenable footer, or no `ts` statistics \
                 to bound the part by — so their rows are NOT in the rebuilt index and stay \
                 unreachable. A part with no range is never admitted, because a fabricated one \
                 lets the read path prune away a file that really overlaps the query. Next: find \
                 them under {}/date=*/ (a truncated tail is the usual cause), move them aside, and \
                 re-run this verb.",
                self.report.parts_unreadable, self.series_dir
            ));
        }
        if self.orphan_commits > 0 {
            out.push(format!(
                "LOSSY: {} commit key(s) recorded on the base are carried by no part, so a rebuild \
                 — which derives keys from part footers — DROPS them. These are the v2 migration's \
                 residue. Next: nothing can restore them; note the count before re-offering any \
                 batch from that era.",
                self.orphan_commits
            ));
        }
        out
    }

    /// The findings that are NOT losses but that an operator must still read — each one evidence
    /// about the store rather than about this rebuild.
    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.report.parts_superseded > 0 {
            out.push(format!(
                "{} part(s) are already inside another part in the same date — the fragments of a \
                 compaction that published its output and crashed before unlinking them. Skipping \
                 them is double-counting PREVENTED, not data dropped; it also means a compaction \
                 died in this series.",
                self.report.parts_superseded
            ));
        }
        if self.report.parts_unpublished_merge > 0 {
            out.push(format!(
                "⚠ {} unpublished merge output(s) (`_tmp-merge-…`) skipped. Their rows are in the \
                 inputs, which ARE indexed — but this state is indistinguishable from a compaction \
                 running in ANOTHER PROCESS right now. Treat it as a live writer until you know \
                 otherwise.",
                self.report.parts_unpublished_merge
            ));
        }
        if self.report.parts_recovered == 0 && self.parts_seen() > 0 {
            out.push(
                "⚠ NOTHING would be recovered even though parts are on disk — every one of them \
                 was skipped. Read the counts above before writing: an empty index published over \
                 a series that has parts is the failure this verb repairs, not one it should cause."
                    .to_string(),
            );
        }
        if let (Some(before), rebuilt) = (self.current_rows, self.rebuilt_rows)
            && rebuilt < before
        {
            // Tense-neutral on purpose: [`RepairPlan::notes`] is rendered by BOTH
            // [`RepairPlan::lines`] (a forecast) and [`RepairPlan::outcome_lines`] (a record), and
            // a sentence that reads as a prediction inside a past-tense block is a sentence an
            // operator has to decide how to read.
            out.push(format!(
                "⚠ the rebuild indexes {rebuilt} rows where the index before it named {before}. A \
                 rebuild is derived from what is ON DISK, so a shortfall means parts the old index \
                 named are gone or unreadable — investigate."
            ));
        }
        out
    }

    /// The plan as an operator reads it.
    ///
    /// ⚠ It does NOT carry the resolved store root — that line is the CALLER's, for the reason
    /// `crate::removal::RemovalPlan::lines` gives: only the process that resolved the root knows
    /// which rung answered, and "which store" is the question a write-shaped verb answers first.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![
            format!("series: {}", crate::removal::describe_id(&self.id)),
            format!("leaf:   {}", self.series_dir),
        ];
        out.push(match (&self.current_error, self.current_parts, self.current_rows) {
            (Some(e), _, _) => format!("index today: UNREADABLE — {e}"),
            (None, Some(parts), Some(rows)) => {
                format!("index today: readable — {parts} parts · {rows} rows")
            }
            // Unreachable: the three are filled together. Spelled rather than unwrapped so a future
            // field change is a strange line and not a panic in an operator's store.
            (None, _, _) => "index today: (not determined)".to_string(),
        });
        out.push(format!(
            "on disk:     _manifest.json {} · _manifest.delta {}",
            match (self.base_present, self.base_version) {
                (false, _) => "MISSING".to_string(),
                // A published base is never version 0 — a `0` beside a file that EXISTS means it
                // did not parse (an unknown `format`, or truncated JSON). Rendered as what it is,
                // because "present (v0)" reads like a fact rather than like a failure.
                (true, 0) => "present but UNREADABLE".to_string(),
                (true, v) => format!("present (v{v})"),
            },
            match (self.delta_frames, &self.delta_error) {
                (_, Some(e)) => format!("UNREADABLE — {e}"),
                (Some(0), None) => "absent or empty".to_string(),
                (Some(n), None) => format!("{n} frame(s)"),
                (None, None) => "(not determined)".to_string(),
            }
        ));
        let r = &self.report;
        out.push(format!(
            "rebuild would: recover {} part(s) · {} rows; skip {} superseded, {} unreadable, {} \
             unpublished merge; {} recovered part(s) carry no commit keys; {} orphan key(s) \
             dropped",
            r.parts_recovered,
            self.rebuilt_rows,
            r.parts_superseded,
            r.parts_unreadable,
            r.parts_unpublished_merge,
            r.parts_without_keys,
            self.orphan_commits,
        ));
        out.push(format!(
            "critical section: {} part footer(s) read with this series' lock HELD — a live writer \
             flushing into that window spins ~4s and then DISCARDS its buffer",
            self.parts_seen()
        ));
        for n in self.notes() {
            out.push(format!("  {n}"));
        }
        if self.is_lossless() {
            out.push(
                "verdict: LOSSLESS — the index would be replaced and nothing else".to_string(),
            );
        } else {
            out.push(
                "verdict: LOSSY — this rebuild costs more than the index it replaces".to_string(),
            );
            for l in self.losses() {
                out.push(format!("  {l}"));
            }
        }
        out
    }

    /// The same verdict, rendered for a rebuild that HAS HAPPENED — past tense, and with the counts
    /// taken from the report the write returned rather than from the rehearsal's.
    ///
    /// The caller builds this by putting the write's `RebuildReport` onto the plan it rehearsed
    /// (`RepairPlan { report, ..plan }`), which is what keeps ONE verdict implementation behind
    /// both tenses: [`RepairPlan::is_lossless`] and [`RepairPlan::losses`] are the same functions
    /// the rehearsal printed, so a plan that read LOSSLESS and an outcome that reads LOSSLESS
    /// cannot mean different things.
    ///
    /// ⚠ [`RepairPlan::orphan_commits`] is deliberately the PLAN's number here, and it has to be:
    /// the keys are dropped BY the rebuild, so reading them back afterwards would answer zero every
    /// time — a loss that erases its own evidence.
    pub fn outcome_lines(&self) -> Vec<String> {
        let r = &self.report;
        // ⚠ No ROW count here, and its absence is deliberate: the write returns a `RebuildReport`
        // and not the manifest it published, so the only rows available are the REHEARSAL's
        // forecast — which a commit landing between the two passes makes stale. A part count from
        // the write's own report is a fact; a row count carried over from the plan would be a
        // number that looks measured and is not. `vike-cli data list` answers rows.
        let mut out = vec![format!(
            "rebuilt {}: indexed {} part(s); skipped {} superseded, {} unreadable, {} \
             unpublished merge; {} indexed part(s) carry no commit keys; {} orphan key(s) dropped",
            crate::removal::describe_id(&self.id),
            r.parts_recovered,
            r.parts_superseded,
            r.parts_unreadable,
            r.parts_unpublished_merge,
            r.parts_without_keys,
            self.orphan_commits,
        )];
        for n in self.notes() {
            out.push(format!("  {n}"));
        }
        if self.is_lossless() {
            out.push(
                "verdict: LOSSLESS — the index was replaced and nothing else was lost".to_string(),
            );
        } else {
            out.push(
                "verdict: LOSSY — the index is rebuilt AND something did not come back".to_string(),
            );
            for l in self.losses() {
                out.push(format!("  {l}"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> RepairPlan {
        RepairPlan {
            id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string())),
            series_dir: "/s/kind=bar/venue=binance/symbol=BTCUSDT/interval=1m".to_string(),
            leaf_present: true,
            base_present: true,
            base_version: 7,
            current_parts: Some(3),
            current_rows: Some(30),
            current_error: None,
            delta_frames: Some(0),
            delta_error: None,
            orphan_commits: 0,
            report: RebuildReport { parts_recovered: 3, ..Default::default() },
            rebuilt_rows: 30,
        }
    }

    /// A clean rebuild says so and lists nothing — the baseline the lossy cases are read against.
    #[test]
    fn a_lossless_plan_has_no_losses_and_says_so() {
        let p = plan();
        assert!(p.is_lossless());
        assert!(p.losses().is_empty());
        assert!(p.notes().is_empty(), "{:?}", p.notes());
        let text = p.lines().join("\n");
        assert!(text.contains("verdict: LOSSLESS"), "{text}");
    }

    /// ⚠ **The headline requirement.** Each of the three lossy shapes flips the verdict on its own,
    /// so no combination of them can hide behind another, and each loss line names the count AND
    /// the next action — a count alone is not something an operator can act on.
    #[test]
    fn each_lossy_shape_flips_the_verdict_alone_and_names_a_next_step() {
        for (name, mutate) in [
            (
                "parts_without_keys",
                (|p: &mut RepairPlan| p.report.parts_without_keys = 2) as fn(&mut RepairPlan),
            ),
            ("parts_unreadable", |p: &mut RepairPlan| p.report.parts_unreadable = 1),
            ("orphan_commits", |p: &mut RepairPlan| p.orphan_commits = 13),
        ] {
            let mut p = plan();
            mutate(&mut p);
            assert!(!p.is_lossless(), "{name} must make a plan lossy");
            let losses = p.losses();
            assert_eq!(losses.len(), 1, "{name}: exactly its own line: {losses:?}");
            assert!(losses[0].starts_with("LOSSY:"), "{name}: {:?}", losses[0]);
            assert!(losses[0].contains("Next:"), "{name} must say what to do: {:?}", losses[0]);
            let text = p.lines().join("\n");
            assert!(text.contains("verdict: LOSSY"), "{name}: {text}");
        }
    }

    /// The two findings that are NOT losses are still REPORTED — the mechanism working is still a
    /// fact about the store (a compaction crashed; a compaction may be live).
    #[test]
    fn the_mechanism_working_is_a_note_rather_than_a_loss() {
        let mut p = plan();
        p.report.parts_superseded = 4;
        p.report.parts_unpublished_merge = 1;
        assert!(p.is_lossless(), "neither is a LOSS");
        let notes = p.notes().join("\n");
        assert!(notes.contains("crashed before unlinking"), "{notes}");
        assert!(notes.contains("ANOTHER PROCESS"), "{notes}");
        let text = p.lines().join("\n");
        assert!(text.contains("verdict: LOSSLESS"), "{text}");
        assert!(
            text.contains("crashed before unlinking"),
            "a note must reach the rendering: {text}"
        );
    }

    /// Recovering NOTHING while parts sit on disk is the one shape a clean-looking exit would be
    /// worst about — it is what publishing an EMPTY index over a full series looks like from here.
    #[test]
    fn recovering_nothing_from_a_non_empty_leaf_is_called_out() {
        let mut p = plan();
        p.report = RebuildReport { parts_unreadable: 3, ..Default::default() };
        p.rebuilt_rows = 0;
        assert_eq!(p.parts_seen(), 3);
        let notes = p.notes().join("\n");
        assert!(notes.contains("NOTHING would be recovered"), "{notes}");
    }

    /// A rebuild that would name FEWER rows than the current index is a shortfall on disk, not a
    /// tidy-up — the plan says so before the operator confirms.
    #[test]
    fn a_row_shortfall_against_the_current_index_is_called_out() {
        let mut p = plan();
        p.rebuilt_rows = 20;
        let notes = p.notes().join("\n");
        assert!(notes.contains("20 rows where the index before it named 30"), "{notes}");
    }

    /// ⚠ **The rehearsal and the record share ONE verdict implementation**, so a plan that read
    /// LOSSLESS and an outcome that reads LOSSLESS cannot mean different things — and a LOSSY
    /// outcome carries the same named losses the rehearsal showed, in the past tense.
    #[test]
    fn the_outcome_carries_the_same_verdict_the_plan_did() {
        let clean = plan();
        let text = clean.outcome_lines().join("\n");
        assert!(text.contains("verdict: LOSSLESS"), "{text}");
        assert!(text.starts_with("rebuilt "), "{text}");

        let mut lossy = plan();
        lossy.report.parts_without_keys = 2;
        let text = lossy.outcome_lines().join("\n");
        assert!(text.contains("verdict: LOSSY"), "{text}");
        assert!(text.contains("DUPLICATE rows"), "the consequence must survive the tense: {text}");
        assert!(text.contains("Next:"), "{text}");
    }

    /// ⚠ **The outcome carries NO row count**, and that is the point: the write returns a report
    /// rather than the manifest it published, so the only rows in hand are the rehearsal's
    /// forecast. A number that looks measured and is not is worse than no number.
    #[test]
    fn the_outcome_reports_parts_and_never_a_stale_row_count() {
        let mut p = plan();
        p.rebuilt_rows = 999_999;
        let text = p.outcome_lines().join("\n");
        assert!(text.contains("indexed 3 part(s)"), "{text}");
        assert!(
            !text.contains("999999"),
            "a plan-time row count must not be reported as a fact: {text}"
        );
    }

    /// ⚠ **The dropped orphan keys are the PLAN's count and must stay so.** They are dropped BY the
    /// rebuild, so a post-write reading would answer zero every time — a loss that erases its own
    /// evidence. The outcome therefore still names them.
    #[test]
    fn dropped_orphan_keys_survive_into_the_outcome() {
        let mut p = plan();
        p.orphan_commits = 13;
        let text = p.outcome_lines().join("\n");
        assert!(text.contains("13 orphan key(s) dropped"), "{text}");
        assert!(text.contains("verdict: LOSSY"), "{text}");
    }

    /// The base-less-with-log state renders as what it IS, so the rendering matches the refusal
    /// that sent the operator here.
    #[test]
    fn the_base_less_state_renders_both_halves() {
        let mut p = plan();
        p.base_present = false;
        p.delta_frames = Some(9);
        p.current_error = Some("has a delta log but NO base".to_string());
        let text = p.lines().join("\n");
        assert!(text.contains("_manifest.json MISSING"), "{text}");
        assert!(text.contains("9 frame(s)"), "{text}");
        assert!(text.contains("index today: UNREADABLE"), "{text}");
    }

    /// The critical section is reported as a COUNT, because it is what a live writer pays and
    /// "this may take a while" is not something an operator can weigh.
    #[test]
    fn the_critical_section_is_reported_as_a_part_count() {
        let mut p = plan();
        p.report = RebuildReport {
            parts_recovered: 10,
            parts_superseded: 4,
            parts_unreadable: 1,
            parts_unpublished_merge: 1,
            parts_without_keys: 0,
        };
        assert_eq!(p.parts_seen(), 16, "every footer opened, whatever became of it");
        let text = p.lines().join("\n");
        assert!(text.contains("16 part footer(s) read with this series' lock HELD"), "{text}");
    }
}
