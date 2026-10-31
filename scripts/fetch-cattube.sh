#!/usr/bin/env bash
# Put the CatTube release named in cattube.lock into a system tree:
#
#   scripts/fetch-cattube.sh ROOT
#
# ROOT/usr/local/bin/cattube and ROOT/usr/local/share/cattube/ (the player's
# controls). The download is kept in ~/.cache/catostv-build and checked
# against the sha256 in the lock file every time; a mismatch stops the
# build. CATTUBE_TARBALL=FILE uses a local release tarball instead (still
# checked), for testing a CatTube build before it is released.
set -euo pipefail
ROOT="${1:?usage: fetch-cattube.sh ROOT}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
LOCK="$HERE/cattube.lock"
value() { sed -n "s/^$1 *= *//p" "$LOCK" | head -n1; }
VERSION="$(value version)"
URL="$(value url)"
SHA="$(value sha256)"
[[ "$VERSION" =~ ^[0-9]+(\.[0-9]+)*$ && "$URL" == https://github.com/coderastic/CatTube/* && "$SHA" =~ ^[0-9a-f]{64}$ ]] \
  || { echo "cattube.lock is not right" >&2; exit 1; }

if [[ -n "${CATTUBE_TARBALL:-}" ]]; then
  FILE="$CATTUBE_TARBALL"
else
  CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/catostv-build"
  mkdir -p "$CACHE"
  FILE="$CACHE/$(basename "$URL")"
  if [[ ! -s "$FILE" ]]; then
    echo "==> downloading CatTube $VERSION"
    curl -fsSL --retry 3 -o "$FILE.part" "$URL"
    mv "$FILE.part" "$FILE"
  fi
fi
GOT="$(sha256sum "$FILE" | cut -d' ' -f1)"
if [[ "$GOT" != "$SHA" ]]; then
  echo "CatTube $VERSION: sha256 is $GOT, cattube.lock says $SHA" >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
tar -xzf "$FILE" -C "$TMP"
SRC="$TMP/cattube-$VERSION-x86_64"
[[ -x "$SRC/bin/cattube" ]] || { echo "no bin/cattube in the CatTube tarball" >&2; exit 1; }
install -Dm755 "$SRC/bin/cattube" "$ROOT/usr/local/bin/cattube"
install -d "$ROOT/usr/local/share/cattube"
install -m644 "$SRC"/share/cattube/* "$ROOT/usr/local/share/cattube/"
echo "==> CatTube $VERSION in $ROOT"
