//! The Data Manager's Polymarket proxy box: the value in force it is seeded with
//! (`seed_polymarket_proxy_box`), and the one write that persists it (`save_polymarket_proxy`).

/// Seed the Data Manager's **Polymarket proxy** box with the value actually IN FORCE — the
/// `venue.polymarket.*` rows the data daemon declares to the bridge (decision 0095), read from the
/// settings directory this binary's boot resolved. A store that will not open seeds an empty box and
/// says why in the log.
pub fn seed_polymarket_proxy_box(settings_dir: Option<&std::path::Path>) -> String {
    let polymarket = settings_dir.and_then(|dir| {
        match vike_secrets::venue_setting::load_venue_settings(dir) {
            Ok(mut all) => all.remove("polymarket"),
            Err(e) => {
                tracing::warn!(error = %e, "the venue_setting table could not be read; the proxy box opens empty");
                None
            }
        }
    });
    proxy_box_value(polymarket.as_ref())
}

/// [`seed_polymarket_proxy_box`]'s pure half: the URL the bridge dials, by the one precedence both
/// read (`crates/vike-venues/src/venue_fields.rs`'s `polymarket_proxy_url`); a direct connection is
/// an empty box.
fn proxy_box_value(polymarket: Option<&vike_secrets::venue_setting::VenueSettings>) -> String {
    vike_venues::venue_fields::polymarket_proxy_url(|field| {
        polymarket
            .and_then(|s| s.get(vike_secrets::venue_setting::SettingTier::Any, field))
            .map(str::to_string)
    })
    .unwrap_or_default()
}

#[path = "polymarket_proxy_tests.rs"]
#[cfg(test)]
mod polymarket_proxy_tests;

/// Persist the box's value as the `venue.polymarket.socks_proxy` row, and RECORD that it changed —
/// a declared SECRET field: never logged, journalled as `<secret>`.
///
/// ⚠ **`creds` is a PARAMETER, and it used to be a walk.** This function called a `_from`-less
/// resolver, which is `$VIKE_SETTINGS_DIR`-BLIND — so a deployment that names its settings directory outright had this
/// box writing into whatever project the working directory sat above. The caller now hands over the
/// settings directory its own boot resolved. That is the half of "only a binary touches the store"
/// that actually matters.
pub fn save_polymarket_proxy(
    value: &str,
    creds: vike_account_manager::AccountManagerCredentialWrite<'_>,
) {
    // The grammar validates the TRIMMED value, so the row stores that value — a pasted URL's
    // trailing space or newline is not part of it (`vike-cli config set venue.*` does the same).
    let value = value.trim();
    // The catalog's grammar first, as `vike-cli config set` checks it — a refused value writes nothing.
    let socks_proxy = &vike_venues::venue_fields::SOCKS_PROXY;
    if let Some(field) = vike_venues::venue_fields::venue_field("polymarket", socks_proxy)
        && let Err(why) = field.grammar.check(value)
    {
        tracing::error!("polymarket proxy NOT saved: {why}");
        return;
    }
    match vike_secrets::set_venue_setting_in(
        creds.settings_dir,
        "polymarket",
        None,
        socks_proxy,
        value,
        true,
        vike_secrets::ChangeStamp {
            actor: vike_change_journal::ChangeActor::Gui,
            process: creds.process.clone(),
            now_ms: creds.now_ms,
        },
    ) {
        Ok((_previous, journal_error)) => {
            if let Some(err) = &journal_error {
                tracing::error!(
                    error = %err,
                    dir = %err.dir.display(),
                    "polymarket proxy write NOT recorded to the change journal (the row IS saved)"
                );
            }
            tracing::info!(
                kind = "set_setting",
                key = %vike_secrets::venue_setting::venue_setting_key("polymarket", None, socks_proxy),
                "polymarket proxy saved; takes effect when the data daemon restarts"
            );
        }
        Err(refusal) => {
            let (e, _journal_error) = *refusal;
            tracing::error!(error = %e, "failed to save the polymarket proxy");
        }
    }
}
