# Android app

The Android app is a [remote control](remote.md) device for your phone. It scans the QR code a
Fontaine host shows, pairs through the confirmation in the host terminal, and then drives the live
session: it shows the transcript, sends prompts and slash lines, answers approvals and questions,
and interrupts a turn. It is a sideloaded APK; there is no store listing.

## Install

Download `fontaine-android.apk` and `fontaine-android.apk.minisig` from a release, verify the
signature as described in [Verifying releases](verifying-releases.md#verify-the-android-app), then open
the APK on the phone and allow your browser or file manager to install apps from unknown sources
when Android asks. The app needs Android 8 (API 26) or newer, an arm64 phone and OpenGL ES 3.

Android's own install signature only lets the system install and update the package. The minisign
signature is what proves who made the file.

## Pair

1. On the host, run `fontaine --remote`, or `/remote-control` or `/remote on` in a session. A QR code
   appears in the ticket dialog.
2. Open the app and point the camera at the code, or scan it with the system camera: Android offers
   to open Fontaine and lands on the same screen.
3. The app shows "Pair with `<host id>`?". Nothing connects until you agree.
4. The host terminal asks you to confirm the device. Press `y` there. The app moves on to the session
   when the host says the pairing is done.

The code carries a `fontaine://pair/…` link. A link only asks to pair: any app or web page can open
it, and the ticket inside is a secret. Only the confirmation in the host terminal authorizes the
phone, so decline a pairing prompt you did not start. The app does not keep the link after reading it.
The phone's name on the confirmation line is its model name.

If the camera is off or denied, paste the link, the prefixed ticket or the bare ticket text into the
field on the scan screen. The camera permission is requested when you tap "Allow camera" on the scan screen.

## Reattach

Paired hosts are listed on the scan screen. Tap one to connect without scanning again, while the host
has remote control on. A host that revoked the phone, or no longer knows it, shows the reason (`this
device was revoked and must pair again`); pair again with a new ticket.

## In a session

- The transcript follows the host, and the status line shows the model, mode and branch.
- The message field sends a prompt. A message that starts with `/` is sent as a slash line and runs
  only if the command is on the [device list](remote.md#what-a-device-sees-and-can-do). A message is at
  most 8 KiB.
- **Interrupt** stops the main turn when one is running.
- An approval shows the tool, the reason and any diff, with the choices the host offered: allow once,
  allow for the session, deny. "Always allow" is never offered, because it writes a permanent rule on
  the host. A question takes one option; a multi-select question is answered at the host.
- Elicitations, the pairing confirmation and other host-only dialogs show "the host is waiting on …".
- While the connection is down the app shows that it is reconnecting and resumes without a new
  pairing. A dropped connection never answers a dialog.

The app sends the same messages as `fontaine attach`, so the host's floor, guardian, device command
list and attribution by node id apply unchanged, and the ledger records the phone's node id.

## Limits

- The app has no TalkBack or other accessibility support.
- The device key lives in the app's private storage, readable only by the app. It is not wrapped by
  the Android Keystore.
- One session at a time; there is no session list, push notification or LAN-only mode.
- Questions allow one option each; typed answers and multi-select answers happen at the host.
- The app reads what the host publishes leniently: an entry kind the app does not know is skipped.
