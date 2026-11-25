/// Decision 0095: a `live` tier is mainnet, anything below it testnet — no variable decides.
#[test]
fn the_tier_chooses_the_network() {
    use vike_model::AccountTier;
    use vike_model::VenueNetwork;
    assert_eq!(VenueNetwork::for_tier(AccountTier::Live), VenueNetwork::Mainnet);
    assert_eq!(VenueNetwork::for_tier(AccountTier::Demo), VenueNetwork::Testnet);
    assert_eq!(VenueNetwork::for_tier(AccountTier::Paper), VenueNetwork::Testnet, "never mainnet");
}

/// **THE SHARING GATE: one per-IP REST weight window per START, not one per transport.**
///
/// Hyperliquid meters every REST call it serves — keyless `/info` reads and signed `/exchange`
/// actions alike — out of a single per-IP budget, and
/// `vike_venues::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight` admits exactly what the
/// venue publishes, so there is no headroom for a second window to hide in. Meanwhile
/// `crate::transport::HyperliquidTransport::new` mints a FRESH
/// `vike_bridge_core::ratelimit::RateGate` on every call. Those two facts together are the
/// hazard this pins: a start that lets each consumer build its own transport runs N mutually
/// invisible full-budget windows, every one of them correctly reporting itself inside quota
/// while the process emits N times the venue's cap — answered with a 429 and then an IP ban,
/// from the box that also signs live orders. It was UNCONDITIONALLY two (the exec thread and
/// the funding poller) and three under `VIKE_RECONCILE=1`.
///
/// [`super::ip_gate_and_transport`] is where the start refuses that, and both of its halves are
/// load-bearing: the handle goes to
/// `crate::execution::client::spawn_with_market_slippage` (which clones it on
/// into the funding poller), and the transport is borrowed by the instruments load and then
/// owned by the `ReconcileClientHyperliquid`. A `RateGate` is an `Arc` inside, so "same window" is
/// observable exactly the way `vike_bridge_core::ratelimit`'s `clone_shares_the_same_window`
/// observes it — spend through one handle, watch the spend show up on the other. Driven through
/// the REAL constructor rather than a rebuild of it, so substituting a fresh `ip_weight_gate()`
/// on either side turns this red.
#[test]
fn an_hl_adapters_rest_paths_ride_one_ip_weight_window() {
    use crate::test_support::{seed_rest, seed_transport};
    use vike_model::VenueNetwork;

    // The budget is READ from the row that owns it, never restated here — this test cannot
    // drift from `vike_venues::venue_rate_limits` and does not become a second authority for it.
    let budget = vike_venues::venue_rate_limits::HYPERLIQUID.rest_ip_weight.admitted();

    // CONTROL — the defect, still reproducible on the raw constructor. Two transports built the
    // plain way share nothing, which is what makes the assertion below mean something rather
    // than merely pass.
    let a = seed_transport(VenueNetwork::Testnet);
    let b = seed_transport(VenueNetwork::Testnet);
    assert!(a.rate_gate().try_proceed_cost(budget), "a fresh window admits its whole budget");
    assert!(
        b.rate_gate().try_proceed(),
        "two plainly-constructed transports must be INDEPENDENT windows — if this ever fails, \
             the control is broken and the real assertion below proves nothing"
    );

    // THE ASSERTION — the start's own seam, both halves, one window.
    let (execution_gate, transport) =
        super::ip_gate_and_transport(VenueNetwork::Testnet, &seed_rest(VenueNetwork::Testnet));
    assert!(execution_gate.try_proceed_cost(budget), "the mount's window admits its whole budget");
    assert!(
        !transport.rate_gate().try_proceed(),
        "the exec half spent the whole per-IP budget, yet the instruments/recon transport \
             still admitted a call: the mount is running two full-budget windows against a venue \
             cap it declares with zero margin, each one reporting itself inside quota"
    );
}

/// **The leverage confirmation's reads spend the ADAPTER's per-IP budget, not a fresh one**
/// (v0.1.71 review, finding 7): `confirmed_leverage_cap` used `HyperliquidTransport::new`, a whole new
/// window per ask. [`super::adapter_window_transport_in`] rides the newest window a start opened, and
/// only a network with no start yet opens one (the way a start does). Own book per test, so no other
/// test's start moves the window under it.
#[test]
fn the_leverage_reads_ride_the_newest_adapter_window() {
    use super::WindowBook;
    use vike_model::VenueNetwork;

    let budget = vike_venues::venue_rate_limits::HYPERLIQUID.rest_ip_weight.admitted();
    let book = WindowBook { newest: std::sync::Mutex::default() };
    let (test, main) = (
        crate::test_support::seed_rest(VenueNetwork::Testnet),
        crate::test_support::seed_rest(VenueNetwork::Mainnet),
    );

    // no start yet: the read opens a window the way a start does, and a second read rides that one
    let first = super::adapter_window_transport_in(&book, VenueNetwork::Testnet, &test);
    assert!(first.rate_gate().try_proceed_cost(budget));
    let second = super::adapter_window_transport_in(&book, VenueNetwork::Testnet, &test);
    assert!(!second.rate_gate().try_proceed(), "two reads, one window");

    // a start opens a newer window: the next read charges THAT one
    let (execution_gate, _transport) =
        super::ip_gate_and_transport_in(&book, VenueNetwork::Testnet, &test);
    let read = super::adapter_window_transport_in(&book, VenueNetwork::Testnet, &test);
    assert!(execution_gate.try_proceed_cost(budget), "the start's window is full");
    assert!(!read.rate_gate().try_proceed(), "the read spent the adapter's budget, not its own");

    // another network has its own window
    let other = super::adapter_window_transport_in(&book, VenueNetwork::Mainnet, &main);
    assert!(other.rate_gate().try_proceed(), "mainnet is a different window");
}

/// **THE DERIVATION LINK of the wiring gate** — venue `meta` → [`super::margin_mode`], with no
/// network. The gate has THREE links, each proven on its own:
///
/// 1. **Derivation** — this test.
/// 2. **Construction** — the value leaving the one instruments load as a `LiveRuntime` field and then
///    as `VenueAdapterExecutionRunning::margin_mode`: `adapter_contract_tests.rs`'s
///    `the_engine_symbols_ruling_margin_mode_reaches_the_outcome`, over a `Symbology::from_meta`
///    universe.
/// 3. **Fold** — `VenueAdapterExecutionRunning::margin_mode` → `vike-tradehub-core`'s `margin_mode_grid` → `Account` →
///    `apply_fill`, which cannot live here (`margin_mode_grid` is `vike-tradehub-core`-only, and the layer
///    rule runs the other way): `crates/vike-gatewayhub/src/trade/adapters/contract_tests/outcome_fold.rs`'s
///    `a_live_outcome_folds_its_grid_legs_multiplier_and_margin_mode_into_the_engine` over a planted
///    row, and `crates/vike-gatewayhub/tests/trade_roster/mount_roster/fee_schedule.rs`'s
///    `an_isolated_only_asset_folds_isolated_through_the_real_start_chain` against this function.
///
/// ⚠ Nothing drives the three END TO END. The step that joins 1 to 2 inside a real start —
/// `live_runtime_for_account` handing its own freshly fetched `instruments` to `RuntimeFacts::of` — runs
/// only after a live `meta` fetch, so no offline test reaches it.
#[test]
fn an_isolated_only_asset_resolves_isolated_and_an_ordinary_one_stays_cross() {
    use vike_model::MarginMode;

    // Real 2026-08-05 row shapes: CASHCAT is the one isolated-only asset still live.
    let meta = serde_json::json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
    ]});
    let symbology =
        crate::instrument::symbology::Symbology::from_meta(&meta, &serde_json::json!({}));

    assert_eq!(
        super::margin_mode(&symbology, "CASHCATUSDC.P"),
        MarginMode::Isolated,
        "isolated-only asset: the venue's per-ASSET truth must reach the fold"
    );
    assert_eq!(
        super::margin_mode(&symbology, "BTCUSDC.P"),
        MarginMode::Cross,
        "ordinary asset in the SAME universe: unchanged, the per-venue default"
    );
    for unlisted in ["NOT-LISTED", "CASHCAT"] {
        assert_eq!(
            super::margin_mode(&symbology, unlisted),
            vike_venues::capabilities_for(crate::transport::consts::VENUE).default_margin_mode,
            "{unlisted}: a name no row answers (the coin is no user name) falls back to the per-venue              declaration, not a literal"
        );
    }
}

/// **A `demo` account whose rows hold only the LIVE addresses does not run, and says which row**: the
/// testnet's `trade` row is asked at the account's own type and never falls back to mainnet, and the
/// refusal comes before any signer or socket.
#[test]
fn a_demo_account_with_no_testnet_row_does_not_run_and_names_the_row() {
    use std::collections::HashMap;
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
    use vike_venues::venue_urls::{UrlType, VenueUrls};

    let rows = VenueUrls::seed_typed(crate::transport::consts::VENUE, UrlType::Live);
    let fields = CredentialFields::from_pairs([(crate::config::PRIVATE_KEY, "0xabc123")]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let (events_tx, _events_rx) = vike_execution::event_channel(8);
    let (attempt, events) = vike_log::capture::captured(|| {
        super::live_runtime_for_account(
            "BTCUSDC.P",
            &[],
            AccountTier::Demo,
            &creds,
            &rows,
            &HashMap::new(),
            super::AdapterFlags::default(),
            &AccountLabel::Default,
            &events_tx,
            None,
            None,
            std::path::PathBuf::new(),
        )
    });
    assert!(attempt.live.is_none() && attempt.master.is_none(), "nothing was asked of any host");
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR
            && e.message.contains("no address row for demo trade rest")),
        "the refusal names the row: {events:?}"
    );
}

/// **B-18: a `live` account with no key is said once, at `warn!`, and a `demo` one stays silent**: the
/// one-key-set arms' shared line (`vike_bridge_core::venue_adapter::report_live_credentials_absent`), with
/// the venue and the account named, and nothing asked of any host.
#[test]
fn a_live_account_with_no_key_is_said_once_and_a_demo_one_is_silent() {
    use std::collections::HashMap;
    use vike_bridge_core::venue_adapter::LIVE_CREDENTIALS_ABSENT;
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_secrets::account_credentials::AccountCredentials;

    let creds = AccountCredentials::new(Some(1), None, None);
    let (events_tx, _events_rx) = vike_execution::event_channel(8);
    for (tier, said) in [(AccountTier::Live, true), (AccountTier::Demo, false)] {
        let (attempt, events) = vike_log::capture::captured(|| {
            super::live_runtime_for_account(
                "BTCUSDC.P",
                &[],
                tier,
                &creds,
                &vike_venues::venue_urls::VenueUrls::seed(crate::transport::consts::VENUE),
                &HashMap::new(),
                super::AdapterFlags::default(),
                &AccountLabel::Default,
                &events_tx,
                None,
                None,
                std::path::PathBuf::new(),
            )
        });
        assert!(attempt.live.is_none() && attempt.master.is_none(), "{tier:?}: nothing was asked");
        let lines: Vec<_> =
            events.iter().filter(|e| e.message.contains(LIVE_CREDENTIALS_ABSENT)).collect();
        assert_eq!(lines.len(), usize::from(said), "{tier:?}: {events:?}");
        if let [line] = lines.as_slice() {
            assert_eq!(line.level, tracing::Level::WARN);
            assert_eq!(line.field("venue"), Some(crate::transport::consts::VENUE));
            assert!(line.field("account").is_some(), "the account is named: {line:?}");
        }
    }
}

/// **A key with a bad character declines the start into paper, and the log it writes names no character
/// of the key.** `Signer::from_private_key`'s `hex::decode` error names the offending character and its
/// position, and the start's line used to print that error. The key is made of characters the
/// line's own words and fields never use (digits and capitals the `DEFAULT` account does not spell), so
/// ANY of its characters in an event is a leak.
#[test]
fn a_malformed_key_declines_the_start_and_the_log_names_no_character_of_it() {
    use std::collections::HashMap;
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};

    let key = "0123456789QZXWJV".repeat(4);
    // The control: the error this used to print DOES carry a character of the key.
    assert!(hex::decode(&key).unwrap_err().to_string().contains('Q'), "the control leaks");

    let fields = CredentialFields::from_pairs([(crate::config::PRIVATE_KEY, key.as_str())]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let (events_tx, _events_rx) = vike_execution::event_channel(8);
    let (attempt, events) = vike_log::capture::captured(|| {
        super::live_runtime_for_account(
            "BTCUSDC.P",
            &[],
            AccountTier::Demo,
            &creds,
            &vike_venues::venue_urls::VenueUrls::seed(crate::transport::consts::VENUE),
            &HashMap::new(),
            super::AdapterFlags::default(),
            &AccountLabel::Default,
            &events_tx,
            None,
            None,
            std::path::PathBuf::new(),
        )
    });
    assert!(attempt.live.is_none() && attempt.master.is_none(), "a malformed key arms nothing");
    let line = events
        .iter()
        .find(|e| e.message.contains("invalid private key"))
        .unwrap_or_else(|| panic!("the refusal is logged: {events:?}"));
    // A key that is present but unusable is an `error!` naming venue and account (A-D3), as every
    // other refusal of this start is.
    assert_eq!(line.level, tracing::Level::ERROR);
    assert_eq!(line.field("venue"), Some(crate::transport::consts::VENUE));
    assert_eq!(line.field("account"), Some(AccountLabel::Default.to_string().as_str()));
    for event in &events {
        let said =
            format!("{} {}", event.message, event.fields.values().cloned().collect::<String>());
        assert!(
            key.chars().all(|c| !said.contains(c)),
            "a character of the key reached the log: {said}"
        );
    }
}

/// **THE WITHDRAW GATE IN THE START (B3.20)**: a LIVE account that signs with the account's OWN key (the venue's
/// `userRole` answers `user`) does not run — refused at `error!` in the gate's words, before the instruments load —
/// while the identity handshake is still handed back for recording. Offline, through the real start: its mainnet
/// REST row is a loopback venue that answers ONE request, the `userRole` read; a start that went on to load the
/// instruments would fail that second request and say so in `failed`.
#[test]
fn a_live_start_on_the_accounts_own_key_is_refused_before_the_instruments_load() {
    use std::collections::HashMap;
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VENUE_URL_SEED, VenueUrls};

    let (venue, asked) =
        vike_log::loopback::serve_replies("", vec![(200, r#"{"role":"user"}"#.to_string())]);
    let rows =
        VENUE_URL_SEED.iter().filter(|s| s.venue == crate::transport::consts::VENUE).map(|s| {
            let live_rest = (s.ty, s.channel, s.protocol)
                == (UrlType::Live, UrlChannel::All, UrlProtocol::Rest);
            (
                s.venue,
                s.ty,
                s.channel,
                s.protocol,
                s.product,
                if live_rest { venue.as_str() } else { s.url },
            )
        });
    let urls = VenueUrls::from_rows(crate::transport::consts::VENUE, rows);
    let key = format!("0x{}", "11".repeat(32));
    let fields = CredentialFields::from_pairs([(crate::config::PRIVATE_KEY, key.as_str())]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let (events_tx, _events_rx) = vike_execution::event_channel(8);
    let (attempt, events) = vike_log::capture::captured(|| {
        super::live_runtime_for_account(
            "BTCUSDC.P",
            &[],
            AccountTier::Live,
            &creds,
            &urls,
            &HashMap::new(),
            super::AdapterFlags::default(),
            &AccountLabel::Default,
            &events_tx,
            None,
            None,
            std::path::PathBuf::new(),
        )
    });
    assert!(attempt.live.is_none(), "the account's own key does not run live");
    assert!(attempt.failed.is_none(), "refused before the instruments load: no second request");
    assert!(attempt.master.is_some(), "the identity handshake is still handed back");
    assert_eq!(asked.lock().expect("log").len(), 1, "one userRole read answers both questions");
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR
            && e.message
                .contains("REFUSING to arm hyperliquid LIVE: the account's own key can WITHDRAW")),
        "the refusal is said in the gate's words: {events:?}"
    );
}

/// The builder fee is a whole number of tenths of a basis point: unset or blank is 0, a legal value is itself, and
/// an unreadable one is 0 and warned (where it used to be 0 in silence) — the warning names the row, never the value.
#[test]
fn the_builder_fee_reads_typed_and_an_unreadable_one_is_warned() {
    use std::collections::HashMap;
    let name = concat!("HYPERLIQUID", "_BUILDER_FEE_TENTHS_BP");
    let fee = |value: Option<&str>| {
        let vars: HashMap<String, String> =
            value.map(|v| (name.to_string(), v.to_string())).into_iter().collect();
        vike_log::capture::captured(|| super::builder_fee_tenths_bp(&vars))
    };
    for (value, want) in [(None, 0), (Some(""), 0), (Some(" 25 "), 25), (Some("0"), 0)] {
        let (got, events) = fee(value);
        assert_eq!(got, want, "{value:?}");
        assert!(events.iter().all(|e| e.level != tracing::Level::WARN), "{value:?}: {events:?}");
    }
    let (got, events) = fee(Some("2.5bp"));
    assert_eq!(got, 0, "an unreadable fee is attribution only");
    let warned: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::WARN).collect();
    assert_eq!(warned.len(), 1, "{events:?}");
    let said = format!(
        "the builder fee ({name}) is not a whole number of tenths of a basis point, so it is 0 \
         (attribution only)"
    );
    assert_eq!(warned[0].message, said, "the value `2.5bp` is never echoed");
}
