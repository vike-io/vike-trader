//! ChatPane: the in-app AI copilot over vike-ai's tested `develop_strategy` loop (Studio SP3 Part
//! B, Task 5). Thin over that loop: `send()` spawns a worker thread and returns a receiver, exactly
//! mirroring `vike-studio-core::run::spawn_run`'s `std::thread::spawn` + `mpsc::channel` shape —
//! `StudioState` owns the `Receiver<ChatOutcome>` (the `chat_rx` field, drained by the 4th `poll()`
//! arm) the same way it owns `run_rx`/`sweep_rx`/`wf_rx`. egui paint is not unit-tested here (SP2
//! discipline); only the pure helpers (`apply_result`, [`ChatApiKeys`], `summary_of`) and `send`'s
//! wiring are.
//!
//! ⚠ This pane reads NO credential store and NO environment (split-plane I8). It used to call the
//! workspace `.env` loader twice — once at construction to decide which providers to offer, once
//! per `send()` for the key itself — which made a GUI pane the thing that opened
//! `<project>/settings/secrets.env`, dragged `vike-bridge-core` (and vike-exec behind it) into a
//! crate that is meant to become a thin client, and put `crates/vike-studio/src/chat.rs` on
//! `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`. The keys arrive as a
//! [`ChatApiKeys`] parameter now, resolved by the composition root from the sweep it already owns.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use vike_ai::{develop_strategy_with_ledger, make_client, AgentResult, LedgerPaths, Provider};
use vike_data::HistStore;

/// Outcome of one chat -> strategy round trip on the worker thread: the agent's result, or a
/// human-readable failure (no API key, or the loop panicked — surfaced by `poll()`'s disconnect arm).
pub type ChatOutcome = Result<AgentResult, String>;

/// Env-var name each provider's key is read from — the naming authority for [`ChatApiKeys::resolve`]
/// and for `send`'s "no key" message alike, so the two cannot name different variables.
fn key_env_var(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "ANTHROPIC_API_KEY",
        Provider::Cerebras => "CEREBRAS_API_KEY",
    }
}

/// The AI-provider keys the ChatPane needs — resolved by the BINARY and handed in, never read here.
///
/// These are NOT venue credentials: they buy strategy generation from a third-party model, and
/// absent, the pane simply offers no provider and the Send button stays disabled. Nothing about
/// trading changes.
///
/// # Where they come from
///
/// `<project>/settings/secrets.env` — the same store every venue credential lives in, resolved by
/// the same `vike_secrets` walk. It arrives as a PARAMETER ([`ChatApiKeys::resolve`]) because this
/// is a library: only the binary reads global configuration state. The same shape and the same
/// argument as `vike_app_core::tools::ToolApiKeys`, which the app root already resolves from the
/// one credential map it loads.
///
/// Resolved ONCE, at construction: a key added to the store afterwards needs a restart. That was
/// already true before the injection — the provider list `has_key()` gates Send on was computed at
/// construction, so a later `send()`'s second store read could never enable a provider the picker
/// was not already offering.
///
/// ⚠ A present-but-BLANK value counts as PRESENT, deliberately preserving the pre-injection
/// behaviour exactly: the old gate was `contains_key`, so an `ANTHROPIC_API_KEY=` line offered the
/// provider and handed the empty string to `make_client`. (`ToolApiKeys` makes the opposite call
/// for its two knobs; there, blank and absent produce the identical blank column, so falling
/// through costs nothing. Here they differ — blank is offered and fails at the provider — and
/// silently changing which providers the picker lists is not this PR's business.)
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChatApiKeys {
    /// `ANTHROPIC_API_KEY`.
    pub anthropic: Option<String>,
    /// `CEREBRAS_API_KEY`.
    pub cerebras: Option<String>,
}

impl ChatApiKeys {
    /// Resolve both keys from an already-loaded credential map.
    ///
    /// PURE — no environment read, no file read, so the provider-availability rule is unit-testable
    /// without a store on disk or a mutated process global.
    pub fn resolve(store: &HashMap<String, String>) -> Self {
        Self {
            anthropic: store.get(key_env_var(Provider::Anthropic)).cloned(),
            cerebras: store.get(key_env_var(Provider::Cerebras)).cloned(),
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
    /// once at `ChatPane::new`, not per frame).
    pub fn providers(&self) -> Vec<Provider> {
        [Provider::Anthropic, Provider::Cerebras]
            .into_iter()
            .filter(|p| self.key(*p).is_some())
            .collect()
    }
}

/// Redacts. A key is a secret, and this type is the only place one is NAMED rather than living as
/// an anonymous local — so it gets the same manual `Debug` every credential type in this workspace
/// has.
impl std::fmt::Debug for ChatApiKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = |v: &Option<String>| if v.is_some() { "<set>" } else { "<unset>" };
        f.debug_struct("ChatApiKeys")
            .field("anthropic", &shown(&self.anthropic))
            .field("cerebras", &shown(&self.cerebras))
            .finish()
    }
}

/// Apply the agent's generated script into the editor buffer. The Studio shows a line-level diff
/// ([`diff_rows`]) of `editor_source` -> `result.code` first, so Apply is a reviewed action rather
/// than a blind clobber; the write itself is still a whole-buffer replace once the user accepts.
pub fn apply_result(editor_source: &mut String, result: &AgentResult) {
    *editor_source = result.code.clone();
}

/// One line of a rendered diff: whether it's unchanged, inserted (in `new`), or deleted (from `old`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Equal,
    Insert,
    Delete,
}

/// A single diff line: its change kind plus the line text (newline stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub text: String,
}

/// Local grounding for the AI chat: a small context block prepended to the user's prompt, built
/// from data already in memory in the running app (the target slice + a valid template as a
/// worked example). NO database, no retrieval index — see the module note; this is "format what
/// you already have." Deliberately does NOT enumerate the `vike-indicators` registry.
///
/// ⚠ The reason CHANGED and the old one is worth recording, because it inverted: this used to say
/// the host binds only `sma`/`ema`/`rsi`, so naming the rest would invite calls that cannot bind.
/// The host now binds most of the registry (`vike_script::RHAI_INDICATORS`, derived from it), so
/// that hazard is gone. What remains is a token budget — a preamble is prepended to EVERY message,
/// and spending it on a fixed roster crowds out the context specific to this chat. A model that
/// wants the roster asks for it; an unbound name raises on the first call rather than returning a
/// plausible number, so a guess fails loudly. Pure/testable.
pub fn grounding_preamble(venue: &str, symbol: &str, interval: &str, example_code: &str) -> String {
    format!(
        "# Context\n\
         Generate a strategy to trade {symbol} on {venue} at the {interval} interval; it is \
         backtested on that instrument's out-of-sample data. Reads/orders are host functions on \
         the CURRENT bar (see the system prompt); there is no cross-bar mutable state in the script.\n\n\
         # A valid strategy for reference (adapt to the request — do not copy verbatim):\n\
         ```rhai\n{}\n```",
        example_code.trim()
    )
}

/// Line-level diff of the current editor buffer (`old`) against the AI-generated script (`new`),
/// for the ChatPane "review changes" view. A pure transform over the two strings (unit-tested);
/// the egui rendering that colors these rows is not (SP2 discipline).
pub fn diff_rows(old: &str, new: &str) -> Vec<DiffRow> {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .map(|change| {
            let kind = match change.tag() {
                similar::ChangeTag::Equal => DiffKind::Equal,
                similar::ChangeTag::Insert => DiffKind::Insert,
                similar::ChangeTag::Delete => DiffKind::Delete,
            };
            DiffRow { kind, text: change.value().trim_end_matches('\n').to_string() }
        })
        .collect()
}

/// One assistant transcript line summarizing a chat outcome — accepted (with the OOS score),
/// rejected (with why), or a hard failure (no key / worker disconnect).
pub fn summary_of(outcome: &ChatOutcome) -> String {
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
/// picker's selected slice, and the editor buffer, none of which `ChatPane` itself owns).
pub struct ChatPane {
    pub input: String,
    /// (role, text) transcript rows — role is "user" or "assistant".
    pub history: Vec<(String, String)>,
    pub provider: Provider,
    pub last: Option<AgentResult>,
    pub running: bool,
    /// The caller-supplied provider keys (see [`ChatApiKeys`]) — the pane opens no store of its own.
    keys: ChatApiKeys,
    /// Providers whose key was present at construction — the picker's "refresh is a one-shot
    /// maintenance walk" pattern, not a per-frame read.
    available: Vec<Provider>,
    /// The copy-pasteable `claude mcp add …` line from the last "Connect to Claude" click.
    pub connect_command: Option<String>,
}

/// A KEYLESS pane: no provider is offered and Send stays disabled. The honest default for a
/// constructor that was handed nothing, and what the headless capture harness wants.
impl Default for ChatPane {
    fn default() -> Self {
        Self::new(ChatApiKeys::default())
    }
}

impl ChatPane {
    /// `keys` is resolved by the CALLER — see [`ChatApiKeys`] for where they come from and why the
    /// pane no longer opens the credential store itself.
    pub fn new(keys: ChatApiKeys) -> Self {
        let available = keys.providers();
        let provider = available.first().copied().unwrap_or_default();
        Self {
            input: String::new(),
            history: Vec::new(),
            provider,
            last: None,
            running: false,
            keys,
            available,
            connect_command: None,
        }
    }

    /// Providers the selector may offer (key present at construction time).
    pub fn available_providers(&self) -> &[Provider] {
        &self.available
    }

    /// The current provider has a usable key — gates the Send button alongside `!running`.
    pub fn has_key(&self) -> bool {
        self.available.contains(&self.provider)
    }

    /// Spawn the worker thread and return its receiver — no-op-shaped: the caller (`StudioState`)
    /// checks `!running && has_key()` before calling this, matching `start_run`'s `if self.running {
    /// return; }` guard at the call site rather than duplicating it here. Consumes `self.input` into
    /// the transcript as the user turn immediately (so it shows up in the panel before the worker
    /// replies).
    ///
    /// `ledger` is the persistent trial ledger + learnings pair (`vike_ai::ledger`), RESOLVED BY
    /// THE CALLER — `StudioState` colocates it with the store root, the same way it resolves
    /// `studio_strategies.json`/`studio_workspace.json`; `vike-ai` itself reads no environment
    /// variable and knows no default location. `None` runs the ledger-less loop (no history read,
    /// nothing recorded, `deflated_sharpe` always `0.0`).
    pub fn send(
        &mut self,
        store: Arc<dyn HistStore + Send + Sync>,
        venue: String,
        symbol: String,
        interval: String,
        ledger: Option<LedgerPaths>,
    ) -> Receiver<ChatOutcome> {
        let prompt = self.input.clone();
        self.history.push(("user".to_string(), prompt.clone()));
        self.input.clear();
        self.running = true;

        // Local grounding: prepend an in-memory context block (target slice + a template example)
        // to the model prompt. The transcript keeps the raw user turn (pushed above).
        let example = vike_studio_core::templates::templates().first().map(|t| t.1).unwrap_or("");
        let grounding = grounding_preamble(&venue, &symbol, &interval, example);
        let prompt = format!("{grounding}\n\n# Request\n{prompt}");

        let provider = self.provider;
        let key = self.keys.key(provider);

        // Same worker-thread spawn shape as the Run pipeline — routed through the shared
        // `vike_studio_core::spawn_outcome` so this `channel()` + `thread::spawn` + `send` wrapper
        // lives in exactly one place.
        vike_studio_core::spawn_outcome(move || match make_client(provider, key) {
            None => Err(format!("set {} in .env", key_env_var(provider))),
            Some(client) => Ok(develop_strategy_with_ledger(
                &prompt,
                &venue,
                &symbol,
                &interval,
                store.as_ref(),
                client.as_ref(),
                2,
                0.3,
                ledger.as_ref(),
            )),
        })
    }

    /// Build the "Connect to Claude" MCP entry + the copy-pasteable `claude mcp add …` command and
    /// stash it in `connect_command` for the panel to render. The spawned server is `vike-cli mcp`
    /// (the one MCP server since the vike-mcp retirement — Phase B of #842): its run/list tools go
    /// over a RUNNING vike-datahub server, so `datahub_addr` (the Studio's Remote-backend address,
    /// when one is set) is threaded into the entry as `--addr`; `None` lets the server fall back to
    /// its own default (`127.0.0.1:7878`).
    ///
    /// TODO STUB: `exe_path` is a best-effort guess (the `vike-cli` binary next to the running
    /// executable) — correct when the two ship co-located, but not a resolved install path. Real
    /// install-path resolution remains a documented follow-up (spec §2 ledger).
    ///
    /// Only compiled to actually build the command under the non-default `mcp` feature (a plain
    /// code gate over the `connect` module — no crate dep since the vike-mcp retirement); without
    /// it the button records a "rebuild with --features mcp" note instead, so the chat panel still
    /// compiles and renders identically minus the copyable command.
    pub fn connect_to_claude(&mut self, datahub_addr: Option<&str>) {
        #[cfg(feature = "mcp")]
        {
            let exe_path = guessed_cli_exe_path();
            let entry = crate::connect::server_entry(&exe_path, datahub_addr);
            self.connect_command =
                Some(crate::connect::claude_code_add_command("vike-studio", &entry));
        }
        #[cfg(not(feature = "mcp"))]
        {
            let _ = datahub_addr;
            self.connect_command = Some(
                "MCP connect helper not compiled in — rebuild vike-studio with `--features mcp` \
                 to generate the `claude mcp add` command that registers `vike-cli mcp` (needs a \
                 running vike-datahub server)"
                    .to_string(),
            );
        }
    }
}

/// Best-effort `vike-cli` binary path: the sibling of the currently running executable. This is a
/// guess, not a resolution — see `connect_to_claude`'s doc comment.
#[cfg(feature = "mcp")]
fn guessed_cli_exe_path() -> String {
    let name = if cfg!(windows) { "vike-cli.exe" } else { "vike-cli" };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(name)))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string())
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
        let opts = ChatApiKeys::resolve(&vars).providers();
        assert!(opts.contains(&Provider::Anthropic));
        assert!(!opts.contains(&Provider::Cerebras)); // no CEREBRAS_API_KEY
    }

    /// The injection's whole point: an EMPTY map is a complete answer, and it is the one a pane
    /// that opens no store gets when the caller has nothing to give it.
    #[test]
    fn no_keys_means_no_providers_and_no_send() {
        let pane = ChatPane::default();
        assert!(pane.available_providers().is_empty());
        assert!(!pane.has_key());
        assert_eq!(ChatApiKeys::resolve(&HashMap::new()), ChatApiKeys::default());
    }

    /// A present-but-BLANK value counts as present — the pre-injection `contains_key` behaviour,
    /// pinned so a later "tidy-up" cannot silently change which providers the picker lists.
    #[test]
    fn a_blank_key_still_offers_the_provider() {
        let mut vars = HashMap::new();
        vars.insert("CEREBRAS_API_KEY".to_string(), String::new());
        assert_eq!(ChatApiKeys::resolve(&vars).providers(), vec![Provider::Cerebras]);
    }

    /// A key must never reach a log line or a panic message through the derived formatter.
    #[test]
    fn debug_redacts_the_keys() {
        let mut vars = HashMap::new();
        vars.insert("ANTHROPIC_API_KEY".to_string(), "sk-secret".to_string());
        let shown = format!("{:?}", ChatApiKeys::resolve(&vars));
        assert!(!shown.contains("sk-secret"), "the key itself must not render: {shown}");
        assert!(shown.contains("<set>") && shown.contains("<unset>"), "{shown}");
    }

    #[test]
    fn summary_of_accepted_includes_sharpe_and_trades() {
        let ok: ChatOutcome = Ok(AgentResult {
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
        let err: ChatOutcome = Err("set ANTHROPIC_API_KEY in .env".into());
        assert!(summary_of(&err).contains("ANTHROPIC_API_KEY"));
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn connect_command_spawns_vike_cli_mcp_with_the_datahub_addr() {
        // The Studio's Remote-backend address reaches the generated `claude mcp add` command (as
        // `--addr`) so the spawned `vike-cli mcp` dials the same datahub the Studio offloads to.
        let mut chat = ChatPane::default();
        chat.connect_to_claude(Some("<host>:7878"));
        let cmd = chat.connect_command.expect("connect command set");
        assert!(cmd.contains("vike-cli"));
        assert!(cmd.contains(" mcp --addr <host>:7878"), "subcommand + addr: {cmd}");
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn connect_command_omits_addr_when_absent() {
        // No Remote backend set — the spawned server falls back to its own default address.
        let mut chat = ChatPane::default();
        chat.connect_to_claude(None);
        let cmd = chat.connect_command.expect("connect command set");
        assert!(cmd.contains("vike-cli"));
        assert!(cmd.ends_with(" mcp"), "bare `mcp` subcommand, no --addr: {cmd}");
        assert!(!cmd.contains("--addr"));
    }

    #[test]
    fn diff_rows_marks_inserts_deletes_and_equals() {
        let rows = diff_rows("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(
            rows,
            vec![
                DiffRow { kind: DiffKind::Equal, text: "a".into() },
                DiffRow { kind: DiffKind::Delete, text: "b".into() },
                DiffRow { kind: DiffKind::Insert, text: "X".into() },
                DiffRow { kind: DiffKind::Equal, text: "c".into() },
            ]
        );
    }

    #[test]
    fn diff_rows_all_equal_when_identical() {
        let rows = diff_rows("one\ntwo\n", "one\ntwo\n");
        assert!(rows.iter().all(|r| r.kind == DiffKind::Equal));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn diff_rows_from_empty_buffer_is_all_inserts() {
        let rows = diff_rows("", "fn on_bar() {}\n");
        assert!(rows.iter().all(|r| r.kind == DiffKind::Insert));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "fn on_bar() {}");
    }

    #[test]
    fn grounding_preamble_names_the_slice_and_carries_the_example() {
        let g = grounding_preamble("binance", "BTCUSDT", "1m", "fn on_bar() { buy(1.0); }");
        assert!(g.contains("BTCUSDT"));
        assert!(g.contains("binance"));
        assert!(g.contains("1m"));
        assert!(g.contains("fn on_bar() { buy(1.0); }"));
        // Must NOT enumerate the indicator registry. The reason is no longer "the host binds only
        // three" — it binds most of the registry now — it is that a grounding preamble is a token
        // budget: pasting ~140 names into every message crowds out the context that is actually
        // specific to this chat. A script that wants the roster asks for it.
        assert!(!g.contains("171"));
    }
}
