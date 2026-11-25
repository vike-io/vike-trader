//! cTrader's Open API error codes, as a [`VenueTaxonomy`]: every name of the two published enums, `ProtoErrorCode` (the
//! common codes) and `ProtoOaErrorCode` (the Open API's), the generated bindings `crate::transport::proto` re-exports.
//!
//! Source: <https://help.ctrader.com/open-api/common-model-messages/#protoerrorcode> and
//! <https://help.ctrader.com/open-api/model-messages/#protooaerrorcode> (read 2026-11-23); they match the vendored
//! `.proto` the bindings are generated from. `errorCode` on the wire (`ProtoOAErrorRes`, `ProtoOAOrderErrorEvent`) is a
//! STRING, "the name of the ProtoErrorCode or the other custom ErrorCodes", so a name is read by the enum's own
//! `from_str_name` and never by a substring: `CH_CLIENT_ALREADY_AUTHENTICATED` carries `AUTH` and is benign.
//!
//! [`VenueTaxonomy`] is keyed by a number, and the two enums reuse `1` and `2` (`UNKNOWN_ERROR` and
//! `OA_AUTH_TOKEN_EXPIRED`) while the Open API's `101..=136` lie in the HTTP-status range the venue-neutral layer reads
//! as a status, so [`code_of`] gives each enum's values a thousand of their own. [`by_code`] matches every variant of
//! both enums, so a regenerated binding with a new code does not compile until it has a row.
//!
//! **An order write's fate** ([`TAXONOMY`]'s [`VenueTaxonomy::submit_outcome_unknown`]; every name of both enums has
//! a row, so the unmapped code is a custom one): `UNKNOWN_ERROR` "Generic error" and `CANT_ROUTE_REQUEST` "Connection
//! to Server is lost or not supported" say only that the server had trouble, maybe after the request went on to the
//! trade server, and a custom code is no documented refusal: each leaves the write OUTCOME UNKNOWN (decision 0303). Every
//! other row is the venue's refusal; `TIMEOUT_ERROR` "Deal execution is reached timeout and rejected" says the deal
//! was not done. The venue's pages say nothing of idempotency or a duplicate `clientOrderId`.

use vike_bridge_core::error_kind::{VenueTaxonomy, classify_venue};
use vike_bridge_core::transport::{ErrorKind, VenueApiError};

use crate::transport::proto::{ProtoErrorCode, ProtoOaErrorCode};

/// Where `ProtoErrorCode`'s values start in [`code_of`]'s numbering.
const COMMON_CODES_FROM: i64 = 1_000;
/// Where `ProtoOaErrorCode`'s values start in [`code_of`]'s numbering.
const OPEN_API_CODES_FROM: i64 = 2_000;
/// The number of every name neither enum holds (a broker's custom code).
///
/// shortcut: every custom code is this one number, so `classify_venue`'s once-per-code WARN names only the first a
/// process meets (the order path warns on each by name); give each name its own number if custom codes are ever seen.
const CUSTOM_CODE: i64 = 3_000;

/// The number [`TAXONOMY`] reads a venue `errorCode` name by: its enum's value in that enum's thousand, or
/// [`CUSTOM_CODE`] for a name neither enum holds.
pub fn code_of(error_code: &str) -> i64 {
    if let Some(code) = ProtoErrorCode::from_str_name(error_code) {
        COMMON_CODES_FROM + i64::from(code as i32)
    } else if let Some(code) = ProtoOaErrorCode::from_str_name(error_code) {
        OPEN_API_CODES_FROM + i64::from(code as i32)
    } else {
        CUSTOM_CODE
    }
}

/// cTrader's codes ([`code_of`]'s numbers) → [`ErrorKind`]. `None` = no name of either enum: a custom code.
pub fn by_code(code: i64) -> Option<ErrorKind> {
    match code {
        COMMON_CODES_FROM..OPEN_API_CODES_FROM => common_code(code).map(common_kind),
        OPEN_API_CODES_FROM..CUSTOM_CODE => {
            let value = i32::try_from(code - OPEN_API_CODES_FROM).ok()?;
            ProtoOaErrorCode::try_from(value).ok().map(open_api_kind)
        }
        _ => None,
    }
}

/// The common code [`code_of`] numbered `code`, if it numbered one.
fn common_code(code: i64) -> Option<ProtoErrorCode> {
    let value = i32::try_from(code.checked_sub(COMMON_CODES_FROM)?).ok()?;
    ProtoErrorCode::try_from(value).ok()
}

fn common_kind(code: ProtoErrorCode) -> ErrorKind {
    use ProtoErrorCode::*;
    match code {
        UnknownError | CantRouteRequest => ErrorKind::ServerError, // the write's fate open: `server_had_trouble`
        TimeoutError => ErrorKind::ServerError, // "timeout and rejected": not done, so not now
        BlockedPayloadType => ErrorKind::RateLimited,
        EntityNotFound => ErrorKind::NotFound,
        UnsupportedMessage
        | InvalidRequest
        | FrameTooLong
        | MarketClosed
        | ConcurrentModification => ErrorKind::InvalidRequest,
    }
}

fn open_api_kind(code: ProtoOaErrorCode) -> ErrorKind {
    use ProtoOaErrorCode::*;
    match code {
        OaAuthTokenExpired
        | AccountNotAuthorized
        | RetNoSuchLogin
        | RetAccountDisabled
        | ChClientAuthFailure
        | ChClientNotAuthenticated
        | ChAccessTokenInvalid
        | ChCtidTraderAccountNotFound
        | ChOaClientNotFound
        | ChannelIsBlocked
        | TradingNotAllowed => ErrorKind::Auth,
        ChServerNotReachable => ErrorKind::ServerError,
        ServerIsUnderMaintenance => ErrorKind::VenueMaintenance,
        RequestFrequencyExceeded | ConnectionsLimitExceeded => ErrorKind::RateLimited,
        NotEnoughMoney => ErrorKind::InsufficientFunds,
        SymbolNotFound | UnknownSymbol | PositionNotFound | OrderNotFound | PositionNotOpen => {
            ErrorKind::NotFound
        }
        AlreadyLoggedIn
        | ChClientAlreadyAuthenticated
        | WorseGslNotAllowed
        | SymbolHasHoliday
        | NotSubscribedToSpots
        | AlreadySubscribed
        | IncorrectBoundaries
        | NoQuotes
        | MaxExposureReached
        | PositionLocked
        | TooManyPositions
        | TradingBadVolume
        | TradingBadStops
        | TradingBadPrices
        | TradingBadStake
        | ProtectionIsTooCloseToMarket
        | TradingBadExpirationDate
        | PendingExecution
        | TradingDisabled
        | UnableToCancelOrder
        | UnableToAmendOrder
        | ShortSellingNotAllowed => ErrorKind::InvalidRequest,
    }
}

/// Does this code say only that the SERVER had trouble, and so nothing about what became of a write? `UNKNOWN_ERROR`
/// and `CANT_ROUTE_REQUEST` (the module doc).
pub fn server_had_trouble(code: i64) -> bool {
    matches!(
        common_code(code),
        Some(ProtoErrorCode::UnknownError | ProtoErrorCode::CantRouteRequest)
    )
}

/// cTrader's taxonomy. Its codes are names, so it reads no message.
pub const TAXONOMY: VenueTaxonomy = VenueTaxonomy {
    venue: "ctrader",
    by_code,
    by_msg: |_| None,
    leaves_write_open: server_had_trouble,
};

/// A venue refusal's kind under [`TAXONOMY`]: a custom code is `ErrorKind::UnmappedCode`, its first sighting one WARN.
pub fn kind_of(error_code: &str, description: &str) -> ErrorKind {
    let msg = format!("{error_code}: {description}");
    classify_venue(Some(&TAXONOMY), &VenueApiError { code: code_of(error_code), msg })
}

/// Did the venue refuse the ACCESS TOKEN itself, the one refusal spending the refresh token cures?
pub fn refuses_the_token(error_code: &str) -> bool {
    matches!(
        ProtoOaErrorCode::from_str_name(error_code),
        Some(ProtoOaErrorCode::OaAuthTokenExpired | ProtoOaErrorCode::ChAccessTokenInvalid)
    )
}

/// Is this a refusal a fresh OAuth grant would fix: the token ([`refuses_the_token`]), the application's
/// credentials, or the authorization of the application or the account on this connection?
pub fn refuses_the_grant(error_code: &str) -> bool {
    refuses_the_token(error_code)
        || matches!(
            ProtoOaErrorCode::from_str_name(error_code),
            Some(
                ProtoOaErrorCode::AccountNotAuthorized
                    | ProtoOaErrorCode::ChClientAuthFailure
                    | ProtoOaErrorCode::ChClientNotAuthenticated
            )
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_utility::errors::FailureOutcome;

    /// What a refusal of an order write in `name` is: its kind's outcome, unless it leaves the write open.
    fn write_outcome(name: &str) -> FailureOutcome {
        if TAXONOMY.submit_outcome_unknown(code_of(name)) {
            FailureOutcome::OutcomeUnknown
        } else {
            kind_of(name, "").failure_outcome()
        }
    }

    /// Every name of both enums, by walking their values.
    fn every_name() -> Vec<&'static str> {
        let common =
            (0..200).filter_map(|v| ProtoErrorCode::try_from(v).ok()).map(|c| c.as_str_name());
        let open_api =
            (0..200).filter_map(|v| ProtoOaErrorCode::try_from(v).ok()).map(|c| c.as_str_name());
        common.chain(open_api).collect()
    }

    /// The table is the venue's two enums, every name once, each a number of its own, none a transport sentinel or an
    /// HTTP status the venue-neutral layer would read.
    #[test]
    fn every_name_of_both_enums_has_a_row_and_a_number_of_its_own() {
        let names = every_name();
        assert_eq!(names.len(), 10 + 43, "the pages' 10 common and 43 Open API codes");
        let mut numbers = std::collections::BTreeSet::new();
        for name in &names {
            let code = code_of(name);
            assert!(by_code(code).is_some(), "{name}");
            assert!(numbers.insert(code), "{name}: {code} is another name's");
            assert!(!(100..600).contains(&code) && code != 0, "{name}: {code}");
        }
        assert_eq!(by_code(code_of("SOME_BROKER_CODE")), None, "a custom code has no row");
        assert_eq!(by_code(COMMON_CODES_FROM), None, "no value 0 in either enum");
        assert_eq!(by_code(i64::MIN), None);
    }

    /// The two reused numbers stay apart: `1` is a generic error in one enum and an expired token in the other.
    #[test]
    fn a_number_both_enums_use_is_read_by_its_own_enum() {
        assert_eq!(kind_of("UNKNOWN_ERROR", ""), ErrorKind::ServerError);
        assert_eq!(kind_of("OA_AUTH_TOKEN_EXPIRED", ""), ErrorKind::Auth);
        assert_eq!(kind_of("UNSUPPORTED_MESSAGE", ""), ErrorKind::InvalidRequest);
        assert_eq!(kind_of("ACCOUNT_NOT_AUTHORIZED", ""), ErrorKind::Auth);
    }

    /// The four words a refused order write is decided on (the venue's pages, row by row); a custom code is outcome
    /// unknown, and the two server-trouble codes are not now for anything but a write.
    #[test]
    fn each_code_lands_on_its_documented_outcome() {
        use FailureOutcome::{NotNow, OutcomeUnknown, Refused};
        for (name, want) in [
            ("UNKNOWN_ERROR", OutcomeUnknown),
            ("CANT_ROUTE_REQUEST", OutcomeUnknown),
            ("SOME_BROKER_CODE", OutcomeUnknown),
            ("TIMEOUT_ERROR", NotNow),
            ("BLOCKED_PAYLOAD_TYPE", NotNow),
            ("REQUEST_FREQUENCY_EXCEEDED", NotNow),
            ("CONNECTIONS_LIMIT_EXCEEDED", NotNow),
            ("CH_SERVER_NOT_REACHABLE", NotNow),
            ("SERVER_IS_UNDER_MAINTENANCE", NotNow),
            ("NOT_ENOUGH_MONEY", Refused),
            ("CH_CLIENT_ALREADY_AUTHENTICATED", Refused),
            ("TRADING_NOT_ALLOWED", Refused),
            ("POSITION_NOT_FOUND", Refused),
            ("INVALID_REQUEST", Refused),
        ] {
            assert_eq!(write_outcome(name), want, "{name}");
        }
        assert_eq!(kind_of("UNKNOWN_ERROR", "").failure_outcome(), NotNow, "a read's");
        assert_eq!(kind_of("SOME_BROKER_CODE", "words"), ErrorKind::UnmappedCode);
    }

    /// A write's fate is open on the two server-trouble codes and a custom code, and on nothing else.
    #[test]
    fn the_write_rule_is_the_two_server_trouble_codes_and_a_custom_code() {
        let open_name = |name: &str| TAXONOMY.submit_outcome_unknown(code_of(name));
        let open: Vec<&str> = every_name().into_iter().filter(|name| open_name(name)).collect();
        assert_eq!(open, ["UNKNOWN_ERROR", "CANT_ROUTE_REQUEST"]);
        assert!(open_name("SOME_BROKER_CODE"));
        assert!(open_name(""), "an empty code names nothing either");
    }

    /// The grant words are exact names: a substring read `CH_CLIENT_ALREADY_AUTHENTICATED` (benign) as a refused grant.
    #[test]
    fn the_grant_and_the_token_are_read_by_exact_names() {
        let grant: Vec<&str> =
            every_name().into_iter().filter(|name| refuses_the_grant(name)).collect();
        assert_eq!(
            grant,
            [
                "OA_AUTH_TOKEN_EXPIRED",
                "ACCOUNT_NOT_AUTHORIZED",
                "CH_CLIENT_AUTH_FAILURE",
                "CH_CLIENT_NOT_AUTHENTICATED",
                "CH_ACCESS_TOKEN_INVALID",
            ]
        );
        let token: Vec<&str> =
            every_name().into_iter().filter(|name| refuses_the_token(name)).collect();
        assert_eq!(token, ["OA_AUTH_TOKEN_EXPIRED", "CH_ACCESS_TOKEN_INVALID"]);
        assert!(!refuses_the_grant("ACCESS_TOKEN_INVALID"), "no such name");
        assert!(!refuses_the_grant("ch_access_token_invalid"), "the names are upper case");
    }

    /// One venue answer a row is proved on: its name and words as the venue sent them, and the file of this repository
    /// (repository-root relative) that pins the reply measured on the venue.
    struct RowSample {
        name: &'static str,
        words: &'static str,
        recorded_in: &'static str,
    }

    /// Every row has a reply measured on the venue recorded in this repository behind it, or is named here as having
    /// none yet (the list only shrinks: a row given a sample leaves it). `vike_bridge_core::error_table`'s walk looks for
    /// a NUMBER in the sample's file, and cTrader's replies carry names, so this is its walk by name.
    #[test]
    fn every_row_has_a_recorded_sample_or_is_listed_without_one() {
        const MEASURED: &str =
            "crates/bridges/ctrader/src/transport/connection/connection_tests.rs";
        const LAPSE: &str = "crates/bridges/ctrader/tests/offline/session_lapse.rs";
        let samples = [
            RowSample {
                name: "INVALID_REQUEST",
                words: "Trading account is not authorized",
                recorded_in: LAPSE,
            },
            RowSample {
                name: "CH_ACCESS_TOKEN_INVALID",
                words: "Access token expired",
                recorded_in: LAPSE,
            },
            RowSample {
                name: "MARKET_CLOSED",
                words: "Trading is not available: Market is closed.",
                recorded_in: MEASURED,
            },
            RowSample {
                name: "TRADING_DISABLED",
                words: "Trading is disabled",
                recorded_in: MEASURED,
            },
            RowSample {
                name: "TRADING_BAD_VOLUME",
                words: "must be multiple of volume step",
                recorded_in: MEASURED,
            },
            RowSample {
                name: "TRADING_BAD_STOPS",
                words: "New SL for BUY pending order should be < entry price",
                recorded_in: MEASURED,
            },
        ];
        let without_sample = [
            "UNKNOWN_ERROR",
            "UNSUPPORTED_MESSAGE",
            "TIMEOUT_ERROR",
            "ENTITY_NOT_FOUND",
            "CANT_ROUTE_REQUEST",
            "FRAME_TOO_LONG",
            "CONCURRENT_MODIFICATION",
            "BLOCKED_PAYLOAD_TYPE",
            "OA_AUTH_TOKEN_EXPIRED",
            "ACCOUNT_NOT_AUTHORIZED",
            "RET_NO_SUCH_LOGIN",
            "ALREADY_LOGGED_IN",
            "RET_ACCOUNT_DISABLED",
            "CH_CLIENT_AUTH_FAILURE",
            "CH_CLIENT_NOT_AUTHENTICATED",
            "CH_CLIENT_ALREADY_AUTHENTICATED",
            "CH_SERVER_NOT_REACHABLE",
            "CH_CTID_TRADER_ACCOUNT_NOT_FOUND",
            "CH_OA_CLIENT_NOT_FOUND",
            "REQUEST_FREQUENCY_EXCEEDED",
            "SERVER_IS_UNDER_MAINTENANCE",
            "CHANNEL_IS_BLOCKED",
            "CONNECTIONS_LIMIT_EXCEEDED",
            "WORSE_GSL_NOT_ALLOWED",
            "SYMBOL_HAS_HOLIDAY",
            "NOT_SUBSCRIBED_TO_SPOTS",
            "ALREADY_SUBSCRIBED",
            "SYMBOL_NOT_FOUND",
            "UNKNOWN_SYMBOL",
            "INCORRECT_BOUNDARIES",
            "NO_QUOTES",
            "NOT_ENOUGH_MONEY",
            "MAX_EXPOSURE_REACHED",
            "POSITION_NOT_FOUND",
            "ORDER_NOT_FOUND",
            "POSITION_NOT_OPEN",
            "POSITION_LOCKED",
            "TOO_MANY_POSITIONS",
            "TRADING_BAD_PRICES",
            "TRADING_BAD_STAKE",
            "PROTECTION_IS_TOO_CLOSE_TO_MARKET",
            "TRADING_BAD_EXPIRATION_DATE",
            "PENDING_EXECUTION",
            "TRADING_NOT_ALLOWED",
            "UNABLE_TO_CANCEL_ORDER",
            "UNABLE_TO_AMEND_ORDER",
            "SHORT_SELLING_NOT_ALLOWED",
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let letters = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
        for s in &samples {
            assert_eq!(
                by_code(code_of(s.name)),
                Some(kind_of(s.name, s.words)),
                "{}: a sample lands on its row's own kind (and a sampled code has a row)",
                s.name
            );
            let file = std::fs::read_to_string(root.join(s.recorded_in));
            assert!(file.is_ok(), "{}: {}: {file:?}", s.name, s.recorded_in);
            let file = file.unwrap_or_default();
            assert!(file.contains(s.name), "{}: not in {}", s.name, s.recorded_in);
            assert!(
                letters(&file).contains(&letters(s.words)),
                "{}: {:?} not in {}",
                s.name,
                s.words,
                s.recorded_in
            );
        }
        for name in every_name() {
            let sampled = samples.iter().any(|s| s.name == name);
            let listed = without_sample.contains(&name);
            assert!(!(sampled && listed), "{name}: sampled, so off the no-sample list");
            assert!(
                sampled || listed,
                "{name}: a row with no sample and not on the no-sample list"
            );
        }
        assert_eq!(
            samples.len() + without_sample.len(),
            every_name().len(),
            "a name of neither enum"
        );
    }
}
