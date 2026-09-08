//! **The byte-identity gate for the spot fee lane** — what each venue's `fetch_fee_rates` actually
//! puts on the wire, recorded rather than reasoned about.
//!
//! `vike_binance::family::recon`'s `ReconPaths` gained a `spot_commission_rate` slot so Aster could
//! price spot fees off its own per-symbol `GET /api/v3/commissionRate` (its `/api/v3/account` fork
//! carries no `commissionRates` object — see `crates/bridges/aster/src/spot.rs`'s
//! `PATH_COMMISSION_RATE`). That slot lives in a table **both venues share**, so the change is only
//! safe if Binance keeps taking the verbatim pre-slot path.
//!
//! A `spot_commission_rate: None` assertion alone would NOT prove that: it pins the input, not the
//! behavior, and the routing `match` could still be rewritten to issue an extra request, reorder the
//! reads, or attach a `symbol` param the account endpoint never had. This file asserts the OUTPUT
//! instead — a recording `RestTransport` captures every `(path, params)` the lane issues, and each
//! test compares the whole request LIST. Same discipline as the capability-matrix pins
//! (`crates/vike-model/src/venue_caps.rs`'s `caps_for`), applied to a wire effect.
//!
//! ⚠ **Both venues are asserted in ONE file on purpose.** The pair IS the property: the same shared
//! `FamilyReconClient::fetch_fee_rates` must produce two DIFFERENT request lists from two specs, and
//! a regression that collapsed the routing to either single arm would still leave a per-venue test
//! green. It lives in `vike-aster` for a dependency reason, not a topical one — vike-aster depends
//! on vike-binance, so only this side can construct both clients; the reverse edge does not exist
//! and adding it (a dev-dep cycle) to host the test in vike-binance would be a worse trade.
//!
//! Offline: no network, no credentials, canned bodies, and nothing `#[ignore]`d — this is the copy
//! that gates CI. `tests/aster_reconcile_smoke.rs` proves the same routing against the LIVE venue,
//! but only when run manually with credentials.

use std::sync::{Arc, Mutex};

use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_exec::recon::ReconClient;
use vike_model::FeeSchedule;

/// Every request this file records, as `(path, params)` in issue order.
type Calls = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Records every signed request and answers with ONE canned body.
///
/// The body deliberately carries BOTH fee grammars at once — the account-shaped
/// `commissionRates{maker,taker}` AND the `commissionRate` endpoint's flat
/// `makerCommissionRate`/`takerCommissionRate` — so the returned `FeeSchedule` identifies WHICH
/// parser ran. A transport answering only one shape would make a mis-routed lane look like an
/// absent field (`Ok(None)`, the fail-soft answer) instead of a wrong one.
struct Recorder {
    calls: Calls,
}

impl Recorder {
    /// Returns the transport and a handle on its call log (the transport is MOVED into the client,
    /// so the log has to be shared rather than borrowed back).
    fn new() -> (Self, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        (Recorder { calls: Arc::clone(&calls) }, calls)
    }
}

/// The account-body grammar's rates, as bps: `0.00090000`/`0.00110000`. Deliberately unlike any real
/// venue schedule and unlike the endpoint pair below, so the bps a test reads back NAMES the parser
/// that produced it.
const ACCOUNT_MAKER_BPS: f64 = 9.0;
const ACCOUNT_TAKER_BPS: f64 = 11.0;
/// The `commissionRate` ENDPOINT grammar's rates, as bps: `0.00005000`/`0.00040000` — and, not
/// coincidentally, aster's real published spot schedule (0.5 bps / 4 bps).
const ENDPOINT_MAKER_BPS: f64 = 0.5;
const ENDPOINT_TAKER_BPS: f64 = 4.0;

impl RestTransport for Recorder {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        let recorded = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.calls.lock().unwrap().push((path.to_string(), recorded));
        Ok(serde_json::json!({
            "commissionRates": { "maker": "0.00090000", "taker": "0.00110000" },
            "makerCommissionRate": "0.00005000",
            "takerCommissionRate": "0.00040000",
        }))
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("the fee lane must never issue an UNSIGNED request (public {path})");
    }
}

fn bps(s: Option<FeeSchedule>) -> (f64, f64) {
    match s {
        Some(FeeSchedule::PercentMakerTaker { maker_bps, taker_bps }) => (maker_bps, taker_bps),
        other => panic!("expected PercentMakerTaker from the fee lane, got {other:?}"),
    }
}

fn calls_of(c: &Calls) -> Vec<(String, Vec<(String, String)>)> {
    c.lock().unwrap().clone()
}

/// **BINANCE BYTE-IDENTITY.** Its spot fee lane must issue EXACTLY ONE signed request, to
/// `/api/v3/account`, with NO parameters — the same account body `fetch_balance` already reads —
/// and must parse it with the account-body `commissionRates` extractor.
///
/// Every clause guards a different way the shared slot could have leaked into this venue:
///   * the PATH pins that binance did not follow aster onto a per-symbol endpoint;
///   * the EMPTY params pin that no `symbol` was attached to an account-wide read;
///   * `len() == 1` pins that no probe was added ALONGSIDE the existing read (a leak that would
///     leave the returned value correct and the request count wrong — invisible to a value assert);
///   * the 9/11 bps pin that `parse_spot_fee_rates` ran and not `parse_commission_rate_body`, which
///     the two grammars in one canned body are what make distinguishable.
#[test]
fn binance_spot_fee_lane_still_reads_the_account_body() {
    let (t, calls) = Recorder::new();
    let client =
        vike_binance::recon_client::BinanceReconClient::spot(NullSigner, t, "https://x", "BTCUSDT");

    let fetched = client.fetch_fee_rates().expect("binance spot fetch_fee_rates");

    assert_eq!(
        calls_of(&calls),
        vec![("/api/v3/account".to_string(), vec![])],
        "binance's spot fee lane must stay the single, parameterless account read it was before the \
         `spot_commission_rate` slot existed"
    );
    assert_eq!(
        bps(fetched),
        (ACCOUNT_MAKER_BPS, ACCOUNT_TAKER_BPS),
        "binance must parse the account body's `commissionRates` object, not the flat \
         `commissionRate` endpoint grammar"
    );
}

/// The declaration side of the same fact, kept because it names the field a future edit would
/// touch: binance opts OUT of the per-symbol slot, and that `None` is what selects the arm above.
/// Also pins that this work left binance's PERP slot alone.
#[test]
fn binance_declares_no_spot_commission_rate_endpoint() {
    let paths = vike_binance::recon_client::BINANCE_RECON.paths;
    assert!(
        paths.spot_commission_rate.is_none(),
        "wiring a spot commissionRate endpoint for binance would change its live fee resolution — \
         it already prices off the account body it fetches anyway"
    );
    assert_eq!(paths.perp_commission_rate, Some("/fapi/v1/commissionRate"));
    assert_eq!(paths.spot_account, "/api/v3/account");
}

/// **ASTER, the change under test.** Its spot fee lane must issue exactly one signed request, to
/// `/api/v3/commissionRate`, carrying the client's symbol — and parse it with the endpoint grammar.
///
/// Before this wiring the same call read `/api/v3/account` and returned `Ok(None)` against the real
/// venue, because aster's account body carries no `commissionRates` (measured; see
/// `crates/bridges/aster/src/recon_client.rs`'s `ASTER_RECON`). The 0.5/4 bps here is the point of
/// the change: a real number where there used to be silence.
#[test]
fn aster_spot_fee_lane_reads_the_per_symbol_commission_rate_endpoint() {
    let (t, calls) = Recorder::new();
    let client =
        vike_aster::recon_client::AsterReconClient::spot(NullSigner, t, "https://x", "BTCUSDT");

    let fetched = client.fetch_fee_rates().expect("aster spot fetch_fee_rates");

    assert_eq!(
        calls_of(&calls),
        vec![(
            "/api/v3/commissionRate".to_string(),
            vec![("symbol".to_string(), "BTCUSDT".to_string())]
        )],
        "aster's spot fee lane must ask the venue's own per-symbol fee endpoint, with the symbol"
    );
    assert_eq!(
        bps(fetched),
        (ENDPOINT_MAKER_BPS, ENDPOINT_TAKER_BPS),
        "aster must parse the `commissionRate` endpoint grammar, not the account-body object"
    );
}

/// Aster's PERP fee lane stays INERT: `perp_commission_rate: None` short-circuits before any
/// request is built. The offline twin of `tests/aster_reconcile_smoke.rs`'s
/// `aster_perp_reconcile_fetch_smoke`, which proves the same against the live venue but only runs
/// `--ignored` — so this is the copy that actually gates CI.
///
/// The EMPTY call list is the assertion that matters: "returned `None`" alone would also be true of
/// a lane that issued a request and then failed to parse the answer.
#[test]
fn aster_perp_fee_lane_issues_no_request_at_all() {
    let (t, calls) = Recorder::new();
    let client =
        vike_aster::recon_client::AsterReconClient::perp(NullSigner, t, "https://x", "BTCUSDT");

    let fetched = client.fetch_fee_rates().expect("aster perp fetch_fee_rates");

    assert_eq!(fetched, None, "aster's perp fee lane is deliberately unwired");
    assert!(
        calls_of(&calls).is_empty(),
        "an unwired perp fee lane must not touch the wire at all — never a probe of a live-money \
         account on a guess: {:?}",
        calls_of(&calls)
    );
}

/// The two venues' spot lanes must remain DISTINGUISHABLE. This is the regression neither
/// per-venue test above can catch on its own: collapsing the routing `match` to a single arm keeps
/// one of them green.
#[test]
fn the_two_spot_fee_lanes_do_not_collapse_into_one() {
    let (bt, bcalls) = Recorder::new();
    let (at, acalls) = Recorder::new();
    let b = vike_binance::recon_client::BinanceReconClient::spot(
        NullSigner,
        bt,
        "https://x",
        "BTCUSDT",
    );
    let a =
        vike_aster::recon_client::AsterReconClient::spot(NullSigner, at, "https://x", "BTCUSDT");

    let bfees = b.fetch_fee_rates().unwrap();
    let afees = a.fetch_fee_rates().unwrap();

    assert_ne!(
        calls_of(&bcalls),
        calls_of(&acalls),
        "binance and aster price spot fees off different endpoints; identical request lists mean \
         the shared routing lost a venue"
    );
    assert_ne!(bfees, afees, "the two lanes read different grammars off the same canned body");
}
