use super::*;

/// the CI box's live `settings/tradehub.toml`, VERBATIM plus the one key the schema requires and the
/// file does not carry.
const PROD2_DAEMON: &str = "venue = \"bybit\"\nasset_class = \"CryptoPerp\"\n\
                                token_id = \"BTCUSDT\"\ninterval = \"1m\"\nqty = 0.001\n\
                                half_spread = 0.0005\ntick_size = 0.1\nseed_cash = 1000.0\n\n\
                                [daemon]\nsummary_ms = 60000\nshutdown_deadline_ms = 10000\n";

/// **EVERY KEY OF THE LIVE DAEMON PROFILE BECOMES A ROW**, in the single-mount spelling, and
/// the round trip is what proves the spelling was preserved.
#[test]
fn the_live_daemon_profile_lowers_to_one_mount_row_and_round_trips() {
    let stored = rows_from_daemon_profile_text("tradehub", PROD2_DAEMON).expect("must round-trip");
    assert_eq!(stored.mounts.len(), 1);
    let m = &stored.mounts[0];
    assert_eq!((m.ord, m.is_primary), (0, false), "the file declares no primary");
    assert_eq!(m.venue, "bybit");
    assert_eq!(m.asset_class, "CryptoPerp");
    assert_eq!(m.token_id.as_deref(), Some("BTCUSDT"), "the file's own spelling is carried");
    assert_eq!(m.symbol, None, "…and the other one stays absent, which the schema CHECKs");
    assert_eq!(m.interval.as_deref(), Some("1m"));
    assert_eq!(m.qty, Some(0.001));
    assert_eq!(m.half_spread, Some(0.0005));
    assert_eq!(m.tick_size, Some(0.1));
    assert_eq!(m.seed_cash, Some(1000.0));
    assert_eq!(m.data_only, None, "absent, not `false` — a stored default breaks the round trip");
    assert_eq!(m.account, None);
    assert_eq!(m.strategy_name, None);
    assert_eq!(m.strategy_rhai, None);
    assert_eq!(m.interval_ms, None);
    assert_eq!(m.resolution_ts_ms, None);
    assert_eq!(stored.settings.get("daemon.summary_ms").map(String::as_str), Some("60000"));
    assert_eq!(
        stored.settings.get("daemon.shutdown_deadline_ms").map(String::as_str),
        Some("10000")
    );
    assert_eq!(stored.settings.len(), 2);
    assert!(stored.params.is_empty());
}

/// **THE SINGLE-MOUNT SPELLING SURVIVES**, which is the state-sidecar defect's fix seen from
/// the migration's side: the rendered document must be the top-level spelling, not `[[mounts]]`.
#[test]
fn a_single_mount_profile_renders_back_as_a_single_mount_profile() {
    let stored = rows_from_daemon_profile_text("tradehub", PROD2_DAEMON).expect("must round-trip");
    let rendered = render_daemon_toml(&stored).expect("renders");
    assert!(!rendered.contains("[[mounts]]"), "the spelling moved:\n{rendered}");
    assert!(rendered.contains("venue = \"bybit\""), "{rendered}");
}

/// …and a `[[mounts]]` profile keeps ITS spelling.
#[test]
fn a_multi_mount_profile_renders_back_as_an_array() {
    let text = "[[mounts]]\nvenue = \"bybit\"\nasset_class = \"CryptoPerp\"\n\
                    symbol = \"BTCUSDT\"\n\n[[mounts]]\nvenue = \"okx\"\n\
                    asset_class = \"CryptoSpot\"\nsymbol = \"BTC-USDT\"\n";
    let stored = rows_from_daemon_profile_text("two", text).expect("must round-trip");
    assert_eq!(stored.mounts.len(), 2);
    assert!(render_daemon_toml(&stored).expect("renders").contains("[[mounts]]"));
}

/// **THE FENCE ITSELF, reached and proven** — a difference only the round-trip comparison can
/// see, not one an earlier `return` catches.
///
/// A ONE-row `[[mounts]]` profile that declares no `primary` is that difference: the rows are
/// indistinguishable from a single-mount profile's, so the renderer emits the single-mount
/// spelling and the document is not the one that was read. The two genuinely mount under
/// different controller ids, so refusing is right — and the message names the repair.
#[test]
fn a_body_that_does_not_reproduce_its_profile_refuses_the_write() {
    let text = "[[mounts]]\nvenue = \"bybit\"\nasset_class = \"CryptoPerp\"\n\
                    symbol = \"BTCUSDT\"\n";
    let e = rows_from_daemon_profile_text("one", text)
        .expect_err("a one-row [[mounts]] profile with no declared primary must refuse");
    assert!(e.contains("REFUSING"), "{e}");
    assert!(e.contains("do not reproduce the profile"), "{e}");
    assert!(e.contains("primary = true"), "the refusal names the repair: {e}");
    assert!(e.contains("the profile as given"), "{e}");
    assert!(e.contains("as the rows render it"), "{e}");
    // …and adding the one line it names makes the same profile storable.
    let fixed = format!("{text}primary = true\n");
    assert!(rows_from_daemon_profile_text("one", &fixed).is_ok(), "the named repair must work");
}

/// A mount that names no product cannot become a row — 0061 phase 5, arriving at the one seam
/// that can enforce it. The message must say the migration cannot answer it.
#[test]
fn a_mount_with_no_asset_class_is_refused_and_says_the_migration_cannot_guess() {
    let e =
        rows_from_daemon_profile_text("tradehub", "venue = \"bybit\"\ntoken_id = \"BTCUSDT\"\n")
            .expect_err("asset_class is NOT NULL");
    assert!(e.contains("asset_class"), "{e}");
    assert!(e.contains("CryptoPerp"), "the message shows the shape of the fix: {e}");
    assert!(e.contains("cannot answer it for you"), "{e}");
    let e = rows_from_daemon_profile_text(
        "tradehub",
        "venue = \"bybit\"\ntoken_id = \"X\"\nasset_class = \"Perp\"\n",
    )
    .expect_err("a word outside the vocabulary is refused by name");
    assert!(e.contains("Perp"), "{e}");
}

/// An unknown key is REFUSED BY NAME at every level — the write-path half of
/// `deny_unknown_fields`. A dropped key is a setting an operator believes is armed.
#[test]
fn an_unknown_key_refuses_the_whole_profile() {
    let e = rows_from_daemon_profile_text(
        "tradehub",
        "venue = \"bybit\"\nsymbol = \"X\"\nasset_class = \"CryptoPerp\"\nqtyy = 1.0\n",
    )
    .expect_err("a misspelled mount key must refuse");
    assert!(e.contains("qtyy"), "{e}");
    assert!(e.contains("Nothing was written"), "{e}");

    let e = rows_from_daemon_profile_text(
        "tradehub",
        "venue = \"bybit\"\nsymbol = \"X\"\nasset_class = \"CryptoPerp\"\n\n[daemon]\n\
             summary_m = 1\n",
    )
    .expect_err("a misspelled [daemon] key must refuse");
    assert!(e.contains("daemon.summary_m"), "names the PATH: {e}");
}

/// Mixing the two mount spellings is refused rather than resolved — a top-level `venue` beside
/// a `[[mounts]]` array is two different claims about what this daemon mounts.
#[test]
fn the_two_mount_spellings_may_not_be_mixed() {
    let e = rows_from_daemon_profile_text(
        "x",
        "venue = \"bybit\"\n\n[[mounts]]\nvenue = \"okx\"\nasset_class = \"CryptoSpot\"\n\
             symbol = \"BTC-USDT\"\nprimary = true\n",
    )
    .expect_err("the two spellings may not be mixed");
    assert!(e.contains("two spellings"), "{e}");
}

/// **A mount that omits `venue` is refused BY NAME**, although the TOML key is optional. The
/// round-trip fence would catch it anyway — the rendered document carries the default the row
/// had to store — but as a generic mismatch printing `venue = ""`, which reads as a migration
/// bug rather than as the one-line repair it is.
#[test]
fn a_mount_that_omits_its_venue_is_refused_by_name_rather_than_by_the_fence() {
    let e = rows_from_daemon_profile_text(
        "poly",
        "token_id = \"12345\"\nasset_class = \"PredictionMarket\"\nqty = 20.0\n",
    )
    .expect_err("a mount ROW must name its venue");
    assert!(e.contains("does not declare `venue`"), "{e}");
    assert!(e.contains("polymarket"), "the refusal names the default it will not write: {e}");
    assert!(!e.contains("REFUSING"), "this is the lowering's refusal, not the fence's: {e}");
    // ...and the named repair makes the same profile storable.
    let fixed = rows_from_daemon_profile_text(
        "poly",
        "venue = \"polymarket\"\ntoken_id = \"12345\"\nasset_class = \"PredictionMarket\"\n\
             qty = 20.0\n",
    )
    .expect("the named repair must work");
    assert_eq!(fixed.mounts[0].venue, "polymarket");
    assert_eq!(fixed.mounts[0].token_id.as_deref(), Some("12345"));
}
