//! Settings > AI's **Connect Claude**: one click that makes the user's Claude Desktop start THIS
//! installed app as its local MCP server, `<this exe> --mcp` over stdio. Nothing leaves the PC for
//! it: no cloud, no public endpoint, no port.
//!
//! It finds Claude Desktop's config file ([`find_claude_desktop`]) and sets one entry in it,
//! `mcpServers.vike = {"command": "<this exe>", "args": ["--mcp"], "env": {"VIKE_SETTINGS_DIR": …}}`
//! ([`connect`]). Claude starts a server with a fixed list of environment variables and no working
//! directory, so the settings folder this desktop booted from travels in `env`, or the `--mcp`
//! process would find no project. A `.mcpb` is not used (research of 2026-11-25): Windows has no
//! file association for it, an extension that runs an exe outside its bundle is outside the spec and
//! unproven, and mcpb issue #281 (a Store build closing the install window) is still open.
//!
//! ⚠ The file is the USER'S and holds their other servers, so the write keeps every other byte:
//! - [`merge_entry`] SPLICES the one entry into the text instead of re-serializing the document (the
//!   workspace's `serde_json` keeps no key order, so a round trip would reorder the user's keys), then
//!   re-parses the result and refuses unless it is the old document plus exactly that entry;
//! - a file that is not a JSON object is refused and left untouched;
//! - an existing file is copied to a dated backup beside it before it is replaced;
//! - the replace is `vike_utility::atomic_file::write_atomic` (never remove-then-write; Windows'
//!   `rename` over an existing file is handled there).
//!
//! Claude reads the file only when it starts. It has no clean quit a program can ask for, so when it
//! is running the line after the click asks the user to quit it from its tray icon; nothing here
//! ends Claude. The environment arrives as the binary's swept map (the library rule).

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_paths::state_path::SETTINGS_DIR_ENV;

use crate::backend::mcp_mode::MCP_FLAG;

/// The key of this app's entry under `mcpServers`.
pub const SERVER_KEY: &str = "vike";

/// The key of Claude Desktop's server table.
const SERVERS: &str = "mcpServers";

/// Claude Desktop's config file name, the same on every platform.
const CONFIG_FILE: &str = "claude_desktop_config.json";

/// What Claude Desktop is told to run: `exe --mcp`, with `settings_dir` as `VIKE_SETTINGS_DIR` when
/// the desktop booted from one.
pub fn entry_for(exe: &str, settings_dir: Option<&str>) -> Value {
    let mut entry = serde_json::json!({ "command": exe, "args": [MCP_FLAG] });
    if let Some(dir) = settings_dir {
        entry["env"] = serde_json::json!({ SETTINGS_DIR_ENV: dir });
    }
    entry
}

/// This executable and `settings_dir` as Claude Desktop's entry ([`entry_for`]); both must be UTF-8.
pub fn this_entry(settings_dir: Option<&Path>) -> Result<Value, ClaudeConnectError> {
    let exe = std::env::current_exe().map_err(ClaudeConnectError::NoExe)?;
    let utf8 = |p: &Path| {
        p.to_str().map(str::to_string).ok_or_else(|| ClaudeConnectError::NotUtf8(p.to_path_buf()))
    };
    Ok(entry_for(&utf8(&exe)?, settings_dir.map(utf8).transpose()?.as_deref()))
}

/// Why Connect Claude writes nothing: a typed reason, worded once by its `Display`.
#[derive(Debug)]
pub enum ClaudeConnectError {
    /// This app cannot find its own file.
    NoExe(std::io::Error),
    /// A path the entry must name is not UTF-8.
    NotUtf8(PathBuf),
    /// The config file exists and cannot be read.
    Unreadable(std::io::Error),
    /// The config file is not UTF-8 text.
    NotText,
    /// The merge left the file as it is.
    Refused(MergeRefusal),
    /// A write failed: the file and the system's words.
    Write(PathBuf, std::io::Error),
}

impl std::fmt::Display for ClaudeConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoExe(e) => write!(f, "this app cannot find its own file ({e})"),
            Self::NotUtf8(path) => write!(f, "{} is not UTF-8", path.display()),
            Self::Unreadable(e) => write!(f, "it cannot be read ({e})"),
            Self::NotText => f.write_str("it is not UTF-8 text"),
            Self::Refused(why) => why.fmt(f),
            Self::Write(path, e) => write!(f, "{} cannot be written ({e})", path.display()),
        }
    }
}

/// `text` split into a leading UTF-8 byte-order mark (an editor's; empty when there is none) and the
/// JSON after it.
fn split_bom(text: &str) -> (&str, &str) {
    text.split_at(if text.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 })
}

/// The platforms Claude Desktop ships for, and the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudePlatform {
    Windows,
    MacOs,
    Other,
}

impl ClaudePlatform {
    /// The platform this binary was built for.
    pub const HERE: Self = if cfg!(windows) {
        Self::Windows
    } else if cfg!(target_os = "macos") {
        Self::MacOs
    } else {
        Self::Other
    };
}

/// Where this PC's Claude Desktop keeps its config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeDesktopFound {
    /// Claude Desktop is not made for this platform.
    Unsupported,
    /// No install of it was found.
    NotFound,
    /// The config file Claude reads (it may not exist yet).
    Config(PathBuf),
}

/// Find the config file Claude Desktop reads, by the platform's layout, from the swept environment.
///
/// - Windows: `%APPDATA%\Claude\claude_desktop_config.json`, for the classic installer
///   (`%LOCALAPPDATA%\AnthropicClaude\`) and the Store (MSIX) package alike. A package reads its own
///   copy, `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\Claude\…`, first when that FILE
///   exists, and the real one otherwise (Windows' rule for a packaged app's AppData; Claude 6.x keeps
///   its data in the real folder).
/// - macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`.
///
/// Claude Desktop counts as installed when its data folder, its program folder or its package exists.
pub fn find_claude_desktop(
    platform: ClaudePlatform,
    vars: &HashMap<String, String>,
) -> ClaudeDesktopFound {
    // Literal keys at each `.get`, so the settings registry proves every read.
    let var = |value: Option<&String>| value.filter(|v| !v.is_empty()).map(PathBuf::from);
    let data_dir = match platform {
        ClaudePlatform::Windows => {
            let local = var(vars.get("LOCALAPPDATA"));
            let mut packages: Vec<PathBuf> = local
                .as_ref()
                .and_then(|l| std::fs::read_dir(l.join("Packages")).ok())
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("Claude_"))
                .map(|e| e.path())
                .collect();
            packages.sort();
            let copy = packages
                .iter()
                .map(|p| p.join("LocalCache").join("Roaming").join("Claude").join(CONFIG_FILE))
                .find(|f| f.is_file());
            if let Some(copy) = copy {
                return ClaudeDesktopFound::Config(copy);
            }
            let program = local.map(|l| l.join("AnthropicClaude"));
            var(vars.get("APPDATA")).map(|a| a.join("Claude")).filter(|dir| {
                !packages.is_empty() || dir.is_dir() || program.is_some_and(|p| p.is_dir())
            })
        }
        ClaudePlatform::MacOs => var(vars.get("HOME"))
            .map(|h| h.join("Library").join("Application Support").join("Claude"))
            .filter(|dir| dir.is_dir() || Path::new("/Applications/Claude.app").is_dir()),
        ClaudePlatform::Other => return ClaudeDesktopFound::Unsupported,
    };
    data_dir
        .map_or(ClaudeDesktopFound::NotFound, |d| ClaudeDesktopFound::Config(d.join(CONFIG_FILE)))
}

/// Why a config file is left exactly as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeRefusal {
    /// It does not parse as JSON (serde_json's words).
    NotJson(String),
    /// It parses, but is not a JSON object.
    NotAnObject,
    /// Its `mcpServers` is there but is not an object.
    ServersNotAnObject,
    /// The splice did not come out as the old document plus the one entry (a defect here, caught).
    SpliceMismatch,
}

impl std::fmt::Display for MergeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson(why) => write!(f, "it is not valid JSON ({why})"),
            Self::NotAnObject => f.write_str("it is not a JSON object"),
            Self::ServersNotAnObject => write!(f, "its \"{SERVERS}\" is not an object"),
            Self::SpliceMismatch => {
                f.write_str("vike could not add its entry without changing others")
            }
        }
    }
}

/// `text` (the file's contents, `None` when it does not exist) with `mcpServers.vike` set to
/// `entry` and every other byte kept; `Ok(None)` when it already holds exactly that entry.
pub fn merge_entry(text: Option<&str>, entry: &Value) -> Result<Option<String>, MergeRefusal> {
    let text = text.unwrap_or("");
    // A byte-order mark (an editor's) is kept, and is not JSON.
    let (bom, body) = split_bom(text);
    if body.trim().is_empty() {
        let doc = serde_json::json!({ SERVERS: { SERVER_KEY: entry } });
        let pretty =
            serde_json::to_string_pretty(&doc).map_err(|_| MergeRefusal::SpliceMismatch)?;
        return Ok(Some(format!("{bom}{pretty}\n")));
    }
    let before: Value =
        serde_json::from_str(body).map_err(|e| MergeRefusal::NotJson(e.to_string()))?;
    let Some(root) = before.as_object() else { return Err(MergeRefusal::NotAnObject) };
    let servers = root.get(SERVERS);
    if servers.and_then(|s| s.get(SERVER_KEY)) == Some(entry) {
        return Ok(None);
    }
    let value = serde_json::to_string(entry).map_err(|_| MergeRefusal::SpliceMismatch)?;
    let top = object_at(body, body.len() - body.trim_start().len());
    let spliced = match (servers, top.last(SERVERS)) {
        (None, _) => {
            insert(body, &top, 1, &format!("\"{SERVERS}\": {{\"{SERVER_KEY}\": {value}}}"))
        }
        (Some(Value::Object(_)), Some(table)) => {
            let inner = object_at(body, table.start);
            match inner.last(SERVER_KEY) {
                Some(old) => splice(body, old, &value),
                None => insert(body, &inner, 2, &format!("\"{SERVER_KEY}\": {value}")),
            }
        }
        _ => return Err(MergeRefusal::ServersNotAnObject),
    };
    let mut expected = before;
    expected[SERVERS][SERVER_KEY] = entry.clone();
    match serde_json::from_str::<Value>(&spliced) {
        Ok(after) if after == expected => Ok(Some(format!("{bom}{spliced}"))),
        _ => Err(MergeRefusal::SpliceMismatch),
    }
}

/// One JSON object's members: each key (decoded) with its value's byte range, and its closing `}`.
struct JsonObject {
    members: Vec<(String, Range<usize>)>,
    close: usize,
}

impl JsonObject {
    /// The value range of the LAST member named `key`: the one a JSON reader keeps.
    fn last(&self, key: &str) -> Option<Range<usize>> {
        self.members.iter().rev().find(|(k, _)| k == key).map(|(_, r)| r.clone())
    }
}

/// The object whose `{` is at `open` in `text`, which `serde_json` has already accepted, so this
/// walk only finds the boundaries. Every byte it stops on is ASCII, so every index is a char
/// boundary.
fn object_at(text: &str, open: usize) -> JsonObject {
    let b = text.as_bytes();
    let mut members = Vec::new();
    let mut i = skip_ws(b, open + 1);
    while b[i] != b'}' {
        let key_end = value_end(b, i);
        let key = serde_json::from_str(&text[i..key_end]).unwrap_or_default();
        let start = skip_ws(b, skip_ws(b, key_end) + 1);
        let end = value_end(b, start);
        members.push((key, start..end));
        i = skip_ws(b, end);
        if b[i] == b',' {
            i = skip_ws(b, i + 1);
        }
    }
    JsonObject { members, close: i }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// One past the end of the JSON value that starts at `i`.
fn value_end(b: &[u8], i: usize) -> usize {
    match b[i] {
        b'"' => {
            let mut j = i + 1;
            while b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            j + 1
        }
        b'{' | b'[' => {
            let (mut depth, mut j) = (0usize, i);
            loop {
                match b[j] {
                    b'"' => {
                        j = value_end(b, j);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => b[i..]
            .iter()
            .position(|c| matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'))
            .map_or(b.len(), |n| i + n),
    }
}

/// `text` with `member` added as the last member of `object`, indented `depth` levels.
fn insert(text: &str, object: &JsonObject, depth: usize, member: &str) -> String {
    let pad = "  ".repeat(depth);
    match object.members.last() {
        Some((_, last)) => splice(text, last.end..last.end, &format!(",\n{pad}{member}")),
        None => {
            let close_pad = "  ".repeat(depth - 1);
            splice(text, object.close..object.close, &format!("\n{pad}{member}\n{close_pad}"))
        }
    }
}

fn splice(text: &str, range: Range<usize>, with: &str) -> String {
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(&text[..range.start]);
    out.push_str(with);
    out.push_str(&text[range.end..]);
    out
}

/// The file's text, `None` when it does not exist; any other failure is an error, never "empty".
fn read_text(path: &Path) -> Result<Option<String>, ClaudeConnectError> {
    let bytes =
        vike_utility::atomic_file::read_if_present(path).map_err(ClaudeConnectError::Unreadable)?;
    bytes.map(String::from_utf8).transpose().map_err(|_| ClaudeConnectError::NotText)
}

/// Set `entry` in the config file at `path`; `Ok(false)` when it was already there.
///
/// An existing file is first copied to `<name>.vike-backup-<stamp>` beside it (refused, never
/// overwritten, when that name exists); a missing file is created, with its directory.
pub fn connect(path: &Path, entry: &Value, stamp: &str) -> Result<bool, ClaudeConnectError> {
    let text = read_text(path)?;
    let Some(merged) = merge_entry(text.as_deref(), entry).map_err(ClaudeConnectError::Refused)?
    else {
        return Ok(false);
    };
    let at = |e: std::io::Error, p: &Path| ClaudeConnectError::Write(p.to_path_buf(), e);
    match &text {
        Some(old) => {
            let backup = backup_path(path, stamp);
            write_new(&backup, old.as_bytes()).map_err(|e| at(e, &backup))?;
        }
        None => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| at(e, dir))?;
            }
        }
    }
    vike_utility::atomic_file::write_atomic(path, merged.as_bytes()).map_err(|e| at(e, path))?;
    Ok(true)
}

/// `<file name>.vike-backup-<stamp>`, beside `path`.
pub fn backup_path(path: &Path, stamp: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".vike-backup-{stamp}"));
    path.with_file_name(name)
}

/// Create `path` holding `bytes`, synced; an existing file is an error, never overwritten.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Claude Desktop and this app, as the Settings window shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeStatus {
    /// Claude Desktop is not made for this platform.
    Unsupported,
    /// No Claude Desktop on this PC.
    NotFound,
    /// This app cannot name itself as Claude's command (no path, or not UTF-8).
    CannotName(String),
    /// The config file starts this app, as this window would write it.
    Connected,
    /// It has no `vike` entry, or one for this executable with other arguments or settings.
    NotConnected,
    /// Its `vike` entry starts another program: another copy, or a hand-written line.
    Elsewhere(String),
    /// The file cannot be read, or would be refused: the file and why.
    Refused(PathBuf, String),
}

/// The status of `found` for `entry` (this app's, or why it has none).
pub fn status_of(
    found: &ClaudeDesktopFound,
    entry: &Result<Value, ClaudeConnectError>,
) -> ClaudeStatus {
    let path = match found {
        ClaudeDesktopFound::Unsupported => return ClaudeStatus::Unsupported,
        ClaudeDesktopFound::NotFound => return ClaudeStatus::NotFound,
        ClaudeDesktopFound::Config(path) => path,
    };
    let entry = match entry {
        Ok(entry) => entry,
        Err(why) => return ClaudeStatus::CannotName(why.to_string()),
    };
    let refused =
        |why: &dyn std::fmt::Display| ClaudeStatus::Refused(path.clone(), why.to_string());
    let text = match read_text(path) {
        Ok(text) => text,
        Err(why) => return refused(&why),
    };
    match merge_entry(text.as_deref(), entry) {
        Ok(None) => return ClaudeStatus::Connected,
        Err(why) => return refused(&why),
        Ok(Some(_)) => {}
    }
    let doc: Option<Value> =
        text.as_deref().and_then(|t| serde_json::from_str(split_bom(t).1).ok());
    let command = |v: &Value| v.get("command").and_then(Value::as_str).map(str::to_string);
    match doc.as_ref().and_then(|d| d.get(SERVERS)?.get(SERVER_KEY)).map(command) {
        Some(theirs) if theirs != command(entry) => {
            ClaudeStatus::Elsewhere(theirs.unwrap_or_else(|| "no command".to_string()))
        }
        _ => ClaudeStatus::NotConnected,
    }
}

/// The process ids of a running Claude Desktop; `None` when this platform or install cannot tell.
///
/// Windows lists the Store package's processes (`tasklist /APPS`, with no console window); a
/// classic install is no package and shares its image name, `claude.exe`, with Claude Code, so it
/// cannot be told apart. macOS asks `pgrep -x Claude`.
pub fn claude_desktop_pids(
    platform: ClaudePlatform,
    vars: &HashMap<String, String>,
) -> Option<Vec<u32>> {
    let run = |program: &str, args: &[&str]| {
        let mut command = std::process::Command::new(program);
        command.args(args);
        vike_client::attach::hide_window(&mut command);
        command.output().ok()
    };
    match platform {
        ClaudePlatform::Windows => {
            let out =
                run("tasklist", &["/APPS", "/FO", "CSV", "/NH"]).filter(|o| o.status.success())?;
            let pids = packaged_claude_pids(&String::from_utf8_lossy(&out.stdout));
            let classic = vars
                .get("LOCALAPPDATA")
                .is_some_and(|l| !l.is_empty() && Path::new(l).join("AnthropicClaude").is_dir());
            (!pids.is_empty() || !classic).then_some(pids)
        }
        ClaudePlatform::MacOs => {
            let out = run("pgrep", &["-x", "Claude"])?;
            match out.status.code() {
                Some(0) => Some(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .filter_map(|l| l.trim().parse().ok())
                        .collect(),
                ),
                Some(1) => Some(Vec::new()),
                _ => None,
            }
        }
        ClaudePlatform::Other => None,
    }
}

/// The Claude Desktop process ids in `tasklist /APPS /FO CSV /NH`'s output: rows like
/// `"claude.exe (Claude)","50976","268304 K","Claude_6.28899.6.0_x64__pzs8sxrjxfjjc"`.
pub fn packaged_claude_pids(csv: &str) -> Vec<u32> {
    csv.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.trim().trim_matches('"').split("\",\"").collect();
            let image = fields.first()?.to_ascii_lowercase();
            let package = fields.get(3)?;
            (image.starts_with("claude.exe") && package.starts_with("Claude_"))
                .then(|| fields.get(1)?.parse().ok())
                .flatten()
        })
        .collect()
}

/// How often an open Settings window looks again (Claude Desktop installed meanwhile, the file
/// edited by hand).
const RECHECK: Duration = Duration::from_secs(3);

/// The AI section's state in one Settings window: the last look, and what the last click did.
#[derive(Debug, Clone)]
pub struct ClaudeConnectView {
    platform: ClaudePlatform,
    checked: Option<(Instant, ClaudeStatus)>,
    /// What the last click did, for the line under the button.
    pub done: Option<Result<String, String>>,
}

impl Default for ClaudeConnectView {
    fn default() -> Self {
        Self::on(ClaudePlatform::HERE)
    }
}

impl ClaudeConnectView {
    /// A view that looks for Claude Desktop by `platform`'s layout.
    pub fn on(platform: ClaudePlatform) -> Self {
        Self { platform, checked: None, done: None }
    }

    /// A view whose last look, just now, found `status`.
    pub fn showing(status: ClaudeStatus) -> Self {
        Self { checked: Some((Instant::now(), status)), ..Self::default() }
    }

    /// The status, looked up again when the last look is older than [`RECHECK`]. `settings_dir` is
    /// the folder the desktop booted from, which the entry carries.
    pub fn status(
        &mut self,
        vars: &HashMap<String, String>,
        settings_dir: Option<&Path>,
    ) -> ClaudeStatus {
        match &self.checked {
            Some((at, status)) if at.elapsed() < RECHECK => status.clone(),
            _ => {
                let found = find_claude_desktop(self.platform, vars);
                let status = status_of(&found, &this_entry(settings_dir));
                self.checked = Some((Instant::now(), status.clone()));
                status
            }
        }
    }

    /// The click: write the config file, say what the user does next, and look again at once.
    pub fn connect_now(&mut self, vars: &HashMap<String, String>, settings_dir: Option<&Path>) {
        let ClaudeDesktopFound::Config(path) = find_claude_desktop(self.platform, vars) else {
            self.done = Some(Err(NOT_FOUND.to_string()));
            return;
        };
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let written = this_entry(settings_dir).and_then(|entry| connect(&path, &entry, &stamp));
        // shortcut: asked once, at the click (tasklist takes ~0.6 s), so the line stays after the user
        // has restarted Claude; ask again on the status recheck, off the frame thread, if that confuses.
        self.done = Some(match written {
            Ok(_) => Ok(next_step(claude_desktop_pids(self.platform, vars).as_deref()).to_string()),
            // The status line's own words for a file left as it is.
            Err(why) => Err(ClaudeStatus::Refused(path, why.to_string()).to_string()),
        });
        self.checked = None;
    }
}

/// What the user does after the write, by whether Claude Desktop runs (`None`: cannot tell).
pub fn next_step(pids: Option<&[u32]>) -> &'static str {
    match pids {
        Some([]) => NEXT_START,
        Some(_) => RESTART_CLAUDE,
        None => MAYBE_RESTART,
    }
}

/// After the write, Claude Desktop not running.
pub const NEXT_START: &str = "Done. Claude Desktop will start vike the next time it opens.";

/// After the write, Claude Desktop running: it reads the file only when it starts.
pub const RESTART_CLAUDE: &str = "Done. Claude Desktop is open: quit it from its icon by the clock \
     (right-click > Quit), then start it again to see vike.";

/// After the write, when this PC cannot tell whether Claude Desktop runs.
pub const MAYBE_RESTART: &str = "Done. If Claude Desktop is open, quit it fully (on Windows: \
     right-click its icon by the clock > Quit), then start it again to see vike.";

/// The line for [`ClaudeStatus::NotFound`].
pub const NOT_FOUND: &str = "Claude Desktop was not found on this computer. Install it from \
     claude.ai/download and open it once, then come back here.";

impl std::fmt::Display for ClaudeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("Claude Desktop is made for Windows and macOS only."),
            Self::NotFound => f.write_str(NOT_FOUND),
            Self::CannotName(why) => write!(f, "Claude cannot be connected: {why}."),
            Self::Connected => f.write_str("Claude is connected to this app."),
            Self::NotConnected => f.write_str("Claude is not connected to this app."),
            Self::Elsewhere(command) => write!(
                f,
                "Claude is connected to another copy of vike ({command}). Connect Claude points \
                 it at this one."
            ),
            Self::Refused(path, why) => write!(
                f,
                "Claude's settings file {} was left as it is: {why}. Fix it in Claude Desktop \
                 (Settings > Developer > Edit Config), then come back here.",
                path.display()
            ),
        }
    }
}

#[cfg(test)]
#[path = "claude_connect_tests.rs"]
mod claude_connect_tests;
