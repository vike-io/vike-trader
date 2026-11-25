//! The `SeriesGaps` verb: a stored series' missing days, judged against its market's TRADING CALENDAR.
//!
//! The store answers every missing UTC day inside a series' span (`vike_data::HistoryStore::series_gaps`,
//! a manifest read). A closed market leaves days no tape can hold, so before this verb judged them a
//! Dukascopy FX pair imported over twelve years listed 618 "gaps" on the CI box (2026-11-16), every one a
//! Saturday, and the Data Manager's Backfill would have asked the venue for all of them. The calendar is
//! the one the backtest's input coverage and the live door read: the series' KEPT row
//! (`<project>/instruments/<venue>.json`, [`vike_catalog::kept_row_of_store_name`]) states its class, and
//! [`vike_catalog::trading_calendar`] turns that into a session; the judgement is
//! [`vike_venues::SessionCalendar::trading_gaps`]. A share CFD runs whole weekdays (its row names no
//! region). A series with no calendar (a 24/7 market, or a class no kept row states: an index or metal
//! CFD, an IBKR stock, a venue with no list) keeps every missing day.

use std::path::Path;

use vike_data::{HistoryStore, SeriesId};
use vike_datahub_client::proto::Response;

/// The `SeriesGaps` verb: the store's gaps of `id`, kept to the ones that are trading time.
pub(super) fn series_gaps_verb(
    store: &dyn HistoryStore,
    id: &SeriesId,
    instruments: Option<&Path>,
) -> Response {
    match store.series_gaps(id) {
        Ok(gaps) => Response::SeriesGaps(trading_gaps(id, gaps, instruments)),
        Err(e) => Response::Error(e.to_string()),
    }
}

/// `gaps` of `id` that its market's calendar holds as trading time; every one when no calendar is known.
/// Only a venue whose kept row can name a session class reads its list ([`vike_catalog::session_venue`]).
fn trading_gaps(
    id: &SeriesId,
    gaps: Vec<(i64, i64)>,
    instruments: Option<&Path>,
) -> Vec<(i64, i64)> {
    if gaps.is_empty() || !vike_catalog::session_venue(&id.venue) {
        return gaps;
    }
    // Through the parsed lists: a Data Manager open asks this of every series that may have gaps.
    let list =
        instruments.and_then(|dir| crate::instrument_lists::parsed_kept_list(dir, &id.venue));
    let row = list.as_ref().and_then(|l| {
        vike_catalog::kept_row_of_store_name(&l.instruments, &id.venue, &id.symbol).ok()
    });
    match vike_catalog::trading_calendar(&id.venue, row) {
        Some(calendar) => calendar.trading_gaps(
            &gaps,
            id.interval.as_deref().and_then(vike_marketdata::interval::interval_ms),
        ),
        None => gaps,
    }
}

#[path = "series_gaps_tests.rs"]
#[cfg(test)]
mod series_gaps_tests;
