//! The demo config gate and the default epic the eight live IG smokes/probes share (setup only),
//! declared ONCE by the `ig_smokes` group binary (`main.rs`), so it compiles once.

use vike_ig::load_ig_config_from;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

/// EUR/USD mini — present on the demo gateway.
pub const EPIC: &str = "CS.D.EURUSD.MINI.IP";

/// The USER name the engine trades [`EPIC`] by (decision 0290): what the exec client, the feed and the reconcile
/// client are handed, each reading the epic off the kept row [`hold_kept_list`] holds.
pub const SYMBOL: &str = "EURUSD";

/// Hold this binary's ig kept list: the rows of [`EPIC`] and of the GBP/USD mini (the paging smoke's second
/// symbol), in the shape the operator's row-writing path (`vike_ig::operator_row`) stores them, so a smoke needs no
/// `<project>/instruments/ig.json`.
pub fn hold_kept_list() {
    let row = |epic: &str, base: &str, quote: &str| {
        let fx = vike_model::ContractFx {
            pip_size: Some(0.0001),
            lot_size: Some(10_000.0),
            ..vike_model::ContractFx::default()
        };
        vike_model::Instrument::new("ig", epic, base, quote, vike_model::Contract::Fx(fx))
            .with_venue_word("CURRENCIES")
    };
    vike_catalog::hold_kept_list(
        "ig",
        vec![row(EPIC, "EUR", "USD"), row("CS.D.GBPUSD.MINI.IP", "GBP", "USD")],
    );
}

/// Same config gate the other IG paths use: the credential store's default DEMO ig account; `None`
/// (-> self-skip) when it is absent or lacks a field. Never sweeps the process env — this repo's
/// credentials live only in the store.
pub fn config() -> Option<vike_ig::IgConfig> {
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ig", &AccountLabel::Default, AccountTier::Demo);
    let urls = vike_venues::venue_urls::VenueUrls::seed("ig");
    load_ig_config_from(AccountTier::Demo, &creds, &urls).expect("the seed's demo row")
}
