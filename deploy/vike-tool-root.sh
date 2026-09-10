#!/usr/bin/env bash
# vike-tool-root.sh — resolve `<project>/bin/<tool>`, the home of a third-party program this project
# RUNS but did not compile. The shell twin of `vike_model::state_path::project_bin_dir_from`.
#
# Sourced by every deploy script that owns a payload directory; also RUNNABLE, which is what
# `crates/vike-ops/tests/deploy_tool_root_gate.rs` drives against the Rust resolver:
#
#     . "$(dirname "$0")/../vike-tool-root.sh"          # from deploy/<tool>/ or <project>/bin/<tool>/
#     ROOT="$(vike_tool_root ibkr-cpapi "${IBKR_CP_ROOT:-}")" || exit 2
#
#     bash deploy/vike-tool-root.sh ibkr-cpapi          # prints the same answer, for the gate
#
# # Why this file exists at all
#
# `PROJECT_BIN_DIR`'s rule is that a runtime artifact resolves against the PROJECT, because an
# installed project has no `crates/` and no `vendor/` — measured, not assumed: the CI box's
# `/var/lib/vike` holds exactly `bin/ data/ settings/ user_data/` and no source tree at
# all. Until this file existed the IBKR stacks resolved against `$HOME` instead, so one box could
# hold a live trading project and a gateway install that belonged to no project, related by nothing
# but an operator's memory.
#
# # THE LADDER — three rungs, and the per-tool variable still wins
#
#   1. the tool's OWN variable (`IBKR_CP_ROOT`, `IBKR_GW_ROOT`), passed in as `$2`. It names the
#      directory outright and beats everything below, which is the same precedence
#      `PROJECT_BIN_DIR`'s doc gives for `JAVA_HOME` and `JFOREX_BRIDGE_JAR` — and the reason there
#      is deliberately no `VIKE_BIN_DIR` for this rung to argue with.
#   2. `$VIKE_SETTINGS_DIR`'s parent. Mirrors `project_bin_dir_from`, and it is the rung that keeps
#      ONE variable relocating a WHOLE project: a the CI box lane runs the smokes with
#      `VIKE_SETTINGS_DIR=/var/lib/vike/settings`, and a script run from that lane must
#      reach the DEPLOYED gateway rather than the throwaway checkout it is standing in.
#   3. this file's own project — `<dir holding this file>/..`. That is the project root in BOTH
#      shapes, which is why no marker walk is duplicated here: a checkout puts this file at
#      `deploy/vike-tool-root.sh` and an install at `<project>/bin/vike-tool-root.sh`, each exactly
#      one level under the root, and every sourcing script sits exactly one level under THIS one
#      (`deploy/<tool>/x.sh` ↔ `<project>/bin/<tool>/x.sh`). A script knows where it is; a walk from
#      the working directory does not.
#
# ⚠ There is no `$HOME` rung and there must not be one. `$HOME/ibkr-cpapi` was the old default, and
# it is the one answer that is the same on a box holding two projects and different on two boxes
# holding one — i.e. it tracks the OPERATOR rather than the deployment.
#
# ⚠ This file sits LOOSE in `bin/` rather than in a tool directory of its own, which is the one
# exception to `PROJECT_BIN_DIR`'s "every tool owns a subdirectory". It is not a tool: it is how the
# tools find themselves, and it must be resolvable by a script that does not yet know the answer.
# Install it with `install -m755 deploy/vike-tool-root.sh <project>/bin/` before any tool directory.
set -uo pipefail

# The project this file belongs to. Computed at SOURCE time, where `${BASH_SOURCE[0]}` is
# unambiguous — inside a function it names the file the function was DEFINED in, which is still this
# one, but computing it here means a caller can read it too.
VIKE_TOOL_ROOT_SH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
VIKE_PROJECT_DIR="$(cd "$(dirname "$VIKE_TOOL_ROOT_SH")/.." && pwd)"

# vike_project_dir — print `<project>`: rungs 2 and 3 of the ladder, with no per-tool rung.
#
# The `<project>` half is split out because one caller needs the ROOT rather than a tool directory:
# `deploy/jre/provision-jre.sh` places the RUNTIME image under `<project>/bin/jre` and the
# BUILD-TIME JDK under `<workspace>/vendor/tools`, and those two roots being visibly disjoint is
# what `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` holds.
vike_project_dir() {
    local project sd="${VIKE_SETTINGS_DIR:-}"
    # TRIM, then test for empty — `project_settings_dir_from` is `override.map(str::trim).filter(|s|
    # !s.is_empty())`, so a blank or whitespace-only value falls through to the next rung on both
    # sides. Mirroring the trim matters: without it a stray-space value would relocate the tools and
    # not the settings.
    sd="${sd#"${sd%%[![:space:]]*}"}"
    sd="${sd%"${sd##*[![:space:]]}"}"
    # $VIKE_SETTINGS_DIR's parent — the strip `project_root_from` performs, including its refusal of
    # an EMPTY parent: a bare relative name like `settings` has no parent to be a project.
    if [ -n "$sd" ]; then
        case "$sd" in
            */*)
                project="$(dirname "$sd")"
                ;;
            *)
                echo "vike_project_dir: VIKE_SETTINGS_DIR='$sd' has no parent directory, so it names no project — give it a path, or unset it" >&2
                return 2
                ;;
        esac
        printf '%s\n' "$project"
        return 0
    fi
    # ...otherwise this file's own project.
    printf '%s\n' "$VIKE_PROJECT_DIR"
}

# vike_tool_root <tool> [per-tool override] — print `<project>/bin/<tool>`, or the override verbatim.
#
# Refuses (exit 2, message on stderr) rather than inventing a path: a tool root guessed wrong is a
# payload downloaded into a tracked source directory, or a gateway started against the wrong
# project's credentials.
vike_tool_root() {
    local tool="${1:-}" override="${2:-}" project
    if [ -z "$tool" ]; then
        echo "vike_tool_root: no tool name given" >&2
        return 2
    fi
    # 1. the tool's own variable, verbatim. A BLANK value is ignored rather than resolving a path to
    #    "" — the same rule `resolve_dukascopy_tools` applies to JAVA_HOME.
    if [ -n "$override" ]; then
        printf '%s\n' "$override"
        return 0
    fi
    # 2 and 3, spelled ONCE, above. `bin` is `vike_model::state_path::PROJECT_BIN_DIR`.
    project="$(vike_project_dir)" || return 2
    printf '%s\n' "$project/bin/$tool"
}

# _vike_physical <path> — print <path> with every symlink on it resolved, WITHOUT requiring the leaf
# to exist. `readlink -f` would do it on Linux and does something else on macOS, and this file is
# sourced by scripts that run on both; `cd`+`pwd -P` is the portable spelling.
#
# ⚠ Resolving the LEAF is the point, not tidiness. A directory inside the project that is a SYMLINK
# to somewhere outside it passes a string-prefix test and is nonetheless outside — which is the
# shape a "fix" for a $HOME payload takes when somebody is in a hurry (`ln -s ~/x <project>/bin/x`).
_vike_physical() {
    local p="${1:-}" d b
    [ -n "$p" ] || return 1
    if [ -d "$p" ]; then
        (cd "$p" 2>/dev/null && pwd -P)
        return $?
    fi
    d="$(dirname "$p")"
    b="$(basename "$p")"
    d="$(cd "$d" 2>/dev/null && pwd -P)" || return 1
    printf '%s/%s\n' "${d%/}" "$b"
}

# vike_require_in_project <path> [what] — print <path>'s PHYSICAL location if it is inside
# `<project>`, and REFUSE (exit 2, message on stderr) if it is not.
#
# # Why a REFUSAL and not a default
#
# `vike_tool_root` answers "where does this project keep its tools", which is a question this
# project gets to decide. This one answers a different question: a THIRD-PARTY INSTALLER has written
# an absolute path into a payload file, and a script is about to trust it. There is no sensible
# default for that — the recorded path is the only thing that names the artifact — so the choice is
# between using it and refusing, and a path leaving the project has to be the refusal.
#
# ⚠ The defect this exists for, measured on the CI box 2026-08-23: IB Gateway's install4j installer drops
# its JRE under `$HOME/.local/share/i4j_jres/…` and records that path in
# `jts/ibgateway/<v>/.install4j/pref_jre.cfg`. `deploy/ibkr-gateway/start-gateway.sh` read the file
# and launched java from it, so an install that had been MOVED into `<project>/bin/ibkr-gateway/`
# and pronounced migrated still could not start without the operator's home directory. It worked,
# which is why nobody saw it: the old path was still there. Inside a container it is not — `$HOME`
# in an image is not the host's — and this whole layout exists so the project folder is the mount.
#
# The caller decides what to do with the refusal; nothing here falls back, because a fallback is how
# a path outside the project gets used anyway with a warning nobody reads.
vike_require_in_project() {
    local path="${1:-}" what="${2:-a path}" project real
    if [ -z "$path" ]; then
        echo "vike_require_in_project: $what is empty — nothing to check" >&2
        return 2
    fi
    project="$(vike_project_dir)" || return 2
    project="$(_vike_physical "$project")" || {
        echo "vike_require_in_project: cannot resolve the project directory" >&2
        return 2
    }
    real="$(_vike_physical "$path")" || {
        echo "vike_require_in_project: $what names '$path', whose parent directory does not exist" >&2
        return 2
    }
    case "$real/" in
        "${project%/}"/*)
            printf '%s\n' "$real"
            return 0
            ;;
    esac
    echo "vike_require_in_project: $what resolves to" >&2
    echo "    $real" >&2
    echo "which is OUTSIDE this project:" >&2
    echo "    ${project%/}" >&2
    echo "A deployment is ONE project folder — it is what gets mounted into the container, and a" >&2
    echo "path leaving it is a path the container does not have. Bring the artifact inside the" >&2
    echo "project and repoint whatever recorded it; for the IB Gateway install that is" >&2
    echo "'<project>/bin/ibkr-gateway/contain-install.sh'." >&2
    return 2
}

# Executed rather than sourced: print the answer and exit. `${BASH_SOURCE[0]}` equals `$0` only in
# that case, which is the whole test.
#
# `--require-in-project <path>` reaches the containment check the same way, which is how
# `crates/vike-ops/tests/deploy_tool_root_gate.rs` drives it over planted trees under real bash.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    if [ "${1:-}" = "--require-in-project" ]; then
        shift
        vike_require_in_project "${1:-}" "${2:-a path}"
        exit $?
    fi
    vike_tool_root "${1:-}" "${2:-}"
    exit $?
fi
