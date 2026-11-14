//! A venue whose live trendbar stream an OUTAGE cuts, for `tests/offline/trendbar_outage.rs`.
//!
//! EURUSD (id 1), every request answered by `serve::script_reply` like any other fake, except two. A
//! `SUBSCRIBE_LIVE_TRENDBAR_REQ` is answered by the scripted live snapshots, each a `SPOT_EVENT` carrying one
//! trendbar, and on the FIRST connection the socket dies after its [`TrendbarOutage::outage_at_subscribe`]-th one. A
//! `GET_TRENDBARS_REQ` is answered with the bars the venue has STORED by then, of the asked period, that open inside
//! the request's window; on a later connection after [`TrendbarOutage::answer_after`], followed by
//! [`TrendbarOutage::live_after_answer`], or refused once ([`TrendbarOutage::refuse_first_fill`]), and with an order's
//! event pushed before it ([`TrendbarOutage::event_at_fill`]), or held until the probe that follows it is refused
//! ([`TrendbarOutage::refuse_probe_during_fill`]). Every answer carries the fake's own envelope id and a fill's refusal
//! none, never the request's: nothing of the fill may depend on the venue echoing it. The probe's refusal alone echoes
//! the probe's id, as the venue echoes an order's. The listener accepts one connection after another.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use prost::Message;

use crate::transport::connection::stream::EofStream;
use crate::transport::framing::{self, FrameReader};
use crate::transport::proto::{
    ProtoOaErrorRes, ProtoOaGetTrendbarsReq, ProtoOaGetTrendbarsRes, ProtoOaSpotEvent,
    ProtoOaTrendbar, pt,
};

use super::fake::AccountsScript;
use super::serve::script_reply;

/// What [`super::FakeCtrader::start_trendbar_outage`]'s venue stores and pushes, before the outage and after it.
#[derive(Clone, Default)]
pub struct TrendbarOutage {
    /// The stored bars the first connection answers a `GET_TRENDBARS_REQ` from (the subscribe's seed).
    pub stored_before: Vec<ProtoOaTrendbar>,
    /// The stored bars every later connection answers from: the bars of the periods the outage hid among them.
    pub stored_after: Vec<ProtoOaTrendbar>,
    /// The live snapshots the first connection pushes at a trendbar subscribe.
    pub live_before: Vec<ProtoOaTrendbar>,
    /// The live snapshots a later connection pushes at a trendbar subscribe.
    pub live_after: Vec<ProtoOaTrendbar>,
    /// How long a later connection takes to answer a `GET_TRENDBARS_REQ` (past the client's wait: a late answer).
    pub answer_after: Duration,
    /// The live snapshots a later connection pushes right after that answer.
    pub live_after_answer: Vec<ProtoOaTrendbar>,
    /// The first connection dies right after its n-th `SUBSCRIBE_LIVE_TRENDBAR_REQ` (0 or 1: the first).
    pub outage_at_subscribe: usize,
    /// A later connection refuses its first `GET_TRENDBARS_REQ` with an `ERROR_RES` that carries no id.
    pub refuse_first_fill: bool,
    /// A later connection pushes an order's `EXECUTION_EVENT` (`super::INFLIGHT_FILL_ORDER_CLIENT_ID`'s fill) the moment it reads
    /// its first `GET_TRENDBARS_REQ`, before it takes [`Self::answer_after`] to answer: an order event read while a fill
    /// is outstanding.
    pub event_at_fill: bool,
    /// A later connection holds the answer to its first `GET_TRENDBARS_REQ` until it reads the actor's next probe
    /// (`TRADER_REQ`), refuses that probe with an `ERROR_RES` echoing its id, in words that end no session, and only
    /// then answers the fill: the probe's refusal read while the fill is outstanding.
    pub refuse_probe_during_fill: bool,
    /// When a later connection read each of its `GET_TRENDBARS_REQ`s, for the pace of a reconnect's fills.
    pub fills_asked: Arc<Mutex<Vec<Instant>>>,
}

/// Accept connections on `listener`, one after another, and serve each as the module doc says.
pub(super) fn spawn_trendbar_outage(
    listener: TcpListener,
    seen: Arc<Mutex<Vec<u32>>>,
    script: TrendbarOutage,
) {
    thread::Builder::new()
        .name("fake-ctrader-trendbar-outage".into())
        .spawn(move || {
            let mut first = true;
            while let Ok((stream, _peer)) = listener.accept() {
                serve_outage(stream, &seen, &script, first);
                first = false;
            }
        })
        .expect("spawn fake ctrader-trendbar-outage");
}

/// One trendbar as a `SPOT_EVENT` of EURUSD.
fn spot(tb: &ProtoOaTrendbar) -> (u32, Vec<u8>, &'static str) {
    let spot = ProtoOaSpotEvent {
        ctid_trader_account_id: 99,
        symbol_id: 1,
        trendbar: vec![*tb],
        ..Default::default()
    };
    (pt::SPOT_EVENT, spot.encode_to_vec(), "srv")
}

/// Serve one connection until the client closes it, or, on the `first`, until its outage.
fn serve_outage(stream: TcpStream, seen: &Mutex<Vec<u32>>, script: &TrendbarOutage, first: bool) {
    let (stored, live) = if first {
        (&script.stored_before, &script.live_before)
    } else {
        (&script.stored_after, &script.live_after)
    };
    let mut subscribes = 0;
    let mut refuse = !first && script.refuse_first_fill;
    let mut event = !first && script.event_at_fill;
    let mut hold = !first && script.refuse_probe_during_fill;
    let mut held: Option<ProtoOaGetTrendbarsReq> = None;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);
    loop {
        let msg = match reader.next_frame() {
            Ok(Some(msg)) => msg,
            Ok(None) if eof.load(Ordering::SeqCst) => return,
            Ok(None) => continue,
            Err(_) => return,
        };
        seen.lock().expect("seen lock").push(msg.payload_type);
        let replies: Vec<(u32, Vec<u8>, &str)> = match msg.payload_type {
            pt::GET_TRENDBARS_REQ if !first => {
                script.fills_asked.lock().expect("fills lock").push(Instant::now());
                let req = ProtoOaGetTrendbarsReq::decode(msg.payload.as_deref().unwrap_or(&[]))
                    .expect("a trendbars request");
                if std::mem::take(&mut refuse) {
                    let err = ProtoOaErrorRes {
                        error_code: "INVALID_REQUEST".to_string(),
                        description: Some("refused by the scripted venue".to_string()),
                        ..Default::default()
                    };
                    vec![(pt::ERROR_RES, err.encode_to_vec(), "")]
                } else if std::mem::take(&mut hold) {
                    held = Some(req);
                    Vec::new()
                } else {
                    if std::mem::take(&mut event) {
                        let fill = super::serve::inflight_fill_event().encode_to_vec();
                        let frame = framing::encode(pt::EXECUTION_EVENT, &fill, "srv");
                        if writer.write_all(&frame).is_err() || writer.flush().is_err() {
                            return;
                        }
                    }
                    thread::sleep(script.answer_after);
                    std::iter::once(trendbars(&req, stored))
                        .chain(script.live_after_answer.iter().map(spot))
                        .collect()
                }
            }
            pt::GET_TRENDBARS_REQ => {
                let req = ProtoOaGetTrendbarsReq::decode(msg.payload.as_deref().unwrap_or(&[]))
                    .expect("a trendbars request");
                vec![trendbars(&req, stored)]
            }
            pt::TRADER_REQ if held.is_some() => {
                let err = ProtoOaErrorRes {
                    error_code: "INVALID_REQUEST".to_string(),
                    description: Some("probe refused by the scripted venue".to_string()),
                    ..Default::default()
                };
                let id = msg.client_msg_id.as_deref().unwrap_or("");
                let frame = framing::encode(pt::ERROR_RES, &err.encode_to_vec(), id);
                if writer.write_all(&frame).is_err() || writer.flush().is_err() {
                    return;
                }
                let req = held.take().expect("a held fill");
                std::iter::once(trendbars(&req, stored))
                    .chain(script.live_after_answer.iter().map(spot))
                    .collect()
            }
            pt::SUBSCRIBE_LIVE_TRENDBAR_REQ => {
                subscribes += 1;
                live.iter().map(spot).collect()
            }
            _ => script_reply(&msg, AccountsScript::OneDemo, false)
                .into_iter()
                .map(|(reply_type, body)| (reply_type, body, "srv"))
                .collect(),
        };
        for (reply_type, body, message_id) in replies {
            let frame = framing::encode(reply_type, &body, message_id);
            if writer.write_all(&frame).is_err() || writer.flush().is_err() {
                return;
            }
        }
        if first && subscribes >= script.outage_at_subscribe.max(1) {
            return; // the outage: the socket dies with the live snapshots just sent
        }
    }
}

/// The answer to `req` from `stored`: the bars of the asked period that open inside its window.
fn trendbars(
    req: &ProtoOaGetTrendbarsReq,
    stored: &[ProtoOaTrendbar],
) -> (u32, Vec<u8>, &'static str) {
    let window = req.from_timestamp.unwrap_or(0)..=req.to_timestamp.unwrap_or(i64::MAX);
    let res = ProtoOaGetTrendbarsRes {
        ctid_trader_account_id: 99,
        period: req.period,
        trendbar: stored
            .iter()
            .filter(|tb| tb.period == Some(req.period))
            .filter(|tb| {
                window.contains(&(i64::from(tb.utc_timestamp_in_minutes.unwrap_or(0)) * 60_000))
            })
            .copied()
            .collect(),
        symbol_id: Some(req.symbol_id),
        ..Default::default()
    };
    (pt::GET_TRENDBARS_RES, res.encode_to_vec(), "srv")
}
