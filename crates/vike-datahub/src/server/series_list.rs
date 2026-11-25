//! The `ListSeries` verb's reply, as the bytes last written for the same listing.
//!
//! The store holds its own walk while no series comes or goes (the DataFusion store's
//! `crates/vike-store/src/datafusion_hist/series_walk.rs`'s `SeriesWalkMemo`), so what was left per ask is writing the
//! listing: 273 kB of JSON for the CI box's 2,020 series (2026-11-19). [`LISTED`] keeps the last reply's MSGPACK beside the
//! listing it was written from (`FEATURE_WIRE_MSGPACK`'s form, the one cache), and a listing EQUAL to it is answered
//! from those bytes, in the form the request came in (`vike_hub_protocol::frame::reply_body_of_msgpack`). One value
//! writes one string of bytes, so the reply is byte for byte what `Response::SeriesList` of the listing writes,
//! whatever store answered it. A JSON request (an older client) is answered the value, written as it always was.
//! Bounded: one listing and its bytes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use vike_data::{HistoryStore, SeriesId};
use vike_datahub_client::proto::Response;
use vike_hub_protocol::frame::{BodyCodec, encode_reply, reply_body_of_msgpack};

use super::step::Step;

/// The server's one listing and its bytes ([`Listed`]).
pub(super) static LISTED: Listed = Listed::new();

/// A listing beside the bytes its reply writes, and how many replies were written (the tests read it).
pub(super) struct Listed {
    held: Mutex<Option<(Vec<SeriesId>, Vec<u8>)>>,
    pub(super) written: AtomicU64,
}

impl Listed {
    pub(super) const fn new() -> Self {
        Self { held: Mutex::new(None), written: AtomicU64::new(0) }
    }

    /// `ListSeries` over `store`, asked in `codec`: from the held msgpack when its listing is the held one, else the
    /// listing written now and held. A store that fails is its error; a JSON request, and a listing that does not
    /// write, are answered as the value.
    pub(super) fn step(&self, store: &dyn HistoryStore, codec: BodyCodec) -> Step {
        let series = match store.list_series() {
            Ok(series) => series,
            Err(e) => return Step::reply(super::route::store_refused(&e)),
        };
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let hit = held.as_ref().is_some_and(|(listed, _)| *listed == series);
        if !hit && codec != BodyCodec::Json {
            // The plain msgpack: the reply's frame less its four reserved bytes and its magic.
            let Ok(frame) = encode_reply(BodyCodec::Msgpack, &Response::SeriesList(series.clone()))
            else {
                return Step::reply(Response::SeriesList(series));
            };
            self.written.fetch_add(1, Ordering::Relaxed);
            *held = Some((series.clone(), frame[8..].to_vec()));
        }
        match held.as_ref().and_then(|(_, msgpack)| reply_body_of_msgpack(codec, msgpack)) {
            Some(body) => Step::Bytes(body),
            _ => Step::reply(Response::SeriesList(series)),
        }
    }
}

#[path = "series_list_tests.rs"]
#[cfg(test)]
mod series_list_tests;
