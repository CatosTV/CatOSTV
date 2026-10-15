#!/usr/bin/env bash
# Make a signed CatosTV update (docs/UPDATES.md).
#
#   ./scripts/make-update.sh OUTDIR --archive-date YYYY/MM/DD \
#       --notes NOTES.txt --key KEYID [--base-url https://host/catostv/stable]
#
# The default base URL is https://updates.catostv.com/stable; publish with
# scripts/publish-update.sh OUTDIR. GPG_PASSPHRASE (if set) unlocks the key.
#
# Writes to OUTDIR: catostv-<VERSION>.tar.zst (bin/ + root/, the files the
# installer would copy), manifest.json (version, Arch Linux Archive day,
# release notes, one per line of NOTES.txt, package URL and sha256) and
# manifest.json.sig (gpg --detach-sign with KEYID). Upload all three to
# --base-url; TVs read <base-url>/manifest.json (/etc/catostv/updates.conf).
# The archive day must be one the build was tested with: every TV moves to
# exactly those Arch packages.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
usage() { sed -n '2,13p' "$0"; exit 2; }
[[ $# -ge 1 ]] || usage
OUT="$1"; shift
DATE="" NOTES="" KEY="" BASE="https://updates.catostv.com/stable"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive-date) DATE="$2"; shift ;;
    --notes) NOTES="$2"; shift ;;
    --key) KEY="$2"; shift ;;
    --base-url) BASE="${2%/}"; shift ;;
    *) usage ;;
  esac
  shift
done
[[ "$DATE" =~ ^[0-9]{4}/[0-9]{2}/[0-9]{2}$ ]] || { echo "--archive-date YYYY/MM/DD" >&2; exit 2; }
[[ -f "$NOTES" && -n "$KEY" && "$BASE" =~ ^https?:// ]] || usage
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || { echo "uncommitted changes, commit first" >&2; exit 1; }
curl -fsI "https://archive.archlinux.org/repos/$DATE/core/os/x86_64/" >/dev/null \
  || { echo "no Arch Linux Archive snapshot for $DATE" >&2; exit 1; }

VERSION="$(tr -d '[:space:]' < VERSION)"
BINS=(catostv-shell catostv-ctl catostv-diag catostv-inputd catostv-cecd catostv-castd catostv-remoted catostv-wlfilter)
CATOSTV_COMMIT="$(git rev-parse --short HEAD)" cargo build --release --workspace --bins

mkdir -p "$OUT"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/root"
for b in "${BINS[@]}"; do install -m755 "target/release/$b" "$STAGE/bin/$b"; done
git ls-files -z target-root | while IFS= read -r -d '' f; do
  install -D -m "$( [[ -x $f ]] && echo 755 || echo 644 )" "$f" "$STAGE/root/${f#target-root/}"
done
# What this version needs installed (updater and installer add what's missing).
target/release/catostv-diag packages > "$STAGE/root/usr/share/catostv/packages.txt"
PKG="catostv-$VERSION.tar.zst"
tar --zstd -cf "$OUT/$PKG" -C "$STAGE" --owner=0 --group=0 bin root
SHA="$(sha256sum "$OUT/$PKG" | cut -d' ' -f1)"

python3 -I - "$OUT/manifest.json" "$VERSION" "$DATE" "$NOTES" "$BASE/$PKG" "$SHA" <<'PY'
import json, sys
out, version, date, notes, url, sha = sys.argv[1:]
lines = [l.strip() for l in open(notes) if l.strip()]
json.dump({"version": version, "archive_date": date, "notes": lines,
           "package": {"url": url, "sha256": sha}}, open(out, "w"), indent=2)
PY
gpg --batch --yes ${GPG_PASSPHRASE:+--pinentry-mode loopback --passphrase-fd 3} --local-user "$KEY" --detach-sign -o "$OUT/manifest.json.sig" "$OUT/manifest.json" 3<<<"${GPG_PASSPHRASE:-}"
echo "Made $OUT/$PKG, manifest.json, manifest.json.sig, upload them to $BASE/"
