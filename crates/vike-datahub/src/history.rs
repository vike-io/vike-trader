//! **The history-channels read — `Request::HistoryChannels`, the server's half.** The rows are
//! `vike_catalog::history_channels_for`'s as THIS build declares them; this module adds the three
//! things only this process can say: whether each built row's lane is in its collector table,
//! whether the credential that lane reads is stored on its box, and what its store holds per kind.
//! `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §2 is the design and
//! `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md` the scope verdict.
//!
//! # ⚠ What this verb must never do, and does not
//!
//! * **Call a collector.** The table is asked [`crate::backfill::BackfillTable::mounts`] — a lookup
//!   over its entries — and nothing else; no `BackfillFn` is reached. A collector is how this daemon
//!   spends a venue's budget and, on the credentialed lane, the operator's token, and 0097's reopener
//!   is exactly "a credentialed lane reachable from an Observe verb".
//! * **Read a credential VALUE.** The presence word comes from the table's probe
//!   ([`crate::backfill::BackfillCredentialProbe`]), which the composition root builds over a names-only
//!   read. Nothing here holds, logs or returns a token.
//! * **Call a venue.** How far back an instrument goes is not answered here; every date in the rows
//!   is a maintainer's read date.
//!
//! The one store read is `HistoryStore::inventory`, the same read the Observe `Inventory` verb serves.

use std::sync::Arc;

use vike_catalog::{HistoryChannelLane, HistoryChannelState, history_channels_for};
use vike_data::HistoryStore;
use vike_datahub_client::history::{
    HistoryChannelVenue, HistoryChannelsReport, channel_report, held_for,
};
use vike_datahub_client::proto::Response;
use vike_datahub_client::refusals::DatahubRefusal;
use vike_model::CredentialPresence;

use crate::backfill::{BackfillTable, needs_a_credential};

/// **Answer one `Request::HistoryChannels`**: every roster venue, in roster order, with `table`'s
/// overlay — `None` (a build that mounts no collector table) reads every built row NOT mounted and
/// every credentialed one [`CredentialPresence::NotChecked`]. `as_of_ms` is this server's clock, the
/// instant every rolling window is resolved against.
///
/// The one lane that is not the table's, [`HistoryChannelLane::ArchiveDownload`], is mounted when
/// the import lane mounts Dukascopy's download (`downloads`; the SEC downloads are no venue's), and its
/// AWS key is
/// [`CredentialPresence::NotChecked`]: the key is read per request, never by this read.
///
/// A store that cannot answer the inventory read is refused (`DatahubRefusal::HistoryChannelsInventory`) with the
/// store's own text inside.
pub(crate) fn history_channels_verb(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    table: Option<&BackfillTable>,
    downloads: bool,
    as_of_ms: i64,
) -> Response {
    let inventory = match store.inventory() {
        Ok(inventory) => inventory,
        Err(e) => {
            let why = e.to_string();
            return Response::Refused(DatahubRefusal::HistoryChannelsInventory { why }.into());
        }
    };
    let venues = vike_model::VENUES
        .iter()
        .map(|&venue| HistoryChannelVenue {
            venue: venue.to_string(),
            channels: history_channels_for(venue)
                .iter()
                .map(|row| {
                    let (mounted, credential) = match row.state {
                        HistoryChannelState::Built(HistoryChannelLane::ArchiveDownload) => {
                            (Some(downloads), CredentialPresence::NotChecked)
                        }
                        HistoryChannelState::Built(lane) => {
                            let credential = match table {
                                Some(t) => t.credential_presence(venue, lane),
                                None if needs_a_credential(lane) => CredentialPresence::NotChecked,
                                None => CredentialPresence::NotNeeded,
                            };
                            (Some(table.is_some_and(|t| t.mounts(venue, lane))), credential)
                        }
                        // No lane serves a designed row, so no credential of this server's is in
                        // question and there is nothing to be mounted.
                        HistoryChannelState::Designed(_) => (None, CredentialPresence::NotNeeded),
                    };
                    channel_report(row, as_of_ms, mounted, credential)
                })
                .collect(),
            held: held_for(venue, &inventory),
        })
        .collect();
    Response::HistoryChannels(HistoryChannelsReport { as_of_ms, venues })
}
