//! The `--profile` scoping rings and the one `ToolAccess` value the roster and the router share.

use serde_json::{Value, json};

use super::is_write_tool;
use super::tool_schemas::tools_spec;
#[cfg(doc)]
use super::{Server, config::parse_config};

/// The scoping RINGS. `full` ⊃ `read-only` ⊃ `offline`, and each inner ring is a PREDICATE over
/// data this file already had rather than a roster somebody typed — the module doc argues both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    /// Every tool this server implements. The default, so an absent `--profile` is byte-identical
    /// to the surface that shipped before the flag existed.
    Full,
    /// Everything except the mandatory-preview write tools — `!`[`is_write_tool`], so the ring
    /// cannot drift from the roster the gate itself routes on.
    ReadOnly,
    /// Only the tools that declare they open no socket (`annotations.openWorldHint == false`): the
    /// offline authoring set. A tool declaring nothing is WITHHELD, because the promise of this
    /// ring is negative and a missing declaration is not evidence for it.
    Offline,
}

/// The spelling of each ring, and the ONLY place a profile name is parsed or printed.
///
/// An array rather than a `match` in three places: the parse, the usage message and the refusal all
/// read it, so an added ring cannot reach one of them and miss another.
const PROFILES: [(&str, Profile); 3] =
    [("full", Profile::Full), ("read-only", Profile::ReadOnly), ("offline", Profile::Offline)];

impl Profile {
    /// Parse a `--profile` value.
    ///
    /// ⚠ **An unknown name is an ERROR and never a fallback.** A `_ => Full` arm would mean a typo
    /// in an MCP client's launch config silently serves the order-write tools to an agent the
    /// operator believed was scoped down — the exact failure a scoping feature exists to prevent.
    pub(super) fn parse(name: &str) -> Result<Self, String> {
        PROFILES.iter().find(|(n, _)| *n == name).map(|(_, p)| *p).ok_or_else(|| {
            format!("unknown --profile {name:?} — valid profiles: {}", profile_names().join(", "))
        })
    }

    /// The wire/CLI spelling.
    fn as_str(self) -> &'static str {
        PROFILES.iter().find(|(_, p)| *p == self).map(|(n, _)| *n).unwrap_or("full")
    }

    /// Does this ring admit the tool this [`tools_spec`] entry describes?
    fn admits_spec(self, tool: &Value) -> bool {
        match self {
            Profile::Full => true,
            Profile::ReadOnly => !is_write_tool(tool["name"].as_str().unwrap_or_default()),
            Profile::Offline => tool["annotations"]["openWorldHint"] == json!(false),
        }
    }
}

/// Every profile name, in declaration order — for the parse error and the usage text, which must
/// name the same set the parser accepts.
pub(super) fn profile_names() -> Vec<&'static str> {
    PROFILES.iter().map(|(n, _)| *n).collect()
}

/// WHICH tools this server serves — the one value `tools/list`, `tools/call`, `resources/list` and
/// `resources/read` all consult.
///
/// ⚠ **`allowed` and `withheld` are complements produced by ONE pass** over [`tools_spec`]. That is
/// the whole design: an advertised roster and a routing gate computed separately are two lists that
/// can disagree, and a tool that is advertised but refused — or worse, withheld but routed — is
/// precisely the bug a scoping feature must not ship with.
#[derive(Debug, Clone)]
pub(crate) struct ToolAccess {
    profile: Profile,
    /// The `--deny-tool` names, each already validated against the served roster.
    denied: Vec<String>,
    /// Served ∧ admitted.
    allowed: Vec<String>,
    /// Served ∧ NOT admitted — the complement, from the same pass.
    withheld: Vec<String>,
}

impl ToolAccess {
    /// Resolve a profile plus a deny list into the served/withheld split.
    ///
    /// A `denied` name this server does not serve is the CALLER's problem to reject (see
    /// [`parse_config`]): a silently-ignored `--deny-tool` is an operator believing a tool is gone.
    pub(crate) fn new(profile: Profile, denied: Vec<String>) -> Self {
        let spec = tools_spec();
        let (mut allowed, mut withheld) = (Vec::new(), Vec::new());
        for tool in spec.as_array().map(Vec::as_slice).unwrap_or_default() {
            let name = tool["name"].as_str().unwrap_or_default().to_string();
            if profile.admits_spec(tool) && !denied.contains(&name) {
                allowed.push(name);
            } else {
                withheld.push(name);
            }
        }
        Self { profile, denied, allowed, withheld }
    }

    /// The unrestricted access — the default, and what every test that predates the flag means by
    /// "a server".
    pub(crate) fn full() -> Self {
        Self::new(Profile::Full, Vec::new())
    }

    /// The profile's name, for a refusal message and for the transcript record.
    pub(crate) fn profile_name(&self) -> &'static str {
        self.profile.as_str()
    }

    /// Does this server serve `name`?
    ///
    /// A name this server does not implement at all is admitted here so that [`Server::call_tool`]'s
    /// own `unknown tool` arm answers it: telling an agent that a nonexistent tool is "withheld by
    /// the profile" would be a lie, and one that sends it looking for a flag to set.
    pub(crate) fn admits(&self, name: &str) -> bool {
        !self.withheld.iter().any(|t| t == name)
    }

    /// The `tools/list` payload for this access — [`tools_spec`] filtered by the SAME set
    /// [`ToolAccess::admits`] reads.
    pub(super) fn advertised(&self) -> Value {
        let spec = tools_spec();
        let tools: Vec<Value> = spec
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|t| self.admits(t["name"].as_str().unwrap_or_default()))
            .cloned()
            .collect();
        Value::Array(tools)
    }

    /// Why `name` is not available, and what to do about it.
    ///
    /// It names the profile, the tools that ARE served, and the fact that this server cannot widen
    /// its own scope — an agent that reads "not available" without the last clause otherwise spends
    /// its next three turns hunting for the tool that turns it on.
    pub(super) fn refusal(&self, name: &str) -> String {
        let by_deny = self.denied.iter().any(|t| t == name);
        let cause = if by_deny {
            format!("it was withheld by `--deny-tool {name}`")
        } else {
            format!("this server is running under the `{}` tool profile", self.profile_name())
        };
        format!(
            "tool `{name}` is not available: {cause}. Tools served in this session: {}. Only the \
             OPERATOR can widen this, by restarting the server with `--profile full` (and without \
             that `--deny-tool`) in the MCP client's launch command — this server cannot widen its \
             own scope, and asking again will not change the answer.",
            self.allowed.join(", ")
        )
    }
}
