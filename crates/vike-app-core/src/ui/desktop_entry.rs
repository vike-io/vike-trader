//! `vike-desktop --install-desktop-entry` and `--uninstall-desktop-entry` (Linux): the launcher
//! entry and icons a Linux desktop needs to show this app's icon (design system spec §6).
//!
//! A Wayland taskbar finds an application's icon ONLY through a desktop entry whose file name is
//! the window's app id ([`vike_ui_theme::brand::APP_ID`]); the window's own icon never reaches it.
//! The release ships one bare binary, so the binary installs its own entry: the icons are the
//! committed, drift-gated files under `assets/brand/`, compiled in, and the entry is written by
//! `vike_ui_theme::brand::desktop_entry` — the function the committed reference copy comes from —
//! with this binary's path and the settings directory the install resolved ([`launch_exec`]).
//! They go under the user's XDG data home (`$XDG_DATA_HOME`, else `~/.local/share`), so no root is
//! needed, and uninstall removes exactly the files install writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_ui_theme::brand::{self, APP_ID};

/// The flag that writes the entry and its icons.
pub const INSTALL_FLAG: &str = "--install-desktop-entry";
/// The flag that removes them.
pub const UNINSTALL_FLAG: &str = "--uninstall-desktop-entry";

/// The hicolor PNGs, one per [`brand::LINUX_ICON_PX`] size, in that order.
const PNGS: [&[u8]; 9] = [
    include_bytes!("../../../../assets/brand/png/app-icon-16.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-22.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-24.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-32.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-48.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-64.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-128.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-256.png"),
    include_bytes!("../../../../assets/brand/png/app-icon-512.png"),
];

/// The scalable icon.
const SVG: &str = include_str!("../../../../assets/brand/app-icon.svg");

/// Every file of the entry, relative to the data home, with its bytes.
fn files(exec: &str) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = vec![(
        Path::new("applications").join(brand::desktop_file_name()),
        brand::desktop_entry(exec).into_bytes(),
    )];
    for (side, png) in brand::LINUX_ICON_PX.iter().zip(PNGS) {
        let rel = format!("icons/hicolor/{side}x{side}/apps/{APP_ID}.png");
        out.push((PathBuf::from(rel), png.to_vec()));
    }
    let rel = format!("icons/hicolor/scalable/apps/{APP_ID}.svg");
    out.push((PathBuf::from(rel), SVG.as_bytes().to_vec()));
    out
}

/// The `Exec=` value for `exe`: as is, or in double quotes when it holds a character the Desktop
/// Entry spec reserves. A character the spec would make us ESCAPE (a double quote, a backtick,
/// `$`, `\`, `%`, a control character) is refused rather than escaped: move the binary instead.
pub fn exec_value(exe: &Path) -> Result<String, String> {
    exec_arg(exe.to_str().ok_or_else(|| format!("{} is not UTF-8", exe.display()))?)
}

/// One `Exec=` argument, by [`exec_value`]'s rule: quoted when it holds a reserved character,
/// refused when it holds one the spec would make us escape.
fn exec_arg(s: &str) -> Result<String, String> {
    if let Some(c) = s.chars().find(|c| matches!(c, '"' | '`' | '$' | '\\' | '%') || c.is_control())
    {
        return Err(format!(
            "{s} holds {c:?}, which a desktop entry would have to escape; move it to a plainer path \
             and run this again"
        ));
    }
    let reserved = |c: char| " '><~|&;*?#()".contains(c);
    Ok(if s.chars().any(reserved) { format!("\"{s}\"") } else { s.to_string() })
}

/// The whole `Exec=` value: `exe`, and — when the install resolved one — the settings directory
/// passed outright as `$VIKE_SETTINGS_DIR` through `env`.
///
/// A launcher starts the app in the user's HOME, where the project walk that finds the settings,
/// the credential store, the node keys and the backend registry finds nothing — so without this the
/// launcher would open an app that is not the one `./vike-desktop` opens from the project folder.
/// Naming the directory outright is how the shipped service units make their daemons independent of
/// the working directory too (`vike_model::state_path::SETTINGS_DIR_ENV`).
pub fn launch_exec(exe: &Path, settings_dir: Option<&Path>) -> Result<String, String> {
    let exe = exec_value(exe)?;
    let Some(dir) = settings_dir else { return Ok(exe) };
    let dir = dir.to_str().ok_or_else(|| format!("{} is not UTF-8", dir.display()))?;
    let var = exec_arg(&format!("{}={dir}", vike_model::state_path::SETTINGS_DIR_ENV))?;
    Ok(format!("env {var} {exe}"))
}

/// The settings directory the app's own boot would resolve if it were started here:
/// `$VIKE_SETTINGS_DIR`, else the project walk from `cwd` — through the same
/// `vike_secrets::project_settings_dir_for` `vike_boot` calls, so the two cannot answer differently.
fn settings_dir(vars: &HashMap<String, String>, cwd: Option<&Path>) -> Option<PathBuf> {
    // A LITERAL, as `vike_boot`'s `settings_dir_override` spells it: the settings registry's
    // map-lookup sweep resolves only this crate's own constants, and a declared read is the point.
    vike_secrets::project_settings_dir_for(vars.get("VIKE_SETTINGS_DIR").map(String::as_str), cwd)
}

/// Write the entry and every icon under `data_home`, launching `exe` with `settings_dir` (see
/// [`launch_exec`]). Returns what was written.
pub fn install(
    data_home: &Path,
    exe: &Path,
    settings_dir: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    let exec = launch_exec(exe, settings_dir)?;
    let mut written = Vec::new();
    for (rel, bytes) in files(&exec) {
        let path = data_home.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// Remove exactly the files [`install`] writes, where present. Returns what was removed.
pub fn uninstall(data_home: &Path) -> Result<Vec<PathBuf>, String> {
    let mut removed = Vec::new();
    for (rel, _) in files("") {
        let path = data_home.join(rel);
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(removed)
}

/// The XDG data home: `$XDG_DATA_HOME`, else `$HOME/.local/share` — the parent of the per-user
/// store `vike_model::store_path` resolves, which keeps that module the workspace's one reader of
/// the platform variables.
pub fn data_home(vars: &HashMap<String, String>) -> Option<PathBuf> {
    vike_model::store_path::user_data_dir_from_vars(vars)
        .and_then(|dir| dir.parent().map(Path::to_path_buf))
}

/// `main`'s one call. When the first argument is one of the two flags, act on it, print each path
/// touched, and return the exit code; otherwise `None`, and the app starts as usual.
pub fn run(args: impl IntoIterator<Item = String>, vars: &HashMap<String, String>) -> Option<i32> {
    let verb = args.into_iter().nth(1)?;
    if verb != INSTALL_FLAG && verb != UNINSTALL_FLAG {
        return None;
    }
    if !cfg!(target_os = "linux") {
        eprintln!(
            "{verb}: a desktop entry is how a Linux desktop finds an app's icon; on this system \
             the icon is part of the executable"
        );
        return Some(2);
    }
    let Some(home) = data_home(vars) else {
        eprintln!("{verb}: neither XDG_DATA_HOME nor HOME is set, so there is no data home");
        return Some(2);
    };
    let done = if verb == INSTALL_FLAG {
        let cwd = std::env::current_dir().ok();
        let settings = settings_dir(vars, cwd.as_deref());
        if settings.is_none() {
            eprintln!(
                "{verb}: no project was found above this directory and VIKE_SETTINGS_DIR is unset, \
                 so the launcher will start Vike Trader with no settings; run this again from the \
                 project folder"
            );
        }
        std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .map_err(|e| format!("where is this binary? {e}"))
            .and_then(|exe| install(&home, &exe, settings.as_deref()))
    } else {
        uninstall(&home)
    };
    match done {
        Ok(paths) => {
            for path in paths {
                println!("{}", path.display());
            }
            Some(0)
        }
        Err(e) => {
            eprintln!("{verb}: {e}");
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_is_the_exec_value_as_is_and_a_spaced_one_is_quoted() {
        assert_eq!(
            exec_value(Path::new("/home/u/bin/vike-desktop")),
            Ok("/home/u/bin/vike-desktop".into())
        );
        assert_eq!(
            exec_value(Path::new("/home/u/My Apps/vike-desktop")),
            Ok("\"/home/u/My Apps/vike-desktop\"".into())
        );
    }

    #[test]
    fn a_path_the_spec_would_make_us_escape_is_refused() {
        for bad in
            ["/home/u/$x/vike-desktop", "/home/u/100%/vike-desktop", "/home/u/a\"b/vike-desktop"]
        {
            assert!(exec_value(Path::new(bad)).is_err(), "{bad}");
        }
    }

    /// The compiled-in PNGs are the hicolor sizes they are filed under (read from each PNG's
    /// header, so no decoder is needed here).
    #[test]
    fn every_compiled_in_png_is_its_hicolor_size() {
        for (side, png) in brand::LINUX_ICON_PX.iter().zip(PNGS) {
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{side}");
            let w = u32::from_be_bytes(png[16..20].try_into().expect("IHDR width"));
            let h = u32::from_be_bytes(png[20..24].try_into().expect("IHDR height"));
            assert_eq!((w, h), (*side, *side));
        }
    }

    /// A launcher starts the app in the user's HOME, where the project walk finds nothing — so the
    /// entry names the settings directory the install resolved, the way the shipped units do.
    #[test]
    fn the_launcher_names_the_settings_directory_the_install_resolved() {
        let exe = Path::new("/home/u/bin/vike-desktop");
        assert_eq!(launch_exec(exe, None), Ok("/home/u/bin/vike-desktop".into()));
        assert_eq!(
            launch_exec(exe, Some(Path::new("/home/u/vike/settings"))),
            Ok("env VIKE_SETTINGS_DIR=/home/u/vike/settings /home/u/bin/vike-desktop".into())
        );
        assert_eq!(
            launch_exec(exe, Some(Path::new("/home/u/My Vike/settings"))),
            Ok("env \"VIKE_SETTINGS_DIR=/home/u/My Vike/settings\" /home/u/bin/vike-desktop"
                .into())
        );
        assert!(launch_exec(exe, Some(Path::new("/home/u/$x/settings"))).is_err());
    }

    #[test]
    fn install_writes_the_settings_directory_into_the_launcher() {
        let home = tempfile::tempdir().expect("tempdir");
        let exe = Path::new("/home/u/bin/vike-desktop");
        install(home.path(), exe, Some(Path::new("/home/u/vike/settings"))).expect("install");
        let entry = home.path().join("applications").join(brand::desktop_file_name());
        let text = std::fs::read_to_string(&entry).expect("the entry");
        let want = "Exec=env VIKE_SETTINGS_DIR=/home/u/vike/settings /home/u/bin/vike-desktop";
        assert!(text.lines().any(|l| l == want), "{text}");
    }

    #[test]
    fn install_writes_the_entry_and_every_icon_and_uninstall_removes_exactly_them() {
        let home = tempfile::tempdir().expect("tempdir");
        let written =
            install(home.path(), Path::new("/home/u/bin/vike-desktop"), None).expect("install");
        assert_eq!(written.len(), 1 + brand::LINUX_ICON_PX.len() + 1);
        let entry = home.path().join("applications").join(brand::desktop_file_name());
        let text = std::fs::read_to_string(&entry).expect("the entry");
        assert!(text.lines().any(|l| l == "Exec=/home/u/bin/vike-desktop"), "{text}");
        for rel in [
            format!("icons/hicolor/16x16/apps/{APP_ID}.png"),
            format!("icons/hicolor/512x512/apps/{APP_ID}.png"),
            format!("icons/hicolor/scalable/apps/{APP_ID}.svg"),
        ] {
            assert!(home.path().join(&rel).is_file(), "{rel}");
        }
        let foreign = home.path().join("applications").join("someone-else.desktop");
        std::fs::write(&foreign, "[Desktop Entry]\n").expect("write");
        assert_eq!(uninstall(home.path()).expect("uninstall").len(), written.len());
        assert!(foreign.is_file(), "uninstall removed a file install never wrote");
        assert!(written.iter().all(|p| !p.exists()));
        assert_eq!(uninstall(home.path()).expect("second uninstall"), Vec::<PathBuf>::new());
    }

    #[cfg(unix)]
    #[test]
    fn the_data_home_is_xdg_else_local_share() {
        use vike_model::store_path::{HOME_VAR, XDG_DATA_HOME_VAR};
        let vars = |kv: &[(&str, &str)]| -> HashMap<String, String> {
            kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let both = vars(&[(XDG_DATA_HOME_VAR, "/x"), (HOME_VAR, "/h")]);
        assert_eq!(data_home(&both), Some(PathBuf::from("/x")));
        assert_eq!(data_home(&vars(&[(HOME_VAR, "/h")])), Some(PathBuf::from("/h/.local/share")));
        assert_eq!(data_home(&vars(&[])), None);
    }

    #[test]
    fn run_ignores_every_other_first_argument() {
        let vars = HashMap::new();
        assert_eq!(run(["vike-desktop".to_string()], &vars), None);
        let observe = ["vike-desktop".to_string(), "--observe".to_string()];
        assert_eq!(run(observe, &vars), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn run_installs_under_xdg_data_home_and_uninstalls() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().to_string_lossy().into_owned();
        let vars = HashMap::from([(vike_model::store_path::XDG_DATA_HOME_VAR.to_string(), dir)]);
        let entry = home.path().join("applications").join(brand::desktop_file_name());
        assert_eq!(run(["x".to_string(), INSTALL_FLAG.to_string()], &vars), Some(0));
        assert!(entry.is_file());
        assert_eq!(run(["x".to_string(), UNINSTALL_FLAG.to_string()], &vars), Some(0));
        assert!(!entry.exists());
    }

    /// `run` resolves the settings directory as the app's own boot does — `$VIKE_SETTINGS_DIR`
    /// first — and the launcher carries it.
    #[cfg(target_os = "linux")]
    #[test]
    fn run_puts_the_resolved_settings_directory_into_the_launcher() {
        let home = tempfile::tempdir().expect("tempdir");
        let settings = home.path().join("project").join("settings");
        let vars = HashMap::from([
            (
                vike_model::store_path::XDG_DATA_HOME_VAR.to_string(),
                home.path().to_string_lossy().into_owned(),
            ),
            (
                vike_model::state_path::SETTINGS_DIR_ENV.to_string(),
                settings.to_string_lossy().into_owned(),
            ),
        ]);
        assert_eq!(run(["x".to_string(), INSTALL_FLAG.to_string()], &vars), Some(0));
        let entry = home.path().join("applications").join(brand::desktop_file_name());
        let text = std::fs::read_to_string(&entry).expect("the entry");
        let want = format!("Exec=env VIKE_SETTINGS_DIR={} ", settings.display());
        assert!(text.lines().any(|l| l.starts_with(&want)), "{text}");
    }
}
