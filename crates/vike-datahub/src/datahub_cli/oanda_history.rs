//! The OANDA history lane's one practice-token read, its presence probe and its startup line.

/// **The OANDA history lane's ONE credential, read out of the store** —
/// docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md,
/// verdict 2. The scope is `vike_oanda::oanda_history_token_names()`: the bridge names its own key
/// (the practice tier's API key, composed through the one site that composes every OANDA name), and
/// this root declares exactly that and nothing else — the shape [`refuse_stranded_venue_settings_in`]
/// has for the declared legacy names (and, until decision 0095's Task 7, a read of Polymarket's
/// five egress names had for those).
///
/// ⚠ **What "one name" guarantees.** The store is the settings DATABASE (the only credential store
/// since 2026-10-07): the read binds the one declared name and selects no other row, so no other
/// credential enters this process (`crates/vike-secrets/src/store/scoped.rs`'s `ScopedSecrets`), and
/// `the_oanda_history_read_holds_the_one_practice_key_and_nothing_else` pins what comes back.
///
/// ⚠ **`announce` is whether the store's permission FINDING is logged, and only the startup read
/// announces.** The lane's provider calls this once per `Backfill` request for oanda, and a finding
/// repeated per request would bury the log it is meant to be read in — the provider logs NOTHING on
/// the path that finds a store. A store that exists and cannot be read is logged either way: that
/// read ends the request, so it is one line per failed request, and the refusal the operator reads
/// (`vike_oanda::HistoryTokenError::StoreUnreadable`'s) sends them to this log for the cause.
///
/// `None` is that unreadable store — never folded into "no credential", because a permissions bug
/// wearing the unconfigured answer would send the operator to store a key they already stored. A
/// store with no such key is `Some` without it. No line this function logs carries a value: the
/// permission finding and the error's `Display` carry paths, modes and OS reasons.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_credentials(
    settings_dir: &std::path::Path,
    announce: bool,
) -> Option<std::collections::HashMap<String, String>> {
    let scope = vike_secrets::KeyScope::of(vike_oanda::oanda_history_token_names());
    match vike_secrets::resolve_store_scoped_in(
        settings_dir,
        vike_secrets::Table::Credential,
        &scope,
    ) {
        Ok(scoped) => {
            // The store's permission finding, surfaced for the reason the egress read gives: this
            // root does not go through `vike_bridge_core::credentials`, which is what logs it for
            // the other roots, and a credential file readable by others is not read in silence.
            if announce && let Some(w) = &scoped.warning {
                tracing::warn!("{w}");
            }
            // `into_map` ends the three-state answer on purpose: the scope IS the point here, and
            // the reader it feeds (`vike_oanda::load_oanda_history_token`) is the bridge's own
            // fixed-name reader over the names the bridge itself declared.
            Some(scoped.into_map())
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "the credential store could not be read for the OANDA history lane's practice \
                 token; a Backfill for oanda is refused, and it is NOT the same as the key being \
                 absent"
            );
            None
        }
    }
}

/// **One read of the practice token** — the three states 0097's verdict 6 names, each its own
/// answer: `Ok` (present), [`vike_oanda::HistoryTokenError::NotConfigured`] (absent from a store
/// that answered — or no settings directory at all, so no store to hold it) and
/// [`vike_oanda::HistoryTokenError::StoreUnreadable`]. The scoped map is dropped before this
/// returns; the `String` is the only copy that leaves.
#[cfg(feature = "backfill-serve")]
pub(super) fn read_oanda_history_token(
    settings_dir: Option<&std::path::Path>,
    announce: bool,
) -> Result<String, vike_oanda::HistoryTokenError> {
    let Some(dir) = settings_dir else {
        return Err(vike_oanda::HistoryTokenError::NotConfigured);
    };
    let credentials = oanda_history_credentials(dir, announce)
        .ok_or(vike_oanda::HistoryTokenError::StoreUnreadable)?;
    vike_oanda::load_oanda_history_token(&credentials)
        .ok_or(vike_oanda::HistoryTokenError::NotConfigured)
}

/// **The token PROVIDER `crate::backfill::real_backfill_table` builds the OANDA row around** — a
/// closure over the settings DIRECTORY, which is all it holds. Every call is a fresh, silent scoped
/// read ([`read_oanda_history_token`]). The row asks it at most ONCE per request —
/// `crate::backfill::credentialed_klines_row` builds each request's source around a read-once memo,
/// the contract `vike_oanda::HistoryTokenProvider`'s own doc sets — and nothing here caches a value
/// between calls, so a token stored, rotated or removed after this daemon started is what the NEXT
/// request sees: 0097's "removing the token disarms the lane", with no restart either way.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_token_reader(
    settings_dir: Option<std::path::PathBuf>,
) -> vike_oanda::HistoryTokenProvider {
    std::sync::Arc::new(move || read_oanda_history_token(settings_dir.as_deref(), false))
}

/// **Whether the OANDA history lane's practice token is STORED — a presence word, never a value.**
/// What the history-channels read reports for OANDA's credentialed row
/// (`docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §2.4; the owner's Q1).
///
/// The same scope and the same store decision [`oanda_history_credentials`] uses, through
/// `vike_secrets::present_names_scoped_in`: on the settings DATABASE — the only credential store —
/// that read selects a boolean and never the value, so an Observe request does not bring the token
/// into this process as a value at all; with no database there is no store and the answer is
/// `Absent`. "Present" means a non-blank value, the rule
/// `vike_oanda::load_oanda_history_token` applies, so this word cannot say a key is armed that the
/// lane then refuses.
///
/// Three states and the no-directory case, each its own answer, and `Unreadable` never collapsed
/// into `Absent` (0097's verdict 6): no settings directory → `Absent` (no store to hold it, the
/// answer [`read_oanda_history_token`] gives); a store without the key → `Absent`; with it →
/// `Present`; a store that exists and will not read → `Unreadable`, logged with the store's error
/// (paths and OS reasons, never a value), once per request that met it.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_presence(
    settings_dir: Option<&std::path::Path>,
) -> vike_datahub_client::history::CredentialPresence {
    use vike_datahub_client::history::CredentialPresence;
    let Some(dir) = settings_dir else { return CredentialPresence::Absent };
    let names = vike_oanda::oanda_history_token_names();
    let scope = vike_secrets::KeyScope::of(&names);
    match vike_secrets::present_names_scoped_in(dir, vike_secrets::Table::Credential, &scope) {
        Ok(present) if names.iter().any(|n| present.contains(n)) => CredentialPresence::Present,
        Ok(_) => CredentialPresence::Absent,
        Err(e) => {
            tracing::error!(
                error = %e,
                "the credential store could not be read to say whether the OANDA history lane's \
                 practice token is stored; the history-channels answer says UNREADABLE, which is \
                 NOT the same as the key being absent"
            );
            CredentialPresence::Unreadable
        }
    }
}

/// The probe `crate::backfill::BackfillTable::with_credential_probe` takes for OANDA's row — a
/// closure over the settings DIRECTORY and nothing else, asking [`oanda_history_presence`] afresh
/// on every call, so a key stored or removed after start is what the next answer says.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_presence_probe(
    settings_dir: Option<std::path::PathBuf>,
) -> crate::backfill::CredentialProbe {
    Box::new(move || oanda_history_presence(settings_dir.as_deref()))
}

/// The startup line that says whether the OANDA history lane is ARMED — 0097's verdict 6: the log
/// says it, and never says a value. `presence` is one read's answer with the token already dropped
/// (`Result<(), _>`), so no value can reach this text by construction; it names the KEY, from the
/// bridge's own declaration rather than a literal.
///
/// Arming is the act of storing the key: there is no second switch, and the text says that the key
/// is read per request so an operator does not restart for it.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_lane_line(
    settings_dir: Option<&std::path::Path>,
    presence: Result<(), vike_oanda::HistoryTokenError>,
) -> String {
    let key = vike_oanda::oanda_history_token_names().join(", ");
    match (settings_dir, presence) {
        (_, Ok(())) => format!(
            "oanda history lane: ARMED — {key}, the practice account's API token, is present in \
             this server's credential store. It is read when a Control-scope Backfill for oanda \
             arrives and dropped when that request ends; nothing holds it in between, and no \
             Observe verb reaches the lane (docs/decisions/0097)"
        ),
        (None, Err(_)) => format!(
            "oanda history lane: not armed — this server resolved no settings directory, so there \
             is no credential store to hold {key}, and a Backfill for oanda is refused"
        ),
        (Some(_), Err(vike_oanda::HistoryTokenError::StoreUnreadable)) => format!(
            "oanda history lane: UNKNOWN — the credential store could not be read (the error above \
             names it), so whether {key} is stored cannot be told, and a Backfill for oanda is \
             refused until it can be. This is not the same as the key being absent"
        ),
        (Some(_), Err(vike_oanda::HistoryTokenError::NotConfigured)) => format!(
            "oanda history lane: not armed — {key}, the practice account's API token, is absent \
             from this server's credential store, so a Backfill for oanda is refused naming the \
             fix. `vike-cli secrets set {key}` on THIS box arms it; the key is read per request, \
             so there is nothing to restart"
        ),
    }
}
