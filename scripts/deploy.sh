#!/usr/bin/env bash
# Push this checkout to an installed CatosTV over SSH, no ISO rebuild, no
# reinstall. Needs Debug SSH switched on on the TV (Settings → Debug; off
# again after every restart): log in as `catostv` (password `catostv` until
# changed; CATOSTV_SUDO_PASSWORD if you changed it, CATOSTV_DEPLOY_USER for
# another login).
#
#   ./scripts/deploy.sh 192.168.1.56              build release binaries, push, restart CatosTV
#   ./scripts/deploy.sh 192.168.1.56 --no-build   push what is in target/release
#   ./scripts/deploy.sh 192.168.1.56 --session    also restart the whole TV session
#                                                (needed after autostart / rc.xml changes)
#   ./scripts/deploy.sh 192.168.1.56 --bins-only  binaries only, leave the overlay alone
#   ./scripts/deploy.sh 192.168.1.56 --no-packages  do not install missing packages
#   ./scripts/deploy.sh 192.168.1.56 --android-image DIR
#                                                also copy a CatosTV-built Android TV
#                                                image (DIR/system.img, vendor.img,
#                                                BUILD.txt from scripts/android-image/
#                                                build.sh) to the TV and reset Android
#                                                so Settings → Android installs it
#   ./scripts/deploy.sh 192.168.1.56 --android-image stock
#                                                drop that image, back to the
#                                                downloaded one (also a fresh setup)
#
# Run it where `cargo build` works (the Arch build container). Tip: run
# `ssh-copy-id catostv@<ip>` once so you are not asked for the password.
#
# What it does on the TV (as root via sudo):
#   * installs every binary of install::INSTALLED_BINARIES to /usr/local/bin;
#   * installs packages that newer versions added (`catostv-diag packages`,
#     only the missing ones, with a full `pacman -Syu` as Arch requires);
#   * copies target-root/ over / like the installer does (modes and owners
#     are not copied; executables are re-chmodded, /home/catostv is chowned),
#     but keeps the TV's own keyboard layout (labwc `environment`);
#   * applies current Android (Waydroid) settings to an existing Android setup;
#   * restarts catostv-shell and the session daemons (their supervisors in
#     labwc autostart start them again within seconds).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() { sed -n '2,34p' "$0"; exit 2; }
[[ $# -ge 1 ]] || usage
HOST="$1"; shift
[[ "$HOST" =~ ^[A-Za-z0-9._:-]+$ ]] || { echo "bad host: $HOST" >&2; exit 2; }
BUILD=1 SESSION=0 OVERLAY=1 PACKAGES=1 ANDROID_IMAGE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) BUILD=0 ;;
    --session) SESSION=1 ;;
    --bins-only) OVERLAY=0 ;;
    --no-packages) PACKAGES=0 ;;
    --android-image) [[ $# -ge 2 ]] || usage; ANDROID_IMAGE="$2"; shift ;;
    *) usage ;;
  esac
  shift
done
USER_AT="${CATOSTV_DEPLOY_USER:-catostv}@$HOST"
# Debug SSH lets `catostv` use sudo with its password, one timestamp for all
# sessions: it is unlocked once, before the first sudo below.
SUDO_PASSWORD="${CATOSTV_SUDO_PASSWORD:-catostv}"
# A CatosTV-built Android TV image.
if [[ -n "$ANDROID_IMAGE" && "$ANDROID_IMAGE" != stock ]]; then
  for f in system.img vendor.img; do
    [[ -s "$ANDROID_IMAGE/$f" ]] || { echo "$ANDROID_IMAGE/$f is missing (build it with scripts/android-image/build.sh)" >&2; exit 1; }
  done
  [[ -s "$ANDROID_IMAGE/BUILD.txt" ]] || echo "note: $ANDROID_IMAGE/BUILD.txt is missing; the image's size and date will identify it" >&2
  (( OVERLAY )) || { echo "--android-image needs the overlay (drop --bins-only)" >&2; exit 2; }
fi

# Keep in sync with install::INSTALLED_BINARIES (a unit test checks).
BINS=(catostv-shell catostv-ctl catostv-diag catostv-inputd catostv-cecd catostv-castd catostv-remoted catostv-wlfilter)

if (( BUILD )); then
  echo "==> cargo build --release"
  # The commit shows in Settings → Debug → Build.
  CATOSTV_COMMIT="$(git rev-parse --short HEAD 2>/dev/null)$(git diff --quiet HEAD 2>/dev/null || echo -changed)" \
    cargo build --release --workspace --bins
fi
for b in "${BINS[@]}"; do
  [[ -x "target/release/$b" ]] || { echo "missing target/release/$b (build first)" >&2; exit 1; }
done

printf '%s\n' "$SUDO_PASSWORD" | ssh "$USER_AT" "sudo -S -p '' -v" \
  || { echo "sudo on the TV failed: is Debug SSH on (Settings → Debug), and the password right?" >&2; exit 1; }

STAGE="/tmp/catostv-deploy"
echo "==> copying to $USER_AT:$STAGE"
ssh "$USER_AT" "rm -rf $STAGE && mkdir -p $STAGE/bin"
rsync -az --info=progress2 "${BINS[@]/#/target/release/}" "$USER_AT:$STAGE/bin/"
if (( OVERLAY )); then
  rsync -az --delete target-root/ "$USER_AT:$STAGE/root/"
  # CatTube, the YouTube app, from its own release (cattube.lock).
  CT="$(mktemp -d)"
  scripts/fetch-cattube.sh "$CT"
  rsync -az "$CT/" "$USER_AT:$STAGE/root/"
  rm -rf "$CT"
fi
ANDROID_ACTION=keep
if [[ -n "$ANDROID_IMAGE" && "$ANDROID_IMAGE" != stock ]]; then
  # Straight into its final folder as root (the images are ~1.5 GB and the
  # TV's /tmp is in RAM); `custom.new` becomes `custom` on the TV below.
  echo "==> copying the Android TV image to $USER_AT:/var/lib/catostv/android/custom.new"
  ssh "$USER_AT" "sudo install -d -m 755 /var/lib/catostv/android/custom.new"
  rsync -a --info=progress2 --rsync-path="sudo rsync" --chown=root:root --chmod=F644 \
    "$ANDROID_IMAGE/system.img" "$ANDROID_IMAGE/vendor.img" \
    $( [[ -s "$ANDROID_IMAGE/BUILD.txt" ]] && echo "$ANDROID_IMAGE/BUILD.txt" ) \
    "$USER_AT:/var/lib/catostv/android/custom.new/"
  ANDROID_ACTION=custom
elif [[ "$ANDROID_IMAGE" == stock ]]; then
  ANDROID_ACTION=stock
fi

echo "==> installing on the TV"
ssh "$USER_AT" "OVERLAY=$OVERLAY SESSION=$SESSION PACKAGES=$PACKAGES ANDROID_ACTION=$ANDROID_ACTION bash -s" <<'REMOTE'
set -euo pipefail
STAGE=/tmp/catostv-deploy
for f in "$STAGE"/bin/*; do
  sudo install -Dm755 "$f" "/usr/local/bin/$(basename "$f")"
done
if [ "$PACKAGES" = 1 ]; then
  # `pacman -T` prints only the packages that are not installed.
  mapfile -t missing < <(pacman -T $(/usr/local/bin/catostv-diag packages) || true)
  if [ "${#missing[@]}" -gt 0 ]; then
    echo "installing new packages: ${missing[*]}"
    sudo pacman -Syu --needed --noconfirm "${missing[@]}"
  fi
fi
if [ "$OVERLAY" = 1 ]; then
  ENV=/home/catostv/.config/labwc/environment
  # The TV's keyboard layout lives in this file; keep it.
  # (/home/catostv is 0700, so every access goes through sudo.)
  if sudo test -f "$ENV"; then sudo cp "$ENV" "$STAGE/environment.keep"; fi
  sudo cp -a --no-preserve=mode,ownership "$STAGE/root/." /
  if sudo test -f "$STAGE/environment.keep"; then sudo cp "$STAGE/environment.keep" "$ENV"; fi
  sudo chmod 755 /usr/local/bin/catostv-session /usr/local/bin/cattube \
    /home/catostv/.config/labwc/autostart /usr/local/lib/catostv/*
  # Only what the overlay ships: NEVER chown -R /home/catostv, Android's
  # data (~/.local/share/waydroid/data) lives there with Android's own uids,
  # and re-owning it broke Android's keystore.
  (cd "$STAGE/root/home/catostv" && find . -mindepth 1 -print0) \
    | sudo xargs -0 -I{} chown -h catostv:catostv "/home/catostv/{}"
  # No ACLs on the TV account's home: a default ACL there made
  # Android create group-writable files and Android TV 13 refused to boot.
  # The debug `ok` user reads catostv's files with sudo instead.
  if command -v getfacl >/dev/null && sudo getfacl -R -s /home/catostv 2>/dev/null | grep -q '^# file:'; then
    echo "removing ACLs from /home/catostv"
    sudo setfacl -R -b /home/catostv
  fi
  sudo systemctl daemon-reload
  command -v dconf >/dev/null && sudo dconf update || true
  sudo udevadm control --reload || true
  # Virtual keyboard for the remotes (70-catostv-uinput.rules).
  sudo modprobe uinput 2>/dev/null || true
  sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput 2>/dev/null || true
  sudo systemctl enable catostv-android-watch.service 2>/dev/null || true
  # Debug SSH is off after every boot (Settings → Debug).
  sudo systemctl enable catostv-debug-boot.service 2>/dev/null || true
  if systemctl is-active -q waydroid-container.service; then
    # restart: a running watcher is the old script.
    sudo systemctl restart catostv-android-watch.service || true
  fi
  # Boot screen (Plymouth) once its package is there.
  if [ -x /usr/local/lib/catostv/boot-splash-setup ]; then
    sudo /usr/local/lib/catostv/boot-splash-setup || echo "boot screen not set up" >&2
  fi
  # A CatosTV-built Android TV image: put in place (or dropped), then
  # Android is set up fresh from it below.
  WIPE_ANDROID=0
  CUSTOM=/var/lib/catostv/android/custom
  case "$ANDROID_ACTION" in
    custom)
      echo "installing the CatosTV-built Android TV image ($(sudo sed -n 's/^build=//p' "$CUSTOM.new/BUILD.txt" 2>/dev/null || true))"
      sudo rm -rf "$CUSTOM"
      sudo mv "$CUSTOM.new" "$CUSTOM"
      WIPE_ANDROID=1
      ;;
    stock)
      if sudo test -d "$CUSTOM"; then
        echo "dropping the CatosTV-built Android TV image"
        sudo rm -rf "$CUSTOM"
        WIPE_ANDROID=1
      fi
      if sudo test -L /etc/waydroid-extra/images && [ "$(sudo readlink /etc/waydroid-extra/images)" = "$CUSTOM" ]; then
        sudo rm -f /etc/waydroid-extra/images
      fi
      ;;
  esac
  # Android TV image: an Android set up from the stock
  # Waydroid image or an older TV image (Android TV 13) is wiped so
  # Settings → Android sets up the current TV image fresh. Apps and sign-ins inside Android go.
  if sudo test -f /var/lib/waydroid/waydroid.cfg && ! sudo grep -qs '^atv16-' /var/lib/catostv/android/image; then
    WIPE_ANDROID=1
  fi
  if [ "$WIPE_ANDROID" = 1 ] && { sudo test -e /var/lib/waydroid/waydroid.cfg || sudo test -e /var/lib/catostv/android/image; }; then
    echo "removing the old Android setup: set it up again from Settings → Android"
    sudo -u catostv XDG_RUNTIME_DIR=/run/user/$(id -u catostv) timeout 20 waydroid session stop 2>/dev/null || true
    sudo systemctl stop waydroid-container.service 2>/dev/null || true
    sudo systemctl disable waydroid-container.service 2>/dev/null || true
    sudo rm -rf /home/catostv/.local/share/waydroid /var/lib/waydroid/overlay /var/lib/waydroid/overlay_rw \
      /var/lib/waydroid/rootfs /var/lib/waydroid/images /var/lib/waydroid/waydroid_base.prop \
      /var/lib/waydroid/waydroid.cfg /var/lib/waydroid/lxc /var/lib/catostv/android/setup-done \
      /var/lib/catostv/android/.microg-done /var/lib/catostv/android/.arm-done /var/lib/catostv/waydroid_script \
      /var/lib/catostv/android/image /var/lib/catostv/android/gsf-id
    sudo rm -f /home/catostv/.local/share/applications/waydroid.*.desktop /home/catostv/.local/share/applications/catostv-android.*.desktop
  fi
  # Existing Android setups: apply the current Waydroid settings (single
  # full-screen window, no LineageOS boot animation, no navigation bar) the
  # way android-setup
  # does for new installs.
  CFG=/var/lib/waydroid/waydroid.cfg
  if sudo test -f "$CFG" && { ! sudo grep -q 'qemu.hw.mainkeys' "$CFG" \
      || sudo grep -q 'multi_windows = true' "$CFG" \
      || ! sudo grep -q 'atv.setup.bt_remote_pairing' "$CFG"; }; then
    echo "applying Android TV settings"
    sudo python3 -I - "$CFG" <<'PY'
import configparser, sys
cfg = configparser.ConfigParser()
cfg.read(sys.argv[1])
if not cfg.has_section("properties"):
    cfg.add_section("properties")
cfg.set("properties", "persist.waydroid.multi_windows", "false")
cfg.set("properties", "debug.sf.nobootanimation", "1")
cfg.set("properties", "qemu.hw.mainkeys", "1")
cfg.set("properties", "atv.setup.bt_remote_pairing", "false")
# The Android TV 16 image decodes zero-copy on its own gralloc,
# unless this TV draws Android in software (NVIDIA / safe graphics).
import os
if not os.path.exists("/etc/catostv/android-software-rendering"):
    cfg.remove_option("properties", "ro.hardware.gralloc")
with open(sys.argv[1], "w") as f:
    cfg.write(f)
PY
    sudo waydroid upgrade --offline || echo "waydroid upgrade failed; settings apply on next setup" >&2
  fi
  # Display properties (case-sensitive names: not via waydroid.cfg)
  # through Waydroid's overlay; Android reads them at boot.
  PROP=/var/lib/waydroid/overlay/vendor/odm/etc/build.prop
  RESTART_ANDROID=0
  if sudo test -d /var/lib/waydroid && ! sudo cmp -s /usr/share/catostv/android-display.prop "$PROP"; then
    echo "installing Android display properties"
    sudo install -D -m 644 /usr/share/catostv/android-display.prop "$PROP"
    RESTART_ANDROID=1
  fi
  # Display-HAL hotplug patch, Escape → Back, 4K60 decoding and the TV
  # feature list: read from the running container or the downloaded image;
  # a restart applies them.
  if [ -e /var/lib/waydroid/images/system.img ]; then
    # The HAL byte patch is for the stock image's build only; the TV
    # image's HAL differs and is left alone.
    FIXES="android-keymap android-codecs android-tv-features"
    sudo grep -qs '^atv' /var/lib/catostv/android/image || FIXES="android-hwc-patch $FIXES"
    for fix in $FIXES; do
      out=$(sudo /usr/local/lib/catostv/$fix 2>&1) || true
      echo "$out"
      case "$out" in *"takes effect when Android restarts"*) RESTART_ANDROID=1 ;; esac
    done
  fi
  if [ "$RESTART_ANDROID" = 1 ]; then
    echo "restarting Android (~20 s)"
    sudo -u catostv XDG_RUNTIME_DIR=/run/user/$(id -u catostv) timeout 20 waydroid session stop 2>/dev/null || true
    # Only once the container is really gone (the keystore check
    # before the next start must not meet a dying Android).
    for _ in $(seq 1 20); do
      [ "$(sudo lxc-info -P /var/lib/waydroid/lxc -n waydroid -sH 2>/dev/null)" = RUNNING ] || break
      sleep 1
    done
    sudo systemctl restart waydroid-container.service || true
  fi
fi
if [ "$SESSION" = 1 ]; then
  # `systemctl restart greetd` alone can leave the old labwc running (it
  # then keeps the screen and the new session shows black): end the TV
  # account's session first.
  echo "restarting the TV session"
  # Stop Android first and wait for it.
  sudo -u catostv XDG_RUNTIME_DIR=/run/user/$(id -u catostv) timeout 20 waydroid session stop 2>/dev/null || true
  for _ in $(seq 1 20); do
    [ "$(sudo lxc-info -P /var/lib/waydroid/lxc -n waydroid -sH 2>/dev/null)" = RUNNING ] || break
    sleep 1
  done
  sudo systemctl stop greetd
  sudo loginctl terminate-user catostv 2>/dev/null || true
  sleep 2
  sudo pkill -9 -u catostv 2>/dev/null || true
  sudo systemctl start greetd
else
  sudo pkill -x catostv-shell || true
  for d in catostv-inputd catostv-cecd catostv-castd catostv-remoted; do sudo pkill -x "$d" || true; done
fi
sudo rm -rf "$STAGE"
echo "done: $(catostv-diag version 2>/dev/null || echo installed)"
REMOTE
