#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# baxters-stereo-mix — build the .deb from a release build of this tree.
#
# THIS PACKAGE SHIPS ITS OWN SOURCE, and that is a licence obligation, not a
# courtesy. LAME is statically linked (verified: the binary imports zero LAME
# symbols and defines them internally), so LGPL-3 section 4(d) requires the
# user be able to relink against a modified LAME. THIRD_PARTY_LICENSES states
# the argument holds only while the source travels with the work. A .deb is a
# binary without source, so the source goes in the package.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
HERE="$PWD"

. "$HOME/workspace/baxters-repo/packaging/bxdeb.sh"

PKG=baxters-stereo-mix
APPDIR=stereo-mix
CONTROL="$HERE/packaging/DEBIAN/control"

VERSION="$(bx_control_version "$CONTROL")"
bx_assert_control "$CONTROL"
CRATE_VERSION="$(bx_cargo_version "$HERE" bsm-ui)"
bx_assert_versions_agree "$VERSION" "$CRATE_VERSION" "the bsm-ui crate"

TARGET_DIR="${CARGO_TARGET_DIR:-$HERE/target}"
BIN="$TARGET_DIR/release/bsm-ui"
bx_assert_binary_fresh "$BIN" "$HERE/crates" "$HERE/Cargo.toml" "$HERE/Cargo.lock" "$HERE/assets"

STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
DEST="$STAGE/opt/baxters/$APPDIR"
mkdir -p "$DEST/licenses" "$DEST/source" "$STAGE/usr/share/applications"

cp -a packaging/DEBIAN "$STAGE/DEBIAN"
cp -a packaging/usr/. "$STAGE/usr/"

# Icons are staged from the generator's output, never from a second committed
# copy of the same files -- two copies drift and the package ships the stale one.
for icon in packaging/icons/stereo-mix_*x*.png; do
    [ -e "$icon" ] || { echo "FATAL: no generated icons in packaging/icons/" >&2; exit 1; }
    dim=$(basename "$icon" .png); dim=${dim#stereo-mix_}
    mkdir -p "$STAGE/usr/share/icons/hicolor/$dim/apps"
    cp -a "$icon" "$STAGE/usr/share/icons/hicolor/$dim/apps/$PKG.png"
done

bx_install_required "$BIN"         "$DEST/bsm-ui" 0755
bx_install_required "$HERE/run.sh" "$DEST/run.sh" 0755
for item in LICENSE README.md THIRD_PARTY_LICENSES; do
    bx_install_required "$HERE/$item" "$DEST/$item"
done
# The licence texts LAME's own LICENSE requires travel with the work.
for lic in licenses/*; do
    bx_install_required "$HERE/$lic" "$DEST/licenses/$(basename "$lic")"
done

# --- the source, for LGPL relinking ----------------------------------------
# Built from git's own file list where possible, so the tarball cannot quietly
# include a build artefact, a key, or anything .gitignore excludes.
SRC_TAR="$DEST/source/baxters-stereo-mix-source.tar.gz"
if git -C "$HERE" rev-parse --git-dir >/dev/null 2>&1; then
    git -C "$HERE" archive --format=tar.gz --prefix="baxters-stereo-mix-$VERSION/" HEAD > "$SRC_TAR"
    echo "  source: from git archive at $(git -C "$HERE" rev-parse --short HEAD)"
else
    tar --exclude=./target --exclude=./.git --exclude=./dist \
        -czf "$SRC_TAR" -C "$HERE" .
    echo "  source: from the working tree (no git repository here)"
fi
[ -s "$SRC_TAR" ] || { echo "FATAL: source tarball is empty" >&2; exit 1; }

# It must actually contain buildable source, not just be a non-empty file.
# NOTE: `tar ... | grep -q` is wrong under `set -o pipefail`. grep -q exits on
# the FIRST match, closing the pipe; tar then dies with SIGPIPE and pipefail
# propagates that failure -- so the check reports "not found" precisely when the
# file IS there. Count instead: grep -c consumes the whole stream.
for needed in Cargo.toml crates/bsm-encode; do
    n=$(tar -tzf "$SRC_TAR" | grep -c "/$needed" || true)
    [ "${n:-0}" -gt 0 ] || {
        echo "FATAL: source tarball does not contain $needed -- it cannot be rebuilt from" >&2
        exit 1; }
done
echo "  source tarball: $(du -h "$SRC_TAR" | cut -f1), $(tar -tzf "$SRC_TAR" | wc -l) entries"

bx_normalise_modes "$STAGE"
chmod 0755 "$DEST/bsm-ui" "$DEST/run.sh"
bx_assert_not_group_writable "$STAGE"
bx_assert_copyright "$STAGE" "$PKG"
bx_assert_desktop "$STAGE" "$STAGE/usr/share/applications/$PKG.desktop"

[ -x "$DEST/bsm-ui" ] || { echo "FATAL: binary not in payload" >&2; exit 1; }
[ -s "$DEST/THIRD_PARTY_LICENSES" ] || { echo "FATAL: THIRD_PARTY_LICENSES not in payload" >&2; exit 1; }
[ -s "$DEST/licenses/LGPL-3.0.txt" ] || { echo "FATAL: LGPL text not in payload" >&2; exit 1; }
[ -s "$SRC_TAR" ] || { echo "FATAL: source not in payload -- LGPL relinking unmet" >&2; exit 1; }

OUT="$HERE/dist/${PKG}_${VERSION}_amd64.deb"
bx_build_deb "$STAGE" "$OUT"
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
