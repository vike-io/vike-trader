//! "Arbitrary input never panics" harness for the PRIVATE text the FFI layer reads back from the
//! shim: [`redact_login_text`] (the SDK's login-error buffer, scrubbed of the FXCM login name before
//! it reaches a log line or a journaled `OrderRejected` reason), the class-code mapping
//! [`LoginFailureClass::from_code`], the NUL guard [`cstr`] and the [`FxcmError`] rendering. The
//! public decoders are covered in `crates/bridges/fxcm/tests/decoder_never_panics.rs`; these are not
//! reachable from outside the crate, so they get a sibling unit file in the `sys_tests.rs` style.
//!
//! The property is TOTALITY (a hostile or TRUNCATED SDK message must not panic the session thread
//! that is already reporting a failure), plus the one invariant that is the reason the redaction
//! exists: the login name never survives it, whether or not the quoted URL was cut off mid-query.
//! No FFI is called and no shim is needed.

use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The redaction is total over arbitrary text — quotes, `?`, multi-byte characters, a
    /// repeated or unterminated `object='` — and never drops the text around the URL.
    #[test]
    fn redact_login_text_survives_arbitrary_text(
        text in prop_oneof![
            any::<String>(),
            "([a-z '?=&]|object='|é|€){0,40}",
        ],
    ) {
        let out = redact_login_text(&text);
        if !text.contains("object='") {
            prop_assert_eq!(out, text);
        }
    }

    /// The login name never survives, whether the SDK's quoted URL is closed or cut off by the
    /// error buffer's size, and the text either side of it is kept byte-identical.
    #[test]
    fn redact_login_text_never_leaks_the_query(
        before in "[a-z ]{0,20}",
        path in "[a-z/.]{0,16}",
        secret in "LN=[A-Z0-9]{6,12}&AT=PLAIN",
        after in "[a-z ]{0,20}",
        truncated in any::<bool>(),
    ) {
        let url = format!("{before}object='/{path}?ID=1&{secret}");
        let text = if truncated { url } else { format!("{url}' {after}") };
        let out = redact_login_text(&text);
        prop_assert!(!out.contains(&secret), "the query survived: {out:?}");
        prop_assert!(!out.contains("LN="), "the login field survived: {out:?}");
        prop_assert!(out.starts_with(&format!("{before}object='/{path}")), "{out:?}");
        if !truncated {
            prop_assert!(out.ends_with(&format!("' {after}")), "{out:?}");
        }
    }

    /// An unknown class code from a newer shim is `NoResponse`, never a panic; every class has a
    /// non-empty label, and every error renders.
    #[test]
    fn error_text_helpers_survive_arbitrary_input(
        code in any::<i32>(),
        message in any::<String>(),
        native_code in any::<i32>(),
    ) {
        let class = LoginFailureClass::from_code(code);
        prop_assert!(!class.label().is_empty());
        let _ = FxcmError::LoginFailed { class, message: message.clone() }.to_string();
        let _ = FxcmError::Native { code: native_code, message }.to_string();
        let _ = FxcmError::Unavailable.to_string();
    }

    /// The NUL guard refuses exactly the strings that contain an interior NUL.
    #[test]
    fn cstr_refuses_exactly_interior_nuls(text in any::<String>()) {
        prop_assert_eq!(cstr(&text).is_err(), text.contains('\0'));
    }
}
