# CatosTV

**A no-nonsense Linux distro made for TV screens.**

I built CatosTV for my own living room. Smart TV software had turned into
an ad billboard: rows of "recommended" things I never asked for, sponsored
tiles, and a home screen that cared more about selling me something than
letting me watch what I wanted. I wanted my TV back.

CatosTV turns an ordinary PC (a mini PC, an old desktop) into a calm, fast
TV box. You turn it on, you see your apps, you pick one, you watch. No ads,
no account, no tracking, nothing you didn't install yourself.

It's based on Arch Linux and it's a rolling release. You install it once,
and from then on new versions come as updates, together with the Arch
packages they were tested with. The whole interface (home screen,
settings, installer, media player front end) is written from scratch in
Rust.

## What you get

- A home screen you drive with a remote. Your apps in one row, big
  readable text, and everything reachable with arrows, OK and Back.
- Android apps, natively. Set up Android TV once from Settings and your
  Android apps sit right next to everything else. They open full screen,
  the remote and controllers work in them, and Back on an app's first
  screen takes you home. See [ANDROID.md](ANDROID.md).
- Catfyn, a built-in player for your own [Jellyfin](https://jellyfin.org)
  server: continue watching, next episode with a countdown, subtitles.
- Linux apps as well. Moonlight game streaming, Spotify and a simple file
  browser are there from the start, and Steam (Big Picture), Kodi and lots
  more install from the built-in Store (Flathub).
- Recents, to jump back to an app or close it, with a picture of what each
  one was showing.
- Use whatever you have to control it: a keyboard, a game controller, a
  Bluetooth remote, your phone (a remote page in the browser, or KDE
  Connect), or your TV's own remote over HDMI-CEC with a USB-CEC adapter.
  One press on a keyboard, remote or controller wakes the TV from sleep.
- Cast to it from phone apps that support DLNA, and use "link with TV" in
  the YouTube app.
- Quick settings for Wi-Fi, Ethernet, Bluetooth, sound, sleep and power,
  one button away, even while an app is open.
- Updates from Settings. They're signed, you choose when to install them,
  and they never start while something is playing.

## Download

Get the latest ISO from the [Releases](../../releases) page and write it to
a USB stick (8 GB or more) with [balenaEtcher](https://etcher.balena.io),
[Ventoy](https://www.ventoy.net) or `dd`:

```bash
sudo dd if=catostv-1.0.0-x86_64.iso of=/dev/sdX bs=4M status=progress oflag=sync
```

You can check the download with the `.sha256` file next to it:
`sha256sum -c catostv-1.0.0-x86_64.iso.sha256`.

You don't need a newer ISO when a new version comes out. The installer
checks for updates before it starts and installs the newest version.

## What you need

- A 64-bit PC (x86-64) that boots in UEFI mode. Almost every PC from the
  last ten years does.
- A disk of 20 GB or more that CatosTV may erase completely. 64 GB or more
  is nicer if you plan to install many Android apps or games.
- An internet connection while installing, over Ethernet or Wi-Fi.
- A TV or monitor over HDMI. Intel, AMD and NVIDIA graphics all work, and
  the installer picks the right drivers.

## Quick start

1. Plug in the USB stick and boot from it (F12, F11 or Esc at power-on
   usually opens the boot menu).
2. Choose **Install CatosTV**. If the screen stays dark on an unusual
   graphics card, try **Install CatosTV with safe graphics**.
3. Follow the installer: keyboard, network, the disk to use, time zone. It
   shows you the disk and asks before erasing anything.
4. When it says **You're all set**, remove the stick and restart.

The TV starts straight into CatosTV. The [manual](MANUAL.md) covers
everything from there: the remote, apps, Android, casting, settings and
what to do when something misbehaves.

## Building it yourself

CatosTV is built on Arch Linux. On an Arch machine:

```bash
sudo pacman -S --needed base-devel rustup archiso grub ttf-dejavu freetype2
rustup default stable
./scripts/build-iso.sh          # makes out/catostv-<version>-x86_64.iso
```

## Credits

- Icons: [Papirus](https://github.com/PapirusDevelopmentTeam/papirus-icon-theme)
  icon theme (GPL-3.0).
- Built on [Arch Linux](https://archlinux.org), with
  [labwc](https://labwc.github.io), [Waydroid](https://waydro.id),
  [mpv](https://mpv.io) and [egui](https://github.com/emilk/egui).
- Font: [Inter](https://rsms.me/inter/).

## License

See [LICENSE](LICENSE).
