# The CatosTV manual

The things you can't tell just by looking at the screen. If something here
doesn't match your TV, please open an issue.

- [Installing](#installing)
- [Keys and buttons](#keys-and-buttons)
- [CatTube (YouTube)](#cattube-youtube)
- [Catfyn (Jellyfin and Plex)](#catfyn-jellyfin-and-plex)
- [Apps that need a mouse](#apps-that-need-a-mouse)
- [Android apps](#android-apps)
- [Your phone](#your-phone)
- [Your TV's remote (HDMI-CEC)](#your-tvs-remote-hdmi-cec)
- [Updates](#updates)
- [Debug SSH](#debug-ssh)
- [When something goes wrong](#when-something-goes-wrong)

---

## Installing

Boot the PC from the CatosTV USB stick in **UEFI mode**. The menu starts the
first entry after ten seconds.

| Entry | When to use it |
|---|---|
| **Install CatosTV** | Almost always. The installer picks the graphics drivers. |
| **Install with NVIDIA's driver** | For GeForce GTX 16 / RTX 20 or newer. |
| **Install with the open-source NVIDIA driver** | Older NVIDIA cards, or when NVIDIA's driver gives no picture. |
| **Install with basic graphics** | The screen stays black or garbled with the others. Fine for installing, too slow for films. |

The installer needs **internet**, because it downloads the newest
version, and it takes 10 to 30 minutes. **The disk you pick is erased.**
After the restart CatosTV opens on Home. There's nothing to sign in to.

---

## Keys and buttons

Hold **OK** on anything for its options. On Home that includes *Move*, to
put your apps in the order you like.

| Keyboard | Controller | Does |
|---|---|---|
| Enter | A | OK |
| Esc | B | Back |
| Esc three times, or hold Esc | | Leave the app you're in |
| Home | Guide (Xbox / PS button) | Home, from anywhere, also inside games |
| F10 | X or Select | Quick settings |
| | Y or Start | Options |
| Start typing | | Search |

While an **Android app** is in front, only the Guide button belongs to
CatosTV; the other controller buttons go to the app.

Each controller can switch to the **Nintendo layout** (A and B swapped) or
use other buttons in **Settings → Navigation & controllers**. Turn off
*Use controllers in CatosTV* if you only want them in games.

---

## CatTube (YouTube)

To sign in, open the account button in CatTube. It shows a code: type it at
**google.com/device** on your phone. More than one account works.

To use the YouTube app on your phone as a remote, go to *Settings → Watch
on TV → Link with TV code* in the phone app and type the code from
CatTube's Settings.

---

## Catfyn (Jellyfin and Plex)

- **Jellyfin**: Catfyn finds servers on your network. If yours isn't
  listed, type its address, for example `192.168.1.20:8096`.
- **Plex**: choose *Sign in with Plex* and type the code it shows at
  **plex.tv/link**.

You can add both. *Incognito* on an account, in Catfyn's settings (the
button with your name), keeps what you watch off CatosTV's Home.

---

## Apps that need a mouse

Desktop apps made for a mouse get a pointer: the arrows move it, OK clicks,
and holding OK is a right click. **Back** puts the remote back to normal.
It comes on by itself for apps that need it; **Mouse** in Quick settings,
while the app is open, turns it on or off for that app for good.

---

## Android apps

Everything about Android (setup, Google Play or Aurora Store, installing
APKs, fixing problems) is in **[the Android guide](ANDROID.md)**.

---

## Your phone

**Settings → Cast & remote** shows the phone remote's address and PIN.
Open the address in your phone's browser on the same Wi-Fi. It has a
touchpad and a keyboard.

- **Casting**: apps that "play to" a DLNA device see the TV as
  **CatosTV**. Casting from the YouTube app plays in CatTube. Turn casting
  on or off with **Settings → Cast & remote → Cast to this TV**.
- **KDE Connect**: the TV shows up as **CatosTV**. Pair it from the phone
  and accept on the TV. KDE Connect's own mouse and keyboard remote doesn't
  work; use the phone remote instead.

---

## Your TV's remote (HDMI-CEC)

The remote that came with your TV works over the HDMI cable. Its *Home* or
*Menu* key goes Home and the *Settings* key opens Quick settings. The power button does nothing to the
PC, on purpose.

---

## Updates

New versions show up in **Settings → Updates**. They don't install while
something is playing. Each update brings the Arch Linux packages it was
tested with, so the system underneath only changes together with CatosTV.

---

## Debug SSH

**Settings → Debug → Debug SSH** lets you log in from another computer. It
switches itself off at every restart.

```bash
ssh catostv@<the TV's IP address>      # Settings → System shows it
```

The password is `catostv` until you change it on the same page. Change it
first: otherwise anyone on your network can log in as administrator.

---

## When something goes wrong

| Problem | Try |
|---|---|
| Black screen after choosing *Install* | Wait for the CatosTV start-up screen; if it never comes, use *Install with basic graphics*. |
| No picture after installing on an NVIDIA card | Reinstall with *Install with the open-source NVIDIA driver*. |
| The edges of the picture are cut off | Settings → Display → *Picture fit*. |
| No sound over HDMI | Settings → Sound → choose the HDMI output, or turn on *Use TV speakers (HDMI)*. |
| An app froze | Press Home, then close it in Recents. |
| CatosTV itself is stuck | Settings → Debug → *Restart the interface*, or Quick settings → Power → Restart. |
| Android won't start or keeps crashing | Settings → Android → *Reset Android* (see [the Android guide](ANDROID.md)). |
