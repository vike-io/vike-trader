//! **Emptying the store: selecting series by identity, and PROVING their provenance before any of
//! them is removed.**
//!
//! The store can be filled from several surfaces and, until 2026-09-07, could be emptied from
//! exactly one — the Data Manager GUI's per-series Delete.
//! `docs/decisions/0049-the-dissolved-engines-reproduction-halves-are-removed.md` named the cost in
//! its own words: *"A store that can be filled from the command line and only emptied from a GUI is
//! a surface with one hand."* Ten series were removed from the the CI box store that day with a throwaway
//! script, and that script did the thing this module is built around — **it read each series'
//! provenance out of its `_manifest.json` commit keys BEFORE deleting, rather than inferring it from
//! the data's shape.**
//!
//! Everything here is PURE and feature-free (like [`crate::series`]): a [`SeriesSelector`] is
//! matched against ids the STORE enumerated, and a [`RemovalPlan`] is a fold over coverage and
//! commit keys the store read. Nothing in this file opens a directory, and nothing in it deletes
//! anything — `crate::DataFusionHist`'s `delete_series_checked` is the one verb that removes bytes,
//! and it re-checks this module's assertion under the series lock before it does.
//!
//! # ⚠ Why the selector NEVER builds a path
//!
//! A removal is driven by INTERSECTING the operator's selector with the store's own enumeration
//! ([`select_series`] over `list_series`). Three properties fall out, and they are the whole safety
//! argument for the grammar:
//!
//! * a typo yields ZERO matches, never a delete of something adjacent;
//! * a PHANTOM leaf cannot be targeted, so the five-bug class `series_dir_of`'s doc records — a path
//!   ending `symbol=` whose empty manifest makes every verb silently succeed — is unreachable here
//!   by construction;
//! * grouped and per-symbol series are told apart by the SOURCE of the id, not by a parse of the
//!   operator's string. `SeriesId` makes `symbol` and `group` ALTERNATIVES (exactly one is
//!   meaningful, and `symbol` is EMPTY when grouped), which no positional `VENUE:SYMBOL:INTERVAL`
//!   spelling can express.
//!
//! # ⚠ Why `--produced-by` is an ASSERTION and not a filter
//!
//! The identity selector picks the set; the prefix then requires that EVERY commit key of EVERY
//! selected series matches. One foreign key refuses the WHOLE run, names the series and the key, and
//! deletes nothing.
//!
//! The case that decides it is a MIXED series — one whose manifest carries both the producer being
//! cleaned and a legitimate one. A filter would silently skip it; an assertion stops the operator and
//! makes them look. That is exactly the hazard 0049's own ordering rule turns on (*"a window takes
//! ONE producer, because the store's idempotency is batch-level on the commit key"*), and it is the
//! difference between removing panel rows and removing the venue's real candles with them.
//! All-or-nothing has a second virtue: a refusal leaves the store in the state the dry run
//! described, so there is no partial-delete state to reason about.
//!
//! # ⚠ Why `venue` cannot answer "who wrote this"
//!
//! `venue` names an EXCHANGE, never a data source
//! (`docs/decisions/0030-data-vike-io-is-a-vendor-not-a-venue.md`), so rows fetched from a metrics
//! VENDOR land under the exchange they describe, beside that exchange's own — **indistinguishable by
//! identity, distinguishable only by commit key.** That is not hypothetical: it is the 2026-09-07
//! incident, where a second producer wrote `kind=bar/venue=hyperliquid/interval=1h` and no selector
//! over the four identity dimensions could have told the two apart.
//!
//! # ⚠ `STORE_KINDS` CLASSIFIES; it does not ADMIT
//!
//! `--produced-by` takes a declared producer's path OR a literal prefix, and the literal spelling is
//! the case that motivated the feature: by the time the the CI box store was cleaned, the producers whose
//! rows were being removed had already been DELETED from the tree (0049's verdict), so no declared
//! row named them and none ever will again. **A check that required membership in the declared
//! roster would have refused exactly the run this exists to serve.** So the plan REPORTS the
//! declared producer when there is one and says so plainly when there is not — information, never a
//! refusal.

use serde::{Deserialize, Serialize};

use crate::series::{SeriesCoverage, SeriesId};
use crate::store_kind::{Partition, key_matches_prefix, kind_ids, producers_for_key, store_kind};

/// The characters a selector value may not contain. A glob is a SECOND matcher with its own
/// escaping rules, and dimension-omission ([`SeriesSelector`]'s `None`) already covers the shape a
/// cleanup needs — so one is refused at the door rather than half-implemented.
const GLOB_CHARS: [char; 3] = ['*', '?', '['];

/// Which series a removal is aimed at: the four identity dimensions of a [`SeriesId`], with an
/// OMITTED dimension as the only wildcard there is.
///
/// `kind` and `venue` are ALWAYS required — they are the two path segments above every leaf of every
/// kind, so the blast radius is always a subtree an operator can name and see, never "the store". A
/// cleanup that genuinely spans venues is N invocations, and N dry runs is the right price for N
/// irreversible acts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesSelector {
    /// The `kind=` segment. Required; matched EXACTLY (never a substring — this is not a browse
    /// filter, and `bar` must not select `barx`).
    pub kind: String,
    /// The `venue=` segment. Required; matched exactly.
    pub venue: String,
    /// `symbol=` for a PER-SYMBOL series. `None` is a wildcard over the dimension. Mutually
    /// exclusive with [`SeriesSelector::group`] — `SeriesId` makes them alternatives.
    #[serde(default)]
    pub symbol: Option<String>,
    /// `group=` for a GROUPED series. `None` is a wildcard. See [`SeriesSelector::symbol`].
    #[serde(default)]
    pub group: Option<String>,
    /// `interval=` for a bar series. `None` is a wildcard — which is a DIFFERENT statement from a
    /// tick kind's structural absence of one, and the reason a positional grammar cannot express
    /// this dimension at all.
    #[serde(default)]
    pub interval: Option<String>,
}

impl SeriesSelector {
    /// A selector over one kind and venue, wildcard in every other dimension.
    pub fn new(kind: impl Into<String>, venue: impl Into<String>) -> Self {
        Self { kind: kind.into(), venue: venue.into(), ..Self::default() }
    }

    /// Does `id` fall in this selection?
    ///
    /// ⚠ A NAMED `symbol` excludes every grouped series and a named `group` excludes every
    /// per-symbol one, because the two are alternatives rather than a pair. With NEITHER named the
    /// selection spans both layouts — which is what a `kind`+`venue` sweep means, and what a store
    /// holding one instrument in both layouts needs it to mean.
    pub fn matches(&self, id: &SeriesId) -> bool {
        if id.kind != self.kind || id.venue != self.venue {
            return false;
        }
        if let Some(sym) = &self.symbol
            && (id.group.is_some() || &id.symbol != sym)
        {
            return false;
        }
        if let Some(grp) = &self.group
            && id.group.as_ref() != Some(grp)
        {
            return false;
        }
        if let Some(iv) = &self.interval
            && id.interval.as_ref() != Some(iv)
        {
            return false;
        }
        true
    }

    /// `true` when this selector can match MORE THAN ONE series — i.e. any of `symbol`/`group`/
    /// `interval` is wildcarded.
    ///
    /// This is the line `--produced-by` is required above, and the reason it is a property of the
    /// SELECTOR rather than of the result: a sweep that happens to match one series today is still
    /// a sweep, and gating on the match COUNT would make the requirement depend on what the store
    /// happened to hold at plan time.
    ///
    /// ⚠ "Fully named" is KIND-DEPENDENT, which is why this consults the layout table rather than
    /// counting `Some`s. A `group=` leaf has no further dimension at all; a per-symbol leaf of a
    /// tick kind has no `interval=` segment either, so `--symbol X` names exactly one series there
    /// while on `bar` it names every interval that symbol was recorded at. Counting flags would
    /// have made `--produced-by` mandatory for a fully-named tick series — more than the GUI's own
    /// Delete asks for the identical act, which is the rule §5.4 refuses to write.
    ///
    /// An UNKNOWN kind reads as a sweep: the conservative direction, since
    /// [`SeriesSelector::validate_against_kinds`] refuses it anyway and this predicate must not be
    /// the lenient one if a caller asks it first.
    pub fn is_sweep(&self) -> bool {
        if self.group.is_some() {
            return false;
        }
        if self.symbol.is_none() {
            return true;
        }
        match store_kind(&self.kind) {
            Some(row) if row.partition == Partition::SymbolInterval => self.interval.is_none(),
            Some(_) => false,
            None => true,
        }
    }

    /// The shape rules that need NO store: the ones an operator gets wrong by typing.
    ///
    /// Everything here is a fact about the command line. What is deliberately NOT here is anything
    /// needing [`crate::store_kind::STORE_KINDS`] — that is
    /// [`SeriesSelector::validate_against_kinds`], because the caller that owns the command line
    /// (`vike-cli`) may not link this crate at all, while the caller that owns the store always can.
    pub fn validate_shape(&self) -> Result<(), String> {
        if self.kind.trim().is_empty() {
            return Err("--kind is required: it is the first dimension a cleanup selects on".into());
        }
        if self.venue.trim().is_empty() {
            return Err("--venue is required: it bounds the blast radius to a subtree".into());
        }
        if self.symbol.is_some() && self.group.is_some() {
            return Err(
                "--symbol and --group are ALTERNATIVES, not a pair: a grouped series has an EMPTY \
                 symbol and a per-symbol series has no group. Pass one."
                    .into(),
            );
        }
        if self.group.is_some() && self.interval.is_some() {
            return Err(
                "--interval does not apply to --group: a grouped series' leaf has no `interval=` \
                 segment at all"
                    .into(),
            );
        }
        for (flag, value) in self.named_values() {
            if value.trim().is_empty() {
                return Err(format!(
                    "{flag} was given an EMPTY value. An empty `symbol=` is the store's \
                     GROUPED-series sentinel, so an empty selector names neither layout — omit the \
                     flag to wildcard the dimension instead."
                ));
            }
            if let Some(c) = value.chars().find(|c| GLOB_CHARS.contains(c)) {
                return Err(format!(
                    "{flag} value {value:?} contains the glob character {c:?}. Globs are refused: \
                     omitting a dimension already wildcards it, and a second matcher with its own \
                     escaping rules is not worth the ways it can be wrong."
                ));
            }
        }
        Ok(())
    }

    /// The rules that need the layout table — checked where the store is, never against a roster
    /// copied into a client.
    ///
    /// ⚠ `STORE_KINDS` is the authority here and it PINS a live contradiction it does not fix:
    /// `crates/vike-backfill/src/regroup.rs`'s `GROUPABLE_KINDS` lists `depth` as groupable while
    /// this crate has no grouped depth form. A `--group` on such a kind is refused HERE, off
    /// [`crate::store_kind::StoreKind::grouped`], which is the side that describes what is on disk.
    pub fn validate_against_kinds(&self) -> Result<(), String> {
        let Some(row) = store_kind(&self.kind) else {
            return Err(format!(
                "unknown kind {:?}. This store writes: {}",
                self.kind,
                kind_ids().collect::<Vec<_>>().join(", ")
            ));
        };
        if self.interval.is_some() && row.partition != Partition::SymbolInterval {
            return Err(format!(
                "kind {:?} does not sub-partition by interval — only `bar` does, so an \
                 `interval=` segment names nothing here",
                self.kind
            ));
        }
        if self.group.is_some() && !row.grouped {
            return Err(format!(
                "kind {:?} has no GROUPED form in this store, so `--group` selects nothing",
                self.kind
            ));
        }
        Ok(())
    }

    /// `(flag, value)` for each dimension the operator actually named — the one place the flag
    /// spellings this module reports are written down.
    fn named_values(&self) -> Vec<(&'static str, &str)> {
        let mut out: Vec<(&'static str, &str)> =
            vec![("--kind", self.kind.as_str()), ("--venue", self.venue.as_str())];
        for (flag, v) in
            [("--symbol", &self.symbol), ("--group", &self.group), ("--interval", &self.interval)]
        {
            if let Some(v) = v {
                out.push((flag, v.as_str()));
            }
        }
        out
    }

    /// The selector as one line, for a plan header and a refusal message.
    pub fn describe(&self) -> String {
        let mut parts = vec![format!("kind={}", self.kind), format!("venue={}", self.venue)];
        for (flag, value) in self.named_values().into_iter().skip(2) {
            parts.push(format!("{}={value}", flag.trim_start_matches("--")));
        }
        if self.symbol.is_none() && self.group.is_none() {
            parts.push("symbol/group=*".to_string());
        }
        if self.interval.is_none() && self.group.is_none() {
            parts.push("interval=*".to_string());
        }
        parts.join(" ")
    }
}

/// The ids in `all` this selector names — the INTERSECTION property the module doc opens with.
///
/// `all` is what the store ENUMERATED (`list_series`), so nothing here can name a series that does
/// not exist. Order is `all`'s, which the store returns sorted, so a plan is deterministic.
pub fn select_series(all: &[SeriesId], selector: &SeriesSelector) -> Vec<SeriesId> {
    all.iter().filter(|id| selector.matches(id)).cloned().collect()
}

/// One distinct commit-key PREFIX inside a series, with how many keys carry it and which declared
/// producers build one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyGroup {
    /// The prefix these keys share — the longest declared one that matched, or the key itself when
    /// no declared producer's prefix did (see [`KeyGroup::producers`]).
    pub prefix: String,
    /// How many of the series' commit keys fall in this group.
    pub count: usize,
    /// Repo-relative paths of the declared producers that build a key with this prefix. EMPTY is
    /// the ordinary answer for a producer that has been REMOVED from the tree, and the plan says so
    /// in those words.
    pub producers: Vec<String>,
}

/// The sentence a [`KeyGroup`] with no declared producer renders. Its exact words matter: it is
/// INFORMATION and not a refusal, and a reader who takes it for one will go looking for a bug that
/// is not there.
pub const NO_DECLARED_PRODUCER: &str = "no declared producer builds a key with this prefix — this may be a producer that has been \
     REMOVED";

/// One series in a plan: what it IS, what it HOLDS, and who WROTE it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedSeries {
    pub id: SeriesId,
    pub coverage: SeriesCoverage,
    /// The manifest's commit log, verbatim (`DataFusionHist::series_commits`). EMPTY is a real
    /// state — a keyless append, or parts sealed before the commit-key metadata existed — and it is
    /// the state that can satisfy no assertion.
    pub commits: Vec<String>,
    /// [`Self::commits`] folded to distinct prefixes, each classified. Derived, never stored twice.
    pub key_groups: Vec<KeyGroup>,
}

impl PlannedSeries {
    /// Fold a series' raw commit log into the plan's per-series row.
    ///
    /// `asserted` is the resolved `--produced-by` prefix when one was given. It participates in the
    /// grouping as a KNOWN prefix — which is not inventing a boundary, because the operator supplied
    /// it — and without it the motivating case renders one line per commit key: a REMOVED producer's
    /// keys match no declared prefix, so a months-long backfill's thousands of window keys would
    /// each become their own group. See [`group_keys`].
    pub fn new(
        id: SeriesId,
        coverage: SeriesCoverage,
        commits: Vec<String>,
        asserted: Option<&str>,
    ) -> Self {
        let key_groups = group_keys(&commits, asserted);
        Self { id, coverage, commits, key_groups }
    }

    /// The keys that do NOT carry `prefix` — the evidence a provenance refusal names.
    pub fn foreign_keys(&self, prefix: &str) -> Vec<&str> {
        self.commits.iter().filter(|k| !key_matches_prefix(k, prefix)).map(String::as_str).collect()
    }
}

/// Fold a commit log into distinct classified prefixes.
///
/// A key is attributed to the LONGEST prefix it carries, over the DECLARED prefixes plus the
/// operator's `asserted` one. Longest, so `pmxt:quote:` beats a hypothetical `pmxt:` rather than
/// both claiming the key and rendering as two groups of one — and so the operator's own prefix wins
/// over a declared ancestor of it, which is the reading that matches what they typed.
///
/// ⚠ **`asserted` is included for a measured reason, not for symmetry.** A REMOVED producer's keys
/// match NO declared prefix, and the fallback for an unmatched key is to make it its own group keyed
/// on the WHOLE key. That is the honest fallback — inventing a prefix boundary the store has no
/// basis for is worse — but for the case this whole feature exists to serve (a months-long
/// backfill's thousands of per-window keys) it renders one plan line per key. The operator's prefix
/// folds exactly those into one counted group, and the fold is still evidence-based: the keys
/// genuinely carry it.
///
/// A group whose prefix no DECLARED producer builds carries an empty [`KeyGroup::producers`], and
/// the render says [`NO_DECLARED_PRODUCER`] — information, not a refusal.
fn group_keys(commits: &[String], asserted: Option<&str>) -> Vec<KeyGroup> {
    let mut groups: Vec<KeyGroup> = Vec::new();
    for key in commits {
        let hits = producers_for_key(key);
        let mut best = "";
        let mut producers: Vec<String> = Vec::new();
        for (_, ck) in &hits {
            if let Some(p) = crate::store_kind::commit_key_prefix(ck.template)
                && p.len() > best.len()
            {
                best = p;
            }
        }
        if let Some(a) = asserted
            && key_matches_prefix(key, a)
            && a.len() > best.len()
        {
            best = a;
        }
        let prefix = if best.is_empty() {
            // No declared prefix and no asserted one matched: the key IS its group.
            key.clone()
        } else {
            for (_, ck) in &hits {
                if crate::store_kind::commit_key_prefix(ck.template) == Some(best) {
                    producers.push(ck.producer.to_string());
                }
            }
            best.to_string()
        };
        producers.sort_unstable();
        producers.dedup();
        match groups.iter_mut().find(|g| g.prefix == prefix) {
            Some(g) => g.count += 1,
            None => groups.push(KeyGroup { prefix, count: 1, producers }),
        }
    }
    groups
}

/// Everything a removal WOULD do, computed before anything is removed — and printed in full whether
/// or not `--dry-run` was asked for.
///
/// The plan is the first half of every run, not a flag you can forget: `--dry-run` only decides
/// whether the second half happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalPlan {
    pub selector: SeriesSelector,
    /// The resolved `--produced-by` PREFIX (never the spelling the operator typed — a producer path
    /// has already been resolved to its prefix by `store_kind::resolve_produced_by`).
    #[serde(default)]
    pub produced_by: Option<String>,
    /// One row per matched series, in the store's enumeration order.
    pub series: Vec<PlannedSeries>,
}

impl RemovalPlan {
    /// How many series matched. **ZERO is a SUCCESS**, not a refusal: `delete_series` is idempotent
    /// and a cleanup that fails on re-run is a cleanup nobody re-runs.
    pub fn matched(&self) -> usize {
        self.series.len()
    }

    /// Total rows across the matched set (manifest fold — no Parquet scan).
    pub fn rows(&self) -> u64 {
        self.series.iter().map(|s| s.coverage.rows).sum()
    }

    /// Total on-disk part bytes across the matched set.
    pub fn bytes(&self) -> u64 {
        self.series.iter().map(|s| s.coverage.bytes).sum()
    }

    /// The provenance VERDICT: `Ok(())` when every selected series satisfies the assertion, or one
    /// refusal line per offending series.
    ///
    /// With no `--produced-by` this is vacuously `Ok`. With one, TWO shapes refuse:
    ///
    /// * a series carrying a key the prefix does not match — named, WITH the key, because "some key
    ///   did not match" is not a fact an operator can act on;
    /// * a series recording NO keys at all — it cannot satisfy an assertion, so under
    ///   `--produced-by` it refuses. (Without the flag it is disclosed as having no provenance on
    ///   record and may be deleted; that asymmetry is the whole difference between deleting by name
    ///   and deleting by a checked property.)
    ///
    /// ⚠ This is the PLAN-TIME check. It is re-run under the series lock immediately before each
    /// removal (`DataFusionHist::delete_series_checked`), because a live recorder can commit a key
    /// in between and a check that ran only here would be a TOCTOU on the one property the verb
    /// turns on.
    pub fn verdict(&self) -> Result<(), Vec<String>> {
        let Some(prefix) = self.produced_by.as_deref() else { return Ok(()) };
        let mut refusals = Vec::new();
        for s in &self.series {
            if s.commits.is_empty() {
                refusals.push(format!(
                    "{}: records NO commit keys, so it cannot satisfy --produced-by {prefix:?} \
                     (a keyless append, or parts sealed before the commit-key metadata existed)",
                    describe_id(&s.id)
                ));
                continue;
            }
            let foreign = s.foreign_keys(prefix);
            if !foreign.is_empty() {
                refusals.push(format!(
                    "{}: {} of its {} commit keys do not carry --produced-by {prefix:?}, e.g. {:?}",
                    describe_id(&s.id),
                    foreign.len(),
                    s.commits.len(),
                    foreign[0]
                ));
            }
        }
        if refusals.is_empty() { Ok(()) } else { Err(refusals) }
    }

    /// The plan as an operator reads it: one block per series, then the totals and the verdict.
    ///
    /// ⚠ It does NOT carry the resolved store root — that line is the CALLER's, because only the
    /// process that resolved the root knows which rung answered, and "which store" is the question
    /// a destructive verb must answer before "which series". See the engine's `--rm-series` arm.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("selector: {}", self.selector.describe())];
        out.push(match &self.produced_by {
            Some(p) => format!("--produced-by: {p:?} (asserted over EVERY key of EVERY series)"),
            None => "--produced-by: none (provenance is REPORTED below, not asserted)".to_string(),
        });
        if self.series.is_empty() {
            out.push("matched 0 series — nothing to delete".to_string());
            return out;
        }
        for s in &self.series {
            let cov = &s.coverage;
            out.push(format!(
                "  {}  {} rows · {} B · {} parts · {} days · {}..{}",
                describe_id(&s.id),
                cov.rows,
                cov.bytes,
                cov.parts,
                cov.dates,
                cov.first_ts,
                cov.last_ts
            ));
            if s.key_groups.is_empty() {
                out.push("      provenance: none recorded".to_string());
            }
            for g in &s.key_groups {
                let who = if g.producers.is_empty() {
                    NO_DECLARED_PRODUCER.to_string()
                } else {
                    g.producers.join(", ")
                };
                out.push(format!("      {} ×{} — {who}", g.prefix, g.count));
            }
        }
        out.push(format!(
            "TOTAL: {} series · {} rows · {} B",
            self.matched(),
            self.rows(),
            self.bytes()
        ));
        match self.verdict() {
            Ok(()) => out.push("provenance: SATISFIED".to_string()),
            Err(refusals) => {
                out.push("provenance: REFUSED — nothing will be deleted".to_string());
                out.extend(refusals.into_iter().map(|r| format!("  {r}")));
            }
        }
        out
    }
}

/// What actually happened, per series — reported whether the run succeeded or not.
///
/// **One broken series is one SKIPPED series**, which is `run_maintenance`'s own rule: a failure
/// mid-run is reported, the rest continue, and the exit is non-zero. Because the delete is
/// idempotent, a re-run finishes the job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalOutcome {
    pub deleted: Vec<SeriesId>,
    /// `(series, why)` — the failures, each naming its own series.
    pub failed: Vec<(SeriesId, String)>,
}

impl RemovalOutcome {
    /// `true` when every attempted removal succeeded (including the zero-attempt case).
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Build the plan for `selector` over `store` — the store-facing half, and the ONE place a plan is
/// assembled, so the CLI, the engine and the datahub wire cannot come to disagree about what a
/// selector means.
///
/// Three trait calls and no directory walk of its own: `inventory()` for the identities and their
/// coverage (a manifest fold, no Parquet scan — cheap enough per series in a loop, which is what
/// `inventory` already is), then `series_commits()` per MATCHED series only. The filter runs before
/// the provenance reads, so a narrow selector against a large store pays for what it selected.
///
/// `produced_by` is the RESOLVED prefix. Resolving a `--produced-by` SPELLING (a producer path or a
/// literal) is `crate::store_kind::resolve_produced_by`, and it happens before this call because it
/// is a fact about the argument, not about the store.
pub fn plan_removal(
    store: &dyn crate::hist::HistStore,
    selector: &SeriesSelector,
    produced_by: Option<&str>,
) -> Result<RemovalPlan, crate::hist::DataError> {
    selector.validate_shape().map_err(crate::hist::DataError::Query)?;
    selector.validate_against_kinds().map_err(crate::hist::DataError::Query)?;
    let inventory = store.inventory()?;
    let mut series = Vec::new();
    for (id, coverage) in inventory {
        if !selector.matches(&id) {
            continue;
        }
        let commits = store.series_commits(&id)?;
        series.push(PlannedSeries::new(id, coverage, commits, produced_by));
    }
    Ok(RemovalPlan {
        selector: selector.clone(),
        produced_by: produced_by.map(str::to_string),
        series,
    })
}

/// Execute a plan whose [`RemovalPlan::verdict`] has already been satisfied.
///
/// ⚠ The verdict is checked HERE TOO rather than being assumed of the caller: this is the last
/// function before bytes go, and "the caller checked" is the shape a refactor silently drops. A
/// refused plan returns `Err` and removes nothing.
///
/// Per-series failures are COLLECTED, never propagated — one broken series is one skipped series
/// (`DataFusionHist::run_maintenance`'s rule), and the caller exits non-zero off
/// [`RemovalOutcome::is_clean`]. Each removal re-asserts the provenance under that series' own lock,
/// so a key committed between the plan and this call refuses that series rather than deleting it.
pub fn execute_removal(
    store: &dyn crate::hist::HistStore,
    plan: &RemovalPlan,
) -> Result<RemovalOutcome, crate::hist::DataError> {
    if let Err(refusals) = plan.verdict() {
        return Err(crate::hist::DataError::Query(format!(
            "provenance REFUSED — nothing was deleted:\n{}",
            refusals.join("\n")
        )));
    }
    let mut outcome = RemovalOutcome::default();
    for s in &plan.series {
        match store.delete_series_checked(&s.id, plan.produced_by.as_deref()) {
            Ok(()) => outcome.deleted.push(s.id.clone()),
            Err(e) => outcome.failed.push((s.id.clone(), e.to_string())),
        }
    }
    Ok(outcome)
}

/// One series' identity as a single readable token — `kind/venue/label[/interval]`, with the
/// grouped/per-symbol alternative resolved by `SeriesId::label` rather than by guessing at
/// `symbol`.
pub fn describe_id(id: &SeriesId) -> String {
    let scope = if id.group.is_some() { "group" } else { "symbol" };
    match &id.interval {
        Some(iv) => format!("{}/{}/{scope}={}/{iv}", id.kind, id.venue, id.label()),
        None => format!("{}/{}/{scope}={}", id.kind, id.venue, id.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(venue: &str, symbol: &str, interval: &str) -> SeriesId {
        SeriesId::per_symbol("bar", venue, symbol, Some(interval.to_string()))
    }

    fn grouped(kind: &str, venue: &str, group: &str) -> SeriesId {
        SeriesId::grouped(kind, venue, group)
    }

    fn all() -> Vec<SeriesId> {
        vec![
            bar("hyperliquid", "BTC", "1h"),
            bar("hyperliquid", "BTC", "1m"),
            bar("hyperliquid", "ETH", "1h"),
            bar("binance", "BTCUSDT", "1h"),
            grouped("book", "polymarket", "btc-5m"),
            SeriesId::per_symbol("book", "polymarket", "0x1234", None),
        ]
    }

    /// The headline: a `kind`+`venue` selector sweeps the subtree and NOTHING outside it.
    #[test]
    fn a_selector_intersects_the_stores_own_enumeration() {
        let sel = SeriesSelector::new("bar", "hyperliquid");
        let got = select_series(&all(), &sel);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got.iter().all(|id| id.venue == "hyperliquid"));
    }

    /// A typo matches NOTHING. It never matches something adjacent, because the match is over ids
    /// the store produced rather than over a path this code built.
    #[test]
    fn a_typo_matches_nothing_rather_than_something_adjacent() {
        let mut sel = SeriesSelector::new("bar", "hyperliquld");
        assert!(select_series(&all(), &sel).is_empty());
        sel = SeriesSelector::new("bar", "hyperliquid");
        sel.symbol = Some("BTCUSDT".to_string()); // binance's symbol, hyperliquid's venue
        assert!(select_series(&all(), &sel).is_empty());
    }

    /// `--symbol` and `--group` select DISJOINT layouts, and naming neither spans both — the
    /// alternative `SeriesId` actually encodes.
    #[test]
    fn symbol_and_group_select_disjoint_layouts_and_neither_spans_both() {
        let mut sel = SeriesSelector::new("book", "polymarket");
        assert_eq!(select_series(&all(), &sel).len(), 2, "both layouts");

        sel.group = Some("btc-5m".to_string());
        let g = select_series(&all(), &sel);
        assert_eq!(g.len(), 1);
        assert!(g[0].group.is_some());

        sel.group = None;
        sel.symbol = Some("0x1234".to_string());
        let s = select_series(&all(), &sel);
        assert_eq!(s.len(), 1);
        assert!(s[0].group.is_none(), "a named --symbol must not select the grouped twin");
    }

    /// An omitted `--interval` is a wildcard, and a named one is exact.
    #[test]
    fn interval_is_a_wildcard_when_omitted() {
        let mut sel = SeriesSelector::new("bar", "hyperliquid");
        sel.symbol = Some("BTC".to_string());
        assert_eq!(select_series(&all(), &sel).len(), 2, "both intervals");
        sel.interval = Some("1h".to_string());
        assert_eq!(select_series(&all(), &sel).len(), 1);
    }

    /// A fully-pinned selector is not a sweep; ANY wildcarded dimension is.
    #[test]
    fn a_sweep_is_any_wildcarded_dimension() {
        let mut sel = SeriesSelector::new("bar", "binance");
        assert!(sel.is_sweep());
        sel.symbol = Some("BTCUSDT".to_string());
        assert!(sel.is_sweep(), "interval still wildcarded");
        sel.interval = Some("1h".to_string());
        assert!(!sel.is_sweep());

        // A grouped series is fully named by kind/venue/group — it HAS no interval dimension.
        let mut g = SeriesSelector::new("book", "polymarket");
        g.group = Some("btc-5m".to_string());
        assert!(!g.is_sweep());

        // ...and so is a per-symbol TICK series, whose leaf carries no `interval=` segment. This is
        // the case a flag COUNT gets wrong, and getting it wrong would demand more of the CLI than
        // the GUI's own Delete asks for the identical act.
        let mut t = SeriesSelector::new("trade", "binance");
        t.symbol = Some("BTCUSDT".to_string());
        assert!(!t.is_sweep());

        // An unknown kind reads as a sweep — the conservative direction.
        let mut u = SeriesSelector::new("nope", "binance");
        u.symbol = Some("BTCUSDT".to_string());
        assert!(u.is_sweep());
    }

    #[test]
    fn the_shape_rules_refuse_what_an_operator_types_wrong() {
        let mut sel = SeriesSelector::new("", "binance");
        assert!(sel.validate_shape().unwrap_err().contains("--kind is required"));

        sel = SeriesSelector::new("bar", "");
        assert!(sel.validate_shape().unwrap_err().contains("--venue is required"));

        sel = SeriesSelector::new("bar", "binance");
        sel.symbol = Some("BTC".into());
        sel.group = Some("g".into());
        assert!(sel.validate_shape().unwrap_err().contains("ALTERNATIVES"));

        sel = SeriesSelector::new("book", "polymarket");
        sel.group = Some("g".into());
        sel.interval = Some("1h".into());
        assert!(sel.validate_shape().unwrap_err().contains("--interval does not apply"));

        sel = SeriesSelector::new("bar", "binance");
        sel.symbol = Some("BTC*".into());
        assert!(sel.validate_shape().unwrap_err().contains("glob"));

        sel = SeriesSelector::new("bar", "binance");
        sel.symbol = Some("  ".into());
        assert!(sel.validate_shape().unwrap_err().contains("EMPTY value"));
    }

    /// The store-side rules read `STORE_KINDS`, never a roster copied into a client.
    #[test]
    fn the_layout_rules_come_from_the_kind_table() {
        let sel = SeriesSelector::new("nope", "binance");
        let err = sel.validate_against_kinds().unwrap_err();
        assert!(err.contains("unknown kind"), "{err}");
        assert!(err.contains("bar"), "the message names the whole set: {err}");

        let mut iv = SeriesSelector::new("trade", "binance");
        iv.interval = Some("1h".into());
        assert!(iv.validate_against_kinds().unwrap_err().contains("only `bar`"));

        // `bar` declares no grouped form, so `--group` on it selects nothing and says so.
        let mut g = SeriesSelector::new("bar", "binance");
        g.group = Some("x".into());
        assert!(g.validate_against_kinds().unwrap_err().contains("no GROUPED form"));

        SeriesSelector::new("bar", "binance").validate_against_kinds().unwrap();
    }

    fn plan(commits: Vec<&str>, produced_by: Option<&str>) -> RemovalPlan {
        RemovalPlan {
            selector: SeriesSelector::new("bar", "hyperliquid"),
            produced_by: produced_by.map(str::to_string),
            series: vec![PlannedSeries::new(
                bar("hyperliquid", "BTC", "1h"),
                SeriesCoverage { rows: 10, bytes: 99, parts: 1, dates: 1, ..Default::default() },
                commits.into_iter().map(str::to_string).collect(),
                produced_by,
            )],
        }
    }

    /// **The MIXED-SERIES case, which is why this is an assertion.** A filter would have skipped
    /// this series and deleted the rest; the assertion refuses the whole run and names the key.
    #[test]
    fn one_foreign_key_refuses_the_whole_run_and_names_it() {
        let p = plan(vec!["panel_bars:hl:BTC:1", "binance:BTCUSDT:1h:0-1"], Some("panel_bars:"));
        let refusals = p.verdict().unwrap_err();
        assert_eq!(refusals.len(), 1);
        assert!(refusals[0].contains("binance:BTCUSDT:1h:0-1"), "{}", refusals[0]);
        assert!(p.lines().iter().any(|l| l.contains("REFUSED")), "{:?}", p.lines());
    }

    /// A series recording no keys cannot satisfy an assertion — and IS deletable without one.
    #[test]
    fn an_empty_commit_log_refuses_under_an_assertion_and_passes_without_one() {
        assert!(plan(vec![], Some("panel_bars:")).verdict().is_err());
        plan(vec![], None).verdict().expect("no assertion, no refusal");
        assert!(
            plan(vec![], None).lines().iter().any(|l| l.contains("none recorded")),
            "the absence is DISCLOSED even when it is not a refusal"
        );
    }

    /// A REMOVED producer's key classifies as unknown and is reported in those words — the case
    /// that motivated the literal-prefix spelling, and a place a refusal would be wrong.
    #[test]
    fn a_removed_producers_key_is_reported_not_refused() {
        let p = plan(vec!["panel_bars:hl:BTC:1", "panel_bars:hl:BTC:2"], Some("panel_bars:"));
        p.verdict().expect("every key carries the prefix");
        let lines = p.lines();
        assert!(lines.iter().any(|l| l.contains(NO_DECLARED_PRODUCER)), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("×2")),
            "keys fold to one counted group: {lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("SATISFIED")), "{lines:?}");
    }

    /// A key a declared producer DOES build resolves to that producer's path.
    #[test]
    fn a_declared_producers_key_names_the_file_that_builds_it() {
        let p = plan(vec!["demo-tape:v1:BTCUSDT:1h"], None);
        assert_eq!(p.series[0].key_groups[0].prefix, "demo-tape:v");
        assert_eq!(p.series[0].key_groups[0].producers, vec!["crates/vike-data/src/demo.rs"]);
    }

    /// ⚠ Without `--produced-by`, an unrecognised key IS its own group — which is honest and, for
    /// the case this feature exists for, unreadable: a months-long backfill's per-window keys would
    /// render one plan line each. The operator's own prefix is what folds them.
    #[test]
    fn the_asserted_prefix_folds_the_keys_no_declared_producer_claims() {
        let unasserted = plan(vec!["panel_bars:hl:BTC:1", "panel_bars:hl:BTC:2"], None);
        assert_eq!(unasserted.series[0].key_groups.len(), 2, "each unknown key is its own group");

        let asserted =
            plan(vec!["panel_bars:hl:BTC:1", "panel_bars:hl:BTC:2"], Some("panel_bars:"));
        assert_eq!(asserted.series[0].key_groups.len(), 1);
        assert_eq!(asserted.series[0].key_groups[0].count, 2);
        assert!(
            asserted.series[0].key_groups[0].producers.is_empty(),
            "folding under the operator's prefix must not INVENT a producer for it"
        );
    }

    /// Zero matches is a plan that says so, with the totals absent rather than zeroed into a table.
    #[test]
    fn nothing_matched_renders_as_nothing_to_delete() {
        let p = RemovalPlan {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: None,
            series: Vec::new(),
        };
        assert_eq!(p.matched(), 0);
        assert!(p.verdict().is_ok(), "an empty set satisfies every assertion");
        assert!(p.lines().iter().any(|l| l.contains("matched 0 series")), "{:?}", p.lines());
    }

    /// The id rendering resolves the grouped/per-symbol alternative rather than printing an empty
    /// `symbol=` for half the store.
    #[test]
    fn an_id_renders_its_scope_rather_than_a_blank_symbol() {
        assert_eq!(describe_id(&bar("hyperliquid", "BTC", "1h")), "bar/hyperliquid/symbol=BTC/1h");
        assert_eq!(
            describe_id(&grouped("book", "polymarket", "btc-5m")),
            "book/polymarket/group=btc-5m"
        );
    }
}
