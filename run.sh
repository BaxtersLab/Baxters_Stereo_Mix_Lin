#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Baxters Stereo Mix — Linux launcher.
#
# DISPLAY BACKEND: GDK_BACKEND is deliberately NOT set (A1 section 3.6).
#
# The main window is eframe/winit, which renders natively on Wayland through
# EGL — the same stack Iris proved on this box, where forcing XWayland would
# only add a translation layer to a path that already works. The app does link
# GTK 3, but only for the native file chooser, not for its own decorations, so
# the CSD hit-offset that forces GDK_BACKEND=x11 on the GTK apps in the suite
# does not apply to this window.
#
# KNOWN RISK, stated rather than hidden: the GTK file dialog IS client-side
# decorated, so on Wayland its ✕ may carry the usual ~26px shadow offset. Its
# Cancel and Open buttons are unaffected, so the dialog remains usable. Setting
# GDK_BACKEND=x11 to fix the ✕ would drag the main window onto XWayland too,
# which is the worse trade. Verify on real hardware before changing this.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"

unset ELECTRON_RUN_AS_NODE

_strip_snap_list() {
    local IFS=':' out=() part
    for part in $1; do
        [[ "$part" == */snap/* ]] || out+=("$part")
    done
    local joined; printf -v joined '%s:' "${out[@]}"
    printf '%s' "${joined%:}"
}
[[ "${PATH:-}" == */snap/* ]] && PATH="$(_strip_snap_list "$PATH")" && export PATH
[[ "${XDG_DATA_DIRS:-}" == */snap/* ]] \
    && XDG_DATA_DIRS="$(_strip_snap_list "$XDG_DATA_DIRS")" && export XDG_DATA_DIRS

while IFS='=' read -r _name _value; do
    case "$_name" in
        PATH|XDG_DATA_DIRS) continue ;;
        *) [[ "$_value" == */snap/* ]] && unset "$_name" ;;
    esac
done < <(env)

# Installed layout first, so the package never launches a developer's stale
# build; dev tree paths after. Warn when a newer candidate exists rather than
# silently preferring one — a thirteen-day-stale binary cost Iris a debugging
# round, and it looked exactly like "the fix did not work".
APP=""
CANDIDATES=("$PWD/bsm-ui")
[[ -n "${CARGO_TARGET_DIR:-}" ]] && CANDIDATES+=("$CARGO_TARGET_DIR/release/bsm-ui")
CANDIDATES+=("$PWD/target/release/bsm-ui")
for candidate in "${CANDIDATES[@]}"; do
    if [[ -x "$candidate" ]]; then APP="$candidate"; break; fi
done
if [[ -z "$APP" ]]; then
    echo "[stereo-mix] no bsm-ui binary found. Looked in:"
    printf '[stereo-mix]   %s\n' "${CANDIDATES[@]}"
    echo "[stereo-mix] build first:  cargo build --workspace --release"
    exit 1
fi
for candidate in "${CANDIDATES[@]}"; do
    [[ "$candidate" == "$APP" ]] && continue
    [[ -x "$candidate" ]] || continue
    [[ "$candidate" -nt "$APP" ]] && echo "[stereo-mix] WARNING: $candidate is NEWER than $APP"
done

exec "$APP" "$@"
