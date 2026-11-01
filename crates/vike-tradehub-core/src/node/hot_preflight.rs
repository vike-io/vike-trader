//! **The startup preflight, scoped to ONE venue** — what a hot start of an account on a venue the boot
//! never preflighted runs (decision 0169, slice S3).
//!
//! The boot runs [`crate::startup::run_startup_preflight`] once, over every venue with an account armed
//! at a venue-client tier. An account that becomes active later may sit on a venue that run never
//! looked at (no account of it was armed then); the hot start crosses the same gate, for that venue and
//! no other. Nothing about the venues that already run is asked again: the table the run judges is the
//! venue's own rows, and the backend it asks ([`OneVenue`]) lists no credential read for any other venue,
//! so a hot start never reads another account's balance or clock.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use vike_config::VenueArming;
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_model::AccountTier;

use crate::preflight::{CredentialGap, ServerTimeGap};
use crate::{
    AdapterBind, AdapterStartQuery, ArmingTable, BackendError, IdentityRecord, NetReading,
    PreflightPlan, PreflightQuery, VenueBackend, VenueFacts,
};

/// `table` reduced to the rows and the facts of `venue`: what a single-venue preflight judges.
pub(super) fn one_venue_table(table: &ArmingTable, venue: &str) -> ArmingTable {
    let facts: BTreeMap<String, VenueFacts> =
        table.facts_of(venue).map(|f| (venue.to_string(), f.clone())).into_iter().collect();
    let rows = table.rows().iter().filter(|row| row.arming.venue == venue).cloned().collect();
    ArmingTable::new(facts, rows)
}

/// A [`VenueBackend`] that answers the preflight's questions for ONE venue and forwards everything else:
/// its plan lists the credential read and the clock policy of `venue` alone.
pub(super) struct OneVenue {
    /// The backend the hot start runs over.
    pub(super) inner: Arc<dyn VenueBackend>,
    /// The only venue the preflight may ask about.
    pub(super) venue: String,
}

impl VenueBackend for OneVenue {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        self.inner.arming_table().map(|table| one_venue_table(&table, &self.venue))
    }

    fn preflight_plan(&self, q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
        let mut plan = self.inner.preflight_plan(q)?;
        plan.credential_venues.retain(|v| *v == self.venue);
        plan.clock_policies.retain(|v, _| *v == self.venue);
        Ok(plan)
    }

    fn server_time_ms(&self, venue: &str, tier: AccountTier) -> Result<i64, ServerTimeGap> {
        self.inner.server_time_ms(venue, tier)
    }

    fn authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        self.inner.authed_read(venue)
    }

    fn net_reading(&self) -> NetReading {
        self.inner.net_reading()
    }

    fn report_not_running(&self, venue: &str, rows: &[VenueArming]) {
        self.inner.report_not_running(venue, rows);
    }

    fn start_adapter(
        &self,
        q: AdapterStartQuery<'_>,
        events: &EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        self.inner.start_adapter(q, events, trigger)
    }

    fn record_identity(
        &self,
        q: &IdentityRecord<'_>,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), BackendError> {
        self.inner.record_identity(q, recon)
    }

    /// FORWARDED: the trait default is a no-op, and the lease a refused hot start opened is the inner
    /// backend's to forget.
    fn release_refused_start(
        &self,
        venue: &str,
        account: &vike_model::accounts::account_keys::AccountLabel,
        route_key: &str,
    ) {
        self.inner.release_refused_start(venue, account, route_key);
    }
}
