//! The account itself: who the key trades, its modes and leverage, its link and quote rate.

// PUBLIC: the gatewayhub's facts lane reads FXCM's account currency, quote currencies and conversion rates
// through these two (decision 0183). `home_conversion` is the PURE judgement of what the shim's
// `forex_connect_system_properties` / `forex_connect_offers` return; `reads` is the registry that reaches an account's running
// reconcile session without a second login.
pub mod quote_rate;
pub mod reads;
// The exec session's state as the SDK states it, in the words the exec thread writes into the account's shared
// `vike_bridge_core::venue_link::VenueLink` (`trade health`'s `gateway-link`).
pub(crate) mod venue_link;
