//! The gatewayhub's refusals, declared once (`vike_utility::refusal_enum!`) in the crate the daemon and every reader
//! of its wire link:
//! - [`GatewayhubRefusal`], the ones a CLIENT decides on, sent as the refusal frame
//!   (`vike_hub_protocol::refusal::HubRefusal`, [`crate::proto::Response::Refused`]) to a client that said it reads one;
//! - [`GatewayhubTradeRefusal`], the daemon's own answers to a trade lease's open and its adapter start, which the
//!   tradehub SHOWS as the account's NOT RUNNING reason. Their frames (`Response::TradeRefused`, `Response::Error`,
//!   `AdapterReply::NotRunning`) have no id slot, so the text they carry is the refusal's `rendered()`
//!   (`error [<id>]: <text>`): every reader, an older tradehub too, shows the id with no change. Code decides on the
//!   frame's `block_word` (`vike_model::ArmingBlock`), never on this text.

use std::io;

use vike_hub_protocol::refusal::HubRefusal;
use vike_market_stream::opener::StreamOpenCapped;
use vike_model::AccountTier;

vike_utility::refusal_enum! {
    /// A gatewayhub refusal a client decides on.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum GatewayhubRefusal with ids GatewayhubRefusalId {
        /// `DataOpen` met the daemon's cap of stream sessions (`crates/vike-gatewayhub/src/server/stream.rs`): a
        /// CAP, which only an operator mends. A datahub's broker and a tradehub read it as such
        /// (`vike_data::DataClientError::Cap` on every subscribe of that session) and never restart on it.
        StreamSessionsFull { cap: usize } => "gatewayhub.stream.sessions_full":
            "gatewayhub: already serving {cap} stream sessions (the daemon's cap): raise the cap or close a \
             session",
    }
}

vike_utility::refusal_enum! {
    /// The daemon's own refusal of a trade lease's open or of its adapter start (the module doc). A `subject` names
    /// the venue, and the account's label when it has one. A bridge's own words (a start the lane refused) are not
    /// here: they cross byte for byte.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum GatewayhubTradeRefusal with ids GatewayhubTradeRefusalId {
        /// The daemon is stopping: the open is answered with an error, and the client asks again.
        Stopping => "gatewayhub.trade.stopping": "gatewayhub: the daemon is stopping",
        /// This build carries no trade adapter for the venue (`served` is the `trade_venue=` list it advertises).
        NoAdapter { venue: String, served: String } => "gatewayhub.trade.no_adapter":
            "gatewayhub: no trade adapter for `{venue}` in this build (trade_venue= advertises: [{served}])",
        /// The open names an account label that does not parse.
        BadAccountLabel { error: String } => "gatewayhub.trade.bad_account_label":
            "gatewayhub: a bad account label: {error}",
        /// A paper account runs the simulator in the tradehub and never opens a lease.
        PaperTier => "gatewayhub.trade.paper_tier":
            "gatewayhub: a paper account runs the simulator and opens no trade lease",
        /// The account table could not be read (a locked or unopenable store): the account was not judged, and
        /// the client asks again.
        AccountTableUnread { subject: String } => "gatewayhub.trade.account_table_unread":
            "gatewayhub: the account table could not be read just now, so {subject} was not judged; open again \
             when the settings database reads",
        /// The account table says the account does not run; `block` is its `ArmingBlock::as_str`.
        DoesNotRun { subject: String, block: &'static str } => "gatewayhub.trade.does_not_run":
            "gatewayhub: {subject} does not run: {block}",
        /// The account table's tier is not the tier the client opened the lease at.
        TierMismatch { subject: String, tier: AccountTier, opened: AccountTier }
            => "gatewayhub.trade.tier_mismatch":
               "gatewayhub: the account table says {subject} runs at {tier:?} and the client opened it at \
                {opened:?}; refused",
        /// The venue session the daemon supervises is down and being re-established: the client asks again.
        VenueSessionDown { venue: String, why: String } => "gatewayhub.trade.venue_session_down":
            "gatewayhub: the {venue} venue session is down ({why}); the daemon is re-establishing it, open again \
             when it is back",
        /// The account's running adapter runs at another tier than the open asks.
        AdapterAtOtherTier { subject: String, running: AccountTier, opened: AccountTier }
            => "gatewayhub.trade.adapter_at_other_tier":
               "gatewayhub: the adapter for {subject} runs at {running:?}; it cannot be opened at {opened:?} until \
                its holder detaches",
        /// The daemon's cap of trade leases is met.
        LeasesFull { cap: usize } => "gatewayhub.trade.leases_full":
            "gatewayhub: already serving {cap} trade leases (the daemon's cap)",
        /// A live account needs the one-live-process lock, which lives in the project's state directory.
        LiveLockWithoutStateDirectory => "gatewayhub.trade.live_lock_without_state_directory":
            "gatewayhub: a live account needs the live-account lock and no project state directory is resolved; \
             refused",
        /// The mount or a declared leg is written another way than the venue's one spelling (`why` is
        /// `vike_catalog::respelled_symbol`'s words).
        SymbolRespelled { why: String } => "gatewayhub.trade.symbol_respelled": "gatewayhub: {why}",
        /// The venue session does not answer yet (`why`, the lane's words): the start is PARKED and the daemon
        /// starts it the moment the venue answers.
        StartParked { why: String, venue: String } => "gatewayhub.trade.start_parked":
            "{why}: the gatewayhub starts the {venue} session by itself the moment it answers, with no restart, and \
             sends nothing to the venue before",
    }
}

impl From<GatewayhubRefusal> for HubRefusal {
    fn from(refusal: GatewayhubRefusal) -> Self {
        HubRefusal::new(refusal.id().word(), &refusal)
    }
}

/// The [`crate::proto::Response::Error`] text a client that does not read the frame is answered for `refusal`: a
/// cap carries the words an older engine classes a cap by (`vike_market_stream::legacy`), every other refusal its
/// text.
#[must_use]
pub fn older_text(refusal: HubRefusal) -> String {
    match GatewayhubRefusalId::from_word(&refusal.id) {
        Some(GatewayhubRefusalId::StreamSessionsFull) => {
            vike_market_stream::legacy::older_cap_text(&refusal.text)
        }
        None => refusal.text,
    }
}

/// The error a refused `DataOpen` is, decided by the frame's id: the session cap is a [`StreamOpenCapped`] (the
/// stream engine then answers every subscribe with a cap), any other refusal its text. Either reads
/// `error [<id>]: <text>` (`vike_utility::errors::render_refusal`): the id is shown, never decided on.
#[must_use]
pub fn open_refused(refusal: HubRefusal) -> io::Error {
    let shown = vike_utility::errors::render_refusal(&refusal.id, &refusal.text);
    match GatewayhubRefusalId::from_word(&refusal.id) {
        Some(GatewayhubRefusalId::StreamSessionsFull) => io::Error::other(StreamOpenCapped(shown)),
        None => io::Error::other(shown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The session cap is a cap by its id**: an open refused by it is a `StreamOpenCapped`, an older client is
    /// answered words its engine classes as a cap, and a frame whose id this build does not know is its text alone.
    #[test]
    fn the_session_cap_is_a_cap_by_its_id_for_every_reader() {
        let frame = HubRefusal::from(GatewayhubRefusal::StreamSessionsFull { cap: 3 });
        assert_eq!(frame.id, "gatewayhub.stream.sessions_full");
        let open = open_refused(frame.clone());
        assert!(open.get_ref().is_some_and(|e| e.is::<StreamOpenCapped>()), "{open}");
        assert_eq!(
            open.to_string(),
            format!("error [gatewayhub.stream.sessions_full]: {}", frame.text)
        );
        assert!(older_text(frame.clone()).starts_with(&frame.text));
        assert_ne!(older_text(frame.clone()), frame.text, "an older reader is told it is a cap");
        let unknown = HubRefusal::new("gatewayhub.newer", "words");
        assert!(
            open_refused(unknown.clone()).get_ref().is_some_and(|e| !e.is::<StreamOpenCapped>())
        );
        assert_eq!(older_text(unknown), "words");
    }

    /// **A trade refusal is shown under its id, its words unchanged after it**: what the daemon puts in a frame with
    /// no id slot is the rendered text.
    #[test]
    fn a_trade_refusal_renders_its_id_before_its_unchanged_words() {
        let refusal = GatewayhubTradeRefusal::TierMismatch {
            subject: "bybit account `main`".into(),
            tier: AccountTier::Demo,
            opened: AccountTier::Live,
        };
        assert_eq!(refusal.id(), GatewayhubTradeRefusalId::TierMismatch);
        assert_eq!(
            refusal.rendered(),
            "error [gatewayhub.trade.tier_mismatch]: gatewayhub: the account table says bybit account `main` runs \
             at Demo and the client opened it at Live; refused"
        );
        let parked = GatewayhubTradeRefusal::StartParked {
            why: "ibkr: refused".into(),
            venue: "ibkr".into(),
        };
        assert!(
            parked.to_string().starts_with("ibkr: refused: the gatewayhub starts the ibkr session")
        );
    }
}
