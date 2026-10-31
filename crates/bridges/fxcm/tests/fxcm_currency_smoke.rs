//! Live demo CURRENCY smoke (decision 0183): what FXCM's own session says the account's money is, and what
//! its Offers table makes one unit of a foreign quote currency worth in it. READ-ONLY: it places nothing.
//!
//! ```text
//! export FCSDK_DIR=<repo>/vendor/fcsdk/linux        # the SDK, so the shim (incl. fc_system_properties,
//!                                                   # fc_offers) is built in; or run against an installed
//!                                                   # shim from this release on the loader's search path
//! cargo test -p vike-fxcm --features fxcm --test fxcm_currency_smoke -- --ignored --nocapture
//! ```
//!
//! ⚠ It opens its OWN reconcile session (a second login beside the daemon's): FXCM sessions are scarce, so
//! run it when that is acceptable (the daemon's FXCM account stopped, or a spare demo login), not casually.
//! Triple-gated like the other `*_smoke.rs`: `#[ignore]`, self-skips with no DEMO fxcm account in the credential
//! store (the active default row, else the one inactive demo row: `tests/common/mod.rs`'s `demo_config`), and
//! self-skips on a box whose shim did not load. It reddens when the shim loaded and the
//! demo login failed, or when the installed shim predates the two reads (the message names the remedy).
//!
//! What it PRINTS is the evidence the runbook compares (`RUNBOOK-fxcm-currency.md`, step 1): the account
//! currency and whether the venue STATED it or it is the ruling's assumed USD, then for a yen pair, a cross
//! and the mounted pair the quote currency, the rate (ACCOUNT units per ONE quote unit) and the lot. The
//! assertions check only what holds whatever the demo account is: a rate is finite and positive and its
//! orientation agrees with the pair's own mid (`account_per_quote == mid` or `1 / mid`).

use vike_fxcm::home_conversion::CcySource;
use vike_fxcm::recon_client_for;

mod common;

/// The registry label this smoke registers its session under: not an account label an operator can write
/// (labels are `[A-Z0-9]+`), so it can never collide with the daemon's in a shared process.
const ACCOUNT: &str = "currency-smoke";

#[test]
#[ignore = "live demo; needs the ForexConnect SDK + a demo fxcm account's credentials — read-only, run manually (see module doc)"]
fn fxcm_account_currency_and_conversion_rates() {
    vike_log::test_init();
    let Some(c) = common::demo_config() else {
        eprintln!("skip: no demo fxcm account credentials");
        return;
    };
    let Some(_client) = recon_client_for(&c, "EURUSD", ACCOUNT) else {
        assert!(
            !vike_fxcm::sdk_available(),
            "FXCM credentials are configured and the ForexConnect shim IS loaded, yet the reconcile \
             client could not be built — the demo login FAILED (re-run with --nocapture: the session \
             thread logs the SDK's own words at `error`)"
        );
        eprintln!("skip: no ForexConnect shim on this box");
        return;
    };

    // 1. The account's currency, and how it is known.
    let told = vike_fxcm::reads::told_account_currency(ACCOUNT).expect("a usable account currency");
    match &told.source {
        CcySource::Venue => {
            println!("ACCOUNT CURRENCY {} (STATED by the venue: BASE_CRNCY)", told.code)
        }
        CcySource::Assumed { why } => println!(
            "ACCOUNT CURRENCY {} (ASSUMED, the venue did not state it: {why}) — compare with the \
             account statement before trusting any rate below",
            told.code
        ),
    }
    assert_eq!(told.code.len(), 3, "a currency code: {}", told.code);

    // 2. The conversion rates, through the registry exactly as the gatewayhub's lane asks.
    for symbol in ["EURUSD", "USDJPY", "EURGBP"] {
        match vike_fxcm::reads::quoted_facts(ACCOUNT, symbol) {
            Ok(facts) => {
                println!(
                    "{symbol}: tick {} lot {} quote {}",
                    facts.properties.tick_size,
                    facts.properties.step_size,
                    facts.quote.as_ref().map_or_else(
                        || format!("in the account's own currency ({})", told.code),
                        |q| format!(
                            "{} at {} {} per 1 {} (the quote's own moment: {} ms)",
                            q.quote_ccy,
                            q.account_per_quote.get(),
                            told.code,
                            q.quote_ccy,
                            q.as_of_ms
                        )
                    )
                );
                assert!(facts.properties.tick_size > 0.0 && facts.properties.step_size > 0.0);
                if let Some(q) = facts.quote {
                    assert!(
                        q.account_per_quote.get().is_finite() && q.account_per_quote.get() > 0.0
                    );
                }
            }
            // A refusal is data too (a closed market on a weekend, a pair this login does not
            // subscribe): printed, not failed.
            Err(why) => println!("{symbol}: REFUSED — {why}"),
        }
    }

    // 3. The orientation, checked against the table itself rather than against my arithmetic: the rate of
    // one JPY in a USD account is `1 / USD/JPY mid`, never the mid. Only meaningful for a USD account.
    let reads = vike_fxcm::reads::account_reads(ACCOUNT).expect("registered");
    let table = reads.offers().expect("an Offers table");
    println!("Offers rows read: {}", table.len());
    // The age of each quote by its OWN time (`quote_age_ms`, a DIFFERENCE of the row's `getTime()` and the
    // session's `getServerTime()`): the first measurement of that frame. Seconds on an open market is the
    // frame shared; hours, or a negative figure, is it not (the lane then refuses every rate, closed).
    for pair in ["EUR/USD", "USD/JPY", "GBP/USD"] {
        let age = table.iter().find(|o| o.instrument == pair).map(|o| o.quote_age_ms);
        println!("{pair}: quote_age_ms = {age:?}");
    }
    if told.code == "USD"
        && let (Some(usdjpy), Ok(facts)) = (
            table.iter().find(|o| o.instrument == "USD/JPY"),
            vike_fxcm::reads::quoted_facts(ACCOUNT, "USDJPY"),
        )
        && let Some(q) = facts.quote
    {
        let mid = (usdjpy.bid + usdjpy.ask) / 2.0;
        assert!(
            (q.account_per_quote.get() - 1.0 / mid).abs() < 1e-9,
            "JPY in USD must be 1 / {mid}, got {}",
            q.account_per_quote.get()
        );
    }
}
