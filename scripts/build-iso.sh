#!/usr/bin/env bash
# Build the CatosTV live installer ISO on an Arch-based host.
#
#   ./scripts/build-iso.sh            # release build + ISO into out/
#
# Needs: cargo (Rust ≥ 1.95), archiso, sudo. Run as a normal user; sudo is
# used only for mkarchiso. build/ is regenerated every time, never edit it.
# Docs: docs/BUILD_AND_ISO.md
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="$ROOT/build/archiso"
OUT="$ROOT/out"
WORK="$ROOT/build/work"
BINARIES=(catostv-shell catostv-installer catostv-ctl catostv-diag catostv-inputd catostv-cecd catostv-castd catostv-remoted catostv-wlfilter)

need() { command -v "$1" >/dev/null 2>&1 || { echo "Missing: $1" >&2; exit 1; }; }
need cargo
need mkarchiso
need sudo
need grub-mkstandalone   # package grub: the live USB's boot menu (uefi.grub)

if [[ $EUID -eq 0 ]]; then
  echo "Run this script as your normal user. It uses sudo only for mkarchiso." >&2
  exit 1
fi


echo "==> Building CatosTV release binaries"
cd "$ROOT"
# The commit shows in Settings → Debug → Build.
CATOSTV_COMMIT="$(git rev-parse --short HEAD 2>/dev/null || true)" \
  cargo build --release --workspace --bins
for b in "${BINARIES[@]}"; do
  [[ -x "$ROOT/target/release/$b" ]] || { echo "Missing build output: target/release/$b" >&2; exit 1; }
done

echo "==> Preparing ArchISO profile (from releng)"
rm -rf "$PROFILE"
sudo rm -rf "$WORK"
mkdir -p "$ROOT/build" "$OUT"
cp -a /usr/share/archiso/configs/releng "$PROFILE"

# pacman ≥ 7 sandboxes downloads with Landlock + the 'alpm' user. Inside
# containers (e.g. a Proxmox LXC build box) that fails with "Landlock is not
# supported by the kernel". If the build host had to disable the sandbox, do
# the same for the ISO build's own pacman.conf, or mkarchiso fails the same way.
if grep -qE '^[[:space:]]*DisableSandbox' /etc/pacman.conf; then
  echo "==> Host pacman has DisableSandbox; applying it to the ISO build"
  sed -i '/^\[options\]/a DisableSandbox' "$PROFILE/pacman.conf"
fi

# ISO identity. The label starts with CATOSTV_, the installer's disk rules
# also recognise the live medium by its iso9660 filesystem.
sed -i 's/^iso_name=.*/iso_name="catostv"/' "$PROFILE/profiledef.sh"
sed -i 's/^install_dir=.*/install_dir="catostv"/' "$PROFILE/profiledef.sh"
# The ISO is named after the release: out/catostv-<VERSION>-x86_64.iso.
sed -i "s/^iso_version=.*/iso_version=\"$(tr -d '[:space:]' < "$ROOT/VERSION")\"/" "$PROFILE/profiledef.sh"
sed -i 's/^iso_label=.*/iso_label="CATOSTV_$(date +%Y%m)"/' "$PROFILE/profiledef.sh"
sed -i 's/^iso_publisher=.*/iso_publisher="CatosTV"/' "$PROFILE/profiledef.sh" || true
sed -i 's/^iso_application=.*/iso_application="CatosTV live installer"/' "$PROFILE/profiledef.sh" || true

# UEFI only (CatosTV refuses legacy BIOS anyway): drop releng's BIOS/syslinux
# boot modes, so a firmware left in Legacy/CSM mode shows no boot entry for
# the stick instead of a cryptic ISOLINUX error.
# (bootmodes=(...) may span several lines, so edit the whole file.)
perl -0pi -e "s/'bios\.[^']*'//g" "$PROFILE/profiledef.sh"
# GRUB instead of systemd-boot: it can draw at 720p with a large font, so
# the menu is readable on a TV (systemd-boot's text is tiny on 4K). Plan B.
perl -0pi -e "s/'uefi\.systemd-boot'/'uefi.grub'/g" "$PROFILE/profiledef.sh"
grep -q "'uefi.grub'" "$PROFILE/profiledef.sh" || { echo "no uefi.grub boot mode in profiledef.sh" >&2; exit 1; }
rm -rf "$PROFILE/efiboot" "$PROFILE/syslinux" "$PROFILE/grub"
install -Dm644 "$ROOT/iso-overlay/grub/grub.cfg" "$PROFILE/grub/grub.cfg"
install -Dm644 "$ROOT/iso-overlay/grub/themes/catostv/theme.txt" "$PROFILE/grub/themes/catostv/theme.txt"
# Theme fonts (DejaVu Sans, package ttf-dejavu) and the selection pill.
# Without them GRUB falls back to its own small font; the menu still works.
T="$PROFILE/grub/themes/catostv"
FONT_DIR=/usr/share/fonts/TTF
# grub-mkfont names a font "<name> <style> <size>": "CatosTV Bold 56".
mkfont() { grub-mkfont -n "CatosTV" -s "$2" -o "$T/catostv-${1,,}-$2.pf2" "$FONT_DIR/$3"; }
if command -v grub-mkfont >/dev/null && [[ -f $FONT_DIR/DejaVuSans.ttf && -f $FONT_DIR/DejaVuSans-Bold.ttf ]] \
    && mkfont Regular 32 DejaVuSans.ttf && mkfont Regular 22 DejaVuSans.ttf \
    && mkfont Bold 32 DejaVuSans-Bold.ttf && mkfont Bold 56 DejaVuSans-Bold.ttf; then
  echo "boot menu fonts made"
else
  echo "note: no boot menu fonts (packages grub, freetype2, ttf-dejavu): GRUB's small font" >&2
fi
python3 -I "$ROOT/scripts/solid-png.py" "$T/select_c.png" 8b7cf6
# The CatosTV start-up screen between the boot menu and the installer
# (without it the screen stayed
# blank until the installer showed): Plymouth in the live initramfs (after kms, so it
# draws as early as the graphics driver allows), the installed system's
# theme, and greetd waiting for it to quit before the installer starts.
sed -i -E '/^HOOKS=/ s/\bkms\b/kms plymouth/' "$PROFILE/airootfs/etc/mkinitcpio.conf.d/archiso.conf"
grep -q 'kms plymouth' "$PROFILE/airootfs/etc/mkinitcpio.conf.d/archiso.conf" \
  || { echo "could not add plymouth to the live initramfs HOOKS" >&2; exit 1; }
install -d "$PROFILE/airootfs/usr/share/plymouth/themes"
cp -a "$ROOT/target-root/usr/share/plymouth/themes/catostv" "$PROFILE/airootfs/usr/share/plymouth/themes/"
install -Dm644 /dev/stdin "$PROFILE/airootfs/etc/plymouth/plymouthd.conf" <<'PLY'
[Daemon]
Theme=catostv
ShowDelay=0
PLY
install -Dm644 "$ROOT/target-root/etc/systemd/system/greetd.service.d/catostv-plymouth.conf" \
  "$PROFILE/airootfs/etc/systemd/system/greetd.service.d/catostv-plymouth.conf"
# No Arch names on screen in the live system.
printf 'catostv-live\n' >"$PROFILE/airootfs/etc/hostname"
: >"$PROFILE/airootfs/etc/motd"
rm -f "$PROFILE/airootfs/usr/local/bin/Installation_guide"
cat >"$PROFILE/airootfs/etc/os-release" <<OSREL
NAME="CatosTV"
PRETTY_NAME="CatosTV live installer"
ID=catostv
ID_LIKE=arch
VERSION_ID="$(tr -d '[:space:]' < "$ROOT/VERSION")"
OSREL

# Live-environment packages. Names verified on archlinux.org (see
# docs/DEPENDENCIES.md). One wrong name aborts mkarchiso.
cat >> "$PROFILE/packages.x86_64" <<'PKGS'
greetd
greetd-agreety
labwc
xorg-xwayland
mesa
vulkan-intel
vulkan-radeon
intel-media-driver
networkmanager
wpa_supplicant
bluez
bluez-utils
pipewire
pipewire-pulse
wireplumber
polkit
gptfdisk
dosfstools
e2fsprogs
util-linux
arch-install-scripts
wtype
inter-font
reflector
noto-fonts
noto-fonts-emoji
adwaita-cursors
plymouth
PKGS
sort -u -o "$PROFILE/packages.x86_64" "$PROFILE/packages.x86_64"

A="$PROFILE/airootfs"
for b in "${BINARIES[@]}"; do
  install -Dm755 "$ROOT/target/release/$b" "$A/usr/local/bin/$b"
done
install -Dm755 "$ROOT/iso-overlay/usr-local-bin-catostv-live-session" "$A/usr/local/bin/catostv-live-session"
install -Dm644 "$ROOT/iso-overlay/etc/greetd/config.toml" "$A/etc/greetd/config.toml"
install -Dm644 "$ROOT/iso-overlay/root/.config/labwc/rc.xml" "$A/root/.config/labwc/rc.xml"
install -Dm755 "$ROOT/iso-overlay/root/.config/labwc/autostart" "$A/root/.config/labwc/autostart"
install -Dm644 "$ROOT/iso-overlay/root/.config/labwc/environment" "$A/root/.config/labwc/environment"
mkdir -p "$A/usr/share/catostv"
cp -a "$ROOT/target-root" "$A/usr/share/catostv/install-root"
# The installer's fallback pin when the update server can't be reached:
# the Arch Linux Archive day this ISO's packages come from (UTC; the
# snapshot of a day is complete once the day is over, so take yesterday).
date -u -d yesterday +%Y/%m/%d >"$A/usr/share/catostv/archive-date"
"$ROOT/target/release/catostv-diag" packages >"$A/usr/share/catostv/install-root/usr/share/catostv/packages.txt"

# Start NetworkManager and the graphical installer in the live environment.
mkdir -p "$A/etc/systemd/system/multi-user.target.wants"
ln -sf /usr/lib/systemd/system/NetworkManager.service "$A/etc/systemd/system/multi-user.target.wants/NetworkManager.service"
ln -sf /usr/lib/systemd/system/greetd.service "$A/etc/systemd/system/display-manager.service"
ln -sf /usr/lib/systemd/system/graphical.target "$A/etc/systemd/system/default.target"
# releng enables iwd; NetworkManager uses wpa_supplicant, so avoid two Wi-Fi daemons.
rm -f "$A/etc/systemd/system/multi-user.target.wants/iwd.service"

# The stock releng ISO autologins root on tty1. The graphical installer replaces it.
rm -rf "$A/etc/systemd/system/getty@tty1.service.d"

# mkarchiso does not preserve file modes from airootfs; declare them: our
# binaries, the live session, and every file git tracks as executable in
# target-root (the installer copies those into the new system).
{
  echo 'file_permissions+=('
  for b in "${BINARIES[@]}" catostv-live-session; do
    echo "  [\"/usr/local/bin/$b\"]=\"0:0:755\""
  done
  echo '  ["/root/.config/labwc/autostart"]="0:0:755"'
  git -C "$ROOT" ls-files -s target-root | awk '$1 == "100755" {print $4}' | while read -r f; do
    echo "  [\"/usr/share/catostv/install-root/${f#target-root/}\"]=\"0:0:755\""
  done
  echo ')'
} >>"$PROFILE/profiledef.sh"

echo "==> Building ISO (sudo mkarchiso)"
sudo mkarchiso -v -r -w "$WORK" -o "$OUT" "$PROFILE"

echo
echo "Done. ISO(s):"
ls -lh "$OUT"/*.iso
echo "Next: ./scripts/qemu-iso.sh   (UEFI VM with a disposable target disk)"
