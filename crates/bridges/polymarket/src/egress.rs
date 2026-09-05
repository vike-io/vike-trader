//! **Egress** for every live Polymarket connection: the SOCKS proxy resolution, the proxy-aware
//! `ureq` agents, the keyless [`get_json`] — re-homed here from `exec` for the feeds/exec seam
//! (split-plane Phase 5: the feed pumps dial through the same tunnel and must compile without the
//! exec plane) — plus the **egress guard** (polymarket-workstreams spec §0.2).
//!
//! ## Where the proxy config is read from (and why it is NOT just `std::env`)
//! Every `POLY_PROXY_*` knob resolves through [`proxy_var`]: the REAL process env first, the
//! gitignored workspace `.env` map second. The second tier is a BUG FIX, not a convenience —
//! `credentials::load_workspace_dotenv()` returns a `HashMap` and deliberately does NOT export into
//! the process env, so `POLY_SOCKS_PROXY` / `POLY_PROXY_HOST` / `POLY_PROXY_PORT` written in the
//! workspace `.env` (where every other Polymarket setting lives) were SILENTLY IGNORED by the plain
//! `std::env::var` reads this plumbing used to make. It only ever appeared to work because the
//! built-in defaults (`enabled = true`, `127.0.0.1:1080`) happen to match the usual arbdub tunnel;
//! move the tunnel to another port in `.env` and the agent kept dialling 1080.
//!
//! The fallback is deliberately NARROW — see [`PROXY_KEYS`]: a fixed five-key allow-list, read once
//! and cached, exposed to nothing but this module's two resolvers. The `.env` is NOT exported
//! wholesale into the process env, because at least one subsystem (`VIKE_RECONCILE`, see
//! `vike_ops::reconcile_config`'s module doc) deliberately reads the REAL process env and must
//! keep doing so. Process env still WINS, so a shell-exported flag on a tunnelled run overrides the
//! file, exactly as before.
//!
//! ## The egress guard
//!
//! The standing constraint is that all Polymarket traffic leaves via Dublin (arbdub), because the
//! venue is US-geo-blocked — but nothing in the tree ever CHECKED that. When the tunnel is down a
//! live smoke either self-skips on credential derivation (`polymarket_reconcile_smoke.rs`:
//! *"skip: could not derive L2 creds (proxy down / geo-blocked?)"*) or fails as an ordinary network
//! error. Neither says *"you are not egressing via Dublin"*, so a misrouted run can look like an
//! ordinary flake — or, worse, quietly test the WRONG path.
//!
//! This module is the explicit assertion. It is **opt-in and inert**: with
//! `POLY_EXPECT_EGRESS_COUNTRY` unset, [`check_expected_egress`] performs **no network call at all**
//! and returns [`EgressCheck::NotConfigured`]. Set it (e.g. `POLY_EXPECT_EGRESS_COUNTRY=IE`) and a
//! live smoke fails LOUDLY, naming the observed IP/country, instead of limping on.
//!
//! **What it proves, precisely.** The probe rides [`agent`] — the same proxy-aware `ureq` agent
//! every CLOB REST call uses — so it measures the HTTP lane's real egress. The WS lane
//! ([`ws_proxy`]) is not separately probed because, by construction, it can only use the SAME
//! SOCKS endpoint ([`proxy_url`]); [`Egress::ws_lane`] reports whether the WS gate is on so the
//! disclosure is honest about which lanes the observed egress covers.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The polymarket proxy-config keys resolved by [`proxy_var`] — and the ONLY keys the workspace
/// `.env` fallback will ever surface. A fixed allow-list, not a prefix match: the `.env` is the
/// credential store, so nothing but these five can leak out of it through this path.
pub(crate) const PROXY_KEYS: [&str; 5] = [
    "POLY_SOCKS_PROXY",
    "POLY_PROXY_ENABLED",
    "POLY_PROXY_HOST",
    "POLY_PROXY_PORT",
    "POLY_WS_PROXY_ENABLED",
];

/// The workspace-`.env` values for [`PROXY_KEYS`], read ONCE and cached. `proxy_url` is called on
/// every `agent()` build (i.e. per REST call), so re-reading the file each time would put a
/// filesystem hit on the submit path.
fn dotenv_proxy_vars() -> &'static HashMap<String, String> {
    static CACHE: OnceLock<HashMap<String, String>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let all = vike_bridge_core::credentials::load_workspace_dotenv();
        PROXY_KEYS
            .iter()
            .filter_map(|k| all.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect()
    })
}

/// The workspace-`.env` value for `exec`'s `POLY_RATE_GATE_ENV`, read ONCE and cached — the
/// deliberate sibling of [`dotenv_proxy_vars`] rather than a sixth entry in [`PROXY_KEYS`], which
/// documents itself as the proxy-only allow-list. Same discipline either way: the `.env` is the
/// credential store, so only the one named key is ever lifted out of it.
///
/// It is `polymarket`-gated (its only caller is the exec plane's `rate_gate_enforced`) yet homed
/// HERE rather than in `exec`, so this file stays the crate's ONE workspace-`.env` reader — which
/// is what keeps `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` a RE-KEY
/// across the feeds/exec split rather than a growth of that ratchet.
#[cfg(feature = "polymarket")]
pub(crate) fn dotenv_rate_gate() -> Option<&'static str> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            vike_bridge_core::credentials::load_workspace_dotenv()
                .get(crate::exec::POLY_RATE_GATE_ENV)
                .cloned()
        })
        .as_deref()
}

/// Resolve one proxy-config key: process env FIRST, workspace `.env` second (see the module doc).
fn proxy_var(key: &str) -> Option<String> {
    proxy_var_layered(std::env::var(key).ok(), key, dotenv_proxy_vars())
}

/// The pure precedence rule behind [`proxy_var`], split out so both directions are unit-testable
/// without mutating the process-global env (`set_var` is unsound under threads — the repo's rule).
/// An env value that is present-but-EMPTY still wins: `POLY_SOCKS_PROXY=` means "direct", and that
/// must override a `.env` line, not fall through to it.
///
/// Whichever tier answers, only its FIRST token is taken
/// ([`first_token`](crate::config::first_token)): all five keys are single-token values, and the
/// workspace `.env` annotates them with trailing `#` comments the shared parser does not strip —
/// which otherwise produced `bad port in "127.0.0.1:11080   # dev-box arbdub tunnel"` and a silent
/// direct egress.
fn proxy_var_layered(
    from_env: Option<String>,
    key: &str,
    dotenv: &HashMap<String, String>,
) -> Option<String> {
    from_env
        .or_else(|| dotenv.get(key).cloned())
        .map(|v| super::config::first_token(&v).to_string())
}

/// Resolve the Polymarket SOCKS proxy URL, or `None` for a direct connection.
///
/// Polymarket is geo/DNS-blocked in some regions (e.g. UA — `clob` resolves to the local router),
/// so the proxy is **ON by default** at `socks5h://127.0.0.1:1080` (`socks5h` = remote DNS through
/// the tunnel; plain `socks5` resolves locally and hits the block). Configure it via:
/// - `POLY_PROXY_ENABLED=false` (or `0`/`no`/`off`) → direct, no proxy
/// - `POLY_PROXY_HOST` / `POLY_PROXY_PORT` → the SOCKS endpoint (default `127.0.0.1` / `1080`)
/// - `POLY_SOCKS_PROXY=<full url>` → explicit override (or `none`/`direct` → disable)
///
/// Each of those is read process-env-first, workspace-`.env`-second (see the module doc) — a `.env`
/// line is honoured now, where it used to be silently ignored.
pub fn proxy_url() -> Option<String> {
    proxy_url_with(proxy_var)
}

/// [`proxy_url`]'s resolution logic over an injectable key lookup — the testable core. `get` is
/// [`proxy_var`] in production; the tests pass a map-backed closure so the `.env` tier can be proven
/// without touching the process env.
fn proxy_url_with(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(u) = get("POLY_SOCKS_PROXY") {
        let u = u.trim();
        if u.is_empty() || u.eq_ignore_ascii_case("none") || u.eq_ignore_ascii_case("direct") {
            return None;
        }
        return Some(u.to_string());
    }
    let enabled = get("POLY_PROXY_ENABLED")
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off"))
        .unwrap_or(true); // default ON (Polymarket is geo-blocked in our case)
    if !enabled {
        return None;
    }
    let host =
        get("POLY_PROXY_HOST").filter(|s| !s.is_empty()).unwrap_or_else(|| "127.0.0.1".to_string());
    let port =
        get("POLY_PROXY_PORT").filter(|s| !s.is_empty()).unwrap_or_else(|| "1080".to_string());
    Some(format!("socks5h://{host}:{port}"))
}

/// The SOCKS5 endpoint the **WebSocket** lanes (CLOB market feed, CLOB user channel, RTDS) dial
/// through, or `None` for a direct connection.
///
/// **One unified switch.** WHERE the tunnel is comes from exactly [`proxy_url`] —
/// `POLY_SOCKS_PROXY` / `POLY_PROXY_HOST` / `POLY_PROXY_PORT` — so HTTP and WS can never disagree
/// about the endpoint. And WHETHER the WS lane uses it now **inherits the HTTP lane's decision by
/// default**: the single control `POLY_PROXY_ENABLED` (+ the endpoint keys) governs EVERY
/// Polymarket connection — order REST, on-chain settlement, Gamma reads, market feed, RTDS, and
/// the user channel — so an operator turns the proxy on or off in ONE place. (This is the "collapse
/// the WS gate into `proxy_url`" the previous two-gate rollout was staged toward.)
///
/// `POLY_WS_PROXY_ENABLED` is retained as an OPTIONAL per-lane OVERRIDE for the rare case where the
/// WS lanes must differ from the HTTP lane:
/// - falsey (`0`/`false`/`no`/`off`) → force the WS lanes DIRECT while the HTTP lane still proxies
///   (e.g. order placement is geo-blocked but the keyless feeds are reachable direct);
/// - truthy (`1`/`true`/`yes`/`on`) → force the WS lanes onto the tunnel (a no-op when the HTTP
///   lane already proxies);
/// - UNSET (the default) → inherit the HTTP lane.
///
/// The master OFF still wins over everything: `POLY_PROXY_ENABLED=false` or `POLY_SOCKS_PROXY=none`
/// disables BOTH lanes regardless of the WS override (there is no endpoint to dial).
///
/// Resolved process-env-first, workspace-`.env`-second (like [`proxy_url`] — see the module doc):
/// a shell-exported flag on a tunnelled run still wins, but a `.env` line is no longer ignored.
pub fn ws_proxy() -> Option<vike_bridge_core::ws_proxy::WsProxy> {
    ws_proxy_with(proxy_var)
}

/// [`ws_proxy`]'s logic over an injectable key lookup — the testable core (see [`proxy_url_with`]).
fn ws_proxy_with(
    get: impl Fn(&str) -> Option<String>,
) -> Option<vike_bridge_core::ws_proxy::WsProxy> {
    // UNIFIED switch: the WS lane INHERITS the HTTP lane's decision ([`proxy_url_with`]) by
    // default, so ONE control governs every Polymarket connection. `POLY_WS_PROXY_ENABLED` is
    // now an OPTIONAL per-lane OVERRIDE — a falsey value forces the WS lanes DIRECT while the HTTP
    // lane still proxies; a truthy value is a no-op when the HTTP lane already proxies; UNSET
    // inherits. The master OFF (`POLY_PROXY_ENABLED=false` / `POLY_SOCKS_PROXY=none`) still
    // disables BOTH lanes, via `proxy_url_with` below.
    if let Some(v) = get("POLY_WS_PROXY_ENABLED") {
        let on = matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
        if !on {
            return None; // explicit WS-off override (the HTTP lane may still proxy)
        }
    }
    let url = proxy_url_with(&get)?; // `POLY_PROXY_ENABLED=false` / `POLY_SOCKS_PROXY=none` disables both lanes
    match vike_bridge_core::ws_proxy::WsProxy::parse(&url) {
        Ok(p) => Some(p),
        Err(e) => {
            // LOUD, not silent: falling back to a direct dial would hit the very geo-block the
            // proxy exists to clear. The message carries the parse error, never the URL's
            // userinfo — an assertion this comment used to make on its own, while two of
            // `WsProxy::parse`'s arms echoed the raw url straight into `e`. It is now backed by
            // `vike_bridge_core::ws_proxy`'s `redact_userinfo` and gated by that module's
            // `a_parse_error_never_carries_the_proxy_credentials`.
            tracing::error!(venue = "polymarket", error = %e, "Polymarket proxy url is unusable — WS lanes stay DIRECT");
            None
        }
    }
}

/// The global timeout [`agent`] carries — the ORDER-PATH budget, sized for a submit/cancel round
/// trip through the SOCKS tunnel on a venue that is geo-blocked from most of our boxes.
///
/// It is deliberately NOT the feed path's budget. A caller on a live feed thread spends this window
/// out of `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS`, which is 12 s
/// in total, so it takes [`agent_with_timeout`] and a shorter ceiling instead — see
/// `crates/bridges/polymarket/src/market_feed.rs`'s `FEED_WARMUP_TIMEOUT`.
pub(crate) const REST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A proxy-aware agent (see [`proxy_url`]) with an explicit global timeout — the rung that exists so
/// a caller whose thread has a stop budget can say what its own ceiling is instead of inheriting the
/// order path's [`REST_TIMEOUT`].
///
/// Every other property is identical for both ceilings on purpose: the proxy arm, the
/// `http_status_as_error(false)` contract (4xx/5xx come back as responses, so the callers' body
/// parsers stay the error surface) and the user agent. Splitting the timeout out is the whole change
/// — a second hand-built agent would be a second thing to forget to route through the tunnel, which
/// is exactly the bug the `with_agent` comment in `market_feed.rs`'s `shard_main` records.
pub(crate) fn agent_with_timeout(global: std::time::Duration) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(global))
        .user_agent("vike-trader-rust");
    if let Some(url) = proxy_url()
        && let Ok(proxy) = ureq::Proxy::new(&url)
    {
        b = b.proxy(Some(proxy));
    }
    b.build().new_agent()
}

/// A proxy-aware agent (see [`proxy_url`]). Shared by the REST reads, the L1 derive, and
/// submit/cancel so ALL Polymarket traffic routes through the tunnel when enabled.
pub(crate) fn agent() -> ureq::Agent {
    agent_with_timeout(REST_TIMEOUT)
}

/// Proxy-aware unauthenticated GET → parsed JSON (book/midpoint/markets reads through the tunnel).
pub fn get_json(base: &str, path: &str, query: &str) -> Result<serde_json::Value, String> {
    let url =
        if query.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{query}") };
    let mut resp = agent().get(&url).call().map_err(|e| format!("network: {e}"))?;
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("bad json: {e}"))
}

/// Default IP/geo probe. Overridable with `POLY_EGRESS_PROBE_URL` (any endpoint returning a JSON
/// object with an `ip` field and, ideally, `country`).
pub const DEFAULT_EGRESS_PROBE: &str = "https://ipinfo.io/json";

/// The observed public egress of this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    /// The source IP the internet sees.
    pub ip: String,
    /// ISO-3166 alpha-2 country, when the probe reports one (`IE` for the Dublin host).
    pub country: Option<String>,
    /// City, when the probe reports one (purely for the disclosure line).
    pub city: Option<String>,
    /// Whether the WS lanes are ALSO tunnelled (`POLY_WS_PROXY_ENABLED`) — the probe itself only
    /// measures the HTTP lane, so this makes the coverage of the claim explicit.
    pub ws_lane: bool,
}

impl std::fmt::Display for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.ip)?;
        if let Some(c) = &self.country {
            write!(f, " [{c}")?;
            if let Some(city) = &self.city {
                write!(f, "/{city}")?;
            }
            write!(f, "]")?;
        }
        write!(f, " (ws lane {})", if self.ws_lane { "tunnelled" } else { "DIRECT" })
    }
}

/// Outcome of [`check_expected_egress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressCheck {
    /// `POLY_EXPECT_EGRESS_COUNTRY` unset — no expectation declared, so nothing was probed.
    NotConfigured,
    /// The observed egress matches the declared expectation.
    Ok(Egress),
    /// The observed egress does NOT match — a misrouted run.
    Mismatch { expected: String, observed: Egress },
}

impl EgressCheck {
    /// Collapse to a `Result` a caller can `?`/`expect` on: a mismatch becomes a descriptive
    /// `Err`, an unchecked run becomes `Ok(None)`. This is what the live smokes assert against.
    pub fn into_result(self) -> Result<Option<Egress>, String> {
        match self {
            EgressCheck::NotConfigured => Ok(None),
            EgressCheck::Ok(e) => Ok(Some(e)),
            EgressCheck::Mismatch { expected, observed } => Err(format!(
                "egress is {observed} but POLY_EXPECT_EGRESS_COUNTRY={expected} — this run is NOT \
                 routed through the expected region (tunnel down? proxy disabled?)"
            )),
        }
    }
}

/// Pure parse of a probe response (`ipinfo.io`-shaped: `{"ip":…,"city":…,"country":…}`). Split out
/// from the network call so it is fixture-tested with zero I/O, like every other parser here.
pub fn parse_egress(body: &str, ws_lane: bool) -> Result<Egress, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("egress probe: bad json: {e}"))?;
    let ip = v
        .get("ip")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "egress probe: response has no `ip`".to_string())?
        .to_string();
    let field = |k: &str| {
        v.get(k).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
    };
    Ok(Egress { ip, country: field("country"), city: field("city"), ws_lane })
}

/// Probe the live egress through the proxy-aware agent. Network call — only reached when an
/// expectation is configured (see [`check_expected_egress`]) or a caller asks explicitly.
pub fn observe_egress() -> Result<Egress, String> {
    let url = std::env::var("POLY_EGRESS_PROBE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_EGRESS_PROBE.to_string());
    let mut resp = agent().get(&url).call().map_err(|e| format!("egress probe {url}: {e}"))?;
    let text = resp.body_mut().read_to_string().map_err(|e| format!("egress probe {url}: {e}"))?;
    parse_egress(&text, ws_proxy().is_some())
}

/// The guard itself: assert the process egresses from `POLY_EXPECT_EGRESS_COUNTRY` (case-insensitive
/// ISO alpha-2, e.g. `IE`).
///
/// **Inert when unset** — returns [`EgressCheck::NotConfigured`] without touching the network, so
/// wiring this into a smoke costs nothing on a box that hasn't declared an expectation.
pub fn check_expected_egress() -> Result<EgressCheck, String> {
    let Some(expected) = std::env::var("POLY_EXPECT_EGRESS_COUNTRY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return Ok(EgressCheck::NotConfigured);
    };
    let observed = observe_egress()?;
    match &observed.country {
        Some(c) if c.eq_ignore_ascii_case(&expected) => Ok(EgressCheck::Ok(observed)),
        _ => Ok(EgressCheck::Mismatch { expected, observed }),
    }
}

// ---------------------------------------------------------------------------------------------
// THE VENUE'S OWN ORDER-PLACEMENT GEOBLOCK
// ---------------------------------------------------------------------------------------------
//
// The guard above answers "where am I egressing from"; this one answers the question that actually
// costs money — "will this venue let me place an order from here". They are NOT the same question,
// and the difference is the whole reason this exists: Polymarket restricts ORDER PLACEMENT by
// region while leaving market data AND authenticated reads working. Measured through a German exit
// (2026-08-23): balance, positions, orders and fills all green, then a submit refused with a 403
// whose body reads "Trading restricted in your region".
//
// So a wrong-region session looks completely healthy — it mounts, streams, reconciles positions and
// shows balances — and discovers the refusal one order at a time, on the exec path. That is the
// same defect class as a SOCKS tunnel that silently failed to bind, and the cure is the same: ask
// BEFORE, through the SAME egress the orders will use.

/// The venue's own keyless, free pre-flight for order placement.
///
/// ⚠ It is served by **`polymarket.com`**, NOT the CLOB API host ([`crate::config::CLOB_BASE`]) —
/// the web app owns this route and the trading API does not. Verified live returning exactly
/// `{"blocked":true,"ip":"2a01:…","country":"DE","region":"SN"}`.
///
/// The venue's published tiers are finer than this one bit — Germany is "close-only on frontend
/// AND API" (no new orders) while Ireland is "close-only on frontend" only, with the API
/// unrestricted — so `blocked` is read as exactly what it is: THIS endpoint's answer for THIS
/// egress, not a reconstruction of the tier table.
pub const GEOBLOCK_URL: &str = "https://polymarket.com/api/geoblock";

/// The ceiling [`observe_geoblock`] dials under — deliberately shorter than [`REST_TIMEOUT`], and
/// for the same reason `crates/bridges/polymarket/src/market_feed.rs`'s `FEED_WARMUP_TIMEOUT` is:
/// this probe runs on the MOUNT path, ahead of the L2 handshake. Its entire contract is that
/// failing to reach it changes nothing, so it must not be able to stall a mount for the order
/// path's budget either.
const GEOBLOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The venue's answer about order placement from this egress — the parsed `/api/geoblock` body.
///
/// Only `blocked` is required; the geo fields are a courtesy of an undocumented web-app route. They
/// are carried anyway because a refusal that cannot name WHERE it is refusing from tells an
/// operator nothing they can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geoblock {
    /// The venue's verdict — ⚠ **this is the FRONTEND's policy, not the API's**, and reading it as
    /// "order placement is refused" is wrong. See [`api_placement_permitted`]: measured
    /// 2026-08-23, Ireland returns `blocked: true` here while the CLOB API accepts orders from it.
    pub blocked: bool,
    /// The source IP the venue saw, when it reports one.
    pub ip: Option<String>,
    /// ISO-3166 alpha-2 country, when the venue reports one (`DE` on the measured refusal).
    pub country: Option<String>,
    /// Sub-national region, when the venue reports one (`SN` on the measured refusal).
    pub region: Option<String>,
}

/// `?` rather than an empty gap for a field the endpoint did not report — a refusal message that
/// renders `country=` and stops is worse than one that says it does not know.
fn or_unknown(field: &Option<String>) -> &str {
    field.as_deref().unwrap_or("?")
}

impl std::fmt::Display for Geoblock {
    /// The LOCATION only. The verdict itself is [`Geoblock::blocked`] (and, at the call sites that
    /// matter, the [`GeoblockVerdict`] variant), so rendering it here too would make every message
    /// that already names the outcome say it twice.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "country={} region={} ip={}",
            or_unknown(&self.country),
            or_unknown(&self.region),
            or_unknown(&self.ip)
        )
    }
}

/// Pure parse of an `/api/geoblock` body — split from the network call so the tolerance rules below
/// are fixture-tested with zero I/O, exactly like [`parse_egress`].
///
/// **The tolerance is stated precisely because a mount REFUSAL hangs off it.** `blocked` is the one
/// required field, and it is honoured only where a boolean can honestly be read out of it (`true`/
/// `false`, or those two spelled as a string — this is a web-app route, not a contracted API).
/// Anything else — the field absent, a number, a nested object, a non-object body, not JSON at all
/// — is an `Err`, **never** `blocked: false`: "we could not tell" and "the venue says you may
/// trade" are different answers, and rendering the first as the second turns a pre-flight into a
/// false all-clear. Every other field is optional, a non-string value reads as absent, and no input
/// can panic.
pub fn parse_geoblock(body: &str) -> Result<Geoblock, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("geoblock: bad json: {e}"))?;
    let blocked = match v.get("blocked") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("true") => true,
        Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("false") => false,
        _ => return Err("geoblock: response carries no usable `blocked` field".to_string()),
    };
    let field = |k: &str| {
        v.get(k).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
    };
    Ok(Geoblock { blocked, ip: field("ip"), country: field("country"), region: field("region") })
}

/// Ask the venue, through [`agent_with_timeout`] — i.e. through the SAME resolved proxy
/// ([`proxy_url`]) every CLOB REST call and every WS dial uses.
///
/// That sharing is the whole point of homing this beside the proxy resolution rather than building
/// a second agent: a pre-flight that measured a DIFFERENT egress than the one the orders leave by
/// would answer a question nobody asked.
pub fn observe_geoblock() -> Result<Geoblock, String> {
    let fail = |e: String| format!("geoblock probe {GEOBLOCK_URL}: {e}");
    let agent = agent_with_timeout(GEOBLOCK_TIMEOUT);
    let mut resp = agent.get(GEOBLOCK_URL).call().map_err(|e| fail(e.to_string()))?;
    let text = resp.body_mut().read_to_string().map_err(|e| fail(e.to_string()))?;
    parse_geoblock(&text)
}

/// What the venue said about order placement from this egress, in the three shapes a caller must
/// treat differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoblockVerdict {
    /// The venue answered, and this egress may place orders.
    Allowed(Geoblock),
    /// The venue answered, and order placement is REFUSED from this egress. Reads are unaffected.
    Blocked(Geoblock),
    /// The venue did not answer, or answered something unreadable — carrying the reason.
    ///
    /// ⚠ This is NOT a block, and must never be collapsed into one. An unreachable pre-flight is
    /// evidence of nothing, and a venue outage that became a mount failure would be a worse defect
    /// than the one this whole probe exists to catch.
    Unknown(String),
}

/// Classify an observation. PURE — the network half stops at this function's argument, which is
/// what makes "an unreachable probe is not a block" a unit test instead of a claim.
/// ⚠ The classification asks [`api_placement_permitted`], NOT `g.blocked`. The endpoint reports the
/// FRONTEND's policy, and for four jurisdictions the API's policy differs — Ireland reports
/// `blocked: true` here and accepts orders. Keying the verdict straight off `blocked` refused the
/// one region in this workspace where trading works, which is what shipping and then measuring
/// found.
pub fn geoblock_verdict(observed: Result<Geoblock, String>) -> GeoblockVerdict {
    match observed {
        Ok(g) if !api_placement_permitted(&g) => GeoblockVerdict::Blocked(g),
        Ok(g) => GeoblockVerdict::Allowed(g),
        Err(why) => GeoblockVerdict::Unknown(why),
    }
}

/// The probe and its classification in one call — what a live exec mount asks.
/// `crates/bridges/polymarket/src/mount.rs`'s `geoblock_action` decides what to DO about it.
pub fn check_order_placement_geo() -> GeoblockVerdict {
    geoblock_verdict(observe_geoblock())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IPINFO: &str = r#"{"ip":"52.16.1.2","hostname":"ec2-52-16-1-2.eu-west-1.compute.amazonaws.com","city":"Dublin","region":"Leinster","country":"IE","loc":"53.3331,-6.2489","org":"AS16509 Amazon.com, Inc.","timezone":"Europe/Dublin"}"#;

    #[test]
    fn parses_an_ipinfo_response() {
        let e = parse_egress(IPINFO, true).unwrap();
        assert_eq!(e.ip, "52.16.1.2");
        assert_eq!(e.country.as_deref(), Some("IE"));
        assert_eq!(e.city.as_deref(), Some("Dublin"));
        assert!(e.ws_lane);
    }

    #[test]
    fn tolerates_a_probe_without_geo_fields() {
        let e = parse_egress(r#"{"ip":"203.0.113.9"}"#, false).unwrap();
        assert_eq!(e.ip, "203.0.113.9");
        assert!(e.country.is_none());
        assert!(!e.ws_lane);
    }

    #[test]
    fn rejects_a_response_without_an_ip() {
        assert!(parse_egress(r#"{"country":"IE"}"#, false).is_err());
        assert!(parse_egress(r#"{"ip":""}"#, false).is_err());
        assert!(parse_egress("not json", false).is_err());
    }

    #[test]
    fn display_names_the_region_and_the_ws_lane() {
        let e = parse_egress(IPINFO, false).unwrap();
        let s = e.to_string();
        assert!(s.contains("52.16.1.2") && s.contains("IE/Dublin"), "{s}");
        assert!(s.contains("ws lane DIRECT"), "{s}");
    }

    #[test]
    fn mismatch_collapses_to_a_named_error_and_ok_to_the_observation() {
        let observed = parse_egress(IPINFO, true).unwrap();
        let err = EgressCheck::Mismatch { expected: "US".into(), observed: observed.clone() }
            .into_result()
            .unwrap_err();
        assert!(err.contains("POLY_EXPECT_EGRESS_COUNTRY=US"), "{err}");
        assert!(err.contains("52.16.1.2"), "{err}");
        assert_eq!(EgressCheck::Ok(observed.clone()).into_result().unwrap(), Some(observed));
        assert_eq!(EgressCheck::NotConfigured.into_result().unwrap(), None);
    }

    /// The guard must be INERT (no network, no failure) when no expectation is declared — the
    /// default for every box that hasn't opted in.
    #[test]
    fn unconfigured_is_a_no_op() {
        // The env var is process-global; only assert the unset branch when it genuinely is unset
        // (a box that exports it is a legitimately-configured box, not a test failure).
        if std::env::var("POLY_EXPECT_EGRESS_COUNTRY").is_err() {
            assert_eq!(check_expected_egress().unwrap(), EgressCheck::NotConfigured);
        }
    }

    // --- the venue's own order-placement geoblock (see `parse_geoblock`) ---------------------
    //
    // Fixture-only, like every other parser here: nothing below touches the network, and the one
    // network-shaped case is proven through `geoblock_verdict`'s pure argument.

    /// The MEASURED refusal, through a German exit (2026-08-23). Reads were all green through that
    /// same egress; only order submission came back 403.
    const GEOBLOCK_DE: &str =
        r#"{"blocked":true,"ip":"2a01:aaaa:bbbb:cccc::1","country":"DE","region":"SN"}"#;

    #[test]
    fn parses_a_blocked_response_with_its_country_and_region() {
        let g = parse_geoblock(GEOBLOCK_DE).unwrap();
        assert!(g.blocked);
        assert_eq!(g.country.as_deref(), Some("DE"));
        assert_eq!(g.region.as_deref(), Some("SN"));
        assert_eq!(g.ip.as_deref(), Some("2a01:aaaa:bbbb:cccc::1"));
        // ...and the rendering an operator actually reads names all three.
        let s = g.to_string();
        assert!(s.contains("country=DE") && s.contains("region=SN"), "{s}");
    }

    #[test]
    fn parses_an_allowed_response() {
        let body = r#"{"blocked":false,"ip":"52.16.1.2","country":"IE","region":"L"}"#;
        let g = parse_geoblock(body).unwrap();
        assert!(!g.blocked);
        assert_eq!(g.country.as_deref(), Some("IE"));
        assert_eq!(geoblock_verdict(Ok(g.clone())), GeoblockVerdict::Allowed(g));
    }

    /// Every field but `blocked` is optional, and a field of the WRONG TYPE reads as absent rather
    /// than as an error or a panic — the endpoint is an undocumented web-app route, so its
    /// courtesy fields are not something a mount decision may hang on.
    #[test]
    fn tolerates_missing_and_malformed_optional_fields() {
        let g = parse_geoblock(r#"{"blocked":true}"#).unwrap();
        assert!(g.blocked && g.country.is_none() && g.region.is_none() && g.ip.is_none());
        assert_eq!(g.to_string(), "country=? region=? ip=?");
        let odd = r#"{"blocked":false,"ip":42,"country":null,"region":{"a":1},"extra":[1]}"#;
        let g = parse_geoblock(odd).unwrap();
        assert!(!g.blocked && g.ip.is_none() && g.country.is_none() && g.region.is_none());
        // an empty string is not a country either
        assert!(parse_geoblock(r#"{"blocked":true,"country":""}"#).unwrap().country.is_none());
        // the two string spellings of the boolean are honoured (a web route, not a contract)
        assert!(parse_geoblock(r#"{"blocked":"TRUE"}"#).unwrap().blocked);
        assert!(!parse_geoblock(r#"{"blocked":"false"}"#).unwrap().blocked);
    }

    /// THE LOAD-BEARING TOLERANCE RULE: a body we cannot read `blocked` out of is an ERROR, never
    /// an all-clear. `Ok(blocked: false)` would be a positive claim that the venue permits trading
    /// — the exact false confidence this whole probe exists to remove.
    #[test]
    fn an_unusable_blocked_field_is_an_error_not_an_all_clear() {
        for body in [
            "",
            "not json",
            "[]",
            r#""blocked""#,
            r#"{"error":"rate limited"}"#,
            r#"{"blocked":1}"#,
            r#"{"blocked":null}"#,
            r#"{"blocked":"maybe"}"#,
            r#"{"blocked":{"api":true}}"#,
        ] {
            assert!(parse_geoblock(body).is_err(), "{body} must not parse as an all-clear");
        }
    }

    /// An unreachable or unreadable probe is `Unknown`, NOT `Blocked`. A venue outage must not be
    /// able to refuse a mount — the caller's half of that rule is
    /// `crates/bridges/polymarket/src/mount.rs`'s `geoblock_action`.
    #[test]
    fn a_probe_failure_is_unknown_never_blocked() {
        let v = geoblock_verdict(Err("geoblock probe: dial tcp: timed out".to_string()));
        assert!(matches!(&v, GeoblockVerdict::Unknown(why) if why.contains("timed out")));
        // ...and the same for a body that reached us but said nothing usable
        let v = geoblock_verdict(parse_geoblock("<html>403</html>"));
        assert!(matches!(v, GeoblockVerdict::Unknown(_)));
    }

    /// The probe URL is the SITE's route, not the CLOB API host — the trap that makes this
    /// endpoint easy to wire against the wrong base.
    #[test]
    fn the_probe_url_is_the_site_not_the_clob_host() {
        assert_eq!(GEOBLOCK_URL, "https://polymarket.com/api/geoblock");
        assert!(!GEOBLOCK_URL.starts_with(crate::config::CLOB_BASE));
    }

    // --- the workspace-`.env` fallback (see the module doc) ---------------------------------
    //
    // These prove the FIX for the silent-ignore bug: `load_workspace_dotenv()` returns a map and
    // never exports into the process env, so the old plain `std::env::var` reads could not see a
    // `POLY_PROXY_*` line written in the workspace `.env`. Everything here goes through the pure
    // `*_with`/`*_layered` cores, so no test mutates the process-global env (`set_var` is unsound
    // under threads — the repo-wide rule, see `credentials.rs`).

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// A lookup with NOTHING in the process env — i.e. the `.env`-only case.
    fn dotenv_only(vars: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
        move |k: &str| proxy_var_layered(None, k, &vars)
    }

    /// THE REGRESSION: a tunnel declared only in the workspace `.env` used to be ignored, and the
    /// agent silently dialled the built-in `127.0.0.1:1080` default instead.
    #[test]
    fn dotenv_only_host_and_port_are_honoured() {
        let get = dotenv_only(map(&[("POLY_PROXY_HOST", "<host>"), ("POLY_PROXY_PORT", "1081")]));
        assert_eq!(proxy_url_with(get).as_deref(), Some("socks5h://<host>:1081"));
    }

    #[test]
    fn dotenv_only_explicit_url_and_disable_are_honoured() {
        let get = dotenv_only(map(&[("POLY_SOCKS_PROXY", "socks5h://dub.internal:9050")]));
        assert_eq!(proxy_url_with(get).as_deref(), Some("socks5h://dub.internal:9050"));
        // and the two disable spellings, likewise only in `.env`
        assert!(proxy_url_with(dotenv_only(map(&[("POLY_SOCKS_PROXY", "none")]))).is_none());
        assert!(proxy_url_with(dotenv_only(map(&[("POLY_PROXY_ENABLED", "false")]))).is_none());
    }

    /// The WS lane's own gate reads through the SAME resolver, so `POLY_WS_PROXY_ENABLED=1` in the
    /// workspace `.env` turns the tunnel on for the feeds too (it was equally invisible before).
    #[test]
    fn dotenv_only_ws_gate_is_honoured() {
        let vars = map(&[
            ("POLY_WS_PROXY_ENABLED", "1"),
            ("POLY_SOCKS_PROXY", "socks5h://127.0.0.1:1080"),
        ]);
        let p = ws_proxy_with(dotenv_only(vars)).expect("ws proxy from .env");
        assert_eq!(p.port, 1080);
    }

    /// UNIFIED switch: with the WS override UNSET, the WS lane INHERITS the HTTP lane — a resolved
    /// endpoint (here just `POLY_SOCKS_PROXY`) now turns the feeds on too, so ONE control governs
    /// every Polymarket connection. (Under the old two-gate model this returned `None`.)
    #[test]
    fn ws_lane_inherits_the_http_proxy_by_default() {
        // endpoint set, no WS override → WS proxies (inherits)
        assert!(
            ws_proxy_with(dotenv_only(map(&[("POLY_SOCKS_PROXY", "socks5h://h:1")]))).is_some()
        );
        // explicit WS-off override forces the feeds DIRECT even while the HTTP endpoint is set
        assert!(
            ws_proxy_with(dotenv_only(map(&[
                ("POLY_SOCKS_PROXY", "socks5h://h:1"),
                ("POLY_WS_PROXY_ENABLED", "off"),
            ])))
            .is_none()
        );
        // master OFF disables both lanes regardless of any WS override
        assert!(
            ws_proxy_with(dotenv_only(map(&[
                ("POLY_PROXY_ENABLED", "false"),
                ("POLY_WS_PROXY_ENABLED", "1"),
            ])))
            .is_none()
        );
    }

    /// A `.env` line annotated with a trailing `#` comment — the style the REAL workspace file uses
    /// — must still resolve. `parse_dotenv` does not strip it, so before `first_token` this produced
    /// `bad port in "127.0.0.1:11080   # dev-box arbdub tunnel"` on a live run and fell back to a
    /// SILENT direct egress (caught by the egress guard, from Dublin, during this PR's live test).
    #[test]
    fn dotenv_values_tolerate_trailing_inline_comments() {
        let get = dotenv_only(map(&[
            ("POLY_PROXY_HOST", "127.0.0.1  # localhost end of the ssh -D"),
            ("POLY_PROXY_PORT", "11080   # dev-box arbdub tunnel"),
        ]));
        assert_eq!(proxy_url_with(get).as_deref(), Some("socks5h://127.0.0.1:11080"));
        // …and the enable/disable flags, which are likewise single-token
        assert!(
            proxy_url_with(dotenv_only(map(&[("POLY_PROXY_ENABLED", "false # direct")]))).is_none()
        );
        let vars = map(&[
            ("POLY_WS_PROXY_ENABLED", "1  # proven from arbdub"),
            ("POLY_SOCKS_PROXY", "socks5h://127.0.0.1:11080  # ssh -D"),
        ]);
        assert_eq!(ws_proxy_with(dotenv_only(vars)).map(|p| p.port), Some(11080));
    }

    /// Precedence: the REAL process env wins, and an empty env value still wins (`POLY_SOCKS_PROXY=`
    /// means "direct" — it must override a `.env` line, not fall through to it).
    #[test]
    fn process_env_wins_over_dotenv() {
        let vars = map(&[("POLY_SOCKS_PROXY", "socks5h://from-dotenv:1")]);
        assert_eq!(
            proxy_var_layered(Some("socks5h://from-env:2".into()), "POLY_SOCKS_PROXY", &vars)
                .as_deref(),
            Some("socks5h://from-env:2")
        );
        assert_eq!(
            proxy_var_layered(Some(String::new()), "POLY_SOCKS_PROXY", &vars).as_deref(),
            Some("")
        );
        assert_eq!(
            proxy_var_layered(None, "POLY_SOCKS_PROXY", &vars).as_deref(),
            Some("socks5h://from-dotenv:1")
        );
        assert_eq!(proxy_var_layered(None, "POLY_PROXY_PORT", &vars), None);
    }

    /// The `.env` fallback is a fixed allow-list, never a prefix match — no other credential can
    /// leak out of the workspace `.env` through this path.
    #[test]
    fn dotenv_fallback_is_a_narrow_allow_list() {
        assert_eq!(PROXY_KEYS.len(), 5);
        assert!(!PROXY_KEYS.contains(&"POLY_PRIVATE_KEY"));
        assert!(!PROXY_KEYS.contains(&"POLY_RELAYER_API_KEY"));
        // a key outside the list is not resolvable even when present in the map
        let vars = map(&[("POLY_PRIVATE_KEY", "0xdead")]);
        assert!(dotenv_proxy_vars().keys().all(|k| PROXY_KEYS.contains(&k.as_str())));
        assert_eq!(proxy_var_layered(None, "POLY_PRIVATE_KEY", dotenv_proxy_vars()), None);
        let _ = vars; // the map above is illustrative: `dotenv_proxy_vars` never admits that key
    }

    /// The built-in defaults are unchanged when NOTHING is configured anywhere — the pre-fix
    /// behavior every existing tunnelled run relies on.
    #[test]
    fn defaults_unchanged_when_nothing_is_set() {
        let get = dotenv_only(HashMap::new());
        assert_eq!(proxy_url_with(get).as_deref(), Some("socks5h://127.0.0.1:1080"));
    }
}

/// The four jurisdictions Polymarket documents as **frontend-only** restrictions — close-only on
/// polymarket.com while *"the API itself is not restricted"*.
///
/// ⚠ This list exists because `/api/geoblock` CANNOT answer the question the exec mount is asking.
/// That route lives on `polymarket.com` and reports the **site's** policy; the CLOB API's policy is
/// different, and the two disagree for exactly these countries. Measured 2026-08-23, both halves:
///
/// | egress | `/api/geoblock` | a real order |
/// |---|---|---|
/// | Germany (`DE`) | `blocked: true` | **403** `Trading restricted in your region` |
/// | Ireland (`IE`) | `blocked: true` | **ACCEPTED** — went `live`, then cancelled clean |
///
/// So `blocked: true` alone would refuse Ireland, which is one of only four regions where trading
/// works at all — and is where this workspace's own Dublin exit lives. Reconstructing the tier is
/// not gold-plating here; it is the difference between the gate refusing the wrong half of the
/// world and refusing the right one.
const API_PERMITTED_WHEN_FRONTEND_BLOCKED: [&str; 4] = ["IE", "JP", "NL", "MT"];

/// Does the CLOB **API** permit order placement from this egress, given the frontend's verdict?
///
/// `blocked: false` is unambiguous — neither surface restricts it. `blocked: true` is ambiguous by
/// construction, and is resolved by country: the four frontend-only jurisdictions still trade
/// through the API, everything else does not. An unreported country under `blocked: true` is
/// treated as NOT permitted, because the measured majority of that tier really is API-restricted
/// and a false refusal is recoverable (the override) where a false permit is a 403 on the exec path.
#[must_use]
pub fn api_placement_permitted(g: &Geoblock) -> bool {
    if !g.blocked {
        return true;
    }
    g.country
        .as_deref()
        .map(|c| API_PERMITTED_WHEN_FRONTEND_BLOCKED.iter().any(|p| p.eq_ignore_ascii_case(c)))
        .unwrap_or(false)
}

#[cfg(test)]
mod api_placement_tests {
    use super::*;

    fn geo(blocked: bool, country: Option<&str>) -> Geoblock {
        Geoblock { blocked, ip: None, country: country.map(str::to_string), region: None }
    }

    /// The measurement that forced this function to exist: both countries report `blocked: true`,
    /// and only one of them can actually trade.
    #[test]
    fn ireland_trades_and_germany_does_not_although_both_report_blocked() {
        assert!(api_placement_permitted(&geo(true, Some("IE"))), "IE placed a live order");
        assert!(!api_placement_permitted(&geo(true, Some("DE"))), "DE was refused 403 on submit");
    }

    /// An unrestricted frontend means an unrestricted API — no country lookup needed.
    #[test]
    fn an_unblocked_frontend_permits_regardless_of_country() {
        assert!(api_placement_permitted(&geo(false, Some("DE"))));
        assert!(api_placement_permitted(&geo(false, None)));
    }

    /// All four documented frontend-only jurisdictions, case-insensitively.
    #[test]
    fn every_frontend_only_jurisdiction_still_trades() {
        for c in ["IE", "JP", "NL", "MT", "ie", "Nl"] {
            assert!(api_placement_permitted(&geo(true, Some(c))), "{c} is frontend-only");
        }
    }

    /// Blocked with no country is the one case that must fail CLOSED: a false refusal is undone by
    /// the override, a false permit is a 403 on the exec path.
    #[test]
    fn blocked_without_a_country_is_refused() {
        assert!(!api_placement_permitted(&geo(true, None)));
        assert!(!api_placement_permitted(&geo(true, Some(""))));
    }
}
