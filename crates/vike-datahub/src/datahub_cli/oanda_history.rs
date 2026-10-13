//! The OANDA history lane's one practice-token read, its presence probe and its startup line.
//!
//! The token is an ACCOUNT's credential (decision 0156 item 6): the `API_KEY` field of the ONE
//! ACTIVE oanda `account` row at tier `demo` with no label — [`PRACTICE_VENUE`], [`PRACTICE_TIER`]
//! and [`PRACTICE_FIELD`] say it once — read through `vike_secrets::CredentialStore::account_field`,
//! the narrowest read the store offers. No credential NAME is composed or read here, and no tier is
//! read out of one: the account row's own `tier` and `active` are the address.

#[cfg(feature = "backfill-serve")]
use vike_model::accounts::account_keys::AccountLabel;

/// The venue whose practice account holds the token.
#[cfg(feature = "backfill-serve")]
const PRACTICE_VENUE: &str = "oanda";

/// The practice account's tier: OANDA's practice environment is the account at tier `demo`.
#[cfg(feature = "backfill-serve")]
const PRACTICE_TIER: vike_model::AccountTier = vike_model::AccountTier::Demo;

/// The token's FIELD on that account — the oanda row of `vike_model::credential_schema`'s
/// `VENUE_KEY_SCHEMAS` names it `API_KEY`.
#[cfg(feature = "backfill-serve")]
const PRACTICE_FIELD: &str = vike_model::credential_keys::API_KEY;

/// The token, in words, for a log line or a refusal: which field of which account. Names and
/// words only — there is no value in scope here.
#[cfg(feature = "backfill-serve")]
fn practice_token_words() -> String {
    format!(
        "the {PRACTICE_FIELD} of the ACTIVE unlabelled {PRACTICE_VENUE} account at tier \
         `{PRACTICE_TIER}`"
    )
}

/// The `error!` an unreadable store earns on the lane's token read — one spelling for both of
/// [`oanda_history_credentials`]'s reads.
#[cfg(feature = "backfill-serve")]
fn log_token_store_unreadable(e: &vike_secrets::SecretsError) {
    tracing::error!(
        error = %e,
        "the credential store could not be read for the OANDA history lane's practice \
         token; a Backfill for oanda is refused, and it is NOT the same as the key being \
         absent"
    );
}

/// **The OANDA history lane's ONE credential, read out of the store** —
/// docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md,
/// verdict 2. The read is `vike_secrets::CredentialStore::account_field` at the practice account's
/// address (this module's doc): one field of one account row, and nothing else (until decision
/// 0095's Task 7, a read of Polymarket's five egress names had the same narrow shape).
///
/// ⚠ **What "one field" guarantees.** The store is the settings DATABASE (the only credential store
/// since 2026-10-07): the read selects that one row's value and no other, so no other credential
/// enters this process, and `the_oanda_history_read_holds_the_one_practice_key_and_nothing_else`
/// pins what comes back among the practice account's own other field, its live, paper and labelled
/// siblings and another venue's key.
///
/// ⚠ **`announce` is whether the store's permission FINDING is logged, and only the startup read
/// announces.** The lane's provider calls this once per `Backfill` request for oanda, and a finding
/// repeated per request would bury the log it is meant to be read in — the provider logs NOTHING on
/// the path that finds a store. The field read returns no findings, so the announcing call takes
/// them from a scoped read of NO names (`vike_secrets::CredentialStore::read` over an empty
/// `vike_secrets::KeyScope`: it selects no row) and hands them to
/// `vike_secrets::ScopedSecrets::report`, the store's one report body. A store that exists and
/// cannot be read is logged either way: that read ends the request, so it is one line per failed
/// request, and the refusal the operator reads (`vike_oanda::HistoryTokenError::StoreUnreadable`'s)
/// sends them to this log for the cause.
///
/// `None` is that unreadable store — never folded into "no credential", because a permissions bug
/// wearing the unconfigured answer would send the operator to store a key they already stored. A
/// store with no such account, or an account without the field (or with it blank), is `Some(None)`.
/// No line this function logs carries a value: the permission finding and the error's `Display`
/// carry paths, modes and OS reasons.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_credentials(
    store: &vike_secrets::CredentialStore,
    announce: bool,
) -> Option<Option<vike_model::secret::Secret<String>>> {
    if announce {
        // The store's findings, surfaced by the ANNOUNCING read: a read returns them as data and
        // logs nothing, `vike_secrets::ScopedSecrets::report` is where they become lines, and a
        // credential store readable by others is not read in silence.
        match store.read(&vike_secrets::KeyScope::default()) {
            Ok(findings) => findings.report(),
            Err(e) => {
                log_token_store_unreadable(&e);
                return None;
            }
        }
    }
    let token =
        store.account_field(PRACTICE_VENUE, &AccountLabel::Default, PRACTICE_TIER, PRACTICE_FIELD);
    match token {
        Ok(token) => Some(token),
        Err(e) => {
            log_token_store_unreadable(&e);
            None
        }
    }
}

/// **One read of the practice token** — the three states 0097's verdict 6 names, each its own
/// answer: `Ok` (present), [`vike_oanda::HistoryTokenError::NotConfigured`] (absent from a store
/// that answered — no such account, or no non-blank field on it — or no settings directory at all,
/// so no store to hold it) and [`vike_oanda::HistoryTokenError::StoreUnreadable`]. The `String` is
/// the only copy that leaves. `store` is the boot's handle, opened over the settings directory the
/// boot resolved — a handle with no location is "no settings directory".
#[cfg(feature = "backfill-serve")]
pub(super) fn read_oanda_history_token(
    store: &vike_secrets::CredentialStore,
    announce: bool,
) -> Result<String, vike_oanda::HistoryTokenError> {
    if store.location().is_none() {
        return Err(vike_oanda::HistoryTokenError::NotConfigured);
    }
    let token = oanda_history_credentials(store, announce)
        .ok_or(vike_oanda::HistoryTokenError::StoreUnreadable)?;
    token
        .map(vike_model::secret::Secret::into_inner)
        .ok_or(vike_oanda::HistoryTokenError::NotConfigured)
}

/// **The token PROVIDER `crate::backfill::real_backfill_table` builds the OANDA row around** — a
/// closure over the credential store HANDLE (a location and a scope, no value), which is all it
/// holds. Every call is a fresh, silent read ([`read_oanda_history_token`]). The row asks it
/// at most ONCE per request — `crate::backfill::credentialed_klines_row` builds each request's source around a read-once memo,
/// the contract `vike_oanda::HistoryTokenProvider`'s own doc sets — and nothing here caches a value
/// between calls, so a token stored, rotated or removed after this daemon started is what the NEXT
/// request sees: 0097's "removing the token disarms the lane", with no restart either way.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_token_reader(
    store: vike_secrets::CredentialStore,
) -> vike_oanda::HistoryTokenProvider {
    std::sync::Arc::new(move || read_oanda_history_token(&store, false))
}

/// **Whether the OANDA history lane's practice token is STORED — a presence word, never a value.**
/// What the history-channels read reports for OANDA's credentialed row
/// (`docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §2.4; the owner's Q1).
///
/// The same address and the same store decision [`oanda_history_credentials`] uses, through
/// `vike_secrets::CredentialStore::account_field_present`: on the settings DATABASE — the only
/// credential store — that read selects a boolean and never the value, so an Observe request does
/// not bring the token into this process as a value at all; with no database there is no store and
/// the answer is `Absent`. "Present" means a non-blank value, the rule the token read applies, so
/// this word cannot say a key is armed that the lane then refuses.
///
/// Three states and the no-directory case, each its own answer, and `Unreadable` never collapsed
/// into `Absent` (0097's verdict 6): no settings directory → `Absent` (no store to hold it, the
/// answer [`read_oanda_history_token`] gives); a store without the account or the field → `Absent`;
/// with it → `Present`; a store that exists and will not read → `Unreadable`, logged with the
/// store's error (paths and OS reasons, never a value), once per request that met it.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_presence(
    store: &vike_secrets::CredentialStore,
) -> vike_datahub_client::history::CredentialPresence {
    use vike_datahub_client::history::CredentialPresence;
    if store.location().is_none() {
        return CredentialPresence::Absent;
    }
    match store.account_field_present(
        PRACTICE_VENUE,
        &AccountLabel::Default,
        PRACTICE_TIER,
        PRACTICE_FIELD,
    ) {
        Ok(true) => CredentialPresence::Present,
        Ok(false) => CredentialPresence::Absent,
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
/// closure over the credential store HANDLE and nothing else, asking [`oanda_history_presence`]
/// afresh on every call, so a key stored or removed after start is what the next answer says.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_presence_probe(
    store: vike_secrets::CredentialStore,
) -> crate::backfill::CredentialProbe {
    Box::new(move || oanda_history_presence(&store))
}

/// The startup line that says whether the OANDA history lane is ARMED — 0097's verdict 6: the log
/// says it, and never says a value. `presence` is one read's answer with the token already dropped
/// (`Result<(), _>`), so no value can reach this text by construction; it names the token by its
/// account and FIELD ([`practice_token_words`]), never by a composed name.
///
/// Arming is the act of storing the key on that account: there is no second switch, and the text
/// says that the key is read per request so an operator does not restart for it.
#[cfg(feature = "backfill-serve")]
pub(super) fn oanda_history_lane_line(
    settings_dir: Option<&std::path::Path>,
    presence: Result<(), vike_oanda::HistoryTokenError>,
) -> String {
    let key = practice_token_words();
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
             fix. On THIS box, `vike-cli secrets set \
             --account <ID> {PRACTICE_FIELD}` arms it, <ID> being that account's row \
             (`vike-cli secrets accounts` lists them; `vike-cli secrets account add --venue \
             {PRACTICE_VENUE} --tier {PRACTICE_TIER} --no-label` creates it). The key is read per \
             request, so there is nothing to restart"
        ),
    }
}
