//! StudioChatPane: the in-app AI copilot over vike-ai's tested `develop_strategy` loop (Studio SP3 Part
//! B, Task 5). Thin over that loop: `send()` spawns a worker thread and returns a receiver, exactly
//! through `vike_studio_core::spawn_outcome`'s `std::thread::spawn` + `mpsc::channel` shape —
//! `StudioState` owns the `Receiver<StudioChatOutcome>` (the `chat_rx` field, drained by the 4th `poll()`
//! arm) the same way it owns `run_rx`/`sweep_rx`/`walkforward_rx`. egui paint is not unit-tested here (SP2
//! discipline); only the pure helpers (`apply_result`, [`StudioChatApiKeys`], `summary_of`) and `send`'s
//! wiring are.
//!
//! ⚠ This pane reads NO credential store and NO environment (split-plane I8). It used to call the
//! workspace `.env` loader twice — once at construction to decide which providers to offer, once
//! per `send()` for the key itself — which made a GUI pane the thing that opened
//! `<project>/settings/secrets.env`, dragged `vike-bridge-core` (and vike-execution behind it) into a
//! crate that is meant to become a thin client, and put `crates/vike-studio/src/panes/chat/mod.rs` on
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`. The keys arrive as a
//! [`StudioChatApiKeys`] parameter now, resolved by the composition root from the sweep it already owns.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use vike_ai::{
    AgentResult, LedgerPaths, OutOfSampleRun, OutOfSampleSlice, Provider,
    develop_strategy_with_ledger, make_client,
};
use vike_data::HistoryStore;

// The "Connect to Claude" `claude mcp add` generator (spawns `vike-mcp`) — only compiled
// under the non-default `mcp` feature, mirroring the retired vike-mcp dep gate it replaced
// (Phase B).
#[cfg(feature = "mcp")]
mod connect;

/// Outcome of one chat -> strategy round trip on the worker thread: the agent's result, or a
/// human-readable failure (no API key, or the loop panicked — surfaced by `poll()`'s disconnect arm).
pub(crate) type StudioChatOutcome = Result<AgentResult, String>;

/// The AI-provider keys the StudioChatPane needs — resolved by the BINARY and handed in, never read here.
///
/// These are NOT venue credentials: they buy strategy generation from a third-party model, and
/// absent, the pane simply offers no provider and the Send button stays disabled. Nothing about
/// trading changes.
///
/// # Where they come from
///
/// The credential store — the same store every venue credential lives in (the settings database;
/// the `secrets.env` file store was removed on 2026-10-07), resolved by the same `vike_secrets`
/// walk. An operator writes them with `vike-cli secrets set ANTHROPIC_API_KEY` (or
/// `CEREBRAS_API_KEY`), the value on stdin. It arrives as a PARAMETER ([`StudioChatApiKeys::resolve`])
/// because this is a library: only the binary reads global configuration state. The app root
/// resolves it from the one credential map it loads.
///
/// Resolved ONCE, at construction: a key added to the store afterwards needs a restart. That was
/// already true before the injection — the provider list `has_key()` gates Send on was computed at
/// construction, so a later `send()`'s second store read could never enable a provider the picker
/// was not already offering.
///
/// ⚠ A present-but-BLANK value counts as PRESENT, deliberately preserving the pre-injection
/// behaviour exactly: the old gate was `contains_key`, so an `ANTHROPIC_API_KEY=` line offered the
/// provider and handed the empty string to `make_client`. Blank and absent differ here — blank is
/// offered and fails at the provider — and silently changing which providers the picker lists is
/// not this type's business.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct StudioChatApiKeys {
    /// `ANTHROPIC_API_KEY`.
    pub anthropic: Option<String>,
    /// `CEREBRAS_API_KEY`.
    pub cerebras: Option<String>,
}

impl StudioChatApiKeys {
    /// Resolve both keys from an already-loaded credential map.
    ///
    /// PURE — no environment read, no file read, so the provider-availability rule is unit-testable
    /// without a store on disk or a mutated process global.
    pub fn resolve(store: &HashMap<String, String>) -> Self {
        Self {
            anthropic: store.get(Provider::Anthropic.credential_key()).cloned(),
            cerebras: store.get(Provider::Cerebras.credential_key()).cloned(),
        }
    }

    /// This provider's key, if one was supplied.
    fn key(&self, provider: Provider) -> Option<String> {
        match provider {
            Provider::Anthropic => self.anthropic.clone(),
            Provider::Cerebras => self.cerebras.clone(),
        }
    }

    /// Which providers have a usable key — the provider selector only ever offers these (computed
    /// once at `StudioChatPane::new`, not per frame).
    pub(crate) fn providers(&self) -> Vec<Provider> {
        [Provider::Anthropic, Provider::Cerebras]
            .into_iter()
            .filter(|p| self.key(*p).is_some())
            .collect()
    }
}

/// Redacts. A key is a secret, and this type is the only place one is NAMED rather than living as
/// an anonymous local — so it gets the same manual `Debug` every credential type in this workspace
/// has.
impl std::fmt::Debug for StudioChatApiKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = |v: &Option<String>| if v.is_some() { "<set>" } else { "<unset>" };
        f.debug_struct("StudioChatApiKeys")
            .field("anthropic", &shown(&self.anthropic))
            .field("cerebras", &shown(&self.cerebras))
            .finish()
    }
}

/// What the chat asks each provider for and how long it waits — the three `preferences.ai_*` rows,
/// resolved by the BINARY and handed in like [`StudioChatApiKeys`], never read here.
///
/// `vike-ai` is a library with no model id or timeout of its own (`vike_ai::make_client` takes
/// both), so THIS is where a default is needed for a pane that was handed nothing; it is
/// [`vike_config::Preferences::default`]'s, the one place the defaults are written. The model ids
/// were validated where the row is written and loaded (1 to 100 characters of ASCII letters,
/// digits and `._:/-`); whether a provider KNOWS an id is its answer, and a wrong one comes back as
/// an HTTP error line in the chat pane — the chat generates and backtests strategies and places no
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudioChatConfig {
    /// `preferences.ai_anthropic_model` — the model id sent to Anthropic.
    pub anthropic_model: String,
    /// `preferences.ai_cerebras_model` — the model id sent to Cerebras.
    pub cerebras_model: String,
    /// `preferences.ai_timeout_secs` — the bound on one provider request.
    pub timeout: Duration,
}

impl Default for StudioChatConfig {
    /// The values a box with no `preferences.ai_*` row runs: the model ids and the timeout this
    /// pane has always used.
    fn default() -> Self {
        let p = vike_config::Preferences::default();
        Self {
            anthropic_model: p.ai_anthropic_model,
            cerebras_model: p.ai_cerebras_model,
            timeout: Duration::from_secs(u64::from(p.ai_timeout_secs)),
        }
    }
}

impl StudioChatConfig {
    /// The model id this provider is asked for.
    fn model(&self, provider: Provider) -> &str {
        match provider {
            Provider::Anthropic => &self.anthropic_model,
            Provider::Cerebras => &self.cerebras_model,
        }
    }
}

/// Apply the agent's generated script into the editor buffer. The Studio shows a line-level diff
/// ([`diff_rows`]) of `editor_source` -> `result.code` first, so Apply is a reviewed action rather
/// than a blind clobber; the write itself is still a whole-buffer replace once the user accepts.
pub(crate) fn apply_result(editor_source: &mut String, result: &AgentResult) {
    *editor_source = result.code.clone();
}

/// One line of a rendered diff: whether it's unchanged, inserted (in `new`), or deleted (from `old`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StudioChatDiffKind {
    Equal,
    Insert,
    Delete,
}

/// A single diff line: its change kind plus the line text (newline stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StudioChatDiffRow {
    pub kind: StudioChatDiffKind,
    pub text: String,
}

/// Local grounding for the AI chat: a small context block prepended to the user's prompt, built
/// from data already in memory in the running app (the target slice + a valid template as a
/// worked example). NO database, no retrieval index — see the module note; this is "format what
/// you already have." Deliberately does NOT enumerate the `vike-indicators` registry.
///
/// ⚠ The reason CHANGED and the old one is worth recording, because it inverted: this used to say
/// the host binds only `sma`/`ema`/`rsi`, so naming the rest would invite calls that cannot bind.
/// The host now binds most of the registry (`vike_rhai::RHAI_INDICATORS`, derived from it), so
/// that hazard is gone. What remains is a token budget — a preamble is prepended to EVERY message,
/// and spending it on a fixed roster crowds out the context specific to this chat. A model that
/// wants the roster asks for it; an unbound name raises on the first call rather than returning a
/// plausible number, so a guess fails loudly. Pure/testable.
pub(crate) fn grounding_preamble(
    venue: &str,
    symbol: &str,
    interval: &str,
    example_code: &str,
) -> String {
    format!(
        "# Context\n\
         Generate a strategy to trade {symbol} on {venue} at the {interval} interval; it is \
         backtested on that instrument's out-of-sample data. Reads are host functions on the \
         CURRENT bar and orders go through the `strategy` object of `fn on_bar(strategy)` (see the \
         system prompt); there is no cross-bar mutable state in the script.\n\n\
         # A valid strategy for reference (adapt to the request — do not copy verbatim):\n\
         ```rhai\n{}\n```",
        example_code.trim()
    )
}

/// Line-level diff of the current editor buffer (`old`) against the AI-generated script (`new`),
/// for the StudioChatPane "review changes" view. A pure transform over the two strings (unit-tested);
/// the egui rendering that colors these rows is not (SP2 discipline).
pub(crate) fn diff_rows(old: &str, new: &str) -> Vec<StudioChatDiffRow> {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .map(|change| {
            let kind = match change.tag() {
                similar::ChangeTag::Equal => StudioChatDiffKind::Equal,
                similar::ChangeTag::Insert => StudioChatDiffKind::Insert,
                similar::ChangeTag::Delete => StudioChatDiffKind::Delete,
            };
            StudioChatDiffRow { kind, text: change.value().trim_end_matches('\n').to_string() }
        })
        .collect()
}

/// One assistant transcript line summarizing a chat outcome — accepted (with the OOS score),
/// rejected (with why), or a hard failure (no key / worker disconnect).
pub(crate) fn summary_of(outcome: &StudioChatOutcome) -> String {
    match outcome {
        Ok(r) if r.accepted => {
            format!("{} (OOS Sharpe {:.2} over {} trades)", r.explanation, r.oos_sharpe, r.n_trades)
        }
        Ok(r) => {
            format!("Not accepted after {} attempt(s): {}", r.attempts, r.problems.join("; "))
        }
        Err(e) => format!("Error: {e}"),
    }
}

/// The in-app copilot chat: transcript + provider selector + the last agent result, rendered by
/// `StudioState::ui` (inlined there like the "Sweep / Validate" panel — it needs the store, the
/// picker's selected slice, and the editor buffer, none of which `StudioChatPane` itself owns).
pub struct StudioChatPane {
    pub input: String,
    /// (role, text) transcript rows — role is "user" or "assistant".
    pub(crate) history: Vec<(String, String)>,
    pub(crate) provider: Provider,
    pub last: Option<AgentResult>,
    pub(crate) running: bool,
    /// The caller-supplied provider keys (see [`StudioChatApiKeys`]) — the pane opens no store of its own.
    keys: StudioChatApiKeys,
    /// The model ids and the request timeout the next `send` uses — [`StudioChatConfig`]. The defaults
    /// until the composition root assigns the `preferences.ai_*` rows (`vike-desktop` does, right
    /// after it builds the Studio, as it seeds the Research host).
    pub settings: StudioChatConfig,
    /// Providers whose key was present at construction — the picker's "refresh is a one-shot
    /// maintenance walk" pattern, not a per-frame read.
    available: Vec<Provider>,
    /// The copy-pasteable `claude mcp add …` line from the last "Connect to Claude" click.
    pub(crate) connect_command: Option<String>,
}

/// A KEYLESS pane: no provider is offered and Send stays disabled. The honest default for a
/// constructor that was handed nothing, and what the headless capture harness wants.
impl Default for StudioChatPane {
    fn default() -> Self {
        Self::new(StudioChatApiKeys::default())
    }
}

impl StudioChatPane {
    /// `keys` is resolved by the CALLER — see [`StudioChatApiKeys`] for where they come from and why the
    /// pane no longer opens the credential store itself.
    pub(crate) fn new(keys: StudioChatApiKeys) -> Self {
        let available = keys.providers();
        let provider = available.first().copied().unwrap_or_default();
        Self {
            input: String::new(),
            history: Vec::new(),
            provider,
            last: None,
            running: false,
            keys,
            settings: StudioChatConfig::default(),
            available,
            connect_command: None,
        }
    }

    /// Providers the selector may offer (key present at construction time).
    pub(crate) fn available_providers(&self) -> &[Provider] {
        &self.available
    }

    /// The current provider has a usable key — gates the Send button alongside `!running`.
    pub(crate) fn has_key(&self) -> bool {
        self.available.contains(&self.provider)
    }

    /// Spawn the worker thread and return its receiver — no-op-shaped: the caller (`StudioState`)
    /// checks `!running && has_key()` before calling this, matching `start_run`'s `if self.running {
    /// return; }` guard at the call site rather than duplicating it here. Consumes `self.input` into
    /// the transcript as the user turn immediately (so it shows up in the panel before the worker
    /// replies).
    ///
    /// `ledger` is the persistent trial ledger + learnings pair (`vike_ai::ledger`), RESOLVED BY
    /// THE CALLER — `StudioState` puts it in its state directory, beside
    /// `studio_strategies.json`; `vike-ai` itself reads no environment
    /// variable and knows no default location. `None` runs the ledger-less loop (no history read,
    /// nothing recorded, `deflated_sharpe` always `0.0`).
    ///
    /// `backtest` runs each candidate the loop compiled, and it is the CALLER's too: `StudioState`
    /// hands the backtest server's (`crate::backend::backtesthub`'s `copilot_backtester`, one summarized
    /// `RunSlice` per candidate, a kept candidate's whole curve paged back), so nothing is backtested in
    /// this process and a candidate is charged as a Run.
    pub(crate) fn send(
        &mut self,
        store: Arc<dyn HistoryStore + Send + Sync>,
        venue: String,
        symbol: String,
        interval: String,
        ledger: Option<LedgerPaths>,
        backtest: impl Fn(&str, OutOfSampleSlice<'_>) -> Result<OutOfSampleRun, String> + Send + 'static,
    ) -> Receiver<StudioChatOutcome> {
        let prompt = self.input.clone();
        self.history.push(("user".to_string(), prompt.clone()));
        self.input.clear();
        self.running = true;

        // Local grounding: prepend an in-memory context block (target slice + a template example)
        // to the model prompt. The transcript keeps the raw user turn (pushed above).
        let example = vike_rhai::TEMPLATES.first().map(|t| t.1).unwrap_or("");
        let grounding = grounding_preamble(&venue, &symbol, &interval, example);
        let prompt = format!("{grounding}\n\n# Request\n{prompt}");

        let provider = self.provider;
        let key = self.keys.key(provider);
        // The model id and the request bound travel into the worker with the key: the settings are
        // read on THIS thread, so the worker holds values and not the pane.
        let model = self.settings.model(provider).to_string();
        let timeout = self.settings.timeout;

        // Same worker-thread spawn shape as the Run pipeline — routed through the shared
        // `vike_studio_core::spawn_outcome` so this `channel()` + `thread::spawn` + `send` wrapper
        // lives in exactly one place.
        vike_studio_core::spawn_outcome(move || match make_client(provider, key, model, timeout) {
            None => {
                let name = provider.credential_key();
                Err(format!("no {name} in the store; run `vike-cli secrets set {name}`"))
            }
            Some(client) => Ok(develop_strategy_with_ledger(
                &prompt,
                &venue,
                &symbol,
                &interval,
                store.as_ref(),
                client.as_ref(),
                &backtest,
                2,
                0.3,
                ledger.as_ref(),
            )),
        })
    }

    /// Build the "Connect to Claude" MCP entry + the copy-pasteable `claude mcp add …` command and
    /// stash it in `connect_command` for the panel to render. The spawned server is `vike-mcp`
    /// (the one MCP server since the vike-mcp retirement — Phase B of #842): its run/list tools go
    /// over a RUNNING vike-datahub server, so `datahub_addr` must be a DATAHUB address.
    /// ⚠ It is NOT the Studio's Source-backend address, which this said until 2026-09-20: since
    /// ruling 7 that one is the backtesthub's, so threading it here points the run/list tools
    /// at a socket that refuses their verbs by plane. The Studio holds no datahub address, so its
    /// only caller passes `None`, which lets the server fall back to
    /// its own default (`127.0.0.1:7878`).
    ///
    /// `exe_path` is `resolve_mcp_exe`'s answer: the `vike-mcp` binary that IS next to the running
    /// executable, else the bare name `vike-mcp`, which `claude` resolves on its own `PATH` (the
    /// scoop, binstall and release-download installs of the README).
    ///
    /// Only compiled to actually build the command under the non-default `mcp` feature (a plain
    /// code gate over the `connect` module — no crate dep since the vike-mcp retirement); without
    /// it the button records a "rebuild with --features mcp" note instead, so the chat panel still
    /// compiles and renders identically minus the copyable command.
    pub(crate) fn connect_to_claude(&mut self, datahub_address: Option<&str>) {
        #[cfg(feature = "mcp")]
        {
            let exe = std::env::current_exe().ok();
            let exe_path = resolve_mcp_exe(exe.as_deref().and_then(std::path::Path::parent));
            let entry = connect::server_entry(&exe_path, datahub_address);
            self.connect_command = Some(connect::claude_code_add_command("vike-studio", &entry));
        }
        #[cfg(not(feature = "mcp"))]
        {
            let _ = datahub_address;
            self.connect_command = Some(
                "MCP connect helper not compiled in — rebuild vike-studio with `--features mcp` \
                 to generate the `claude mcp add` command that registers `vike-mcp` (needs a \
                 running vike-datahub server)"
                    .to_string(),
            );
        }
    }
}

/// The `vike-mcp` to register with Claude: the file beside the running executable when it exists
/// (the two ship co-located), else the bare file name — never a path to a file that is not there,
/// which is what this returned until it learned to look. The bare name is the `PATH` rung: the
/// command is run by `claude`, whose `PATH` resolves it. (Compiled for the tests too: the `mcp`
/// feature is built by no CI lane, so this is what keeps the ladder exercised.)
#[cfg(any(feature = "mcp", test))]
fn resolve_mcp_exe(exe_dir: Option<&std::path::Path>) -> String {
    let name = format!("vike-mcp{}", std::env::consts::EXE_SUFFIX);
    match exe_dir.map(|d| d.join(&name)) {
        Some(beside) if beside.is_file() => beside.to_string_lossy().into_owned(),
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_overwrites_editor_source() {
        let mut source = "old".to_string();
        let result =
            AgentResult { code: "new script".into(), accepted: true, ..Default::default() };
        apply_result(&mut source, &result);
        assert_eq!(source, "new script");
    }

    #[test]
    fn provider_options_hide_absent_keys() {
        let mut vars = HashMap::new();
        vars.insert("ANTHROPIC_API_KEY".to_string(), "k".to_string());
        let options = StudioChatApiKeys::resolve(&vars).providers();
        assert!(options.contains(&Provider::Anthropic));
        assert!(!options.contains(&Provider::Cerebras)); // no CEREBRAS_API_KEY
    }

    /// The injection's whole point: an EMPTY map is a complete answer, and it is the one a pane
    /// that opens no store gets when the caller has nothing to give it.
    #[test]
    fn no_keys_means_no_providers_and_no_send() {
        let pane = StudioChatPane::default();
        assert!(pane.available_providers().is_empty());
        assert!(!pane.has_key());
        assert_eq!(StudioChatApiKeys::resolve(&HashMap::new()), StudioChatApiKeys::default());
    }

    /// A present-but-BLANK value counts as present — the pre-injection `contains_key` behaviour,
    /// pinned so a later "tidy-up" cannot silently change which providers the picker lists.
    #[test]
    fn a_blank_key_still_offers_the_provider() {
        let mut vars = HashMap::new();
        vars.insert("CEREBRAS_API_KEY".to_string(), String::new());
        assert_eq!(StudioChatApiKeys::resolve(&vars).providers(), vec![Provider::Cerebras]);
    }

    /// A key must never reach a log line or a panic message through the derived formatter.
    #[test]
    fn debug_redacts_the_keys() {
        let mut vars = HashMap::new();
        vars.insert("ANTHROPIC_API_KEY".to_string(), "sk-secret".to_string());
        let shown = format!("{:?}", StudioChatApiKeys::resolve(&vars));
        assert!(!shown.contains("sk-secret"), "the key itself must not render: {shown}");
        assert!(shown.contains("<set>") && shown.contains("<unset>"), "{shown}");
    }

    /// A pane handed no `preferences.ai_*` rows runs what it always ran: the two model ids and the
    /// 120-second request bound. The defaults are `Preferences::default()`'s, so there is exactly
    /// one place they are written — this pin is the values the owner's "no behaviour change" rests on.
    #[test]
    fn a_pane_with_no_rows_asks_for_the_models_and_timeout_it_always_did() {
        let pane = StudioChatPane::default();
        assert_eq!(pane.settings, StudioChatConfig::default());
        assert_eq!(pane.settings.model(Provider::Anthropic), "claude-opus-4-8");
        assert_eq!(pane.settings.model(Provider::Cerebras), "llama-3.3-70b");
        assert_eq!(pane.settings.timeout, Duration::from_secs(120));
    }

    /// The rows, once assigned, are what the next send names — each provider its OWN model id, and
    /// one timeout for both.
    #[test]
    fn each_provider_is_asked_for_its_own_configured_model() {
        let pane = StudioChatPane {
            settings: StudioChatConfig {
                anthropic_model: "claude-sonnet-4-5".into(),
                cerebras_model: "llama-3.1-8b".into(),
                timeout: Duration::from_secs(45),
            },
            ..StudioChatPane::default()
        };
        assert_eq!(pane.settings.model(Provider::Anthropic), "claude-sonnet-4-5");
        assert_eq!(pane.settings.model(Provider::Cerebras), "llama-3.1-8b");
        assert_eq!(pane.settings.timeout, Duration::from_secs(45));
    }

    #[test]
    fn summary_of_accepted_includes_sharpe_and_trades() {
        let ok: StudioChatOutcome = Ok(AgentResult {
            explanation: "sma cross".into(),
            accepted: true,
            oos_sharpe: 1.25,
            n_trades: 7,
            ..Default::default()
        });
        let s = summary_of(&ok);
        assert!(s.contains("sma cross"));
        assert!(s.contains("1.25"));
        assert!(s.contains('7'));
    }

    #[test]
    fn summary_of_error_includes_the_message() {
        let err: StudioChatOutcome = Err("no ANTHROPIC_API_KEY in the credential store".into());
        assert!(summary_of(&err).contains("ANTHROPIC_API_KEY"));
    }

    /// The `vike-cli` registered with Claude is the one beside the running executable WHEN IT IS
    /// THERE, and the bare name (`claude`'s `PATH`) when it is not: it used to name the sibling
    /// path whether or not a file sat at it.
    #[test]
    fn the_server_is_the_sibling_that_exists_else_the_bare_name() {
        let name = format!("vike-mcp{}", std::env::consts::EXE_SUFFIX);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_mcp_exe(None), name, "no executable directory");
        assert_eq!(resolve_mcp_exe(Some(dir.path())), name, "no vike-mcp beside it");
        std::fs::write(dir.path().join(&name), b"").unwrap();
        assert_eq!(
            resolve_mcp_exe(Some(dir.path())),
            dir.path().join(&name).to_string_lossy(),
            "the file that is there"
        );
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn connect_command_spawns_vike_mcp_with_the_datahub_address() {
        // The Studio's Source-backend address reaches the generated `claude mcp add` command (as
        // `--addr`) so the spawned `vike-mcp` dials the same datahub the Studio offloads to.
        let mut chat = StudioChatPane::default();
        chat.connect_to_claude(Some("<host>:7878"));
        let command = chat.connect_command.expect("connect command set");
        assert!(command.contains("vike-mcp --addr <host>:7878"), "program + addr: {command}");
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn connect_command_omits_address_when_absent() {
        // No Source backend set — the spawned server falls back to its own default address.
        let mut chat = StudioChatPane::default();
        chat.connect_to_claude(None);
        let command = chat.connect_command.expect("connect command set");
        assert!(command.contains("vike-mcp"), "{command}");
        assert!(!command.contains("--addr"), "the bare program: {command}");
    }

    #[test]
    fn diff_rows_marks_inserts_deletes_and_equals() {
        let rows = diff_rows("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(
            rows,
            vec![
                StudioChatDiffRow { kind: StudioChatDiffKind::Equal, text: "a".into() },
                StudioChatDiffRow { kind: StudioChatDiffKind::Delete, text: "b".into() },
                StudioChatDiffRow { kind: StudioChatDiffKind::Insert, text: "X".into() },
                StudioChatDiffRow { kind: StudioChatDiffKind::Equal, text: "c".into() },
            ]
        );
    }

    #[test]
    fn diff_rows_all_equal_when_identical() {
        let rows = diff_rows("one\ntwo\n", "one\ntwo\n");
        assert!(rows.iter().all(|r| r.kind == StudioChatDiffKind::Equal));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn diff_rows_from_empty_buffer_is_all_inserts() {
        let rows = diff_rows("", "fn on_bar() {}\n");
        assert!(rows.iter().all(|r| r.kind == StudioChatDiffKind::Insert));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "fn on_bar() {}");
    }

    #[test]
    fn grounding_preamble_names_the_slice_and_carries_the_example() {
        let example =
            r#"fn on_bar(strategy) { strategy.order("buy", OrderSide::Long).size(1.0); }"#;
        let g = grounding_preamble("binance", "BTCUSDT", "1m", example);
        assert!(g.contains("BTCUSDT"));
        assert!(g.contains("binance"));
        assert!(g.contains("1m"));
        assert!(g.contains(example));
        // Must NOT enumerate the indicator registry. The reason is no longer "the host binds only
        // three" — it binds most of the registry now — it is that a grounding preamble is a token
        // budget: pasting ~140 names into every message crowds out the context that is actually
        // specific to this chat. A script that wants the roster asks for it.
        assert!(!g.contains("171"));
    }
}
