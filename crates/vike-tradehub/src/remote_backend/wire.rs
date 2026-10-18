//! **The total conversions between the gatewayhub's mirror DTOs and the fold's own types** — and the
//! two tables the tradehub derives for itself when the gatewayhub is not there.
//!
//! The wire carries WORDS, grids and static facts (`vike_gatewayhub_client::proto`'s
//! `backend_dto`): a hub client may not name `vike-config`, so an `ArmingBlock` crosses as its
//! [`ArmingBlock::word`] and comes back here through [`ArmingBlock::from_word`]. Every conversion is
//! an exhaustive `match` (no `_` arm), so a variant added on either side fails to COMPILE here until
//! somebody decides what it means, and an UNKNOWN word from a newer gatewayhub is **"adapter
//! failed", never armed** ([`block_of_word`]).
//!
//! # The two derived tables
//!
//! * [`unavailable_table`] — the gatewayhub did not answer: every `paper` account keeps the rows a
//!   paper-only box shows, every `demo`/`live` account is NOT RUNNING with
//!   [`ArmingBlock::GatewayUnavailable`] (decision 0164, D7; never a paper stand-in, decision 0156).
//!   It starts from `PaperBackend`'s table, which already answers from the `account` rows alone, and
//!   re-words the one block that differs, so the two cannot disagree about which account is paper.
//! * [`with_data_only`] — a venue declared `data_only` keeps its feed credentials and loses its exec
//!   ones: the gatewayhub is told so per start (`Withhold::DataOnly`), and this lays the same answer
//!   over the table so the armed set, the reconcile gate and the venue-arming journal read what the
//!   start will do (a `LocalBackend` got this by stripping the credentials from its own policy).

use std::collections::{BTreeMap, HashSet};

use vike_config::{ArmingBlock, VenueArming};
use vike_gatewayhub_client::proto;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub_core::symbol_grid::GridSource;
use vike_tradehub_core::{ArmingRow, ArmingTable, VenueBackend as _, VenueFacts};

/// The block a wire word names; an unknown word is [`ArmingBlock::AdapterFailed`] (logged with the
/// raw word), never a block that runs.
pub(super) fn block_of_word(word: &str, context: &str) -> ArmingBlock {
    ArmingBlock::from_word(word).unwrap_or_else(|| {
        tracing::error!(
            word,
            "{context}: the gatewayhub answered an arming word this build does not know — read as \
             `adapter failed`, never as armed (a newer gatewayhub than this tradehub?)"
        );
        ArmingBlock::AdapterFailed
    })
}

/// The block a NOT RUNNING reply carries. A word that names a block which RUNS (`none`,
/// `paper_tier`, `orders_held`) in a refusal is a protocol violation and reads as `adapter failed`
/// too: a refusal must never be the way an account gets armed.
pub(super) fn refusal_block(word: &str, context: &str) -> ArmingBlock {
    let block = block_of_word(word, context);
    if block.runs() {
        tracing::error!(
            word,
            "{context}: the gatewayhub refused a start with a word that names a RUNNING block — \
             read as `adapter failed`"
        );
        return ArmingBlock::AdapterFailed;
    }
    block
}

/// The fold's grid source for the wire's. Exhaustive both ways.
pub(super) fn grid_source_of_wire(source: proto::GridSource) -> GridSource {
    match source {
        proto::GridSource::InHand => GridSource::InHand,
        proto::GridSource::PerSymbolFetch => GridSource::PerSymbolFetch,
        proto::GridSource::NoGrid => GridSource::NoGrid,
    }
}

/// The wire's grid source for the fold's. Exhaustive both ways.
#[cfg(test)]
pub(super) fn grid_source_to_wire(source: GridSource) -> proto::GridSource {
    match source {
        GridSource::InHand => proto::GridSource::InHand,
        GridSource::PerSymbolFetch => proto::GridSource::PerSymbolFetch,
        GridSource::NoGrid => proto::GridSource::NoGrid,
    }
}

/// The `clock_decl_word` a fact carries, as the `&'static str` the fold's facts hold. The three
/// words are `VenueFacts::clock_decl_word`'s; anything else is shown as `unknown` (display only: no
/// decision reads it).
fn clock_word(word: &str) -> &'static str {
    match word {
        "wired" => "wired",
        "not-wired" => "not-wired",
        "absent" => "absent",
        _ => "unknown",
    }
}

fn facts_of_wire(facts: &proto::VenueFacts) -> VenueFacts {
    VenueFacts {
        adapter_present: facts.adapter_present,
        addresses_accounts: facts.addresses_accounts,
        takes_recon_trigger: facts.takes_recon_trigger,
        grid_source: grid_source_of_wire(facts.grid_source),
        process_exclusive: facts.process_exclusive.clone(),
        clock_decl_word: clock_word(&facts.clock_decl_word),
    }
}

/// One wire row as the fold's [`ArmingRow`], or `None` for a row this build cannot name (a venue
/// that is not on its roster, an unparseable label) — logged, and the account is simply absent from
/// the table, which the fold reads as "no row for this account" (NOT RUNNING, `adapter failed`).
fn row_of_wire(row: &proto::ArmingRow) -> Option<ArmingRow> {
    let Some(venue) = vike_model::VENUES.iter().copied().find(|v| *v == row.venue) else {
        tracing::warn!(
            venue = %row.venue,
            "the gatewayhub's arming table names a venue this build's roster does not carry — row skipped"
        );
        return None;
    };
    let label = match row.label.as_deref() {
        None => AccountLabel::Default,
        Some(text) => match AccountLabel::parse(text) {
            Ok(label) => label,
            Err(e) => {
                tracing::warn!(
                    venue,
                    error = ?e,
                    "the gatewayhub's arming table names an account label this build cannot parse — row skipped"
                );
                return None;
            }
        },
    };
    let known = ArmingBlock::from_word(&row.block_word);
    let block = known.unwrap_or_else(|| block_of_word(&row.block_word, venue));
    // A row whose word this build does not know does NOT RUN, whatever tier the daemon claims for it.
    let effective = if known.is_some() { row.effective } else { None };
    Some(ArmingRow {
        arming: VenueArming {
            venue,
            label,
            tier: row.tier,
            effective,
            block,
            account_ids: row.account_ids.clone(),
        },
        book: row.book.clone(),
    })
}

/// The fold's arming table for the wire's. Rows come out in the daemon's order.
pub(crate) fn table_of_wire(table: &proto::ArmingTable) -> ArmingTable {
    let facts: BTreeMap<String, VenueFacts> =
        table.facts.iter().map(|(venue, facts)| (venue.clone(), facts_of_wire(facts))).collect();
    let rows: Vec<ArmingRow> = table.rows.iter().filter_map(row_of_wire).collect();
    ArmingTable::new(facts, rows)
}

/// The table of a tradehub that has no gatewayhub to ask. See the module doc.
pub(super) fn unavailable_table(accounts: &AccountDirectory) -> ArmingTable {
    let paper = vike_tradehub_core::PaperBackend::new(accounts)
        .arming_table()
        .expect("the paper backend answers from the account rows alone");
    rebuild(&paper, |row| {
        if row.arming.block == ArmingBlock::LiveSwitchOff {
            row.arming.block = ArmingBlock::GatewayUnavailable;
        }
    })
}

/// `table` with each row of a `data_only` venue that RUNS at `demo`/`live` read as the start will
/// leave it: NOT RUNNING for want of exec credentials (`no credentials` at `demo`, `no live
/// credentials` at `live` — the words a `LocalBackend` over the credential-stripped policy gave).
pub(super) fn with_data_only(table: &ArmingTable, data_only: &HashSet<String>) -> ArmingTable {
    if data_only.is_empty() {
        return table.clone();
    }
    rebuild(table, |row| {
        let a = &mut row.arming;
        if !data_only.contains(a.venue) || !a.block.runs() {
            return;
        }
        let tier = match a.tier {
            Some(tier @ (AccountTier::Demo | AccountTier::Live)) => tier,
            Some(AccountTier::Paper) | None => return,
        };
        a.effective = None;
        a.block = match tier {
            AccountTier::Live => ArmingBlock::LiveCredentialsAbsent,
            AccountTier::Demo | AccountTier::Paper => ArmingBlock::NoCredentials,
        };
    })
}

/// `table` with `edit` applied to every row; the facts are carried over untouched.
fn rebuild(table: &ArmingTable, mut edit: impl FnMut(&mut ArmingRow)) -> ArmingTable {
    let facts: BTreeMap<String, VenueFacts> = vike_model::VENUES
        .iter()
        .filter_map(|v| table.facts_of(v).map(|f| ((*v).to_string(), f.clone())))
        .collect();
    let rows: Vec<ArmingRow> = table
        .rows()
        .iter()
        .cloned()
        .map(|mut row| {
            edit(&mut row);
            row
        })
        .collect();
    ArmingTable::new(facts, rows)
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;
