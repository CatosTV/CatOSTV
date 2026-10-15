#!/usr/bin/env bash
# Boot CatosTV in a UEFI QEMU VM with a disposable virtual target disk.
#
#   ./scripts/qemu-iso.sh                 boot newest out/*.iso (installer) + test disk
#   ./scripts/qemu-iso.sh path/to.iso     boot a specific ISO
#   ./scripts/qemu-iso.sh --installed     boot the test disk only (after installing)
#   ./scripts/qemu-iso.sh --fresh         delete the test disk first
#
# Env: CATOSTV_VM_DISK_GB (default 32), CATOSTV_VM_RAM (default 4G),
#      CATOSTV_VM_DISPLAY (default gtk; use "sdl" or "none" + VNC if needed).
# Needs: qemu-desktop (qemu-system-x86_64, qemu-img), edk2-ovmf.
# The VM must be UEFI: the installer refuses legacy BIOS.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/out"
DISK="$OUT/catostv-test-disk.qcow2"
VARS="$OUT/catostv-ovmf-vars.fd"
mkdir -p "$OUT"

installed=0
iso=""
for a in "$@"; do
  case "$a" in
    --installed) installed=1 ;;
    --fresh) rm -f "$DISK" "$VARS" ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) iso="$a" ;;
  esac
done

command -v qemu-system-x86_64 >/dev/null || { echo "Install qemu-desktop" >&2; exit 1; }

code=""; vars_template=""
for c in /usr/share/edk2/x64/OVMF_CODE.4m.fd /usr/share/edk2-ovmf/x64/OVMF_CODE.fd /usr/share/OVMF/OVMF_CODE.fd; do
  [[ -f "$c" ]] && { code="$c"; vars_template="${c/CODE/VARS}"; break; }
done
[[ -n "$code" && -f "$vars_template" ]] || { echo "OVMF firmware not found; install edk2-ovmf" >&2; exit 1; }
[[ -f "$VARS" ]] || cp "$vars_template" "$VARS"
[[ -f "$DISK" ]] || qemu-img create -f qcow2 "$DISK" "${CATOSTV_VM_DISK_GB:-32}G" >/dev/null

args=(
  -enable-kvm -machine q35 -cpu host -smp 4 -m "${CATOSTV_VM_RAM:-4G}"
  -drive "if=pflash,format=raw,readonly=on,file=$code"
  -drive "if=pflash,format=raw,file=$VARS"
  -drive "file=$DISK,if=virtio,format=qcow2"
  -nic user,model=virtio-net-pci
  -device virtio-vga -display "${CATOSTV_VM_DISPLAY:-gtk}"
  -device qemu-xhci -device usb-kbd -device usb-tablet
)

if (( installed == 0 )); then
  [[ -n "$iso" ]] || iso="$(ls -1t "$OUT"/*.iso 2>/dev/null | head -n1 || true)"
  [[ -n "$iso" && -f "$iso" ]] || { echo "No ISO found. Build one with ./scripts/build-iso.sh" >&2; exit 1; }
  args+=(-cdrom "$iso" -boot order=d)
  echo "Booting installer $iso with test disk $DISK (shows as /dev/vda)"
else
  echo "Booting installed system from $DISK"
fi
exec qemu-system-x86_64 "${args[@]}"
