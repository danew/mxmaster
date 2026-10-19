# mxmaster

A small Rust daemon that gives the Logitech **MX Master 3S** its gesture button
on macOS without Logi Options+. No GUI, no telemetry, no network, no Python, no
Logitech software. It runs as your user from a LaunchAgent.

## What it does

Only the gesture (thumb) button is touched. It is diverted to the daemon over
HID++ with raw XY coordinates, using the *temporary* bits only, so the mouse
behaves stock again on its next reconnect if the daemon is not running.

| Gesture button | Action |
|---|---|
| Tap | Mission Control |
| Hold + up | Mission Control |
| Hold + down | App Exposé |
| Hold + left / right | Switch to the Space on the left / right |

Everything else stays native and untouched: pointer, clicks, both wheels,
back/forward (buttons 4/5), and the SmartShift button, which toggles
ratchet/free-spin in the mouse firmware.

The mapping is hardcoded in `src/actions.rs` (`action_for`).

## Requirements

* macOS 13 or later. Developed and tested on macOS 26 on Apple silicon; Intel
  Macs are untested.
* A Rust toolchain ([rustup](https://rustup.rs)) to build.
* An MX Master 3S paired over Bluetooth (or a Logi Bolt receiver, untested).

## Install

```sh
./install.sh
```

This builds `MXMaster.app` (a background-only bundle, no Dock icon), installs it
to `~/Applications`, and loads the LaunchAgent `io.github.danew.mxmaster`, which
starts it at login and restarts it if it crashes.

Then grant **MXMaster** both permissions in **System Settings → Privacy &
Security**:

* **Input Monitoring** — needed to talk to the mouse
* **Accessibility** — needed to post the Space-switch shortcuts

macOS should prompt for both on first launch. After granting, restart it:

```sh
launchctl kickstart -k gui/$(id -u)/io.github.danew.mxmaster
```

Log: `~/Library/Logs/mxmasterd.log`. Run with `MXMASTER_DEBUG=1` to log every
press, release and rawXY report.

**Logi Options+ must not run at the same time.** It diverts the same controls.
Uninstall it, or stop it until the next login with
`launchctl bootout gui/$(id -u)/com.logi.cp-dev-mgr`.

### Keeping permissions across rebuilds

macOS keys permission grants on the code signature. An ad-hoc signature changes
with every build, so after each `./install.sh` the grants have to be given
again. To avoid that, create a local self-signed code-signing identity once:

```sh
scripts/make-signing-identity.sh   # adds "MXMaster Local Signing" to your login keychain
./install.sh                       # now signs with it
```

### Uninstall

```sh
./uninstall.sh                      # stop and remove the app and LaunchAgent
./uninstall.sh --reset-permissions  # also clear its privacy entries
```

## Reconnection

The diversion is volatile on the mouse and is re-applied automatically on:

* a new IOHIDDevice (Bluetooth reconnect, mouse power cycle, Easy-Switch back)
* HID++ `0x1D4B` Wireless Device Status "reconnection"
* Logi Bolt receiver connection notification (`0x41`)
* system wake

Failed setup attempts retry with backoff (1 s up to 30 s).

## Bluetooth and Bolt

Bluetooth LE is implemented and tested. Logi Bolt (receiver PID `0xC548`, device
slots 1–6) is implemented from the reference implementations but **untested on
hardware**. See `docs/HIDPP.md` for how the two transports differ.

## Probe tool

`mxprobe` is a read-only HID++ inspector used to develop this:

```sh
cargo run --release --bin mxprobe                         # enumerate, dump features/controls
cargo run --release --bin mxprobe -- --listen 30          # temporary divert, print events, revert
cargo run --release --bin mxprobe -- --watch 60           # hotplug + notifications
cargo run --release --bin mxprobe -- --action space-right # test an action
```

## Private macOS APIs

There are no public APIs for these actions, so the daemon uses the same private
calls as other mouse utilities. They may break in a future macOS release.

* `CoreDockSendNotification`: opens Mission Control and App Exposé
* `CGSGetSymbolicHotKeyValue` / `CGSIsSymbolicHotKeyEnabled` /
  `CGSSetSymbolicHotKeyEnabled`: read your Space-switch shortcut so remapped
  shortcuts keep working
* `CGSGetActiveSpace`: detects when a Space switch has finished

## Credits

This project was built by studying two MIT-licensed projects. No code was
copied, but much of what makes it work comes from their research:

* **[RRY/lazy-mouse](https://github.com/RRY/lazy-mouse)** by Ralf Ryback — the
  macOS Bluetooth LE transport findings: the HID++ collection is an extra
  `0xFF43` usage pair on the mouse's own IOHIDDevice and must be matched by
  vendor ID; only long reports exist; `IOHIDDeviceSetReport` silently sends
  nothing, so writes go through `IOHIDDeviceSetValue`; macOS replaces the
  IOHIDDevice on every reconnect and the mouse forgets diversions. Also the
  `setCidReporting` flag/valid-bit layout, and the self-signed code-signing
  approach that keeps permission grants across rebuilds
  (`scripts/make-signing-identity.sh` follows its README).
* **[TomBadash/Mouser](https://github.com/TomBadash/Mouser)** by Tom Badash —
  the gesture-button design: diverting `0x00C3` with rawXY (falling back to
  button-only), decoding the button and rawXY notifications, and requiring
  several reports per stroke so the jolt of a click cannot read as a swipe.
  Also the Logi Bolt receiver handling (PID `0xC548`, device slots 1–6), and
  the macOS actions: `CoreDockSendNotification` for Mission Control / App
  Exposé and the symbolic-hotkey lookup for switching Spaces.

Where measurements on a real MX Master 3S differed from either project, the
differences are noted in [`docs/HIDPP.md`](docs/HIDPP.md), which documents
every HID++ transaction used.

## License

MIT, see [LICENSE](LICENSE).

Not affiliated with or endorsed by Logitech. Logitech, MX Master, Logi Options+
and Logi Bolt are trademarks of Logitech.
