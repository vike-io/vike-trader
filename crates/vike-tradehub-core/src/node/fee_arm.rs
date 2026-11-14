//! **The one fee rate per account reaches the mounts built on it.** The assembly builds every engine
//! first, and [`crate::resolve_fee_schedule`] reads each account's reported rate there (the reconcile
//! client's `fetch_fee_rates`, one bounded read at start; a failed read keeps the static lane and
//! warns). Only then, before the core is spawned, does a [`MountFeeRebuild`] rebuild a mount whose bar is the
//! rate — the A-S maker's break-even floor, the xEMM round trip — from its own engine's schedule
//! ([`EngineFees`]). Nothing here does I/O: the rate was read with the engine, so the node's startup
//! order is the one it was. A paper account has no venue account to report a rate: its engine holds
//! the static lane, and so does every mount on it.

use vike_model::FeeSchedule;
use vike_model::accounts::account_keys::AccountLabel;

/// Every engine of one assembly with the fee schedule it resolved, by route key.
pub struct EngineFees(Vec<(String, Option<FeeSchedule>)>);

impl EngineFees {
    /// The engines' resolved schedules, read off [`vike_execution::Engine::fee_schedule`].
    pub(super) fn of_engines<'a, C: vike_execution::ExecutionClient + 'a>(
        engines: impl Iterator<Item = &'a vike_execution::Engine<C>>,
    ) -> Self {
        EngineFees(engines.map(|e| (e.route_key.clone(), e.fee_schedule)).collect())
    }

    /// The fee schedule the engine of `account` on `venue` resolved (an absent account is the
    /// venue's default one): the account's reported rate when its venue reported one, else the static
    /// lane. `None` when no engine of that account runs here.
    #[must_use]
    pub fn of(&self, venue: &str, account: Option<&AccountLabel>) -> Option<FeeSchedule> {
        let key = vike_model::accounts::account_keys::route_key_of(
            venue,
            account.unwrap_or(&AccountLabel::Default),
        );
        self.0.iter().find(|(k, _)| *k == key).and_then(|(_, fee)| *fee)
    }
}

/// What rebuilds a strategy mount from [`EngineFees`]: called once, after every engine is built and
/// before the core is spawned, with the core's config to change in place. It may replace a mount's
/// STRATEGY; the mounts themselves (venue, symbol, account) were already judged against the engines.
pub type MountFeeRebuild<'a> = Box<dyn FnOnce(&EngineFees, &mut vike_core::CoreConfig) + 'a>;
