# Android apps on CatosTV

CatosTV runs a real Android TV (Android TV 16) next to its own apps. Once
it's set up, Android apps are simply apps: they have a tile on Home, they
open full screen, your remote and controllers work in them, and Back on an
app's first screen brings you home. There's no emulator window and no
second home screen to deal with.

## Setting it up

1. Open **Settings → Android** and choose **Set up Android TV**.
2. Pick an app store:
   - **Google Play**: the real Play Store with your Google account. Google
     has to be told about the TV once (see *Google Play certification*
     below).
   - **Aurora Store**: installs the same apps from Google Play without a
     Google account. Good if you'd rather not sign in to Google on your TV.
3. CatosTV downloads Android (about 1 GB) and sets it up. The page shows
   each step and the download as it goes. It takes a few minutes.

From then on Android starts with the TV in the background, so apps open
quickly.

## Google Play certification

Google only lets "certified" devices sign in to Google Play. A home-built TV
box isn't certified, so you register it yourself, once:

1. In **Settings → Android**, **Google Play certification** shows a long
   number (the device ID). If it says *Waiting for Android…*, give Android
   a few minutes after the first start.
2. On a phone or computer, open
   **[google.com/android/uncertified](https://www.google.com/android/uncertified)**,
   sign in with the Google account you'll use on the TV and enter that
   number.
3. Wait a few minutes (sometimes longer), then sign in to Google Play on
   the TV. If it still refuses, restart the TV once.

Aurora Store doesn't need any of this.

## Installing apps

- **From the store**: open **Google Play** or **Aurora Store** from Home
  and install as usual. The new app gets its own tile on Home and in Apps
  within a few seconds, with its icon.
- **From a USB drive** (an `.apk` file): plug in the drive, then
  **Settings → Android → Install an APK**. CatosTV lists the APKs it finds
  (you can open folders); OK installs.

Only apps made for Android TV, or that work well with a remote, are
pleasant on a TV. Phone-only apps open but may expect a touch screen.

## Using Android apps

- **Opening**: like any other app, OK on its tile.
- **Back**: goes back inside the app. On the app's first screen, Back
  brings you to CatosTV Home, just like CatosTV's own apps. (On a keyboard,
  Esc is Back.)
- **Home**: the Home key (or Guide on a controller) always takes you to
  CatosTV Home. The app keeps running and waits in **Recents**.
- **Switching**: open another app from Home or go back to it from Recents;
  Android apps resume where you left them.
- **Controllers**: while an Android app or game is in front, every button
  goes to it, except **Guide**, which is always CatosTV's Home.
- **Quick settings** (F10, or the TV remote's Settings key) work over
  Android apps too: volume, Wi-Fi, Bluetooth, and *Back to app* / *Close*.
- **Uninstalling**: hold OK on the app's tile → **Uninstall**.

## When an app misbehaves

- **An app freezes**: CatosTV notices when Android reports an app as not
  responding, closes it and tells you ("… stopped responding and was
  closed, open it again"). Open it again and it starts fresh.
- **An app keeps crashing or won't open**: close it in Recents and open it
  again. Still broken: uninstall and reinstall it.
- **Android itself won't start, or everything crashes**: **Settings →
  Android → Reset Android**. It removes all Android apps, sign-ins and app
  data and sets Android up again from scratch (a few minutes). CatosTV, your
  media and your settings aren't touched. A copy of the old Android data is
  kept until the next reset. With Google Play you'll need to register the
  TV again. The page shows the new ID.

## Good to know

- **Graphics**: on Intel and AMD graphics Android draws with the GPU. With
  NVIDIA's driver, the open NVIDIA driver, or *basic graphics*, Android draws
  in software: fine for most apps, slow for 3D games and very high
  resolutions. CatosTV's own video player (Catfyn) isn't affected.
- **Streaming services**: Android on CatosTV has software copy
  protection (Widevine L3), not the hardware kind only Google-certified
  devices have. YouTube for Android TV plays once the TV is registered
  (see *Google Play certification* above). SmartTube, Jellyfin, Plex,
  Kodi, Twitch and most free and live TV apps play normally. Apps that
  insist on hardware protection, such as Netflix, Prime Video and Disney+,
  refuse to play, keep loading or drop to low quality.
- **Casting to Android apps**: Android's built-in Chromecast receiver isn't
  available. CatosTV's own casting (DLNA, YouTube "link with TV") still
  works. See the [manual](MANUAL.md#your-phone).
- **Storage**: Android apps and their data live on CatosTV's disk; Settings
  → System shows how much space is left.
