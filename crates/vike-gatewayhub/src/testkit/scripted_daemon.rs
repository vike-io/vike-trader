//! **The scripted gatewayhub DAEMON**: the real `vike-gatewayhub` process (its boot, its hub keys, its server) over a
//! scripted POLYMARKET data lane, for a suite that can only SPAWN binaries: `crates/vike-agent-evaluation`'s world runs
//! a real datahub over this daemon, so its paper tradehub has a price path and no venue is dialled.
//!
//! Its binary is `src/bin/vike-gatewayhub-scripted.rs`, built only with `test-support` and `data` (the manifest's
//! `[[bin]]` row), so no release builds it. It verifies its clients with the project's own gatewayhub pair, as the
//! real daemon does (`crate::gatewayhub_cli::run_scripted`).
//!
//! What it serves: [`VENUE`]'s data lane ([`super::data::DataLaneScript`]), where every book or quote subscription is
//! answered inside the call with one two-sided quote ([`BID`] / [`ASK`]), and a REST lane that answers nothing. It links
//! no bridge.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use vike_data::DataClientSink;
use vike_marketdata::TickQuote;

use super::data::{DataLaneScript, SubVerb, SubscribeCall};
use super::rest::RestLaneScript;
use crate::server::lanes::ServerLanes;

/// The one venue served.
pub const VENUE: &str = "polymarket";
/// The quote's bid: above every buy the agent-evaluation world rests (0.33 to 0.40), so none of them fills.
pub const BID: f64 = 0.55;
/// The quote's ask.
pub const ASK: f64 = 0.60;

/// The daemon, taking what `crate::gatewayhub_cli::run` takes.
pub fn run(vars: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    let data = DataLaneScript::new(&[VENUE]);
    data.set_default_hook(Arc::new(quote_on_subscribe));
    let lanes = ServerLanes { data: Arc::new(data), rest: Arc::new(RestLaneScript::new()) };
    crate::gatewayhub_cli::run_scripted(vars, cwd, args, lanes)
}

/// The seed a venue emits inside a book or quote subscription: one two-sided quote of its symbol.
fn quote_on_subscribe(sink: &dyn DataClientSink, call: &SubscribeCall) {
    if matches!(call.verb, SubVerb::Book | SubVerb::Quotes) {
        let now = vike_model::now_ms();
        let quote = TickQuote {
            timestamp: now,
            local_timestamp: now,
            bid: BID,
            ask: ASK,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: call.symbol.as_str().into(),
        };
        sink.quote(VENUE, &call.symbol, quote);
    }
}
