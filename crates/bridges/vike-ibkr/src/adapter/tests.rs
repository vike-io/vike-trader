//! `IbkrVenueAdapter`'s unit tests, one file per role; the fixtures two or more of them share.

use super::*;
use vike_bridge_core::venue_adapter_fixture::{AdapterFixture, found_tier_events};
use vike_log::capture::captured;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{FillReport, OrderRequest, OrderStatusReport, PositionStatusReport};
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts};

/// The one field `load_ibkr_config_from` requires: the Gateway holds the login, so the account
/// number is the whole live gate.
const KEYS: &[(&str, &str)] = &[("ACCOUNT", "DU1234567")];

/// The DEMO tier's `venue.ibkr.demo.<field>` rows — the gateway, which the start reads from
/// `AdapterInputs::settings` (decision 0095, Task 7).
fn demo_gateway(rows: &[(&str, &str)]) -> vike_secrets::venue_setting::VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: Some(AccountTier::Demo),
            field: field.to_ascii_uppercase(),
            value: (*value).to_string(),
        })
        .collect();
    vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &rows)
}

/// A fixture whose started account, `label`, holds `KEYS`.
fn keyed(label: &AccountLabel) -> AdapterFixture {
    let mut fx = AdapterFixture::new(KEYS);
    fx.account = label.clone();
    fx
}

/// The seam's stand-in for the cpapi reconcile client: it answers nothing, and nothing asks it.
struct NoReports;

impl ReconClient for NoReports {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(vec![])
    }
}

/// The seam's stand-in for a connected Gateway session: it accepts every call and sends nothing.
struct Inert;

impl ExecutionClient for Inert {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The LIVE tier's `venue.ibkr.live.<field>` rows.
fn live_gateway(rows: &[(&str, &str)]) -> vike_secrets::venue_setting::VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: Some(AccountTier::Live),
            field: field.to_ascii_uppercase(),
            value: (*value).to_string(),
        })
        .collect();
    vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &rows)
}

/// One `account` row of `venue` at `tier` for `label` (`None` = the default account).
fn account_row(venue: &str, tier: &str, label: Option<&str>, active: bool) -> Account {
    Account {
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
    }
}

/// A fixture whose started account `who` holds an `ACCOUNT` (the value is [`SECRET`], so a leak can
/// fail a test) and whose `account` table answered with `rows`.
fn live_store(who: &AccountLabel, rows: Vec<Account>) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[("ACCOUNT", SECRET)]);
    fx.account = who.clone();
    fx.accounts = AccountDirectory::from_rows(Accounts::Known(rows), None);
    fx
}

fn named(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// Start `fx` at `tier` with the three network steps as panicking doubles — so the
/// start provably dialled nothing and built no reconcile client — and return the message of the ONE
/// `error!` a refused LIVE tier says.
fn refused_start_says(fx: &AdapterFixture, tier: AccountTier) -> String {
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        let mut req = fx.request(tier, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        start_with(
            req,
            |_, _| panic!("a refused live tier builds no reconcile client"),
            |_, _| panic!("(cpapi) a refused live tier builds no reconcile client"),
            |_, _| panic!("a refused live tier dials nothing"),
            |_, _| panic!("a refused live tier fetches no grid"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a refusal carries `found_tier`, never `tier`: {e:?}");
    assert!(
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
mod held_and_recon;
#[cfg(test)]
mod live_arm;
#[cfg(test)]
mod live_diag;
