//! The lane-twin table: a venue whose ONE account row trades TWO exec lanes (binance: spot, and USDⓈ-M
//! perpetuals told apart by a `.P` suffix) runs a SECOND ENGINE for the lane its default engine does not
//! trade, derived from the SAME row and the SAME keys. No second `account` row, no second credential.
//!
//! # Why a second engine and not a second lane in one
//!
//! An engine has ONE ledger (`Account::apply_account_state` sets its single `balance` from whichever
//! lane spoke last), ONE user-data stream per lane and ONE reconcile leg, and Binance keeps spot and
//! futures in two wallets. Two lanes in one engine would overwrite one wallet's cash with the other's.
//! Two engines are two books exactly as the venue keeps them.
//!
//! # What "derived" means here
//!
//! [`Accounts::with_lane_twins`] adds, IN MEMORY and at read time, a row for the twin: the parent's
//! `id` (so its credential fields, key names and book resolve unchanged), the table's `label`, the
//! parent's tier, `active` and exposure ceiling, and the parent's `leverage` (a perpetual's setting).
//! The PARENT row loses its `leverage` in that view, because a stated leverage is a futures setting
//! and the spot engine must not be asked to apply it (it was refused there, at ERROR, every start).
//! Nothing is written to the store, and `vike-cli secrets accounts` (which reads the table itself)
//! still lists ONE account.
//!
//! ⚠ **The derived row is MARKED, not recognised by its label.** [`Accounts::derive_lane_twins`] returns
//! the `(venue, label)` pairs it added and `AccountDirectory::is_derived_lane_twin` answers from them; an
//! operator's own account that spelled the label is a real account (the twin then stands down). The label
//! is also RESERVED at the writer (`DbErrorKind::AccountLabelReserved`: `account add` / `rename` refuse
//! it), so a new real row cannot take the twin's route key. The twin is derived only while the venue's
//! default account resolves to ONE tier (no `ActiveTier::Conflict`): a parent that does not run has no
//! lane to run.
//!
//! # Opt-in, and why it is the leverage
//!
//! A twin exists only for a row that STATES a leverage (`account.leverage`) at a tier the table opens.
//! A spot-only account states none, so it derives nothing and sees no change at all. The leverage is
//! the one existing, ratified (decision 0181/0182) account attribute that is already about perpetuals;
//! a dedicated switch would be a new column. ⚠ The consequence the operator must be told: un-stating
//! the leverage makes the twin row VANISH from the view, which `vike-tradehub`'s `account_follow` reads
//! as a stop of the twin engine (a lane twin whose row is gone), not as "no answer".
//!
//! # Closed or open by DATA, like [`super::LEVERAGE_VENUES`]
//!
//! A tier a row does not list is closed: a twin starts a second signing engine on the same keys, and
//! on mainnet that is a decision with a risk budget, an exposure ceiling and a live lock per engine.
//! binance's `live` tier was opened by the owner (2026-10-19) by the one-line edit of [`LANE_TWINS`]
//! this paragraph used to promise; bybit, okx and every other venue have no row at all.
//!
//! ⚠ **A demo twin and a live twin share the route key `binance#PERP`, exactly as one account's demo
//! and live rows share theirs, and they are told apart the same way: by the TIER of the row that is
//! ACTIVE.** The store keeps one unlabelled row per `(venue, tier)`, two ACTIVE non-`paper` default rows
//! are an [`ActiveTier::Conflict`] that derives NO twin, and an inactive sibling's twin is inactive too,
//! so at most one twin of the label can ever be active. Every consumer that picks a row by `(venue,
//! label)` filters on `active` and on the tier it was asked for (`AccountDirectory::account_row`), so a
//! live twin signs with the LIVE parent's keys and never the demo sibling's.

use vike_model::AccountTier;

use super::model::fold_active_tier;
use super::{Account, Accounts, ActiveTier};

/// One row of [`LANE_TWINS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneTwin {
    /// A `vike_model::VENUES` id.
    pub venue: &'static str,
    /// The twin's account label: the route key of its engine is `venue#label`. A valid
    /// `vike_model::accounts::account_keys::AccountLabel`, and RESERVED on this venue for the twin: a real
    /// row carrying it makes the twin stand down ([`Accounts::with_lane_twins`]).
    pub label: &'static str,
    /// The symbol the twin's engine is mounted on, spelled as the venue's exec adapter reads it (the
    /// lane suffix decides the lane).
    pub symbol: &'static str,
    /// The tiers a twin is derived at. EMPTY = the code is there and the row is closed.
    pub tiers: &'static [AccountTier],
}

/// A stated leverage the view DROPPED because it sits on a LIVE row that is not the one a lane twin is derived
/// from ([`Accounts::derive_lane_twins_ignoring`]).
#[derive(Debug, Clone, PartialEq)]
pub struct IgnoredLiveLeverage {
    /// The row's id.
    pub id: i64,
    /// Its venue.
    pub venue: String,
    /// Its label (`None` = the default account).
    pub label: Option<String>,
    /// The figure it states, which nothing asks of the venue.
    pub figure: f64,
}

// `figure` is an `AccountLeverage` (finite, 1..=1000) by construction, never NaN, so equality is reflexive; the
// marker lets `AccountDirectory` keep its `Eq`.
impl Eq for IgnoredLiveLeverage {}

/// The venues whose default engine trades one lane and whose account row may also run the other.
///
/// binance is open at `demo` AND `live` (the owner opened `live` on 2026-10-19: "live has to do the
/// same as demo"). `live` is MAINNET, so a live twin is a second real-money engine on the same keys:
/// it exists only for a live default row that STATES a leverage (the owner states it, nothing here
/// ever does), and every ceiling of its parent applies to it unchanged.
pub const LANE_TWINS: &[LaneTwin] = &[LaneTwin {
    venue: "binance",
    label: "PERP",
    symbol: "BTCUSDT.P",
    tiers: &[AccountTier::Demo, AccountTier::Live],
}];

/// The row for `venue`, whatever tiers it is open at.
#[must_use]
pub fn lane_twin(venue: &str) -> Option<&'static LaneTwin> {
    LANE_TWINS.iter().find(|row| row.venue == venue)
}

/// The symbol the twin of `venue` is mounted on when `label` (the account label's TEXT) is the twin's
/// label, else `None`. The one answer the engine assembly reads to mount a twin on its own lane.
#[must_use]
pub fn lane_twin_symbol(venue: &str, label: &str) -> Option<&'static str> {
    lane_twin(venue).filter(|row| row.label == label).map(|row| row.symbol)
}

/// Whether `label` is the label of `venue`'s lane twin, by the table alone (no row is consulted).
#[must_use]
pub fn is_lane_twin_label(venue: &str, label: &str) -> bool {
    lane_twin_symbol(venue, label).is_some()
}

impl Accounts {
    /// **These rows plus the lane twins they imply** (module doc): [`Self::derive_lane_twins`] without the
    /// list of what it derived.
    #[must_use]
    pub fn with_lane_twins(self) -> Accounts {
        self.derive_lane_twins().0
    }

    /// **These rows plus the lane twins they imply, and WHICH `(venue, label)` pairs were derived**
    /// (module doc). [`Accounts::Unanswerable`] is returned as it came, with no twin. Deterministic and
    /// pure: the same rows always derive the same twins, so the tradehub and the gatewayhub, which each
    /// read the table, agree on them.
    ///
    /// The list is the MARKER of a derived row. A row with the twin's label is a real account or a derived
    /// twin, and the label cannot tell them apart; whoever must know (`AccountDirectory::is_derived_lane_twin`)
    /// asks this list, never the label.
    ///
    /// A twin stands down (derives nothing, and the parent keeps its leverage, exactly as before) when
    /// ANY row of the venue, of any tier and either activity, already carries the twin's label: a real
    /// account that spelled it wins, and the engine under that key stays the operator's.
    ///
    /// It also derives nothing while the venue's DEFAULT account does not resolve to one tier (two active
    /// non-`paper` tiers: `ActiveTier::Conflict`, decision 0156). The parent is then NOT RUNNING, and a
    /// twin carrying its keys on the one tier the table opens would trade an account that does not run.
    #[must_use]
    pub fn derive_lane_twins(self) -> (Accounts, Vec<(String, String)>) {
        let (rows, derived, _) = self.derive_lane_twins_ignoring();
        (rows, derived)
    }

    /// [`Self::derive_lane_twins`] and the LIVE figures it ignored (review r2 re-review).
    ///
    /// ⚠ **At `live` the owner's ruling (2026-10-19) opened the TWIN only.** On a venue whose lane twin is open
    /// at `live`, a stated leverage on any LIVE row that is not the one the twin is derived from (a labelled
    /// account, or the default account when the twin stood down) is dropped from this view: it reads as
    /// nothing stated for the tradehub and the gatewayhub alike, so no figure is asked of or written to the
    /// real account for it and it starts exactly as it did before the table opened. Each dropped figure is
    /// returned so the node can say so (`AccountDirectory::ignored_live_leverages`). The demo tier is not
    /// touched.
    #[must_use]
    pub fn derive_lane_twins_ignoring(
        self,
    ) -> (Accounts, Vec<(String, String)>, Vec<IgnoredLiveLeverage>) {
        let Accounts::Known(mut rows) = self else { return (self, Vec::new(), Vec::new()) };
        let mut derived: Vec<(String, String)> = Vec::new();
        let mut ignored: Vec<IgnoredLiveLeverage> = Vec::new();
        for spec in LANE_TWINS {
            let stands_down = rows
                .iter()
                .any(|r| r.venue == spec.venue && r.label.as_deref() == Some(spec.label))
                || fold_active_tier(
                    rows.iter().filter(|r| r.venue == spec.venue && r.label.is_none()),
                ) == ActiveTier::Conflict;
            let mut twins: Vec<Account> = Vec::new();
            if !stands_down {
                for parent in rows.iter_mut() {
                    if parent.venue != spec.venue
                        || parent.label.is_some()
                        || !spec.tiers.contains(&parent.tier)
                    {
                        continue;
                    }
                    let Some(leverage) = parent.leverage.take() else { continue };
                    twins.push(Account {
                        label: Some(spec.label.to_string()),
                        // The twin is no book of its own and nothing handshook as it.
                        venue_account_id: None,
                        last_verified_at: None,
                        leverage: Some(leverage),
                        ..parent.clone()
                    });
                }
            }
            if spec.tiers.contains(&AccountTier::Live) {
                for row in rows.iter_mut() {
                    if row.venue == spec.venue
                        && row.tier == AccountTier::Live
                        && let Some(figure) = row.leverage.take()
                    {
                        ignored.push(IgnoredLiveLeverage {
                            id: row.id,
                            venue: row.venue.clone(),
                            label: row.label.clone(),
                            figure: figure.get(),
                        });
                    }
                }
            }
            if !twins.is_empty() {
                derived.push((spec.venue.to_string(), spec.label.to_string()));
            }
            rows.extend(twins);
        }
        // `Accounts::Known` is id-ordered; a stable sort keeps a parent ahead of its twin (same id).
        rows.sort_by_key(|r| r.id);
        (Accounts::Known(rows), derived, ignored)
    }
}

#[path = "lane_twins_tests.rs"]
#[cfg(test)]
mod lane_twins_tests;
