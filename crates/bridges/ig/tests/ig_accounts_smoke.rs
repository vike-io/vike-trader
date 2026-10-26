//! Live demo smoke: WHICH accounts the IG demo login holds, and what the account rule
//! (`crates/bridges/ig/src/account_pick.rs`) does with them. Two tests.
//!
//!     cargo test -p vike-ig --test ig_accounts_smoke -- --ignored --nocapture ig_accounts_read_smoke
//!     cargo test -p vike-ig --test ig_accounts_smoke -- --ignored --nocapture ig_account_switch_smoke
//!
//! `ig_accounts_read_smoke` is READ-ONLY: it logs in (`POST /session`), lists `GET /accounts` and `GET
//! /session`, prints one line per account (type, currency, status, preferred, the LAST THREE characters of
//! the id: never a token, a credential or a whole account number), and only if the login's default already is
//! an enabled USD CFD account does it run the production login (`IgSession::login`), which then sends nothing
//! but the same two reads. It never sends a `PUT`.
//!
//! `ig_account_switch_smoke` is the one test that SWITCHES, and only THIS test's own sessions: `PUT /session`
//! with `defaultAccount: false` (the login's default is never written). Run it BY NAME only (the running daemon
//! holds a session of the same login, so it is not for a blanket `--ignored` run and not in the live-smoke
//! workflow); it proves, by a second session opened BEFORE the switch, that the switch is per session, and if
//! the login's default ever turned out different afterwards it puts it back (`defaultAccount: true`) and says
//! RESTORED.
//!
//! Double-gated like every other `*_smoke.rs`: absent demo-account creds, then a failed demo login, are a
//! printed skip and never a red.

mod common;

use common::cfg;
use serde_json::{Value, json};
use vike_ig::{IgConfig, IgSession};

/// The last three characters of an account id: all this smoke ever prints of one.
fn tail3(id: &str) -> String {
    let n = id.chars().count();
    format!("..{}", id.chars().skip(n.saturating_sub(3)).collect::<String>())
}

/// What a raw call answered: the status, the body, and the token pair when the answer carried one.
struct Raw {
    status: u16,
    body: Value,
    cst: Option<String>,
    xst: Option<String>,
}

fn answer(mut resp: ureq::http::Response<ureq::Body>) -> Raw {
    let status = resp.status().as_u16();
    let hdr =
        |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let (cst, xst) = (hdr("CST"), hdr("X-SECURITY-TOKEN"));
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    Raw { status, body: serde_json::from_str(&text).unwrap_or(Value::Null), cst, xst }
}

fn login_raw(agent: &ureq::Agent, c: &IgConfig) -> Option<Raw> {
    let body = json!({ "identifier": c.identifier, "password": c.password }).to_string();
    let resp = agent
        .post(&format!("{}/session", c.rest_base))
        .header("X-IG-API-KEY", &c.api_key)
        .header("Version", "2")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body.as_bytes())
        .ok()?;
    let raw = answer(resp);
    (raw.status == 200 && raw.cst.is_some() && raw.xst.is_some()).then_some(raw)
}

fn get_raw(agent: &ureq::Agent, c: &IgConfig, s: &Raw, path: &str) -> Raw {
    answer(
        agent
            .get(&format!("{}{path}", c.rest_base))
            .header("X-IG-API-KEY", &c.api_key)
            .header("CST", s.cst.as_deref().unwrap_or_default())
            .header("X-SECURITY-TOKEN", s.xst.as_deref().unwrap_or_default())
            .header("Version", "1")
            .header("Accept", "application/json")
            .call()
            .expect("GET"),
    )
}

fn put_session_raw(agent: &ureq::Agent, c: &IgConfig, s: &Raw, id: &str, default: bool) -> Raw {
    let body = json!({ "accountId": id, "defaultAccount": default }).to_string();
    answer(
        agent
            .put(&format!("{}/session", c.rest_base))
            .header("X-IG-API-KEY", &c.api_key)
            .header("CST", s.cst.as_deref().unwrap_or_default())
            .header("X-SECURITY-TOKEN", s.xst.as_deref().unwrap_or_default())
            .header("Version", "1")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(body.as_bytes())
            .expect("PUT"),
    )
}

fn logout_raw(agent: &ureq::Agent, c: &IgConfig, s: &Raw) {
    let _ = agent
        .delete(&format!("{}/session", c.rest_base))
        .header("X-IG-API-KEY", &c.api_key)
        .header("CST", s.cst.as_deref().unwrap_or_default())
        .header("X-SECURITY-TOKEN", s.xst.as_deref().unwrap_or_default())
        .header("Version", "1")
        .call();
}

fn keys(v: &Value) -> String {
    v.as_object().map_or_else(String::new, |o| o.keys().cloned().collect::<Vec<_>>().join(","))
}

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("-")
}

/// One printed line per account of a `GET /accounts` body; returns `(id, type, currency, status, preferred)`.
fn print_accounts(body: &Value) -> Vec<(String, String, String, String, bool)> {
    let rows = body.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
    println!(
        "FINDING GET /accounts: {} account(s); keys of the first row: [{}]",
        rows.len(),
        rows.first().map_or_else(String::new, keys)
    );
    rows.iter()
        .map(|r| {
            let id = text(r, "accountId").to_string();
            let preferred = r.get("preferred").and_then(Value::as_bool).unwrap_or(false);
            println!(
                "FINDING   account {}  type={}  currency={}  status={}  preferred={preferred}",
                tail3(&id),
                text(r, "accountType"),
                text(r, "currency"),
                text(r, "status")
            );
            (
                id,
                text(r, "accountType").to_ascii_uppercase(),
                text(r, "currency").to_ascii_uppercase(),
                text(r, "status").to_ascii_uppercase(),
                preferred,
            )
        })
        .collect()
}

#[test]
#[ignore = "live demo; needs a demo ig account's credentials: read-only, never a PUT (see module doc)"]
fn ig_accounts_read_smoke() {
    let Some(c) = cfg() else {
        eprintln!("skip: no demo ig account credentials");
        return;
    };
    let agent = vike_bridge_core::http::blocking_agent();
    let Some(login) = login_raw(&agent, &c) else {
        eprintln!("skip: IG demo login failed (creds present but no session)");
        return;
    };
    let default = text(&login.body, "currentAccountId").to_string();
    println!(
        "FINDING POST /session v2: keys=[{}]  currentAccountId={}  accountType={}  currencyIsoCode={}",
        keys(&login.body),
        tail3(&default),
        text(&login.body, "accountType"),
        text(&login.body, "currencyIsoCode"),
    );
    if let Some(list) = login.body.get("accounts").and_then(Value::as_array) {
        println!(
            "FINDING POST /session v2 `accounts`: {} row(s); keys of the first: [{}]",
            list.len(),
            list.first().map_or_else(String::new, keys)
        );
    }
    let listed = get_raw(&agent, &c, &login, "/accounts");
    assert_eq!(listed.status, 200, "GET /accounts");
    let rows = print_accounts(&listed.body);
    assert!(!rows.is_empty(), "a login lists at least one account");
    let now = get_raw(&agent, &c, &login, "/session");
    println!(
        "FINDING GET /session v1: status={} keys=[{}] accountId={} currency={}",
        now.status,
        keys(&now.body),
        tail3(text(&now.body, "accountId")),
        text(&now.body, "currency"),
    );
    let fits = |r: &(String, String, String, String, bool)| {
        r.1 == "CFD" && r.2 == "USD" && r.3 == "ENABLED"
    };
    let fitting = rows.iter().filter(|r| fits(r)).count();
    println!("FINDING usd-cfd accounts: {fitting} of {}", rows.len());
    // The row's book, when the store names one: the id's tail only.
    let dir = vike_secrets::smoke_directory();
    let book = dir
        .account_row(
            "ig",
            &vike_model::accounts::account_keys::AccountLabel::Default,
            vike_model::AccountTier::Demo,
        )
        .and_then(|r| r.venue_account_id.clone());
    println!(
        "FINDING the demo row's venue_account_id: {}",
        book.as_deref().map_or("not set".to_string(), tail3)
    );

    match rows.iter().find(|r| r.0 == default) {
        Some(d) if fits(d) => {
            // The production login now sends the same two reads and no PUT: the default is the account.
            let session = IgSession::login(&c).expect("the production login settles");
            println!(
                "FINDING production login: account {} currency={:?}  (the default already fits: no PUT sent)",
                tail3(&session.account_id),
                session.account_currency
            );
            assert_eq!(session.account_id, default, "a fitting default is never moved");
            session.logout();
        }
        other => println!(
            "FINDING the default {} is not an enabled USD CFD account (type/currency/status: {:?}): the rule would \
             move or refuse this login. NOT run here: it needs a PUT, see ig_account_switch_smoke",
            tail3(&default),
            other.map(|r| (&r.1, &r.2, &r.3))
        ),
    }
    logout_raw(&agent, &c, &login);
}

#[test]
#[ignore = "live demo; SWITCHES this test's own sessions (defaultAccount false): run by name only (see module doc)"]
fn ig_account_switch_smoke() {
    let Some(c) = cfg() else {
        eprintln!("skip: no demo ig account credentials");
        return;
    };
    let agent = vike_bridge_core::http::blocking_agent();
    let Some(observer) = login_raw(&agent, &c) else {
        eprintln!("skip: IG demo login failed");
        return;
    };
    let default = text(&observer.body, "currentAccountId").to_string();
    let listed = get_raw(&agent, &c, &observer, "/accounts");
    let rows = print_accounts(&listed.body);
    println!("FINDING the login's default is {}", tail3(&default));
    let Some(other) = rows.iter().find(|r| r.0 != default && r.3 == "ENABLED") else {
        println!("FINDING one account only: no switch to another account is possible");
        // What CAN be read: IG's answer to the request SHAPE the bridge sends, asked for the account the session
        // is already on (it moves nothing, and `defaultAccount` is false, so the login's default is not written).
        let same = put_session_raw(&agent, &c, &observer, &default, false);
        println!(
            "FINDING PUT /session v1 {{accountId: <the current one>, defaultAccount: false}} -> status={} errorCode={} body keys=[{}] CST header: {} X-SECURITY-TOKEN header: {}",
            same.status,
            text(&same.body, "errorCode"),
            keys(&same.body),
            same.cst.is_some(),
            same.xst.is_some()
        );
        let after = get_raw(&agent, &c, &observer, "/session");
        println!(
            "FINDING the session still reads accountId {}",
            tail3(text(&after.body, "accountId"))
        );
        logout_raw(&agent, &c, &observer);
        return;
    };

    // 1. the production login: what the rule does with THIS login (a PUT only when the default does not fit).
    let session = IgSession::login(&c);
    match &session {
        Ok(s) => {
            let confirmed =
                s.get("/session", "1", "").expect("GET /session on the settled session");
            println!(
                "FINDING production login: default {} -> session account {}  GET /session says {}  currency={:?}",
                tail3(&default),
                tail3(&s.account_id),
                tail3(text(&confirmed, "accountId")),
                s.account_currency
            );
            assert_eq!(
                text(&confirmed, "accountId"),
                s.account_id,
                "the session really is on the account it names"
            );
        }
        Err(e) => println!("FINDING production login REFUSED (as the rule says it should): {e}"),
    }

    // 2. the raw switch of a third session to `other`, to read what the answer carries and that it is per session.
    let probe = login_raw(&agent, &c).expect("a second login");
    let moved = put_session_raw(&agent, &c, &probe, &other.0, false);
    println!(
        "FINDING PUT /session -> {}: status={} body keys=[{}]  CST header: {}  X-SECURITY-TOKEN header: {} (differs from the login's: {})",
        tail3(&other.0),
        moved.status,
        keys(&moved.body),
        moved.cst.is_some(),
        moved.xst.is_some(),
        moved.xst.is_some() && moved.xst != probe.xst,
    );
    assert!((200..300).contains(&moved.status), "the switch is accepted: {:?}", moved.body);
    let on_probe = moved.xst.clone().map_or_else(
        || get_raw(&agent, &c, &probe, "/session"),
        |xst| {
            get_raw(
                &agent,
                &c,
                &Raw {
                    xst: Some(xst),
                    cst: moved.cst.clone().or(probe.cst.clone()),
                    status: 0,
                    body: Value::Null,
                },
                "/session",
            )
        },
    );
    println!(
        "FINDING the switched session reads accountId {}",
        tail3(text(&on_probe.body, "accountId"))
    );
    assert_eq!(
        text(&on_probe.body, "accountId"),
        other.0,
        "the switch took effect on its own session"
    );

    // 3. per session: the observer opened BEFORE the switch is where it was, and a fresh login still lands on the default.
    let observer_now = get_raw(&agent, &c, &observer, "/session");
    let fresh = login_raw(&agent, &c).expect("a third login");
    let fresh_default = text(&fresh.body, "currentAccountId").to_string();
    println!(
        "FINDING after the switch: the earlier session reads {}, a fresh login lands on {} (the default was {})",
        tail3(text(&observer_now.body, "accountId")),
        tail3(&fresh_default),
        tail3(&default)
    );
    if fresh_default != default || text(&observer_now.body, "accountId") != default {
        // Never leave the login's default (or another session) moved: put it back, loudly.
        let back = put_session_raw(&agent, &c, &fresh, &default, true);
        println!("FINDING RESTORED the default {}: status {}", tail3(&default), back.status);
        panic!("the switch was NOT per session; the default was put back");
    }
    for s in [&probe, &fresh, &observer] {
        logout_raw(&agent, &c, s);
    }
    if let Ok(s) = session {
        s.logout();
    }
}
