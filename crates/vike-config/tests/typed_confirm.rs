//! **The retyped-key line, gated against ROT** — `vike_config::typed_confirm_reason`'s flags half.
//!
//! The policy half of that predicate is structural: it matches on the section and on two exempt
//! table names, so it cannot name a key that stopped existing. The FLAGS half is a written list
//! (`CONFIRMED_FLAG_KEYS`), and a written list of field names has exactly one failure mode —
//! a rename or a deletion leaves a row protecting nothing, silently, while the predicate keeps
//! answering `Some` for a key no file can hold.
//!
//! ⚠ **Only one direction is gated, and the other is DECLARED rather than assumed away.**
//! `FLAG_REGISTRY` carries an owner, a review date and a disposition per flag, and no "is a safety
//! override" column — so nothing in this crate can derive the set, and a NEW override added to
//! `flags.rs` does not join the ceremony on its own. What IS gated here is that every row still
//! names a real, writable `flags.toml` field. Closing the other direction means a column on
//! `FlagMeta` and a row-per-flag edit, which is a change to the registry and its own gate rather
//! than to this predicate.

/// **Every confirmed flag key is a REAL field of `flags.toml`** — proven by driving it through the
/// same loader a write faces, not by reading the struct.
///
/// `validate_settings_text` is the exact gate `set_setting` runs before a byte lands
/// (`deny_unknown_fields` into `FlagsPatch`, then `apply`), so a row naming a renamed or deleted
/// field fails here with the loader's own `unknown field` message — which is also the message an
/// operator would have met after being made to retype a key that could never be written.
#[test]
fn every_confirmed_flag_is_a_real_flags_field() {
    for key in FLAG_KEYS {
        assert!(
            vike_config::requires_typed_confirm(key),
            "{key} is listed here but the predicate does not demand a confirm for it"
        );
        let leaf = key.strip_prefix("flags.").unwrap_or_else(|| panic!("{key} is not a flags key"));
        let text = format!("{leaf} = true\n");
        vike_config::validate_settings_text(
            vike_config::SettingsFile::Flags,
            std::path::Path::new("flags.toml"),
            &text,
        )
        .unwrap_or_else(|e| {
            panic!(
                "`{key}` demands the typed confirm but is not a field the loader accepts — the \
                 field was renamed or deleted and the ceremony now protects nothing: {e}"
            )
        });
    }
}

/// …and the class is drawn where the predicate says it is: the flags half answers with its OWN
/// reason, never the policy one, so a surface rendering the refusal cannot tell an operator that
/// `flags.tradehub_live` is a policy risk ceiling.
#[test]
fn a_confirmed_flag_answers_with_the_flag_reason_and_not_the_ceiling_one() {
    for key in FLAG_KEYS {
        let why = vike_config::typed_confirm_reason(key).expect("a reason");
        assert!(!why.contains("RISK CEILING"), "{key}: {why}");
        assert!(why.contains("flag"), "the reason must name what the key IS: {key}: {why}");
    }
    let why = vike_config::typed_confirm_reason("policy.max_notional_per_order").expect("a reason");
    assert!(why.contains("RISK CEILING"), "{why}");
}

/// The keys under test.
///
/// ⚠ **A second copy of `CONFIRMED_FLAG_KEYS`, and deliberately so**: that const is private, and
/// making it public to feed a test would publish an internal list as API. The copy is safe in the
/// direction that matters because the FIRST assertion in each test above is
/// `requires_typed_confirm`, so a key dropped from the real list fails here rather than passing
/// vacuously. A key ADDED to the real list and not here is simply untested, which is the ordinary
/// cost of a written list and is what the module doc declares.
const FLAG_KEYS: [&str; 7] = [
    "flags.tradehub_live",
    // ⚠ The OPENER of the daemon's remote order-origination surface. It joined the real list after
    // a review found it outside a ceremony that `flags.tradehub_allow_public_bind` — which only
    // widens that surface's bind ADDRESS — was already inside; the argument is at the const.
    "flags.tradehub_control",
    "flags.tradehub_allow_public_bind",
    "flags.allow_withdraw_keys",
    "flags.preflight_skip",
    "flags.reconcile_off",
    // ⚠ It joined the real list when the unread-settings sweep WIRED the flag — before that a
    // write of it armed nothing, and the exclusion said so. Polymarket has no testnet, so a write
    // of this key puts real money on Polygon mainnet with no demo tier to land on instead.
    "flags.poly_exec",
];
