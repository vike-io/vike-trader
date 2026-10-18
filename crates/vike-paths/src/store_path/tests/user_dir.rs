use super::*;

#[cfg(not(windows))]
#[test]
fn unix_user_dir_prefers_xdg_then_home() {
    assert_eq!(
        user_data_dir(Some("/xdg"), Some("/home/u"), None),
        Some(PathBuf::from("/xdg/vike-data"))
    );
    assert_eq!(
        user_data_dir(None, Some("/home/u"), None),
        Some(PathBuf::from("/home/u/.local/share/vike-data"))
    );
    assert_eq!(user_data_dir(None, None, None), None);
    assert_eq!(user_data_dir(Some(""), Some(""), None), None, "empty is absent");
}

/// **The behaviour-preservation gate for the map form.** The four callers that used to paste
/// three `std::env::var` lines now pass a map; this asserts the map form answers EXACTLY what
/// those three arguments answered, on a unix-shaped AND a windows-shaped environment, on
/// whichever platform the test runs. Absent, present and blank values all agree by
/// construction, because the map form only chooses the three arguments — it re-implements none
/// of the precedence.
#[test]
fn the_map_form_answers_exactly_what_the_three_argument_form_answers() {
    let cases: &[&[(&str, &str)]] = &[
        // unix-shaped: XDG set, HOME set, no LOCALAPPDATA
        &[("XDG_DATA_HOME", "/home/u/.local/share"), ("HOME", "/home/u")],
        // unix-shaped, XDG absent — the `~/.local/share` arm
        &[("HOME", "/home/u")],
        // windows-shaped: LOCALAPPDATA + HOME, no XDG
        &[("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"), ("HOME", "C:\\Users\\u")],
        // windows-shaped, LOCALAPPDATA absent — the bare-home arm
        &[("HOME", "C:\\Users\\u")],
        // both platforms' variables present at once (MSYS/Git-Bash on Windows)
        &[
            ("XDG_DATA_HOME", "/xdg"),
            ("HOME", "/home/u"),
            ("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"),
        ],
        // blank values must behave exactly like absent ones
        &[("XDG_DATA_HOME", ""), ("HOME", "/home/u"), ("LOCALAPPDATA", "")],
        // a scrubbed daemon environment
        &[],
    ];
    for pairs in cases {
        let vars = env(pairs);
        let want = user_data_dir(
            vars.get("XDG_DATA_HOME").map(String::as_str),
            vars.get("HOME").map(String::as_str),
            vars.get("LOCALAPPDATA").map(String::as_str),
        );
        assert_eq!(user_data_dir_from_vars(&vars), want, "diverged for {pairs:?}");
    }
}

/// …and the concrete answers are PINNED, not merely self-consistent: a refactor that changed
/// both forms together would still pass the equivalence test above. These are the paths a live
/// install resolves to today.
#[cfg(not(windows))]
#[test]
fn the_pinned_unix_answers() {
    assert_eq!(
        user_data_dir_from_vars(&env(&[("XDG_DATA_HOME", "/xdg"), ("HOME", "/home/u")])),
        Some(PathBuf::from("/xdg/vike-data"))
    );
    assert_eq!(
        user_data_dir_from_vars(&env(&[("HOME", "/home/u")])),
        Some(PathBuf::from("/home/u/.local/share/vike-data"))
    );
    assert_eq!(user_data_dir_from_vars(&env(&[])), None);
}

#[cfg(windows)]
#[test]
fn the_pinned_windows_answers() {
    assert_eq!(
        user_data_dir_from_vars(&env(&[
            ("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"),
            ("HOME", "C:\\Users\\u"),
        ])),
        Some(PathBuf::from("C:\\Users\\u\\AppData\\Local").join("vike-data"))
    );
    assert_eq!(
        user_data_dir_from_vars(&env(&[("HOME", "C:\\Users\\u")])),
        Some(PathBuf::from("C:\\Users\\u").join("vike-data"))
    );
    assert_eq!(user_data_dir_from_vars(&env(&[])), None);
}

#[cfg(windows)]
#[test]
fn windows_user_dir_prefers_localappdata() {
    assert_eq!(
        user_data_dir(None, Some("C:\\Users\\u"), Some("C:\\Users\\u\\AppData\\Local")),
        Some(PathBuf::from("C:\\Users\\u\\AppData\\Local").join("vike-data"))
    );
    assert_eq!(
        user_data_dir(None, Some("C:\\Users\\u"), None),
        Some(PathBuf::from("C:\\Users\\u").join("vike-data"))
    );
    assert_eq!(user_data_dir(None, None, None), None);
}

/// Every reserved character is refused — the whole set, not the two that motivated it.
///
/// ⚠ Spelled as a LOOP over the const rather than as nine cases, so a character added to
/// [`PATH_HOSTILE_IN_A_SYMBOL`] is covered the moment it is added. The reverse — a case list
/// that silently stops covering a new member — is the shape this repo's ratchets exist against.
#[test]
fn every_reserved_character_is_refused_wherever_it_sits() {
    for bad in PATH_HOSTILE_IN_A_SYMBOL {
        for candidate in [format!("{bad}BTC"), format!("BT{bad}C"), format!("BTC{bad}")] {
            let err = refuse_a_path_hostile_symbol(&candidate)
                .expect_err("a reserved character must be refused wherever it sits");
            assert!(
                err.contains(&format!("{bad:?}")),
                "the refusal must NAME the offending character, or an operator cannot act on \
                     it: {err}"
            );
        }
    }
}

/// The symbols this rule exists for, and the ones it must not disturb.
///
/// The passing half is the point: this function is a fence around a defect, and a fence that
/// also refuses working symbols is worse than none. Every spelling below is one this workspace
/// stores TODAY or would store after the catalog renders it.
#[test]
fn the_real_symbols_fall_on_the_sides_they_must() {
    for refused in [
        "HYPE/USDC", // hyperliquid spot, our unified BASE/QUOTE spelling — 328 of them
        "BTC/USD",   // alpaca crypto, the venue's own wire spelling
        "xyz:TSLA",  // a hyperliquid builder-dex perp — 289 across eleven dexes
        "para:GOLD", // ...and a second dex, so the case is not one literal
    ] {
        assert!(
            refuse_a_path_hostile_symbol(refused).is_err(),
            "{refused:?} cannot be a directory name and must be refused"
        );
    }
    for allowed in [
        "BTC",                 // hyperliquid core perp — bare coin, 234 of them
        "kPEPE",               // ...including the k-prefixed ones
        "HYPE-USDC",           // the rendered spot pair: one character changed from the venue's
        "HYPE-USDT0",          // ...and its second quote, since 12 bases have more than one
        "TSLA.d-xyz",          // the rendered builder-dex perp
        "BTCUSDT",             // binance spot
        "BTCUSDT.P",           // binance perp, the suffix that already exists
        "EUR_USD",             // oanda FX — its slash is only in `displayName`
        "BTC-1JAN27-100000-C", // deribit option
        "btc-updown-5m",       // polymarket, which keys on a group rather than a symbol
    ] {
        assert_eq!(
            refuse_a_path_hostile_symbol(allowed),
            Ok(()),
            "{allowed:?} is a symbol this workspace stores; refusing it would be a regression"
        );
    }
}

/// The two failure modes are NOT interchangeable, and the message has to say which one it is —
/// a slash is a silent mis-partition on every platform, a colon is a loud Windows-only refusal.
#[test]
fn the_refusal_distinguishes_a_silent_split_from_a_windows_refusal() {
    let slash = refuse_a_path_hostile_symbol("HYPE/USDC").expect_err("a slash is refused");
    // ⚠ Matched case-INSENSITIVELY on purpose. Written as `contains("separator")` it failed on
    // its first run against a message that says `SEPARATOR` — the assertion was pinning this
    // sentence's typography rather than its content, which is the wrong thing for a test about
    // whether an operator is told the right cause.
    assert!(
        slash.to_lowercase().contains("separator"),
        "a slash must be explained as a separator: {slash}"
    );
    let colon = refuse_a_path_hostile_symbol("xyz:TSLA").expect_err("a colon is refused");
    assert!(colon.contains("Windows"), "a colon must name the platform it breaks: {colon}");
    assert_ne!(slash, colon, "the two modes must not share one message");
}
