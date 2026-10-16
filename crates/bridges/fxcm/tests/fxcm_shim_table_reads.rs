//! **The shim's reconcile and quote reads ask the venue; they do not read the login-time snapshot.**
//! Gated on a box with no ForexConnect SDK, like `fxcm_shim_envelope_grammar.rs`: no compiler on any
//! CI runner has ever seen `crates/bridges/fxcm/src/shim/fcshim.cpp`, so a regression in WHERE it
//! reads its tables is invisible to every other gate.
//!
//! The defect this pins (measured on the demo login, 2026-10-13): the SDK documents
//! `getLoginRules()->getTableRefreshResponse(T)` as the table "loaded during login". On this login
//! `isTableLoadedByDefault` is FALSE for Orders and Trades, so that call is NULL in every session and
//! the shim turned NULL into `"[]"` — `FxcmReconClient` reported zero orders, zero fills and a flat
//! position forever — while Offers and Accounts, which ARE loaded, froze at login (a quote that never
//! moved, a balance that never changed). The cure is a refresh-table request per read
//! (`createRefreshTableRequest[ByAccount]`, the SDK's own PrintTable / GetOffers way).
//!
//! These checks read the C++ SOURCE with its comments stripped (the file's prose names the very
//! calls it forbids) and pin the SHAPE of the fix. They cannot prove the shim returns current data —
//! only a run against the demo account does (`docs/ops/fxcm-forexconnect.md`, the reconcile smoke) —
//! so each has a positive control: it must find what it looks for in a place it is known to be, or
//! the comparison below it would pass on an empty string.

use std::path::PathBuf;

/// `fcshim.cpp` with `//` and `/* */` comments removed. String literals are copied verbatim, so a
/// `//` inside one (a URL) does not eat the rest of its line.
fn shim_code() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/shim/fcshim.cpp");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the shim source {}: {e}", path.display()));
    strip_comments(&src)
}

fn strip_comments(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < c.len() {
        match (c[i], c.get(i + 1).copied()) {
            ('/', Some('/')) => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ('"', _) => {
                out.push('"');
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' && i + 1 < c.len() {
                        out.push(c[i]);
                        i += 1;
                    }
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() {
                    out.push('"');
                    i += 1;
                }
            }
            // A CHAR literal (`'"'` in `jesc`) must not open a string.
            ('\'', _) => {
                out.push('\'');
                i += 1;
                if i < c.len() && c[i] == '\\' {
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() {
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() && c[i] == '\'' {
                    out.push('\'');
                    i += 1;
                }
            }
            (ch, _) => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// The text of the exported function `name`: from its `FC_EXPORT int name(` to the next `FC_EXPORT`.
fn body_of(code: &str, name: &str) -> String {
    let opener = format!("FC_EXPORT int {name}(");
    let start = code
        .find(&opener)
        .unwrap_or_else(|| panic!("fcshim.cpp no longer defines `{name}` — the roster moved"));
    let rest = &code[start + opener.len()..];
    let end = rest.find("FC_EXPORT").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Orders and Trades are not loaded at login, so the login rules never hold them: a read of either
/// through `getTableRefreshResponse` is NULL, and the shim's old `"[]"` for NULL made the reconcile
/// client blind. (ClosedTrades is not loaded either.)
#[test]
fn orders_and_trades_are_never_read_from_the_login_rules() {
    let code = shim_code();

    // Positive control: the finder sees a login-rules read where one legitimately remains. The
    // Accounts snapshot is `firstAccount`'s (the account's IDENTITY), the Offers snapshot is
    // `instrumentForOfferId`'s and the market path's (a NAME and an id, which do not move).
    assert!(
        code.contains("getTableRefreshResponse(Accounts)")
            && code.contains("getTableRefreshResponse(Offers)"),
        "the comment stripper or the finder is broken: the two legitimate login-snapshot reads \
         (`firstAccount`, `instrumentForOfferId`) were not found in the stripped source"
    );

    for table in ["Orders", "Trades", "ClosedTrades"] {
        assert!(
            !code.contains(&format!("getTableRefreshResponse({table})")),
            "fcshim.cpp reads the {table} table through `getTableRefreshResponse`. That is the \
             table \"loaded during login\" and {table} is NOT loaded by default, so the call is \
             NULL in every session — the shim reports an empty table and the reconcile client is \
             blind. Ask the venue with a refresh-table request (`refreshTable`)."
        );
    }
}

/// Each read the reconcile client or the order path depends on being CURRENT goes through a
/// refresh-table request: the Orders and Trades snapshots, the balance, and a LIMIT entry's quote.
#[test]
fn every_current_read_is_a_refresh_table_request() {
    let code = shim_code();

    // `refreshTable` is the only place a request is built, and it builds both SDK spellings.
    assert!(code.contains("createRefreshTableRequestByAccount(table, account)"));
    assert!(code.contains("createRefreshTableRequest(table)"));

    let orders = body_of(&code, "fc_orders");
    assert!(
        orders.contains("refreshForAccount(h->session, Orders,"),
        "fc_orders no longer refreshes the Orders table for the tradable account: {orders}"
    );
    let trades = body_of(&code, "fc_trades");
    assert!(
        trades.contains("refreshForAccount(h->session, Trades,"),
        "fc_trades no longer refreshes the Trades table for the tradable account: {trades}"
    );
    let account = body_of(&code, "fc_account");
    assert!(
        account.contains("refreshTable(h->session, Accounts,"),
        "fc_account no longer refreshes the Accounts table for the balance: {account}"
    );

    // `refreshForAccount` is what ties the reconcile reads to the account order flow targets.
    assert!(
        code.contains("firstAccount(s);") && code.contains("account->getAccountID(), resp, err"),
        "refreshForAccount no longer refreshes by the account `firstAccount` selects"
    );
}

/// Only a quote that is PRICED OFF must be current: a LIMIT entry whose rate the shim derives from the
/// quote reads a refreshed Offers table; a market order, and a limit at a rate the CALLER named
/// (`fc_place_entry_at`), keep the login snapshot (they use the offer's id and digits alone and gain
/// nothing from a round trip that can only add a failure). One flag decides it, so pin the flag.
#[test]
fn only_a_limit_entry_pays_for_a_fresh_quote() {
    let code = shim_code();
    assert!(
        code.contains("const bool fresh = !market && !exact_rate;")
            && code.contains("findOffer(h->session, instrument, fresh,"),
        "fc_place no longer derives findOffer's `fresh` flag from `!market && !exact_rate`: a limit \
         entry priced off the quote must read it as it stands now, while a market order and a \
         limit at a caller-named rate must not depend on a refresh"
    );
    assert!(
        code.contains("refreshTable(s, Offers, 0, out.resp, err)"),
        "findOffer's fresh branch no longer asks the venue for the Offers table"
    );
}

/// A failed read is a negative return, never an empty table: the Orders and Trades readers return
/// the refresh failure code BEFORE any row is formatted, and the reason travels in `out`.
#[test]
fn a_failed_refresh_returns_its_code_before_any_row_is_formatted() {
    let code = shim_code();
    for name in ["fc_orders", "fc_trades"] {
        let body = body_of(&code, name);
        let fail = body
            .find("if (rc != 0) { cpy(out, out_len, err.c_str()); return rc; }")
            .unwrap_or_else(|| panic!("{name} has no refresh-failure return: {body}"));
        let first_row = body.find("snprintf(").expect("the row formatter is in the body");
        assert!(fail < first_row, "{name} formats rows before it checks the refresh");
        assert!(
            !body.contains("return emit_json(\"[]\""),
            "{name} returns a literal empty array on some path"
        );
    }
}

/// The comment stripper is the load-bearing half of every check above, so it gets its own control.
#[test]
fn the_comment_stripper_removes_prose_and_keeps_code_and_strings() {
    let src =
        "a // getTableRefreshResponse(Orders)\n/* getTableRefreshResponse(Trades) */ b \"//x\" c";
    let out = strip_comments(src);
    assert!(!out.contains("getTableRefreshResponse"), "{out}");
    assert!(out.contains("\"//x\""), "a string literal survives verbatim: {out}");
    assert!(out.contains('a') && out.contains('b') && out.contains('c'), "{out}");

    // A char literal holding a quote does not open a string (fcshim.cpp's `jesc` has one), so a
    // comment AFTER it is still a comment.
    let out = strip_comments("if (*p=='\"') x(); // getTableRefreshResponse(Orders)\ny();");
    assert!(!out.contains("getTableRefreshResponse"), "{out}");
    assert!(out.contains("y();"), "{out}");

    // …and the real file strips clean: its prose names the forbidden call, its code does not.
    let real = shim_code();
    assert!(!real.contains("getTableRefreshResponse(T)"), "the doc prose survived stripping");
}

/// **A requested limit price reaches the wire as the order's RATE (shim ABI 2).** The Rust adapter
/// refused every priced limit from #1470 because the only entry point took an `int pips_away`; the
/// SDK never limited the rate (`Rate` is a double in the value map). This pins the shape that cured
/// it, since no CI box compiles the C++: the version is at least 2, the two ABI 2 exports exist,
/// the priced entry hands the CALLER's rate to `fc_place` (not a pip distance), an off-grid rate is
/// refused rather than rounded, and the amend is an `EditOrder` carrying the order id and the rate.
/// The loader's side of the same contract is `loader.rs`'s `bind_opt!` gated on `abi_version >= 2`.
#[test]
fn a_requested_limit_rate_and_an_amend_are_wired_through_abi_2() {
    let code = shim_code();
    let version: u32 = code
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FC_ABI_VERSION"))
        .and_then(|v| v.trim().parse().ok())
        .expect("fcshim.cpp no longer defines FC_ABI_VERSION as a number");
    assert!(version >= 2, "the priced entry and the amend are ABI 2; the shim reports {version}");

    let at = body_of(&code, "fc_place_entry_at");
    assert!(
        at.contains("&rate"),
        "fc_place_entry_at must pass the caller's rate to fc_place: {at}"
    );
    let place = {
        let start = code.find("static int fc_place(").expect("fc_place is gone");
        code[start..].split("FC_EXPORT").next().unwrap_or_default().to_string()
    };
    assert!(
        place.contains("snapRate(") && place.contains("*exact_rate"),
        "fc_place no longer grid-checks a caller-named rate: {place}"
    );
    let snap = {
        let start = code.find("static int snapRate(").expect("snapRate is gone");
        code[start..].split("extern \"C\"").next().unwrap_or_default().to_string()
    };
    assert!(
        snap.contains("return -10") && snap.contains("nearest / scale"),
        "snapRate must REFUSE an off-grid rate (-10) and only absorb float noise: {snap}"
    );

    let edit = body_of(&code, "fc_edit_order");
    for needle in [
        "O2G2::Commands::EditOrder",
        "setString(OrderID, orderId)",
        "setString(AccountID, accountId)",
        "setDouble(Rate, snapped)",
        "snapRate(",
    ] {
        assert!(edit.contains(needle), "fc_edit_order lost `{needle}`: {edit}");
    }

    // The loader gates BOTH on the version as well as on the name.
    let loader =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/loader.rs"))
            .expect("read loader.rs")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    for sym in ["fc_place_entry_at", "fc_edit_order"] {
        assert!(
            loader.contains(&format!("if abi_version >= 2 {{ bind_opt!(\"{sym}\"")),
            "loader.rs must bind `{sym}` optionally and only at ABI 2"
        );
    }
}
