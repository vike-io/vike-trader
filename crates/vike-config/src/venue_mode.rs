//! [`VenueMode`] and [`VenuePolicy`] — **per-venue arming, as a CEILING that only ever refuses.**
//!
//! # The defect this exists for
//!
//! Today a venue goes live because its CREDENTIALS are present, and nothing else. MEASURED on the
//! the CI box daemon: its run profile mounts ONE venue (bybit) while the daemon's own log prints
//! `live_venues={"hyperliquid","deribit","okx","bybit","alpaca","aster","binance","ig","oanda"}` —
//! nine authenticated live exec sessions, none of them asked for, all of them a consequence of a
//! credential pair sitting in one file. `vike_run::build_node` issues a `make_engine` call for
//! every venue in its `WIRED_MARKETS` table and takes each one live iff its credentials resolve, so
//! "which venues does this box trade on" has no answer an operator can write down anywhere.
//!
//! This module is the TYPE for that answer: **stage 2 of a three-stage program**. Stage 1 made the
//! startup banner stop lying about the mount. **Stage 3 has landed** — the ceiling is folded in at
//! `crates/vike-mount/src/lib.rs`'s `make_engine_with_legs`, above the credential read, and
//! `crates/vike-config/tests/policy_is_consumed.rs`'s `venues` row names that fold and opens the
//! file to check it is still there. Until that row was promoted this module was a setting that
//! parsed, validated, defaulted, disclosed and was READ BY NOTHING; the row is what made the
//! promotion unmissable rather than something to remember.
//!
//! # ⚠ A CEILING, not a selector — and the ordering is the whole of it
//!
//! `paper < demo < live`, and the effective tier stage 3 computes is
//! [`min`](VenueMode::cap)`(mode, whatever the existing mechanisms decide)` — **never `max`**. That
//! single choice is what makes this switch safe to ship into a live deployment: it can REFUSE an
//! arming the credentials would have produced, and it can never ARM something that is not armed
//! today. A `venues.bybit = "live"` line does not put bybit live; it declines to stop bybit going
//! live if its credentials, its `{VENUE}_MAINNET` flag and its mount arm all already say so.
//!
//! The ordering is therefore encoded in the TYPE rather than in a comment somebody has to obey:
//! [`VenueMode`] derives `PartialOrd`/`Ord` over variants declared in ascending risk order, and
//! [`VenueMode::cap`] is the one spelling of the fold. A stage-3 author reaching for `max` has to
//! delete a method and write their own, which is a diff a reviewer can see.
//!
//! # ⚠ The default is a FILLED map, one `paper` per roster venue — never an empty one
//!
//! `crates/vike-config/tests/provenance.rs`'s `settings_leaves` walks
//! `serde_json::to_value(Settings::default())` and only records a leaf at a NON-object node. An
//! empty map has no leaves, so `every_settings_field_has_a_provenance_row` would pass VACUOUSLY and
//! `vike-cli config show` would say nothing about venues at all — a setting that exists, validates,
//! and is invisible to the one command whose job is disclosing settings. A filled default yields
//! one provenance row per venue, exactly as `crates/vike-config/src/provenance.rs`'s `setting_keys`
//! derives one row per [`crate::FLAG_REGISTRY`] entry.
//!
//! It is also the honest reading of a ceiling: **absence of a `policy.toml` must be the SAFEST
//! answer**, and the safest per-venue answer is `paper`. (That it is safe does not make it
//! BINDING — nothing consumes it yet, which is the paragraph above.)
//!
//! # ⚠ Every ROSTER venue gets a row, including ones this build cannot mount
//!
//! `ibkr` is behind `vike-app`'s `ibkr` feature, `polymarket` and `fxcm` behind `vike-mount`'s.
//! Their rows exist here anyway, and the reasons are three:
//!
//! 1. **This crate cannot see those features.** They are declared in binaries and in `vike-mount`,
//!    far above `vike-config`, so a feature-conditional roster here would be a guess — wrong in
//!    whichever direction it guessed, and silently so.
//! 2. **A ceiling for an unmountable venue costs nothing.** `min(mode, what the mount decided)`
//!    where the mount decided nothing is still paper; the row can only ever refuse.
//! 3. **A `policy.toml` must be PORTABLE.** Refusing `ibkr = "demo"` in a build without the feature
//!    would mean the same file loads on the GUI box and is refused at startup by the daemon — an
//!    operator moving a reviewed file between two boxes of the same deployment would get a hard
//!    failure for a line that is correct. Which venues a BUILD can mount is a property of the
//!    binary; which venues an OPERATOR permits is a property of the file, and they are allowed to
//!    disagree.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use vike_model::VENUES;
use vike_model::account_keys::AccountLabel;

/// How far this deployment permits ONE venue to arm. **A ceiling, not a selector** — see the
/// module doc.
///
/// ⚠ The variants are declared in ASCENDING risk order and the `Ord` derive follows declaration
/// order, so `Paper < Demo < Live` is a property of this list rather than of a comment. Reordering
/// them silently inverts every comparison in stage 3; there is deliberately no explicit
/// discriminant to make the ordering look like an arbitrary numbering that could be edited.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum VenueMode {
    /// Simulated fills against the local paper exchange. No credential is used, no order leaves
    /// the process. **The default for every venue**, because the safest answer is what a machine
    /// with no `policy.toml` must get.
    #[default]
    Paper,
    /// The venue's own demo/testnet/sandbox account — real wire, real rejections, no real funds.
    Demo,
    /// The venue's production account. Real money.
    Live,
}

impl VenueMode {
    /// Every mode, in ascending risk order — the order [`Ord`] agrees with, so a caller rendering a
    /// menu and a caller comparing two modes cannot disagree.
    pub const ALL: [VenueMode; 3] = [VenueMode::Paper, VenueMode::Demo, VenueMode::Live];

    /// The wire/file spelling, which is also what serde reads and writes (`rename_all` above).
    ///
    /// Every message and every rendering goes through this rather than through a literal, so the
    /// legal set an error prints cannot drift from the set the parser accepts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VenueMode::Paper => "paper",
            VenueMode::Demo => "demo",
            VenueMode::Live => "live",
        }
    }

    /// **THE fold, and the only one.** The effective tier is the LOWER of the operator's ceiling
    /// and whatever the existing mechanisms decided — `min`, never `max`.
    ///
    /// Stage 3 calls this at the venue-mount seam. It exists as a named method rather than as a
    /// `.min()` at each call site for one reason: `a.max(b)` is one character from `a.min(b)` and
    /// reads just as plausible, while the two differ by "the file can only refuse" versus "the file
    /// can arm real money". Naming the operation puts that difference in a diff.
    ///
    /// ```
    /// use vike_config::VenueMode;
    /// // A `live` ceiling over a demo-credentialled venue stays DEMO — the ceiling never promotes.
    /// assert_eq!(VenueMode::Live.cap(VenueMode::Demo), VenueMode::Demo);
    /// // A `paper` ceiling over a live-credentialled venue REFUSES it down to paper.
    /// assert_eq!(VenueMode::Paper.cap(VenueMode::Live), VenueMode::Paper);
    /// ```
    #[must_use]
    pub fn cap(self, decided: VenueMode) -> VenueMode {
        self.min(decided)
    }
}

impl fmt::Display for VenueMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The legal modes, rendered for a refusal message — `paper / demo / live`.
///
/// Derived from [`VenueMode::ALL`] so the message and the parser cannot disagree about what is
/// legal; the same argument as `crates/vike-config/src/write.rs`'s `unknown_file_message`.
#[must_use]
pub fn legal_modes() -> String {
    VenueMode::ALL.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(" / ")
}

/// The canonical roster id equal to `name`, or `None` for a string
/// [`vike_model::VENUES`](VENUES) does not carry.
///
/// Returning the `&'static str` from the roster rather than a bool is what lets [`VenuePolicy`] key
/// on `&'static str` and be, by construction, incapable of holding a venue that does not exist.
#[must_use]
pub fn roster_id(name: &str) -> Option<&'static str> {
    VENUES.iter().copied().find(|v| *v == name)
}

/// The roster id a misspelling most plausibly MEANT — used ONLY to say "did you mean `bybit`?" in a
/// refusal, never to accept the misspelling.
///
/// ⚠ **Deliberately not a fallback.** `Bybit = "live"` is REFUSED, not silently read as `bybit`: a
/// spelling the loader repairs on the operator's behalf is a spelling nobody learns, and the next
/// reader of that file greps the tree for a key that matches nothing in it.
///
/// Two rules, both cheap and both total (no edit-distance library, no scoring): a case-insensitive
/// match, then a PREFIX relation in either direction with at least three characters in common —
/// which covers the two typos this shape actually produces, a shouted id (`BYBIT`) and a slipped
/// key (`bybitt`, `binanc`). Anything further from the roster gets no suggestion and the full list
/// instead, which is the honest answer: a guess at `kraken` would name a venue vike cannot trade.
#[must_use]
pub(crate) fn did_you_mean(name: &str) -> Option<&'static str> {
    if let Some(exact) = VENUES.iter().copied().find(|v| v.eq_ignore_ascii_case(name)) {
        return Some(exact);
    }
    let lower = name.to_ascii_lowercase();
    if lower.len() < 3 {
        return None;
    }
    VENUES
        .iter()
        .copied()
        .find(|v| v.len() >= 3 && (v.starts_with(&lower) || lower.starts_with(*v)))
}

/// The per-venue ceilings, one entry per [`vike_model::VENUES`](VENUES) id — **always all of them**
/// (see the module doc for why an empty map is the wrong default and why a feature-absent venue
/// still gets a row).
///
/// Keys are the roster's own `&'static str`s, so a value that is in this map is a venue that exists.
/// Construction goes through [`VenuePolicy::set`], which only accepts what [`roster_id`] returned.
///
/// # ⚠ Why it carries a SECOND fact, and why nothing else could
///
/// [`VenuePolicy::is_declared`] answers *"did a policy FILE ever name a venue in `[venues]`?"*, and
/// the map alone structurally cannot: the default is one `paper` per roster venue, so a deployment
/// that wrote `[venues]` with everything at `paper` produces a map BYTE-IDENTICAL to one that never
/// wrote the table at all. Those two are the same ceiling and OPPOSITE states of knowledge — the
/// first operator chose paper, the second has not heard of the key — and the stage-3 migration
/// warning (`vike_mount::venue_arming_migration`) has to fire for exactly one of them. Deriving it
/// instead from `crates/vike-config/src/provenance.rs`'s `describe` (which CAN tell them apart, via
/// each `policy.venues.<venue>` row's [`crate::Origin`]) would mean a second read of the settings
/// files at every mount; recording it where [`Self::set`] already runs costs nothing and cannot
/// disagree with the load that produced it.
///
/// It is `#[serde(skip)]`, so it is invisible on the wire, invisible to `vike-cli config show`'s
/// leaf walk and invisible to the provenance completeness gate — the map still serializes
/// transparently as a flat `{venue: mode}` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct VenuePolicy {
    ceilings: BTreeMap<&'static str, VenueMode>,
    /// See [`VenuePolicy::is_declared`]. `false` for the compiled-in default AND for an empty
    /// `[venues]` table, which names no venue and therefore states nothing.
    #[serde(skip)]
    declared: bool,
    /// **Per-ACCOUNT ceilings**, keyed `(roster venue, label text)` — the `[accounts]` table. Empty
    /// on every box that has one credential set per venue, which is every box today.
    ///
    /// ⚠ `#[serde(skip)]` **here**, and disclosed one level UP: this struct is
    /// `#[serde(transparent)]` over [`Self::ceilings`], so it can carry no second serialized field
    /// at all. [`crate::Policy`]'s hand-written `Serialize` emits [`Self::accounts_by_venue`] as its
    /// `accounts` field instead, which is the leaf `crates/vike-config/src/provenance.rs`'s row and
    /// `settings/policy.example.toml`'s line hang off. The map is still owned HERE and copied
    /// nowhere.
    #[serde(skip)]
    accounts: BTreeMap<(&'static str, String), VenueMode>,
}

impl Default for VenuePolicy {
    /// Every roster venue at [`VenueMode::Paper`], DECLARED BY NOBODY.
    fn default() -> Self {
        VenuePolicy {
            ceilings: VENUES.iter().map(|v| (*v, VenueMode::Paper)).collect(),
            declared: false,
            accounts: BTreeMap::new(),
        }
    }
}

impl VenuePolicy {
    /// This venue's ceiling. An id the roster does not carry answers [`VenueMode::Paper`] — the
    /// safe answer, and the one a caller asking about an unknown string must get rather than a
    /// panic or an `Option` it will `unwrap_or(Live)`.
    #[must_use]
    pub fn get(&self, venue: &str) -> VenueMode {
        self.ceilings.get(venue).copied().unwrap_or(VenueMode::Paper)
    }

    /// Every `(venue, ceiling)` pair, in venue-id order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, VenueMode)> + '_ {
        self.ceilings.iter().map(|(v, m)| (*v, *m))
    }

    /// How many venues this map covers — the roster's length, by construction.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ceilings.len()
    }

    /// Never true for a [`VenuePolicy`] built here; present because clippy asks for it beside
    /// [`Self::len`], and because a future empty one would be a bug worth being able to ASK about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ceilings.is_empty()
    }

    /// **Did a policy FILE ever name a venue under `[venues]`?** — see the type's own doc for why
    /// the ceiling map cannot answer this and why the answer lives here rather than being re-read
    /// from disk.
    ///
    /// `true` the moment [`Self::set`] runs, which [`crate::Policy::apply`] calls once per venue the
    /// file names. `false` for the compiled-in default, for a policy file with no `[venues]` table,
    /// and for an EMPTY `[venues]` table — the last of those deliberately: a table naming nothing
    /// sets nothing, and treating "the operator typed a section header" as an arming decision would
    /// silence the migration warning for a deployment that has still not stated one.
    #[must_use]
    pub fn is_declared(&self) -> bool {
        self.declared
    }

    /// **State one venue's ceiling** — the builder form of [`Self::set`], and the only way to
    /// construct a non-default [`VenuePolicy`] from outside this crate.
    ///
    /// It exists because [`Self::set`] is `pub(crate)` (its `&'static str` parameter IS the
    /// roster check, and only [`crate::Policy::apply`] holds a `roster_id` result) while the
    /// consumers of the ceiling live several crates up: `vike_mount::MountPolicy`'s field is `pub`,
    /// so a caller can already replace the whole map — it just had no way to build one that says
    /// anything. This adds no authority the type did not have: a ceiling can only ever REFUSE.
    ///
    /// An id [`roster_id`] does not recognise is IGNORED rather than refused — the resulting map
    /// answers [`VenueMode::Paper`] for it either way ([`Self::get`]), so refusing would be a panic
    /// or a `Result` for a call that cannot change an outcome. The FILE path is where a misspelling
    /// is refused by name, because there an operator believes they capped something.
    ///
    /// ⚠ It marks the map [`declared`](Self::is_declared), like a file line does — a caller that
    /// states a ceiling in code has stated one.
    #[must_use]
    pub fn declare(mut self, venue: &str, mode: VenueMode) -> Self {
        if let Some(id) = roster_id(venue) {
            self.set(id, mode);
        }
        self
    }

    /// Set one venue's ceiling. `venue` must be a roster id — the `&'static str` [`roster_id`]
    /// returned — which is why this takes `&'static str` rather than `&str`: the type is the check.
    ///
    /// ⚠ This is also the ONE site that records [`Self::is_declared`], and that is not incidental:
    /// [`crate::Policy::apply`] reaches it exactly once per venue a FILE names, so "a file named a
    /// venue" and "this function ran" are the same event and cannot drift apart.
    pub(crate) fn set(&mut self, venue: &'static str, mode: VenueMode) {
        self.ceilings.insert(venue, mode);
        self.declared = true;
    }

    // -- per-ACCOUNT ceilings -----------------------------------------------------------------
    //
    // ⚠ THESE ARE FOLDED. `crates/vike-mount/src/arming.rs`'s `account_ceiling` consults
    // [`Self::account`] above the credential read, and `make_engine_accounts` mounts one engine per
    // ACTIVE account — so an `[accounts]` line changes what this box trades, and the load-time
    // "binds nothing yet" warning that stood in for enforcement is gone with it.
    //
    // What did NOT change is the box that has no `[accounts]` table:
    // `crates/vike-config/tests/venue_accounts_table.rs`'s
    // `an_accounts_table_changes_no_venue_level_answer` still holds, because the DEFAULT account
    // resolves to its venue's own line exactly and a labelled account with no line of its own
    // resolves to `paper`.

    /// **This ACCOUNT's ceiling** — `min(the venue's ceiling, this account's own line)`.
    ///
    /// Two defaults, and the asymmetry is the whole safety argument:
    ///
    /// * the **DEFAULT** account ([`AccountLabel::Default`]) with no line of its own resolves to
    ///   the VENUE ceiling exactly. That is what makes today's files resolve unchanged: a box with
    ///   `bybit = "live"` and no `[accounts]` table asks about the default account and gets `live`,
    ///   the same answer [`Self::get`] gives.
    /// * a **LABELLED** account with no line of its own resolves to [`VenueMode::Paper`]. The
    ///   `bybit = "live"` line was written when bybit had one account; reading it as consent for an
    ///   account that did not exist when it was written is precisely the silent escalation the
    ///   ceiling exists to prevent. A second account must be named to be armed.
    ///
    /// The venue ceiling is applied with [`VenueMode::cap`] in BOTH cases, so `bybit = "paper"`
    /// disarms every bybit account whatever their own lines say — the ceiling still only ever
    /// refuses, one level down.
    #[must_use]
    pub fn account(&self, venue: &str, label: &AccountLabel) -> VenueMode {
        let venue_ceiling = self.get(venue);
        let stated = label.text().and_then(|l| lookup_owned(&self.accounts, venue, l)).copied();
        match (stated, label.is_default()) {
            (Some(mode), _) => venue_ceiling.cap(mode),
            // No line of its own: the default account inherits, a labelled one does not.
            (None, true) => venue_ceiling,
            (None, false) => VenueMode::Paper,
        }
    }

    /// Every `(venue, label, stated ceiling)` the FILE named, in venue-then-label order. The
    /// stated line, NOT the resolved answer — [`Self::account`] is what resolves.
    pub fn accounts(&self) -> impl Iterator<Item = (&'static str, &str, VenueMode)> + '_ {
        self.accounts.iter().map(|((v, l), m)| (*v, l.as_str(), *m))
    }

    /// **Did a policy FILE name any account under `[accounts]`?** — the per-account twin of
    /// [`Self::is_declared`].
    ///
    /// Read by `vike_mount::venue_arming_migration_message`, which uses it to decide whether the
    /// upgrade warning may name accounts at all: a deployment that has never written the table has
    /// nothing to be told about a second account, and a paste-ready block listing accounts nobody
    /// declared would teach the wrong shape.
    #[must_use]
    pub fn accounts_declared(&self) -> bool {
        !self.accounts.is_empty()
    }

    /// The per-account ceilings in the FILE's own nested shape — `{venue: {LABEL: mode}}`.
    ///
    /// **The disclosure projection**, and the only reason it exists: [`crate::Policy`]'s hand-written
    /// `Serialize` emits this as its `accounts` field, which is what gives the table a
    /// `crates/vike-config/src/provenance.rs` row, a `vike-cli config show` cell and a
    /// `settings/policy.example.toml` line. This type is `#[serde(transparent)]` over its flat
    /// ceilings map and can carry no second serialized field of its own, so the projection is
    /// assembled here and OWNED here — `Policy` holds no copy.
    ///
    /// The nesting matches the file's, deliberately: `provenance::describe` decides whether a row was
    /// `adjusted` by comparing the resolved value against the raw file value, and a differently
    /// shaped rendering would report every account table as adjusted.
    #[must_use]
    pub fn accounts_by_venue(&self) -> BTreeMap<&'static str, BTreeMap<String, VenueMode>> {
        let mut out: BTreeMap<&'static str, BTreeMap<String, VenueMode>> = BTreeMap::new();
        for ((venue, label), mode) in &self.accounts {
            out.entry(venue).or_default().insert(label.clone(), *mode);
        }
        out
    }

    /// State one ACCOUNT's ceiling — the out-of-crate builder, the twin of [`Self::declare`].
    ///
    /// An id [`roster_id`] does not recognise is IGNORED, and so is [`AccountLabel::Default`]: the
    /// default account's ceiling IS the venue's line, so accepting one here would create a second
    /// place to state one fact. The FILE path refuses both by name, where an operator believes
    /// they capped something.
    #[must_use]
    pub fn declare_account(mut self, venue: &str, label: &AccountLabel, mode: VenueMode) -> Self {
        if let (Some(id), Some(text)) = (roster_id(venue), label.text()) {
            self.set_account(id, text, mode);
        }
        self
    }

    /// Set one account's ceiling. `venue` must be a roster id and `label` a validated
    /// [`AccountLabel`] text — the same "the type is the check" construction [`Self::set`] uses.
    ///
    /// ⚠ It deliberately does NOT touch [`Self::is_declared`]. That flag answers "did a file state
    /// a VENUE ceiling", which is what `vike_mount::venue_arming_migration` self-silences on; an
    /// `[accounts]` table states no venue ceiling, so letting it set the flag would silence the
    /// migration warning for a deployment that has still not stated one.
    pub(crate) fn set_account(&mut self, venue: &'static str, label: &str, mode: VenueMode) {
        self.accounts.insert((venue, label.to_string()), mode);
    }
}

/// A tuple-keyed `BTreeMap` cannot be probed by a borrowed pair (`(&'static str, String)` borrows
/// as no `(&str, &str)`), and the caller that matters holds a NON-`'static` venue string — the
/// venue label an engine carries. So the lookup walks instead of allocating a key.
///
/// Linear in the operator's own declared-account count, on a path that runs at mount time and on a
/// settings screen, never on the fold. The allocating alternative would still not accept a
/// non-`'static` venue, which is the reason this is a function rather than a `get`.
fn lookup_owned<'a>(
    accounts: &'a BTreeMap<(&'static str, String), VenueMode>,
    venue: &str,
    label: &str,
) -> Option<&'a VenueMode> {
    accounts.iter().find(|((v, l), _)| *v == venue && l == label).map(|(_, m)| m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The ordering that makes this a ceiling.** Pinned as a full comparison chain rather than as
    /// two `assert!(a < b)` lines, because the property stage 3 depends on is the TOTAL order.
    #[test]
    fn paper_is_below_demo_is_below_live() {
        assert!(VenueMode::Paper < VenueMode::Demo);
        assert!(VenueMode::Demo < VenueMode::Live);
        assert!(VenueMode::Paper < VenueMode::Live);
        // …and `ALL` is that order, so a menu and a comparison cannot disagree.
        let mut sorted = VenueMode::ALL;
        sorted.sort_unstable();
        assert_eq!(sorted, VenueMode::ALL);
        assert_eq!(VenueMode::default(), VenueMode::Paper, "the safe end is the default");
    }

    /// **THE property: the ceiling can only ever REFUSE.** Over every ordered pair of modes, the
    /// capped result is never above EITHER input — so no `policy.toml` line can arm something the
    /// existing mechanisms did not already arm.
    ///
    /// Exhaustive over the 9 pairs rather than sampled: the whole set is three by three, and a
    /// sampled version of a total-order property is a test that can be right by luck.
    #[test]
    fn a_ceiling_never_promotes_whatever_the_other_side_decided() {
        for ceiling in VenueMode::ALL {
            for decided in VenueMode::ALL {
                let effective = ceiling.cap(decided);
                assert!(effective <= ceiling, "{ceiling} capped UP to {effective}");
                assert!(effective <= decided, "{decided} was PROMOTED to {effective}");
                assert_eq!(effective, ceiling.min(decided), "cap must be min, never max");
            }
        }
        // The two cases the module doc names, spelled out so a reader sees the asymmetry.
        assert_eq!(VenueMode::Live.cap(VenueMode::Demo), VenueMode::Demo);
        assert_eq!(VenueMode::Paper.cap(VenueMode::Live), VenueMode::Paper);
        // …and `cap` is symmetric, which is what "the lower of the two" means.
        for a in VenueMode::ALL {
            for b in VenueMode::ALL {
                assert_eq!(a.cap(b), b.cap(a));
            }
        }
    }

    /// The file spelling round-trips through serde, and `as_str` is the same set the parser takes.
    #[test]
    fn the_three_spellings_round_trip() {
        for mode in VenueMode::ALL {
            let text = format!("m = {:?}\n", mode.as_str());
            #[derive(Deserialize)]
            struct One {
                m: VenueMode,
            }
            assert_eq!(toml::from_str::<One>(&text).expect("parses").m, mode);
            assert!(legal_modes().contains(mode.as_str()), "{mode} missing from {}", legal_modes());
        }
    }

    /// The default covers the WHOLE roster and nothing else — the property `provenance` derives its
    /// rows from, and the one an empty map would silently break.
    #[test]
    fn the_default_is_one_paper_entry_per_roster_venue() {
        let p = VenuePolicy::default();
        assert_eq!(p.len(), VENUES.len());
        assert!(!p.is_empty());
        for venue in VENUES {
            assert_eq!(p.get(venue), VenueMode::Paper, "{venue}");
        }
        let held: Vec<&str> = p.iter().map(|(v, _)| v).collect();
        let mut roster = VENUES.to_vec();
        roster.sort_unstable();
        assert_eq!(held, roster, "exactly the roster, in id order");
    }

    /// An unknown venue string answers PAPER rather than panicking or being absent — the safe
    /// answer for a caller that has a venue label from somewhere else.
    #[test]
    fn an_unknown_venue_reads_as_paper() {
        let p = VenuePolicy::default();
        assert_eq!(p.get("not-a-venue"), VenueMode::Paper);
        assert_eq!(p.get(""), VenueMode::Paper);
        assert_eq!(p.get("BYBIT"), VenueMode::Paper, "ids are lowercase; a shout is not one");
    }

    /// [`roster_id`] is exact and case-SENSITIVE; [`did_you_mean`] is a SEPARATE function that only
    /// ever feeds a message. Keeping them apart is the property: a lookup that quietly accepted
    /// either of the near misses below would be repairing the operator's file for them.
    #[test]
    fn the_roster_lookup_is_exact_and_the_hint_is_separate() {
        assert_eq!(roster_id("bybit"), Some("bybit"));
        assert_eq!(roster_id("Bybit"), None, "ids are lowercase and the lookup does not repair");
        assert_eq!(roster_id("bybitt"), None);

        assert_eq!(did_you_mean("BYBIT"), Some("bybit"), "a shouted id");
        assert_eq!(did_you_mean("bybitt"), Some("bybit"), "a slipped key");
        assert_eq!(did_you_mean("binanc"), Some("binance"), "a truncated one");
        // …and a name that is simply a different venue gets NO guess: naming a roster venue for
        // `kraken` would suggest trading somewhere vike has no bridge for.
        assert_eq!(did_you_mean("kraken"), None);
        assert_eq!(did_you_mean(""), None);
        assert_eq!(did_you_mean("ib"), None, "too short to be evidence of anything");
    }

    /// The map serializes as a flat `{venue: mode}` object — `#[serde(transparent)]`, so the
    /// newtype is invisible on the wire and `policy.venues.<venue>` is a real leaf path.
    ///
    /// ⚠ …and the DECLARED flag is invisible with it. It is `#[serde(skip)]`, so a set flag adds no
    /// key: were it serialized it would appear as a fifteenth leaf beside the fourteen venues, and
    /// `crates/vike-config/tests/provenance.rs`'s completeness gate would demand a
    /// `policy.venues.declared` row for a fact no operator can set.
    #[test]
    fn the_map_serializes_transparently_as_venue_to_mode() {
        let mut p = VenuePolicy::default();
        p.set("bybit", VenueMode::Live);
        let table = toml::Table::try_from(&p).expect("serializes as a table");
        assert_eq!(table["bybit"].as_str(), Some("live"));
        assert_eq!(table["binance"].as_str(), Some("paper"));
        assert_eq!(table.len(), VENUES.len(), "the roster, and not one key more");
        assert!(p.is_declared(), "…while the flag itself is set and simply does not serialize");
    }

    /// **THE fact the ceiling map structurally cannot carry**: "everything at paper" and "nobody
    /// ever wrote the table" produce the SAME map, and the stage-3 migration warning has to fire
    /// for exactly one of them.
    ///
    /// Asserted as a three-way comparison rather than as two `assert!`s, because the property is
    /// that the two states are indistinguishable BY CEILING and distinguishable BY DECLARATION — a
    /// test that only checked the flag would still pass if the maps had silently diverged, and the
    /// whole point is that they do not.
    #[test]
    fn an_all_paper_table_is_the_default_map_and_is_still_declared() {
        let never_written = VenuePolicy::default();
        let mut written_all_paper = VenuePolicy::default();
        for venue in VENUES {
            written_all_paper.set(venue, VenueMode::Paper);
        }
        assert!(
            written_all_paper.iter().eq(never_written.iter()),
            "an all-paper table must be the SAME ceiling as no table — otherwise this fact is not \
             the one being distinguished"
        );
        assert!(!never_written.is_declared(), "the compiled-in default is nobody's decision");
        assert!(written_all_paper.is_declared(), "…and an all-paper table IS one");
    }

    /// [`VenuePolicy::declare`] is the out-of-crate builder: it states what it names, leaves every
    /// other venue alone, marks the map declared, and IGNORES a non-roster id rather than panicking
    /// (that map answers `paper` for the id either way — the refusal belongs on the FILE path,
    /// where an operator believes they capped something).
    #[test]
    fn declare_states_one_venue_and_ignores_a_non_venue() {
        let p = VenuePolicy::default().declare("bybit", VenueMode::Live);
        assert_eq!(p.get("bybit"), VenueMode::Live);
        assert_eq!(p.get("binance"), VenueMode::Paper, "an unnamed venue keeps the default");
        assert_eq!(p.len(), VENUES.len(), "…and the map is still exactly the roster");
        assert!(p.is_declared());

        let ignored = VenuePolicy::default().declare("Bybit", VenueMode::Live);
        assert_eq!(ignored.get("bybit"), VenueMode::Paper, "a shouted id states nothing");
        assert_eq!(ignored.get("Bybit"), VenueMode::Paper, "…and is not stored under its own key");
        assert_eq!(ignored.len(), VENUES.len(), "a non-venue never joins the map");
    }
}
