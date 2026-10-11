//! `AccountGrids`: every account the Connections grid can show, each with its own tier grid.

use std::collections::HashMap;

use vike_model::accounts::account_keys::{AccountLabel, split_account_key};

use super::{VenueCredStatus, credential_status, credential_status_for_account};

/// **Every account the Connections grid can show, each with its own tier grid** — the input a
/// screen that renders ONE account at a time takes (a grid for the default account plus one per
/// labelled account beside it).
///
/// ⚠ **`labelled` is EMPTY on a single-account box**, and that is what makes "unchanged" a
/// structural property rather than a careful one: with no labelled account the view has exactly
/// one account to offer, reads [`Self::default_grid`] — the very `Vec` [`credential_status`]
/// always produced — and composes every key name through [`AccountLabel::Default`], which
/// `vike_model::accounts::account_keys::account_key` returns unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGrids {
    /// The DEFAULT account's grid — what [`credential_status`] returns, verbatim.
    default_grid: Vec<VenueCredStatus>,
    /// One entry per LABELLED account the store actually holds, sorted by label and deduplicated.
    labelled: Vec<(AccountLabel, Vec<VenueCredStatus>)>,
    /// **The store's PUBLIC values only** — every entry the caller's map held whose key
    /// `crate::keys::key_sensitivity` classifies [`crate::keys::Sensitivity::Public`].
    ///
    /// ⚠ **The FILTER is the security boundary, and it is here rather than at the widget on
    /// purpose.** The editor has to be able to show a server name or an account number back — a
    /// masked field that starts empty is right for a key and is exactly how an endpoint an operator
    /// set six months ago becomes unknowable from inside the app that wrote it. But a renderer that
    /// holds the whole store and *chooses* not to draw the secrets is one mistaken line away from
    /// drawing one. Filtering at the DERIVATION means `view` is never handed a secret at all, so
    /// the pane's first security rule — a stored plaintext is never read back into the UI — stays a
    /// property of what exists rather than of what each call site remembers.
    /// `crates/vike-connections/tests/panel/a11y_secrets.rs`'s
    /// `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor` is the
    /// end-to-end proof, and it now plants TWO sentinels so it proves the classification rather
    /// than the absence of any value at all.
    ///
    /// EMPTY for [`Self::new`]/[`Self::single`]: a caller that injected grids has no store to read.
    readable: HashMap<String, String>,
}

impl AccountGrids {
    /// **The derivation.** The default account's grid, plus one grid per labelled account the store
    /// holds — *"holds"* meaning **this grid lights at least one dot for it**, which is the only
    /// definition that cannot disagree with what the screen then shows.
    ///
    /// # Why not `vike_model::accounts::account_keys::accounts_in_store`
    ///
    /// That function is the store's own account enumerator and it is the right one for a screen
    /// built on `{VENUE}_{TIER}{SUFFIX}` keys — but it answers `None` for several of the credential
    /// families this grid genuinely edits, by its own documented classification, and it does so for
    /// TWO structurally different reasons:
    ///
    /// * **a tier token outside `vike_model::credential_keys::CREDENTIAL_TIERS`** — aster's
    ///   `TESTNET`, alpaca's `SANDBOX`, dukascopy's account-indexed `DEMO1`/`DEMO2` — so
    ///   `account_ref_from_key` finds no tier and skips the key; and
    /// * **a key prefix that is not a roster venue at all** — polymarket's store spelling is
    ///   `POLY_`, which `venue_prefix_of` cannot match against the `polymarket` slug (that venue's
    ///   own `load_polymarket_creds_for_account` says so in its doc).
    ///
    /// Every one of those is a shape `crates/vike-connections/tests/key_shapes/account_status.rs`'s
    /// `every_bespoke_shape_is_reachable_by_label` proves the READ side supports, so they would be
    /// accounts an operator could store credentials for and never see listed.
    /// `crates/vike-connections/tests/key_shapes/account_grids.rs`'s
    /// `the_store_enumerator_cannot_see_the_non_conforming_families` measures that gap rather
    /// than asserting it away.
    ///
    /// ⚠ Deliberately no COUNT of either the families or the bespoke shapes: this paragraph carried
    /// "two of the six" until the alpaca/ctrader/ibkr/polymarket arms landed and made it "four of
    /// the ten" in one commit. The test named above enumerates them; read it there.
    ///
    /// So the label set comes from `split_account_key` — the SAME parse, one rung lower, applied to
    /// the whole key rather than to a classified one — and the "does it exist" question is then
    /// answered by [`credential_status_for_account`] itself. A store key that is not a credential
    /// at all (an attribution code, a sidecar path) lights nothing and drops out; a malformed label
    /// fails the parse and drops out. Neither can produce an account nobody can fill in.
    ///
    /// Pure, like everything else here: the caller supplies the parsed store.
    #[must_use]
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        let mut labels: Vec<AccountLabel> = vars
            .keys()
            .filter_map(|k| split_account_key(k).ok())
            .filter(|s| !s.label.is_default())
            .map(|s| s.label)
            .collect();
        labels.sort();
        labels.dedup();
        let labelled = labels
            .into_iter()
            .map(|l| {
                let grid = credential_status_for_account(vars, &l);
                (l, grid)
            })
            .filter(|(_, grid)| grid.iter().any(|s| s.sim || s.demo || s.live))
            .collect();
        // ⚠ The ONE place a store value is carried past this function, and it carries only the
        // half that is not a secret — see the field's own doc for why the filter is here.
        let readable = vars
            .iter()
            .filter(|(k, _)| crate::keys::key_sensitivity(k) == crate::keys::Sensitivity::Public)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Self { default_grid: credential_status(vars), labelled, readable }
    }

    /// Already-computed grids, injected — the constructor a caller that is not deriving from a
    /// store uses, and the one a test harness that wants ONE venue's row rather than the whole
    /// roster needs. [`Self::from_vars`] is the DERIVATION and the only thing a production caller
    /// should reach for; this one asserts nothing about whether `labelled` agrees with any store.
    #[must_use]
    pub fn new(
        default_grid: Vec<VenueCredStatus>,
        labelled: Vec<(AccountLabel, Vec<VenueCredStatus>)>,
    ) -> Self {
        Self { default_grid, labelled, readable: HashMap::new() }
    }

    /// A single-account view over an already-computed grid — the shape a test harness that injects
    /// one venue's row needs, and the shape a caller with no store to enumerate has.
    #[must_use]
    pub fn single(default_grid: Vec<VenueCredStatus>) -> Self {
        Self::new(default_grid, Vec::new())
    }

    /// The DEFAULT account's grid.
    #[must_use]
    pub fn default_grid(&self) -> &[VenueCredStatus] {
        &self.default_grid
    }

    /// **The store's PUBLIC values** — what the editor may read back into a field, and nothing
    /// else. The field this returns carries the reason this map cannot contain a secret.
    #[must_use]
    pub fn readable_values(&self) -> &HashMap<String, String> {
        &self.readable
    }

    /// This account's grid, or `None` when the store holds nothing for it.
    ///
    /// ⚠ `None` is NOT "fall back to the default account" — it is the same no-borrowing rule
    /// [`credential_status_for_account`] states: an account with no keys of its own reads absent,
    /// because green dots beside an account that cannot sign anything is the one answer worse than
    /// no answer. It is also the ordinary state of an account an operator has just NAMED and not
    /// yet filled in.
    #[must_use]
    pub fn grid_for(&self, label: &AccountLabel) -> Option<&[VenueCredStatus]> {
        if label.is_default() {
            return Some(&self.default_grid);
        }
        self.labelled.iter().find(|(l, _)| l == label).map(|(_, g)| g.as_slice())
    }

    /// The LABELLED accounts, in order. Empty on a single-account box.
    pub fn labels(&self) -> impl Iterator<Item = &AccountLabel> + '_ {
        self.labelled.iter().map(|(l, _)| l)
    }

    /// An all-absent grid over the same venues, in the same order — what a NAMED-but-unfilled
    /// account renders as. Built from [`Self::default_grid`]'s venue list rather than from
    /// [`vike_model::VENUES`] so a caller that injected a subset of the roster gets its subset back.
    #[must_use]
    pub fn absent_grid(&self) -> Vec<VenueCredStatus> {
        self.default_grid
            .iter()
            .map(|s| VenueCredStatus {
                venue: s.venue.clone(),
                sim: false,
                demo: false,
                live: false,
            })
            .collect()
    }
}
