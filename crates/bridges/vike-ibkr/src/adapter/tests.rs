//! `VenueAdapterIbkr`'s unit tests, one file per role; the fixtures two or more of them share.

use super::*;
use vike_bridge_core::venue_adapter_fixture::{
    AdapterFixture, ExecutionClientInert, found_tier_events,
};
use vike_execution::recon::ReconcileClientFake;
use vike_log::capture::captured;
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;
use vike_secrets::{AccountRow, AccountRows};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};

/// The one field `load_ibkr_config_from` requires (on the socket the gateway's login pair stands in
/// for it: `live_arm.rs`'s `login_only`).
const KEYS: &[(&str, &str)] = &[("ACCOUNT", "DU1234567")];

/// The DEMO tier's `venue.ibkr.demo.<field>` rows — the gateway's backend, which the start reads
/// from `AdapterInputs::settings` (decision 0095, Task 7) — beside the release's addresses.
fn demo_gateway(rows: &[(&str, &str)]) -> VenueSettings {
    vike_bridge_core::venue_adapter_fixture::venue_settings(VENUE, "demo", rows)
        .with_urls(VenueUrls::seed(VENUE))
}

/// No setting row, the release's addresses: what a fresh box hands the bridge.
fn seeded() -> VenueSettings {
    VenueSettings::default().with_urls(VenueUrls::seed(VENUE))
}

/// No setting row and no ibkr address row: every address lookup is refused, naming the venue.
fn no_addresses() -> VenueSettings {
    VenueSettings::default().with_urls(VenueUrls::from_rows(VENUE, std::iter::empty()))
}

/// The release's addresses with `ty`'s two replaced by `socket` (`host:port`) and `cpapi`: an
/// operator's `venue_url` rows, e.g. a gateway at a test port.
fn addresses(ty: UrlType, socket: &'static str, cpapi: &'static str) -> VenueUrls {
    let rows = vike_venues::venue_urls::VENUE_URL_SEED
        .iter()
        .filter(|s| s.venue == VENUE && s.ty != ty)
        .map(|s| (s.venue, s.ty, s.channel, s.protocol, s.product, s.url))
        .chain([
            (VENUE, ty, UrlChannel::Trade, UrlProtocol::Socket, "gateway", socket),
            (VENUE, ty, UrlChannel::Trade, UrlProtocol::Rest, "cpapi", cpapi),
        ]);
    VenueUrls::from_rows(VENUE, rows)
}

/// A fixture whose started account, `label`, holds `KEYS`, on the release's addresses.
fn keyed(label: &AccountLabel) -> AdapterFixture {
    let mut fx = AdapterFixture::new(KEYS);
    fx.account = label.clone();
    fx.settings = seeded();
    fx
}

/// What a connect double answers: `client`, its session trading the config's own `ACCOUNT` (a gateway
/// that manages it).
fn session(
    config: &IbkrConfig,
    client: impl ExecutionClient + Send + 'static,
) -> Result<(Box<dyn ExecutionClient + Send>, String), IbkrError> {
    Ok((Box::new(client), config.account.clone()))
}

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The LIVE tier's `venue.ibkr.live.<field>` rows, beside the release's addresses.
fn live_gateway(rows: &[(&str, &str)]) -> VenueSettings {
    vike_bridge_core::venue_adapter_fixture::venue_settings(VENUE, "live", rows)
        .with_urls(VenueUrls::seed(VENUE))
}

/// One `account` row of `venue` at `tier` for `label` (`None` = the default account).
fn account_row(venue: &str, tier: &str, label: Option<&str>, active: bool) -> AccountRow {
    AccountRow {
        id: 1,
        venue: venue.to_string(),
        tier: tier.parse().expect("a tier"),
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol(venue).map(str::to_string),
        facts: Default::default(),
    }
}

/// A fixture whose started account `who` holds an `ACCOUNT` (the value is [`SECRET`], so a leak can
/// fail a test) and whose `account` table answered with `rows`.
fn live_store(who: &AccountLabel, rows: Vec<AccountRow>) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[("ACCOUNT", SECRET)]);
    fx.account = who.clone();
    fx.accounts = AccountDirectory::from_rows(AccountRows::Known(rows), None);
    fx.settings = seeded();
    fx
}

fn named(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// Start `fx` at `tier` with the three network steps as panicking doubles — so the
/// start provably dialled nothing and built no reconcile client — and return the message of the ONE
/// `error!` a refused LIVE tier says.
fn refused_start_says(fx: &AdapterFixture, tier: AccountTier) -> String {
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        let mut req = fx.request(tier, "AAPL", &tx);
        req.reconcile_enabled = true;
        start_with(
            req,
            |_, _| panic!("a refused live tier builds no reconcile client"),
            |_, _| panic!("(cpapi) a refused live tier builds no reconcile client"),
            |_, _| panic!("a refused live tier dials nothing"),
            |_, _| panic!("a refused live tier fetches no grid"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a refusal carries `found_tier`, never `tier`: {e:?}");
    assert!(
        // secret-absent: the planted `ACCOUNT` value never reaches a log line
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
    e.message.clone()
}

fn says(message: &str, needle: &str) {
    assert!(message.contains(needle), "expected `{needle}` in: {message}");
}

#[cfg(test)]
mod demo;
#[cfg(test)]
mod held_and_reconcile;
#[cfg(test)]
mod live_arm;
#[cfg(test)]
mod live_diag;
