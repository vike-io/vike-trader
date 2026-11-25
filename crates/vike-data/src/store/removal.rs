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
//! Everything here is PURE and engine-free (like [`crate::store::series`]): a [`SeriesSelector`] is
//! matched against ids the STORE enumerated, and a [`RemovalPlan`] is a fold over coverage and
//! commit keys the store read. Nothing in this file opens a directory, and nothing in it deletes
//! anything — `HistoryStoreDataFusion`'s `delete_series_checked` is the one verb that removes bytes,
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

use crate::store::series::{SeriesCoverage, SeriesId};
use crate::store::store_kind::{
    StoreKind, StoreKindPartition, key_matches_prefix, producers_for_key,
};
use vike_model::{StoreKindId, StorePlane};

vike_utility::refusal_enum! {
    /// A delete refused for its SELECTOR or its `produced_by` before anything is planned. ONE copy for every door:
    /// `vike-cli data history rm` and MCP's `delete_series` refuse with it before they dial, the datahub's
    /// `DeleteSeries` and `vike-backend datahub store rm` through [`plan_removal`] (decision 0301: decided on the
    /// type, shown through the macro's `rendered()`). A flag is named as the command lines spell it; MCP's argument
    /// is the same word (`--symbol` is `symbol`), and a dimension by its path segment, which `--lane` and the
    /// store tool's `--source` both name (`source=`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SelectorRefusal with ids SelectorRefusalId {
        /// A blank `venue`: with `kind` it bounds the blast radius.
        VenueRequired => "data.delete.venue_required":
            "--venue is required: it bounds the blast radius to a subtree",
        /// Both `symbol` and `group`: `SeriesId` makes them alternatives.
        SymbolAndGroup => "data.delete.symbol_and_group":
            "--symbol and --group are ALTERNATIVES, not a pair: a grouped series has an EMPTY symbol and a \
             per-symbol series has no group. Pass one.",
        /// An `interval` beside a `group`.
        IntervalOnGroup => "data.delete.interval_on_group":
            "--interval does not apply to --group: a grouped series' leaf has no `interval=` segment at all",
        /// A named dimension that is blank: an empty `symbol=` is the grouped sentinel.
        EmptyValue { segment: &'static str } => "data.delete.empty_value":
            "`{segment}=` was given an EMPTY value. An empty `symbol=` is the store's GROUPED-series sentinel, so \
             an empty selector names neither layout — omit it to wildcard the dimension instead.",
        /// A dimension carrying a glob character (`GLOB_CHARS`).
        Glob { segment: &'static str, value: String, glob: char } => "data.delete.glob":
            "`{segment}=` value {value:?} contains the glob character {glob:?}. Globs are refused: omitting a \
             dimension already wildcards it, and a second matcher with its own escaping rules is not worth the \
             ways it can be wrong.",
        /// An ACCOUNT kind ([`account_data_refusal`]); `extra` is the funding kind's sentence, or empty.
        AccountData { kind: StoreKindId, extra: &'static str } => "data.delete.account_data":
            "`{kind}` is ACCOUNT data, not market data, and no delete door removes it{extra}. Your fills, orders, \
             funding payments, forecasts and equity are history no venue serves twice. The market funding RATE \
             is unaffected and is not an account kind — it is `--kind bar --interval funding`.",
        /// A `produced_by` carrying a glob character ([`produced_by_refusal`]).
        ProducedByGlob { produced_by: String, glob: char } => "data.delete.produced_by_glob":
            "--produced-by value {produced_by:?} contains the glob character {glob:?}. A prefix is matched with \
             `starts_with`, never globbed: pass the literal prefix the rows carry",
        /// A producer PATH on a delete through a datahub ([`produced_by_refusal`]).
        ProducedByPath { produced_by: String } => "data.delete.produced_by_path":
            "--produced-by {produced_by:?} looks like a PRODUCER PATH, and a datahub that has not been redeployed \
             since 2026-09-11 resolves none: it would be asserted as a literal prefix, match no commit key in any \
             series, and report your data as foreign when in fact it was never resolved. This protocol carries no \
             capability string for \"this server resolves producer paths\", so a path is refused before anything \
             is dialled. Pass the commit-key PREFIX literally (e.g. `panel_bars:`; a dry run reports each \
             series' keys), or run the delete against a local store (`--store DIR`), which always resolves a path.",
    }
}

/// The refusal of a removal aimed at ACCOUNT data, at every delete door (`vike-cli data history rm` and MCP's
/// `delete_series` before they dial, the datahub's `DeleteSeries` the desktop Data Manager sends, and
/// `vike-backend datahub store rm`): `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md`
/// verdict 3. `None` for a market kind.
///
/// ⚠ On the REMOVAL path only ([`plan_removal`]), never in [`SeriesSelector::validate_against_kinds`]:
/// `vike-backend datahub store repair` runs that check too, and an account series is the one whose
/// manifest most needs rebuilding (history no venue serves twice).
pub fn account_data_refusal(kind: StoreKindId) -> Option<SelectorRefusal> {
    if kind.plane() != StorePlane::Account {
        return None;
    }
    let extra = if kind == StoreKindId::ExecFunding {
        " — and for this kind a scoped delete is not merely unimplemented but UNEXPRESSIBLE: the \
         series partitions on (venue, coin) while the account rides in the commit key, so one \
         series holds every account's payments for that coin and a removal deletes by series"
    } else {
        ""
    };
    Some(SelectorRefusal::AccountData { kind, extra })
}

/// The rules of a `produced_by` SPELLING a door checks before it dials or spawns, on top of
/// `crate::store::store_kind::resolve_produced_by` (blank, undeclared path): a glob is refused (the prefix is
/// matched with `starts_with`), and on a delete `through_a_datahub` so is a producer PATH.
///
/// ⚠ **The path refusal is a COMPATIBILITY guard.** The datahub resolves a path since 2026-09-11
/// (`crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb`), but this protocol is capability-negotiated
/// and carries no string for "this server resolves producer paths", so a door cannot tell a redeployed datahub
/// from an older one, which would assert the path as a literal prefix, match no key, and report the store as
/// foreign (#1754). A local store (`vike-backend datahub store rm`) always resolves one.
pub fn produced_by_refusal(produced_by: &str, through_a_datahub: bool) -> Option<SelectorRefusal> {
    if let Some(glob) = produced_by.chars().find(|c| GLOB_CHARS.contains(c)) {
        return Some(SelectorRefusal::ProducedByGlob {
            produced_by: produced_by.to_string(),
            glob,
        });
    }
    (through_a_datahub && produced_by.contains('/'))
        .then(|| SelectorRefusal::ProducedByPath { produced_by: produced_by.to_string() })
}

/// The characters a selector value may not contain. A glob is a SECOND matcher with its own
/// escaping rules, and dimension-omission ([`SeriesSelector`]'s `None`) already covers the shape a
/// cleanup needs — so one is refused at the door rather than half-implemented.
const GLOB_CHARS: [char; 3] = ['*', '?', '['];

/// The refusal for a selector with no `--kind`.
const KIND_REQUIRED: &str = "--kind is required: it is the first dimension a cleanup selects on";

/// Which series a removal is aimed at: the four identity dimensions of a [`SeriesId`], with an
/// OMITTED dimension as the only wildcard there is.
///
/// `kind` and `venue` are ALWAYS required — they are the two path segments above every leaf of every
/// kind, so the blast radius is always a subtree an operator can name and see, never "the store". A
/// cleanup that genuinely spans venues is N invocations, and N dry runs is the right price for N
/// irreversible acts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesSelector {
    /// The `kind=` segment. Required, and CLOSED: a [`StoreKindId`], so a string no `STORE_KINDS` row
    /// declares is refused where it is parsed (the command line, the wire) rather than here.
    pub kind: StoreKindId,
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
    /// `source=` for one LANE of a series ([`SeriesId::source`]). ⚠ **The one dimension whose `None` is
    /// not a wildcard:** it selects the sourceless series and reaches no lane, so a removal takes a lane
    /// only when it names one. Absent on the wire when `None`, so a frame without it is the frame every
    /// older peer knows; a peer that would ignore a `Some` is refused by
    /// `vike_datahub_client`'s `FEATURE_SOURCE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl SeriesSelector {
    /// A selector over one kind and venue's sourceless series, wildcard in the other dimensions.
    pub fn new(kind: StoreKindId, venue: impl Into<String>) -> Self {
        Self { kind, venue: venue.into(), symbol: None, group: None, interval: None, source: None }
    }

    /// The `--kind` ARGUMENT, parsed where it is typed: absent or blank is refused as required, and
    /// a string no `STORE_KINDS` row declares is refused by [`StoreKindId`]'s `FromStr`, whose
    /// message names the whole declared set. The one spelling of both refusals for a command line
    /// that builds a selector.
    pub fn parse_kind(raw: Option<&str>) -> Result<StoreKindId, String> {
        match raw {
            None => Err(KIND_REQUIRED.to_string()),
            Some(k) if k.trim().is_empty() => Err(KIND_REQUIRED.to_string()),
            Some(k) => k.parse::<StoreKindId>().map_err(|e| e.to_string()),
        }
    }

    /// Does `id` fall in this selection?
    ///
    /// ⚠ A NAMED `symbol` excludes every grouped series and a named `group` excludes every
    /// per-symbol one, because the two are alternatives rather than a pair. With NEITHER named the
    /// selection spans both layouts — which is what a `kind`+`venue` sweep means, and what a store
    /// holding one instrument in both layouts needs it to mean.
    pub fn matches(&self, id: &SeriesId) -> bool {
        if id.kind != self.kind || id.venue != self.venue || id.source != self.source {
            return false;
        }
        if let Some(symbol) = &self.symbol
            && (id.group.is_some() || &id.symbol != symbol)
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
    pub fn is_sweep(&self) -> bool {
        if self.group.is_some() {
            return false;
        }
        if self.symbol.is_none() {
            return true;
        }
        StoreKind::of(self.kind).partition == StoreKindPartition::SymbolInterval
            && self.interval.is_none()
    }

    /// The shape rules that need NO store: the ones an operator gets wrong by typing. The ONE copy:
    /// `vike-cli data history rm` and MCP's `delete_series` call it before they dial, [`plan_removal`]
    /// on the far side.
    ///
    /// Everything here is a fact about the command line. What is deliberately NOT here is anything
    /// needing the layout table's ROWS — that is [`SeriesSelector::validate_against_kinds`]. An
    /// unknown `--kind` never reaches either: the field is a [`StoreKindId`], so it is refused where
    /// the string is parsed.
    pub fn validate_shape(&self) -> Result<(), SelectorRefusal> {
        if self.venue.trim().is_empty() {
            return Err(SelectorRefusal::VenueRequired);
        }
        if self.symbol.is_some() && self.group.is_some() {
            return Err(SelectorRefusal::SymbolAndGroup);
        }
        if self.group.is_some() && self.interval.is_some() {
            return Err(SelectorRefusal::IntervalOnGroup);
        }
        for (flag, value) in self.named_values() {
            let segment = flag.trim_start_matches("--");
            if value.trim().is_empty() {
                return Err(SelectorRefusal::EmptyValue { segment });
            }
            if let Some(glob) = value.chars().find(|c| GLOB_CHARS.contains(c)) {
                return Err(SelectorRefusal::Glob { segment, value: value.to_string(), glob });
            }
        }
        Ok(())
    }

    /// The rules that need the layout table — checked where the store is, never against a roster
    /// copied into a client.
    ///
    /// ⚠ `STORE_KINDS` is the authority here: a `--group` on a kind with no grouped form (every
    /// kind but `quote`, `trade` and `book`; `depth` included) is refused HERE, off
    /// [`crate::store::store_kind::StoreKind::grouped`], which is the side that describes what is on disk.
    pub fn validate_against_kinds(&self) -> Result<(), String> {
        let row = StoreKind::of(self.kind);
        if self.interval.is_some() && row.partition != StoreKindPartition::SymbolInterval {
            return Err(format!(
                "kind {:?} does not sub-partition by interval — only `bar` does, so an \
                 `interval=` segment names nothing here",
                self.kind.word()
            ));
        }
        if self.group.is_some() && !row.grouped {
            return Err(format!(
                "kind {:?} has no GROUPED form in this store, so `--group` selects nothing",
                self.kind.word()
            ));
        }
        Ok(())
    }

    /// `(flag, value)` for each dimension the operator actually named — the one place the flag
    /// spellings this module reports are written down.
    fn named_values(&self) -> Vec<(&'static str, &str)> {
        let mut out: Vec<(&'static str, &str)> =
            vec![("--kind", self.kind.word()), ("--venue", self.venue.as_str())];
        for (flag, v) in [
            ("--symbol", &self.symbol),
            ("--group", &self.group),
            ("--interval", &self.interval),
            ("--source", &self.source),
        ] {
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
pub struct RemovalPlanKeyGroup {
    /// The prefix these keys share — the longest declared one that matched, or the key itself when
    /// no declared producer's prefix did (see [`RemovalPlanKeyGroup::producers`]).
    pub prefix: String,
    /// How many of the series' commit keys fall in this group.
    pub count: usize,
    /// Repo-relative paths of the declared producers that build a key with this prefix. EMPTY is
    /// the ordinary answer for a producer that has been REMOVED from the tree, and the plan says so
    /// in those words.
    pub producers: Vec<String>,
}

/// The sentence a [`RemovalPlanKeyGroup`] with no declared producer renders. Its exact words matter: it is
/// INFORMATION and not a refusal, and a reader who takes it for one will go looking for a bug that
/// is not there.
pub const NO_DECLARED_PRODUCER: &str = "no declared producer builds a key with this prefix — this may be a producer that has been \
     REMOVED";

/// One series in a plan: what it IS, what it HOLDS, and who WROTE it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalPlanSeries {
    pub id: SeriesId,
    pub coverage: SeriesCoverage,
    /// The manifest's commit log, verbatim (`HistoryStoreDataFusion::series_commits`). EMPTY is a real
    /// state — a keyless append, or parts sealed before the commit-key metadata existed — and it is
    /// the state that can satisfy no assertion.
    pub commits: Vec<String>,
    /// [`Self::commits`] folded to distinct prefixes, each classified. Derived, never stored twice.
    pub key_groups: Vec<RemovalPlanKeyGroup>,
}

impl RemovalPlanSeries {
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
/// A group whose prefix no DECLARED producer builds carries an empty [`RemovalPlanKeyGroup::producers`], and
/// the render says [`NO_DECLARED_PRODUCER`] — information, not a refusal.
fn group_keys(commits: &[String], asserted: Option<&str>) -> Vec<RemovalPlanKeyGroup> {
    let mut groups: Vec<RemovalPlanKeyGroup> = Vec::new();
    for key in commits {
        let hits = producers_for_key(key);
        let mut best = "";
        let mut producers: Vec<String> = Vec::new();
        for (_, ck) in &hits {
            if let Some(p) = crate::store::store_kind::commit_key_prefix(ck.template)
                && p.len() > best.len()
            {
                best = p;
            }
        }
        // A venue window key (`oanda:EUR_USD:5s:{from}-{to}`) has no prefix its template can state, so
        // it is recognised by SHAPE: without this a day-chunked series listed every one of its
        // thousands of keys on its own line, each marked "no declared producer".
        let venue_prefix = crate::store::store_kind::venue_window_key_prefix(key);
        if let Some(v) = venue_prefix
            && v.len() > best.len()
        {
            best = v;
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
                let own = crate::store::store_kind::commit_key_prefix(ck.template);
                if own == Some(best) || (own.is_none() && venue_prefix == Some(best)) {
                    producers.push(ck.producer.to_string());
                }
            }
            best.to_string()
        };
        producers.sort_unstable();
        producers.dedup();
        match groups.iter_mut().find(|g| g.prefix == prefix) {
            Some(g) => g.count += 1,
            None => groups.push(RemovalPlanKeyGroup { prefix, count: 1, producers }),
        }
    }
    groups
}

vike_utility::refusal_enum! {
    /// A delete of stored history refused BEFORE anything is deleted (decision 0301: decided on the type, shown
    /// through the macro's `rendered()`), by [`RemovalPlan::refusal`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RemovalRefusal with ids RemovalRefusalId {
        /// `series` of the matched series do not carry the asserted commit-key prefix `produced_by`.
        ProvenanceRefused { series: usize, produced_by: String } => "data.delete.provenance_refused":
            "provenance REFUSED: {series} series do not carry produced_by {produced_by:?}; nothing was deleted",
    }
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
    pub series: Vec<RemovalPlanSeries>,
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
    /// removal (`HistoryStoreDataFusion::delete_series_checked`), because a live recorder can commit a key
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

    /// **The provenance verdict as ONE typed refusal**, the line every door prints before its confirm: `vike-cli data
    /// history rm --addr` refuses on it, and the MCP's `delete_series` issues no token on it and asks it again at the
    /// confirm (the store may have moved since the plan). `None` when [`Self::verdict`] passes; its lines name each
    /// series and stay the plan's.
    pub fn refusal(&self) -> Option<RemovalRefusal> {
        let series = self.verdict().err()?.len();
        let produced_by = self.produced_by.clone().unwrap_or_default();
        Some(RemovalRefusal::ProvenanceRefused { series, produced_by })
    }

    /// The plan as an operator reads it: one block per series, then the totals and the verdict.
    ///
    /// ⚠ It does NOT carry the resolved store root — that line is the CALLER's, because only the
    /// process that resolved the root knows which rung answered, and "which store" is the question
    /// a destructive verb must answer before "which series". See `vike-datahub store rm`.
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
                cov.first_timestamp,
                cov.last_timestamp
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
/// the provenance reads, so a narrow selector against a large store pays for what it selected. A
/// selector that names a lane enumerates `list_series()` instead, because `inventory` holds no lane,
/// and reads each match's coverage and keys with `series_facts()`.
///
/// `produced_by` is the RESOLVED prefix. Resolving a `--produced-by` SPELLING (a producer path or a
/// literal) is `crate::store::store_kind::resolve_produced_by`, and it happens before this call because it
/// is a fact about the argument, not about the store.
///
/// An ACCOUNT kind is refused here, before anything is enumerated ([`account_data_refusal`]).
pub fn plan_removal(
    store: &dyn crate::store::hist::HistoryStore,
    selector: &SeriesSelector,
    produced_by: Option<&str>,
) -> Result<RemovalPlan, crate::store::hist::HistoryStoreError> {
    selector
        .validate_shape()
        .map_err(|refusal| crate::store::hist::HistoryStoreError::Query(refusal.to_string()))?;
    selector.validate_against_kinds().map_err(crate::store::hist::HistoryStoreError::Query)?;
    if let Some(refusal) = account_data_refusal(selector.kind) {
        return Err(crate::store::hist::HistoryStoreError::Query(refusal.to_string()));
    }
    let mut series = Vec::new();
    if selector.source.is_some() {
        for id in store.list_series()?.into_iter().filter(|id| selector.matches(id)) {
            let (coverage, commits) = store.series_facts(&id)?;
            series.push(RemovalPlanSeries::new(id, coverage, commits, produced_by));
        }
    } else {
        for (id, coverage) in store.inventory()? {
            if !selector.matches(&id) {
                continue;
            }
            let commits = store.series_commits(&id)?;
            series.push(RemovalPlanSeries::new(id, coverage, commits, produced_by));
        }
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
/// (`HistoryStoreDataFusion::run_maintenance`'s rule), and the caller exits non-zero off
/// [`RemovalOutcome::is_clean`]. Each removal re-asserts the provenance under that series' own lock,
/// so a key committed between the plan and this call refuses that series rather than deleting it.
///
/// ⚠ **One `tracing::warn!` per series that went**, here because every route ends here (the wire's
/// `DeleteSeries` and `vike-datahub store rm`) and no route journals a delete: this line is the only
/// record the store's only copy lost a series. It writes no data.
pub fn execute_removal(
    store: &dyn crate::store::hist::HistoryStore,
    plan: &RemovalPlan,
) -> Result<RemovalOutcome, crate::store::hist::HistoryStoreError> {
    // [`plan_removal`] refuses account data already; re-checked here for this doc's reason.
    if let Some(refusal) = account_data_refusal(plan.selector.kind) {
        return Err(crate::store::hist::HistoryStoreError::Query(refusal.to_string()));
    }
    if let Err(refusals) = plan.verdict() {
        return Err(crate::store::hist::HistoryStoreError::Query(format!(
            "provenance REFUSED — nothing was deleted:\n{}",
            refusals.join("\n")
        )));
    }
    let mut outcome = RemovalOutcome::default();
    for s in &plan.series {
        match store.delete_series_checked(&s.id, plan.produced_by.as_deref()) {
            Ok(()) => {
                tracing::warn!(
                    series = %describe_id(&s.id),
                    prefix = %plan.produced_by.as_deref().unwrap_or("none"),
                    rows = s.coverage.rows,
                    "series deleted"
                );
                outcome.deleted.push(s.id.clone());
            }
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
    let lane = id.source.as_ref().map_or(String::new(), |s| format!("source={s}/"));
    match &id.interval {
        Some(iv) => format!("{}/{}/{lane}{scope}={}/{iv}", id.kind, id.venue, id.label()),
        None => format!("{}/{}/{lane}{scope}={}", id.kind, id.venue, id.label()),
    }
}

#[path = "removal_tests.rs"]
#[cfg(test)]
mod removal_tests;
