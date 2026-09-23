//! ⚠ **SECURITY-CRITICAL** — the gates and the chat allowlist: the two process-env flags, the bot
//! token, and the ONE authorization question this channel asks ([`TelegramConfig::allows`]).
//!
//! A bug here means an UNAUTHORIZED chat can command the node. See the module doc of
//! [`crate::telegram`] for the four-gate contract this file's half of.

use std::collections::HashMap;

/// The two PROCESS-ENV gates, as a pure function: both must be the EXACT string `"1"` (never a
/// fuzzy truthy parse — the `VIKE_RECONCILE` discipline). `false` ⇒ the caller must not read the
/// workspace `.env`, must not build a [`TelegramConfig`], must not build an agent, and must not
/// spawn a thread.
///
/// The variable NAMES deliberately live in the daemon BINARY (`main.rs`), not here: libraries take
/// configuration as parameters, and the settings-registry gate scores a bare env-shaped literal in
/// a library file as an injected read regardless of what reads it.
pub fn control_gates_open(tradehub_control: Option<&str>, telegram_control: Option<&str>) -> bool {
    tradehub_control == Some("1") && telegram_control == Some("1")
}

/// The channel's credentials + allowlist, built from an ALREADY-LOADED workspace `.env` map — the
/// `auth::from_vars` idiom: the caller owns the I/O, this is a pure parser.
#[derive(Clone)]
pub struct TelegramConfig {
    /// The bot token. SECRET — redacted in [`Debug`], never logged, never interpolated into an
    /// error string (it is embedded in every API URL).
    ///
    /// `pub(super)` ONLY because `deps.rs` builds the API base URL from it; the module split is
    /// what widened this from private, and the reach stops at `telegram`'s own submodules.
    pub(super) bot_token: String,
    /// The chats that may command this node. NON-EMPTY by construction: an empty allowlist means
    /// the channel is disabled, not "everyone".
    allowed_chat_ids: Vec<i64>,
    /// The users that may command this node, ON TOP of the chat allowlist — OPT-IN, and the ONE
    /// list here whose empty case means "not configured" rather than "disabled".
    ///
    /// The asymmetry is deliberate and is the whole design of this field. A chat allowlist is
    /// mandatory because without one the channel would be open to the world; a USER allowlist is
    /// optional because without one the channel is exactly as authorized as it has always been —
    /// and the overwhelmingly common deployment is a single operator's DM, where a chat IS a
    /// person and requiring a second list would only be a way to lock somebody out of their own
    /// bot on upgrade. In a GROUP the two differ sharply, and that is the case worth being able to
    /// tighten: an allowlisted supergroup grants order authority to every member, and to every
    /// member anyone can later add, with no config change and nothing to review.
    ///
    /// ⚠ It can only ever NARROW: [`Self::allows_user`] is ANDed with [`Self::allows`], never
    /// ORed, and a variable that is set but yields no usable id refuses to configure the channel
    /// at all rather than degrading to this empty (= permissive) state — see [`Self::from_vars`].
    allowed_user_ids: Vec<i64>,
}

impl std::fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("bot_token", &"<redacted>")
            .field("allowed_chat_ids", &self.allowed_chat_ids)
            .field("allowed_user_ids", &self.allowed_user_ids)
            .finish()
    }
}

impl TelegramConfig {
    /// Build from the workspace `.env` map. `None` — the channel stays OFF — unless BOTH a
    /// non-blank bot token AND at least one parseable chat id are present. This is the
    /// absent-credential-is-the-gate convention the venues use, applied to a write surface: an
    /// empty or unparseable allowlist can never widen to "any chat".
    /// The OPTIONAL user allowlist rides here too: absent or blank ⇒ no user allowlist ⇒ chat-only
    /// authorization, byte-identical to before it existed. ⚠ **Set but unusable is an ERROR, not an
    /// empty list**: `VIKE_TELEGRAM_ALLOWED_USER_IDS=alice` parses to no ids, and treating that as
    /// "unconfigured" would silently authorize every member of every allowlisted chat while the
    /// operator believed they had just restricted it to one person. The channel refuses to arm
    /// instead — the same direction the chat list already takes (a blank list disables rather than
    /// widens).
    pub fn from_vars(vars: &HashMap<String, String>) -> Option<Self> {
        let bot_token = vars.get("VIKE_TELEGRAM_BOT_TOKEN").map(|s| s.trim()).unwrap_or("");
        if bot_token.is_empty() {
            return None;
        }
        let allowed_chat_ids =
            parse_chat_ids(vars.get("VIKE_TELEGRAM_ALLOWED_CHAT_IDS").map(String::as_str)?);
        if allowed_chat_ids.is_empty() {
            return None;
        }
        let allowed_user_ids =
            match vars.get("VIKE_TELEGRAM_ALLOWED_USER_IDS").map(|s| s.trim()).unwrap_or("") {
                "" => Vec::new(), // not configured ⇒ chat-only, exactly as before
                raw => {
                    let ids = parse_chat_ids(raw);
                    if ids.is_empty() {
                        tracing::error!(
                            "VIKE_TELEGRAM_ALLOWED_USER_IDS is set but contains no usable numeric \
                             user id — Telegram control NOT started. Reading it as 'no user \
                             allowlist' would authorize every member of every allowlisted chat \
                             while you believe access was just restricted. Use comma-separated \
                             numeric ids (a @handle is not one), or unset it"
                        );
                        return None;
                    }
                    ids
                }
            };
        Some(TelegramConfig {
            bot_token: bot_token.to_string(),
            allowed_chat_ids,
            allowed_user_ids,
        })
    }

    /// Construct directly (tests, and any embedder that resolved the values itself). `None` under
    /// the same rule [`Self::from_vars`] applies. No user allowlist — see [`Self::allowing_users`].
    pub fn new(bot_token: impl Into<String>, allowed_chat_ids: Vec<i64>) -> Option<Self> {
        let bot_token: String = bot_token.into();
        if bot_token.trim().is_empty() || allowed_chat_ids.is_empty() {
            return None;
        }
        Some(TelegramConfig {
            bot_token: bot_token.trim().to_string(),
            allowed_chat_ids,
            allowed_user_ids: Vec::new(),
        })
    }

    /// Add the optional per-USER allowlist to a [`Self::new`]-built config. A builder rather than a
    /// third argument to `new`, so every existing call site keeps meaning exactly what it meant.
    pub fn allowing_users(mut self, allowed_user_ids: Vec<i64>) -> Self {
        self.allowed_user_ids = allowed_user_ids;
        self
    }

    /// May this chat command the node? Half of the authorization question — see [`Self::allows_user`].
    pub fn allows(&self, chat_id: i64) -> bool {
        self.allowed_chat_ids.contains(&chat_id)
    }

    /// May this USER command the node? `true` when no user allowlist is configured (chat-only
    /// authorization, the default and the single-operator DM case), otherwise membership.
    ///
    /// ⚠ ANDed with [`Self::allows`] by the caller, never ORed: this list may only ever narrow who
    /// can command the node, so an unlisted user in an allowlisted chat is refused — and an
    /// allowlisted user in an unlisted chat still is too.
    pub fn allows_user(&self, from_id: i64) -> bool {
        self.allowed_user_ids.is_empty() || self.allowed_user_ids.contains(&from_id)
    }

    /// How many chats are allowlisted (for the enable-time log line — never the ids themselves at
    /// `warn`, they are semi-private).
    pub fn chat_count(&self) -> usize {
        self.allowed_chat_ids.len()
    }

    /// How many users are allowlisted; `0` means chat-only authorization. Same log-line use, same
    /// reason for not printing the ids.
    pub fn user_count(&self) -> usize {
        self.allowed_user_ids.len()
    }
}

/// Parse a comma-separated chat-id list. An unparseable entry is SKIPPED (with a warning), never
/// widened into a wildcard — skipping narrows the allowlist, which is the safe direction.
pub fn parse_chat_ids(raw: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        match t.parse::<i64>() {
            Ok(id) => {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            Err(_) => tracing::warn!(
                "VIKE_TELEGRAM_ALLOWED_CHAT_IDS: skipping unparseable entry (not an integer chat id)"
            ),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The minimum that configures the channel: a token and one allowlisted chat.
    fn base_vars() -> HashMap<String, String> {
        HashMap::from([
            ("VIKE_TELEGRAM_BOT_TOKEN".to_string(), "123456:test-token".to_string()),
            ("VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string(), "42".to_string()),
        ])
    }

    #[test]
    fn gates_need_both_flags_exactly_one() {
        assert!(control_gates_open(Some("1"), Some("1")));
        assert!(!control_gates_open(None, Some("1")));
        assert!(!control_gates_open(Some("1"), None));
        assert!(!control_gates_open(None, None));
        // Fuzzy truthy spellings arm NOTHING — the `VIKE_RECONCILE` discipline.
        assert!(!control_gates_open(Some("1"), Some("true")));
        assert!(!control_gates_open(Some("true"), Some("1")));
        assert!(!control_gates_open(Some("1"), Some("")));
        assert!(!control_gates_open(Some("0"), Some("1")));
    }

    #[test]
    fn config_needs_a_token_and_a_non_empty_allowlist() {
        let mut vars = HashMap::new();
        assert!(TelegramConfig::from_vars(&vars).is_none(), "empty env ⇒ nothing");
        vars.insert("VIKE_TELEGRAM_BOT_TOKEN".to_string(), "123:abc".to_string());
        assert!(TelegramConfig::from_vars(&vars).is_none(), "token without an allowlist ⇒ nothing");
        vars.insert("VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string(), "  ".to_string());
        assert!(TelegramConfig::from_vars(&vars).is_none(), "blank allowlist ⇒ nothing");
        vars.insert("VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string(), "42,-100777".to_string());
        let cfg = TelegramConfig::from_vars(&vars).expect("both present ⇒ configured");
        assert!(cfg.allows(42) && cfg.allows(-100777));
        assert!(!cfg.allows(43));
        // …and a blank token never arms it either.
        vars.insert("VIKE_TELEGRAM_BOT_TOKEN".to_string(), "   ".to_string());
        assert!(TelegramConfig::from_vars(&vars).is_none());
    }

    /// The user allowlist is OPT-IN and can only NARROW. Unset ⇒ chat-only, byte-identical to the
    /// behaviour before it existed — this is the property that keeps an upgrade from locking a DM
    /// operator out of their own bot.
    #[test]
    fn an_absent_user_allowlist_means_chat_only() {
        let mut vars = base_vars();
        let cfg = TelegramConfig::from_vars(&vars).expect("configured");
        assert_eq!(cfg.user_count(), 0);
        for anybody in [7, -3, 0, i64::MAX] {
            assert!(cfg.allows_user(anybody), "no user allowlist ⇒ every sender passes");
        }
        // A blank value is the same as absent — not "an empty list that permits nobody".
        vars.insert("VIKE_TELEGRAM_ALLOWED_USER_IDS".into(), "   ".into());
        assert_eq!(TelegramConfig::from_vars(&vars).expect("configured").user_count(), 0);
    }

    #[test]
    fn a_configured_user_allowlist_admits_only_its_members() {
        let mut vars = base_vars();
        vars.insert("VIKE_TELEGRAM_ALLOWED_USER_IDS".into(), " 55 , 66 ,, 55 ".into());
        let cfg = TelegramConfig::from_vars(&vars).expect("configured");
        assert_eq!(cfg.user_count(), 2, "deduped, blanks skipped");
        assert!(cfg.allows_user(55) && cfg.allows_user(66));
        assert!(!cfg.allows_user(77), "a chat-allowlisted stranger is refused");
        // ⚠ An update with no `message.from` is UNKNOWN_USER_ID, and that is not a wildcard: once a
        // user allowlist exists, an anonymous group admin cannot command the node.
        assert!(!cfg.allows_user(crate::telegram::UNKNOWN_USER_ID));
        // The two lists are ANDed by the caller, never ORed: the chat gate is untouched.
        assert!(cfg.allows(42) && !cfg.allows(43));
    }

    /// ⚠ **SET BUT UNUSABLE IS A REFUSAL, NOT AN EMPTY LIST.** `VIKE_TELEGRAM_ALLOWED_USER_IDS=bob`
    /// (a @handle, the obvious mistake) parses to no ids. Degrading that to "no user allowlist"
    /// would authorize every member of every allowlisted chat while the operator believed they had
    /// just restricted the bot to one person — the widening failure this whole file exists to
    /// avoid. The channel refuses to arm instead.
    #[test]
    fn a_user_allowlist_that_parses_to_nothing_refuses_to_configure() {
        for unusable in ["bob", "@bob", "alice,bob", ",,,", "*"] {
            let mut vars = base_vars();
            vars.insert("VIKE_TELEGRAM_ALLOWED_USER_IDS".into(), unusable.into());
            assert!(
                TelegramConfig::from_vars(&vars).is_none(),
                "{unusable:?} must refuse to configure, NEVER widen to chat-only"
            );
        }
    }

    #[test]
    fn the_direct_constructor_defaults_to_chat_only_and_can_add_users() {
        let cfg = TelegramConfig::new("123456:test", vec![42]).expect("configured");
        assert!(cfg.allows_user(999), "new() adds no user allowlist");
        let tightened = cfg.allowing_users(vec![55]);
        assert!(tightened.allows_user(55) && !tightened.allows_user(999));
    }

    /// The allowlists are semi-private but not secret; the TOKEN is. Both lists may appear.
    #[test]
    fn config_debug_redacts_the_bot_token() {
        let cfg = TelegramConfig::new("123456:SUPER-SECRET", vec![7]).unwrap();
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("SUPER-SECRET"), "the token must never reach Debug: {rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn chat_ids_parse_skipping_garbage_and_dupes() {
        assert_eq!(parse_chat_ids(" 42 , -100777 ,, 42 "), vec![42, -100777]);
        assert_eq!(parse_chat_ids("nope"), Vec::<i64>::new());
        assert_eq!(parse_chat_ids(""), Vec::<i64>::new());
        // A garbage entry NARROWS (it is dropped), it never widens into a wildcard.
        assert_eq!(parse_chat_ids("7,*,8"), vec![7, 8]);
    }
}
