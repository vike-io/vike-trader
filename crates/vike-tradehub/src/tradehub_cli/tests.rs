//! The unit tests of `tradehub_cli`, one child per concern; `NullSink`, `StubFeed`/`StubCtors`, `policy_holding`,
//! `wired_1m_config` and `assert_wired_and_advertised` are shared by several.

use super::*;
use crate::hub_server::account_admin_source;
use crate::summary::summary_line;
use crate::venue_arming::deadman_absent_warning;
use vike_execution::CoreSnapshot;
use vike_secrets::account_credentials::CredentialRegistry;

#[cfg(test)]
mod accepted_argv;
#[cfg(test)]
mod admin_barrier;
#[cfg(test)]
mod alpaca_ctrader;
#[cfg(test)]
mod args;
#[cfg(test)]
mod awaited_build;
#[cfg(test)]
mod cex_arm;
#[cfg(test)]
mod deadman;
#[cfg(test)]
mod deribit;
#[cfg(test)]
mod drift_watch;
#[cfg(test)]
mod execution_badge;
#[cfg(test)]
mod feed_splice;
#[cfg(test)]
mod first_balance_wait;
#[cfg(test)]
mod forward_runs;
#[cfg(test)]
mod hub_wait;
#[cfg(test)]
mod ig;
#[cfg(test)]
mod inputs;
#[cfg(test)]
mod oanda;
#[cfg(test)]
mod past_sessions;
#[cfg(test)]
mod paths_and_alerts;
#[cfg(test)]
mod precedence;
#[cfg(test)]
mod ready_banner;
#[cfg(test)]
mod rhai_mount_at_start;
#[cfg(test)]
mod settings_and_reconcile;
#[cfg(test)]
mod stop_and_deadlines;
#[cfg(test)]
mod summary_line;
#[cfg(test)]
mod two_leg_rehearsal;

/// A `DataClientSink` that discards everything — enough to hand a feed constructor a sink when a test
/// asserts on the feed object and not on what it delivers.
struct NullSink;

impl DataClientSink for NullSink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: vike_marketdata::TickQuote) {}
    fn trade(&self, _v: &str, _s: &str, _t: vike_marketdata::TickTrade) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_marketdata::L2Book>) {}
}

/// A 1m `MountPlacement` for `venue` on the symbol its default account states (the fixture's,
/// `vike_secrets::fixture_account_symbol`; none on an account-wide venue, polymarket), at the venue's own
/// `tick_size` — the plan input every per-venue feed-arm child builds.
fn wired_1m_config(venue: &str, tick_size: f64) -> MountPlacement {
    let symbol = vike_secrets::fixture_account_symbol(venue).unwrap_or_default();
    MountPlacement::new(venue, symbol, 100_000.0, tick_size)
}

/// `config` through `venue_feed_plan`, over a box whose DEFAULT `config.venue` account is `demo` and states the venue's
/// fixture symbol: the dispatcher's default-symbol refusal comes first, then the arm's own plan.
fn planned_over_the_default_account(config: &MountPlacement) -> Result<StrategyLanePlan, String> {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        &config.venue,
        &vike_model::accounts::account_keys::AccountLabel::Default,
        vike_model::AccountTier::Demo,
    );
    venue_feed_plan(
        config,
        &crate::scripted_arming_tests::plan_table(&HashMap::new(), &policy),
        &policy,
    )
}

/// A feed double for the arms that hold a [`DataClient`]: every `subscribe_*` answers a fresh id, or —
/// with a `refusal` — fails with that text, as a strategy-lane dial does when the gatewayhub refuses the
/// stream the datahub asked it for (an absent credential, a handshake that failed). Records nothing;
/// the splice tests that need the calls have their own doubles.
#[derive(Default)]
struct StubFeed {
    refusal: Option<String>,
    next: u64,
}

impl StubFeed {
    fn answer(
        &mut self,
    ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
        match &self.refusal {
            Some(text) => Err(vike_data::DataClientError::Subscribe(text.clone())),
            None => {
                self.next += 1;
                Ok(vike_data::DataClientSubscriptionId(self.next))
            }
        }
    }
}

impl vike_data::DataClient for StubFeed {
    crate::testutil::lanes_answer!(self => self.answer(); bars, quotes, trades, book);
    fn unsubscribe(&mut self, _id: vike_data::DataClientSubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// A [`StrategyLaneFactories`] whose every client is a [`StubFeed`], and whose strategy-lane dial (the CEX arm's
/// concrete [`DataClientStrategyLane`](vike_datahub_client::strat::DataClientStrategyLane)) is refused: no test here has a datahub.
struct StubCtors {
    refusal: Option<String>,
}

impl StrategyLaneFactories for StubCtors {
    crate::testutil::scripted_strategy!();

    fn client(
        &self,
        _venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn DataClientSink>,
    ) -> Result<Box<dyn vike_data::DataClient + Send>, String> {
        Ok(Box::new(StubFeed { refusal: self.refusal.clone(), next: 0 }))
    }
}

/// A mount policy planting ONE active, unlabelled `venue` account row at `tier` (row id 1) that
/// holds `pairs` — the `account` snapshot the REAL mount path resolves that account's credentials
/// out of (`AccountDirectory::credentials_for`). Rows first, credentials after:
/// `AdapterPolicy::with_account` rebuilds the directory and would drop a book planted before it.
fn policy_holding(
    venue: &str,
    tier: vike_model::AccountTier,
    pairs: &[(&str, &str)],
) -> vike_tradehub_core::AdapterPolicy {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        venue,
        &vike_model::accounts::account_keys::AccountLabel::Default,
        tier,
    );
    let mut book = CredentialRegistry::new();
    for (field, value) in pairs {
        book.insert_account(1, *field, *value);
    }
    vike_tradehub_core::AdapterPolicy {
        accounts: policy.accounts.clone().with_credentials(book),
        ..policy
    }
}

/// `slug` is joined to the exec plane the way every live-wired slug is: an engine row in
/// `vike_tradehub_core::wired_markets` AND a `FEED_ARMS` row.
fn assert_wired_and_advertised(slug: &str) {
    assert!(
        vike_venues::venue_wiring::mount_of(slug).is_some(),
        "{slug} has a live feed arm but `assemble_runtime` builds no engine for it"
    );
    assert!(crate::feeds::has_feed_arm(slug), "{slug} has no FEED_ARMS row");
}
