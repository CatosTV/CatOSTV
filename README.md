# CatosTV

**A no-nonsense Linux distro made for TVs.**

I built CatosTV for my own living room. Smart TV software had turned into
an ad billboard: rows of "recommended" things I never asked for, sponsored
tiles, a home screen that cared more about selling me something than about
letting me watch what I wanted. I wanted my TV back.

CatosTV turns any ordinary PC, a mini PC, an old desktop, into a calm,
fast TV box. You turn it on, you see your apps, you pick one, you watch.
No ads, no account, no tracking, nothing you didn't install yourself.

## What you get

- **A TV home screen you drive with a remote.** Your apps in one row, big
  readable text, everything reachable with arrows, OK and Back.
- **Android apps, natively.** Set up Android TV once from Settings and your
  Android apps sit right next to everything else on Home. They open full
  screen, the remote and controllers just work, and Back on an app's first
  screen takes you home. No emulator window, no second launcher.
  See [ANDROID.md](ANDROID.md).
- **Catfyn**, a built-in app for your own [Jellyfin](https://jellyfin.org)
  media server: continue watching, next episode with a countdown, subtitles.
- **Linux apps too**: Moonlight game streaming, Spotify and a simple file
  browser are there from the start; Steam (Big Picture), Kodi and many
  more install from the built-in Store (Flathub).
- **Control it your way**: a keyboard, a game controller, a Bluetooth
  remote, your phone (a remote page in the browser, or KDE Connect), or your
  TV's own remote over HDMI-CEC with a USB-CEC adapter.
- **Cast to it** from phone apps that support DLNA, and "link with TV" from
  the YouTube app.
- **Quick settings** for Wi-Fi, Ethernet, Bluetooth, sound and power, one
  button away, even while an app is open.
- **Updates you control**, never during a film.

## Download

Get the latest ISO from the **[Releases](../../releases)** page and write it
to a USB stick (8 GB or more) with [balenaEtcher](https://etcher.balena.io),
[Ventoy](https://www.ventoy.net) or `dd`:

```bash
sudo dd if=catostv-1.0.0-x86_64.iso of=/dev/sdX bs=4M status=progress oflag=sync
```

Check the download with the `.sha256` file next to it if you like:
`sha256sum -c catostv-1.0.0-x86_64.iso.sha256`.

## What you need

- A 64-bit PC (x86-64) that boots in **UEFI** mode (almost every PC from
  the last ten years).
- A disk of **20 GB or more** that CatosTV may erase completely. 64 GB or
  more is nicer if you plan to install many Android apps or games.
- An internet connection while installing, Ethernet or Wi-Fi.
- A TV or monitor over HDMI. Intel, AMD and NVIDIA graphics all work; the
  installer picks the right drivers.

## Quick start

1. Plug in the USB stick and boot from it (usually F12, F11 or Esc at
   power-on opens the boot menu).
2. Choose **Install CatosTV**. If the screen stays dark on an unusual
   graphics card, try **Install CatosTV with safe graphics**.
3. Follow the installer: keyboard, network, the disk to use, time zone.
   It shows you the disk and asks before erasing anything.
4. When it says **You're all set**, remove the stick and restart.

The TV starts straight into CatosTV. The [manual](MANUAL.md) covers
everything from there: the remote, apps, Android, casting, settings and
what to do when something misbehaves.

## Building it yourself

CatosTV is written in Rust and built on Arch Linux. On an Arch machine:

```bash
sudo pacman -S --needed base-devel rustup archiso grub ttf-dejavu freetype2
rustup default stable
./scripts/build-iso.sh          # → out/catostv-<version>-x86_64.iso
```

## License

See [LICENSE](LICENSE).
