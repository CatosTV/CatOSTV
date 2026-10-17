# The CatosTV manual

Everything you need to install CatosTV and live with it. If something
here doesn't match what you see on your TV, that's a bug, so please open an
issue.

- [Installing](#installing)
- [Getting around](#getting-around)
- [Apps and the Store](#apps-and-the-store)
- [Catfyn (Jellyfin)](#catfyn-jellyfin)
- [Android apps](#android-apps)
- [Your phone: remote, casting, KDE Connect](#your-phone-remote-casting-kde-connect)
- [Controllers and remotes](#controllers-and-remotes)
- [Quick settings](#quick-settings)
- [Volume](#volume)
- [Screensaver](#screensaver)
- [Settings, page by page](#settings-page-by-page)
- [Updates](#updates)
- [For tinkerers: Debug SSH](#for-tinkerers-debug-ssh)
- [When something goes wrong](#when-something-goes-wrong)

---

## Installing

### The boot menu

Boot the PC from the CatosTV USB stick. A menu appears for ten seconds:

| Entry | When to use it |
|---|---|
| **Install CatosTV** | Almost always. The installer looks at your graphics card and picks the drivers. |
| **…with NVIDIA graphics** | Force NVIDIA's own driver (GeForce GTX 16 / RTX 20 or newer). |
| **…with open graphics drivers** | Use the open-source drivers even on an NVIDIA card. |
| **…with safe graphics** | The screen stays black or garbled with the others. Everything is drawn without acceleration: fine for installing, too slow for films. |
| **Start from this computer's disk** | Leave the stick in but boot what's already installed. |
| **Firmware settings** | Opens your PC's UEFI setup (to change boot order, for example). |

After choosing, the CatosTV start-up screen shows while it loads; the
installer follows.

### The installer

You can use a keyboard, a mouse or a game controller.

1. **Welcome**: *Let's go*.
2. **Keyboard**: pick your layout. There's a field to try it out.
3. **Network**: with a cable plugged in you're already online; otherwise
   choose your Wi-Fi and type its password. CatosTV downloads its system
   during installation, so this step is required.
4. **Disk**: choose where CatosTV goes. **The whole disk is erased.** The
   installer refuses the USB stick it runs from and warns you about
   removable drives and disks that already have files on them.
5. **Time zone.**
6. **Ready to install?**: the disk, your choices and the graphics drivers
   it will set up. *Erase and install* starts; *Cancel* goes back.
7. It works through about a dozen steps (10 to 30 minutes, mostly
   downloading). Keep the TV on and the stick in.
8. **You're all set**: remove the stick and choose *Restart now*.

The installed CatosTV starts straight into its home screen. There's no
login and no account.

---

## Getting around

Everything works with **arrows, OK and Back**. Hold **OK** on something for
its options.

| Keyboard | What it does |
|---|---|
| Arrow keys | Move |
| Enter | OK · hold for options |
| Esc | Back |
| Esc three times, or hold Esc | Leave the app you're in |
| Home | CatosTV Home, from anywhere |
| F10 | Quick settings |
| Just start typing | Search |

The key guide is in **Settings → Navigation & controllers → Key guide** if
you forget.

### The tabs

The bar at the top has **Home · Recents · Apps · Store · Search**. Press Up
from the page to reach it; the tab you're on lights up.

- **Home**: your apps in a row, and *Continue watching* from Catfyn.
- **Recents**: the apps that are open, each with a picture of what it was
  showing and how much CPU and memory it uses. OK goes back to it; Up
  reaches its **×** to close it; *Close all* is underneath. When nothing is
  open it says *No background apps*.
- **Apps**: every app, in a grid.
- **Store**: apps to install (see below).
- **Search**: apps, settings and your Jellyfin library.

### App options

Hold **OK** on an app (or press the Options/Menu button) for *Open* or
*Resume*, *Restart*, *Close*, and *Uninstall* for apps from the Store or
Android.

### Leaving an app

Press **Home** (or the Home/Guide button on your remote or controller).
The app keeps running in **Recents**; to really close it, close it there or
from its options.

---

## Apps and the Store

The **Store** tab lists apps from [Flathub](https://flathub.org): Steam,
Kodi, VLC, Spotify, emulators and lots more. Pick one and choose
*Install*; it appears on Home when it's done. *Remove* is in the same
place, or *Uninstall* in the app's options.

**Settings → Apps** lists everything installed (with a search field) and
has *Update apps* to update them all.

Steam has a tile on Home from the start, marked *Not installed*. Install
it from the Store first, then it opens in its TV mode (Big Picture).

---

## Catfyn (Jellyfin)

Catfyn plays films and series from your own
[Jellyfin](https://jellyfin.org) server.

1. Open **Catfyn** from Home. It looks for Jellyfin servers on your
   network; pick yours, or choose to enter the address
   (e.g. `192.168.1.20:8096`).
2. Sign in with your Jellyfin user name and password.

Then:

- **Home** in Catfyn shows *Continue watching*, *Next up* and the newest
  items in each library; the tabs at the top open your libraries.
- OK on a film or episode plays it (or resumes it); hold OK for its
  details: *Resume*, *Play from start*, *Go to series*, *Episodes*.
- Your *Continue watching* also appears on CatosTV's own Home.
- When an episode's end credits start, a card offers the **next episode**
  with a countdown. *Play now*, or *Cancel* to stay. After three episodes
  in a row it asks whether you're still watching.

**Catfyn's settings** are behind the account button at the top right (the
one with your name and a gear): your accounts (switch, add, sign out, or
*Incognito*: nothing you watch is marked as watched), whether the next
episode plays automatically, and how long the countdown is.

If loading fails (server off, network down), Catfyn says so. Press OK to
try again.

---

## Android apps

CatosTV can run Android TV apps natively, right next to everything else.
Setting it up takes one trip to Settings. Everything about it (setup,
Google Play or Aurora Store, installing apps, controllers, fixing problems)
is in **[ANDROID.md](ANDROID.md)**.

---

## Your phone: remote, casting, KDE Connect

**Settings → Cast & remote** shows what's available.

### Phone remote

Open the address shown there in your phone's browser (same Wi-Fi) and
enter the PIN. You get the arrows and OK, Back, Home, Quick settings,
volume, mute, play/pause, and a text box to type into the TV.

### Casting

- **DLNA**: apps on your phone that can "play to" a DLNA/UPnP renderer
  (many video and music apps, file managers) see the TV as **CatosTV**.
- **YouTube**: in the YouTube app choose *Cast → Link with TV code*; the
  TV opens YouTube's TV page so you can link it.

### KDE Connect

Install [KDE Connect](https://kdeconnect.kde.org) on your phone (Android,
iPhone) or laptop. The TV appears as **CatosTV**; pair it from the phone
(the TV asks you to accept) or from the TV.

Then you can control what's playing (play, pause, seek, volume) from the
phone's notification, share links and files to the TV, and use the
phone's clipboard. In **Settings → Cast & remote**, OK on your phone gives
*Ring phone* (find it in the sofa) and *Unpair*.

KDE Connect's own mouse/keyboard remote doesn't work on CatosTV. Use the
phone remote above for that.

---

## Controllers and remotes

### Game controllers

Xbox, PlayStation, Switch and most USB/Bluetooth pads work:

| Button | Does |
|---|---|
| D-pad / left stick | Move |
| A | OK |
| B | Back |
| Y or Start | Options |
| X or Select | Quick settings |
| Guide (Xbox / PS button) | Home, also from inside apps and games |

While an **Android app** is in front, only the Guide button is CatosTV's;
every other button goes to the app or game.

**Settings → Navigation & controllers**:

- *Pair a controller or remote* (Bluetooth: put it in pairing mode first).
- Each connected controller: switch to the **Nintendo layout** (A and B
  swapped).
- *Use controllers in CatosTV*: turn off if you only want them in games.
- *Controller: …*: choose a different button for OK, Back, Options, Home
  or Quick settings.

### Your TV's remote (HDMI-CEC)

Most PCs can't receive the TV remote over HDMI by themselves. With a
**USB-CEC adapter** (for example Pulse-Eight's) plugged in between, the
TV's remote works: arrows, OK (hold for options), Back/Exit, Home (the
*Home*/*Menu* key), the *Settings* key for Quick settings, numbers, volume
and play/pause. The power button deliberately does nothing to the PC.

### Bluetooth remotes and keyboards

Pair them in **Settings → Navigation & controllers** or **Settings →
Bluetooth**.

---

## Quick settings

Press **F10** (keyboard), **X / Select** (controller) or the **Settings**
key (TV remote). It works over apps too: it opens on top of whatever is
playing, and then it also has *Back to app*, *Full screen* and *Close* for
the app.

- **Network**: shows *Ethernet* or your Wi-Fi network. OK opens it: Wi-Fi
  on/off, nearby networks (OK to join), and **Ethernet** with its details
  and IP settings.
- **Bluetooth**: shows the connected device. OK opens it: on/off, your
  paired devices (OK connects or disconnects) and *Pair a new device*.
- **Mute**, **Settings**, **Sleep**, **Power**. Sleep shows the
  screensaver. With the screensaver turned off, it puts the TV to sleep
  instead, and one press on the keyboard, remote or controller wakes it
  (with a USB-CEC adapter, the TV's own remote too).
- The volume bar underneath: Left/Right to change, OK to mute.

---

## Volume

The volume keys bring up a slider on the right edge of the screen, over
apps too, with the sound output it plays on beside it. While it's there:

- **Up/Down** or the volume keys change the volume. Turning it up also
  unmutes.
- **OK** mutes and unmutes.
- **Left** lists your sound outputs (TV, Bluetooth headphones and so on).
  Pick one with OK.
- **Back** closes it. It also goes away by itself after a few seconds.

---

## Screensaver

After 3 minutes without a key press on CatosTV's own screens, the
screensaver comes on: a slideshow with the time, the date and the
weather. Any button takes you back to Home. It never starts over an app
or a film.

In **Settings → Customization** you can turn it off, pick **Cats** or
**Photos**, and change when it starts. Cat pictures come from TheCatAPI,
photos from Lorem Picsum and the weather from wttr.in, which works out
roughly where you are from your internet address. Pictures are kept on
the TV, so the slideshow still runs when the internet is down.

---

## Settings, page by page

- **Network**: Wi-Fi on/off, the connection, *Choose a network*, and
  **Ethernet**: status, gateway, DNS, MAC, and IP settings, either
  *Automatic* or *Manual* (address like `192.168.1.50/24`, gateway, DNS
  servers), then *Save*. The TV reconnects with the new settings.
- **Bluetooth**: on/off, paired devices, pair new ones.
- **Display**: resolution (it asks whether to keep a new one and goes back
  by itself if you don't answer), *Interface size*, *Picture fit* (shrink
  the picture if the edges are cut off), adaptive sync, HDR.
- **Sound**: volume, mute, output device, and *Use TV speakers (HDMI)*
  for when the TV stays silent.
- **Customization**: accent colour, 12/24-hour clock, the glow behind the
  focused app on Home, and the screensaver.
- **Apps**: installed apps, search, *Update apps*.
- **Android**: set up and manage Android ([ANDROID.md](ANDROID.md)).
- **Cast & remote**: casting, phone remote, KDE Connect devices.
- **Navigation & controllers**: pairing, controller layouts and buttons,
  the key guide.
- **System**: version, IP address, storage, restart, power off.
- **Updates**: see below.
- **Debug**: for tinkerers, see below.

---

## Updates

**Settings → Updates** checks for a new CatosTV version when you open it
and shows what's new. *Install* downloads it, checks it was really made by
CatosTV (it's signed), updates the system and CatosTV, and restarts the
interface. It refuses while something is playing, so close it first.

CatosTV is a rolling release. Each update brings the Arch Linux packages
it was tested with, so the system underneath only changes together with
CatosTV, never on its own. You never need a new ISO either: the installer
looks for updates before it starts and installs the newest version.

If it says *Updates aren't set up yet*, the TV was installed from 1.0.0,
which shipped without the update settings. Reinstalling from a newer
ISO fixes that (your apps and Android are on the disk that gets erased, so
note what you had).

---

## For tinkerers: Debug SSH

**Settings → Debug** has *Debug SSH*, off by default and **off again after
every restart**. Switched on, you can log in from another computer:

```bash
ssh catostv@<the TV's IP address>      # Settings → System shows it
```

The password is `catostv` until you change it with *Change password* on
the same page. Do that first: anyone on your network who knows the
default could otherwise log in and become administrator. `sudo` works with
the same password while Debug SSH is on.

The page also shows the graphics driver in use, the exact build, and has
*Restart the interface* if the screen ever gets stuck.

---

## When something goes wrong

| Problem | Try |
|---|---|
| Black screen after choosing *Install* | Wait for the CatosTV start-up screen; if it never comes, use *…with safe graphics*. |
| No picture after installing on an NVIDIA card | Reinstall with *…with open graphics drivers*. |
| The edges of the picture are cut off | Settings → Display → *Picture fit*. |
| No sound over HDMI | Settings → Sound → choose the HDMI output, or *Use TV speakers (HDMI)*. |
| An app froze | Press Home, then close it in Recents. Android apps that stop responding are closed for you, with a message. |
| The interface itself is stuck | Settings → Debug → *Restart the interface*, or Quick settings → Power → Restart. |
| Android won't start or keeps crashing | Settings → Android → *Reset Android* ([ANDROID.md](ANDROID.md)). |
| Wi-Fi password rejected | Wi-Fi passwords have at least 8 characters; check upper/lower case. |
| Lost? | Press Home. |
