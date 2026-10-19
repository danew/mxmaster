# HID++ transactions required for the MX Master 3S on macOS

This is the subset of HID++ 2.0 the daemon needs, extracted from
[TomBadash/Mouser](https://github.com/TomBadash/Mouser) (`core/hid_gesture.py`,
`core/key_simulator.py`) and [RRY/lazy-mouse](https://github.com/RRY/lazy-mouse)
(`Sources/HIDPPKit`, README "Protocol findings"). Entries marked **[probe]** were
confirmed against a physical MX Master 3S with `mxprobe`; see
[Probe results](#probe-results).

## 1. Transport

| | Bluetooth LE (direct) | Logi Bolt receiver |
|---|---|---|
| IOHIDDevice | the mouse itself, VID `0x046D` PID `0xB034` | the receiver, VID `0x046D` PID `0xC548` |
| HID++ collection | extra usage pair on the *same* device as the mouse collection: usage page `0xFF43`, usage `0x0202` (lazy-mouse) | vendor usage page `0xFF00` (usage `0x0001` short, `0x0002` long) |
| Matching | by VID (+ PID), then locate the vendor output element; usage-page matching does not find it | by VID + PID `0xC548` |
| Report IDs | long `0x11` only (19-byte body) | short `0x10` (6-byte body) and long `0x11` |
| Device index | `0xFF` (responses may carry `0x00`) | `0x01`–`0x06` (receiver slots); `0xFF` addresses the receiver |
| Sending | `IOHIDDeviceSetValue` on the 19-byte output array element; `IOHIDDeviceSetReport` returns success but sends nothing (lazy-mouse). Mouser uses `SetReport`. **[probe] decides.** | `IOHIDDeviceSetReport` (output, report ID `0x11`) — standard USB path |
| Receiving | `IOHIDDeviceRegisterInputReportCallback`; buffer includes report ID at byte 0 | same |
| Reconnect | macOS destroys the IOHIDDevice on disconnect and creates a new one on reconnect (sleep, power cycle, Easy-Switch) | receiver IOHIDDevice persists; the mouse drops off/on its slot silently |
| Permission | Input Monitoring (`IOHIDManagerOpen` → `kIOReturnNotPermitted` `0xE00002E2` without it) | same |

The two transports differ in device topology, report sizes, device index and
reconnect signalling, so the daemon models them separately.

## 2. Message format (long report)

```
byte 0     report ID 0x11
byte 1     device index (0xFF BLE, 1..6 Bolt)
byte 2     feature index
byte 3     (function << 4) | software ID      software ID 0 = device notification
byte 4..19 parameters
```

* HID++ 2.0 error: `<dev> FF <feat> <func|sw> <code>` — codes: 1 unknown, 2 invalid
  argument, 3 out of range, 4 hardware, 5 logitech internal, 6 invalid feature
  index, 7 invalid function, 8 busy, 9 unsupported.
* HID++ 1.0 error (from a receiver, e.g. empty slot): sub-ID `0x8F`.
* Responses are matched on device index, feature index, function and software ID.
  Notifications (software ID 0) can arrive between a request and its answer and
  must be dispatched, not dropped.

## 3. Transactions

### 3.1 Feature discovery — IRoot `0x0000` (always index 0)

| Function | Request | Response |
|---|---|---|
| 0 getFeature | `featID_hi featID_lo` | `index type version` (index 0 = absent) |
| 1 ping | `0 0 data` | `protocol_major protocol_minor data` |

Used on every (re)connect to resolve the indices below; indices are not cached
across connections.

### 3.2 Device name — `0x0005` (informational, probe only)

fn0 → name length; fn1(offset) → name chunk.

### 3.3 Reprogrammable controls — `0x1B04` REPROG_CONTROLS_V4

| Function | Request | Response |
|---|---|---|
| 0 getCount | — | `count` |
| 1 getCidInfo | `index` | `cid(2) task(2) flags pos group gmask additionalFlags` |
| 2 getCidReporting | `cid(2)` | `cid(2) flags remap(2) …` |
| 3 setCidReporting | `cid(2) flags 00 remap(2)` | echo |

getCidInfo flags (16-bit = byte4 | byte8<<8): `0x20` divertable, `0x40` persistently
divertable, `0x80` virtual, `0x100` rawXY, `0x200` force rawXY.

setCidReporting flags byte — every value bit has a "valid" bit; the device only
applies values whose valid bit is set:

| bit | meaning |
|---|---|
| `0x01` / `0x02` | temporary divert / valid |
| `0x04` / `0x08` | **persistent** divert / valid |
| `0x10` / `0x20` | rawXY / valid |

(Mouser's comments label `0x02` "persistent divert"; the spec and lazy-mouse's
measurements show it is the divert-valid bit.)

The daemon uses only:

* `0x33` divert + rawXY (temporary) — gesture source
* `0x03` divert, button only (temporary) — fallback when rawXY is rejected
* `0x22` / `0x02` undivert — on clean shutdown

The persistent bits are never set, so nothing survives a power cycle and the
mouse behaves stock if the daemon is not running.

Relevant CIDs on the MX Master 3S:

| CID | Control | Daemon |
|---|---|---|
| `0x0050`/`0x0051`/`0x0052` | left / right / middle | untouched (native HID) |
| `0x0053` / `0x0056` | back / forward | untouched (native HID buttons 4/5) |
| `0x00C3` | gesture (thumb) button | divert (+rawXY if accepted) |
| `0x00C4` | SmartShift (wheel mode) button | untouched — firmware toggles ratchet/free-spin natively |
| `0x00D7` | virtual gesture button | per lazy-mouse the only control that advertises rawXY on the 3S; candidate rawXY source |

### 3.4 Diverted-control notifications (software ID 0)

* fn0 — `<dev> <reprogIdx> 00 cid1(2) cid2(2) cid3(2) cid4(2)`: set of currently
  pressed diverted CIDs; `0000` terminates. Empty set = all released.
* fn1 — `<dev> <reprogIdx> 10 dx(2) dy(2)`: rawXY, signed 16-bit big-endian, only
  while the rawXY-diverted control is held. The firmware stops moving the pointer
  during the hold, so no global mouse hook is needed to suppress it.

### 3.5 SmartShift — `0x2110` / `0x2111`

Not written by the daemon. The `0x00C4` button switches modes on-device and the
auto-shift threshold lives in the mouse; read-only use in the probe:
`0x2110` fn0 → `mode(1=freespin,2=ratchet) autoDisengage defaultAutoDisengage`.

### 3.6 Wheels — `0x2121` HiRes wheel, `0x2150` thumb wheel

Not written. Reading `0x2121` fn1 (wheel mode) and `0x2150` fn1 (status) in the
probe confirms the wheels are in native (non-diverted) mode.

### 3.7 Reconnection signalling

* BLE: IOHIDManager device-removal / device-matching callbacks (new IOHIDDevice).
* `0x1D4B` Wireless device status, event fn0 `status request reason`: sent by the
  device after it (re)connects; `status = 1` "reconnection" means host software
  must re-apply volatile configuration (diversions).
* Bolt: HID++ 1.0 receiver notification sub-ID `0x41` (device connection) on the
  slot index; `params[0] & 0x40` = link *not* established. The receiver only
  forwards it once wireless notifications are enabled (HID++ 1.0 register `0x00`,
  flag `0x000100`); this is a volatile receiver setting.

Diversions are volatile: the mouse forgets them on every disconnect/sleep, so they
are re-applied on each of the signals above.

## 4. macOS actions (no HID++)

| Action | Mechanism |
|---|---|
| Mission Control | `CoreDockSendNotification("com.apple.expose.awake")` |
| App Exposé | `CoreDockSendNotification("com.apple.expose.front.awake")` |
| Space left / right | `CGSGetSymbolicHotKeyValue(79 / 81)` → post key down/up via `CGEventPost` (needs Accessibility) |

## Probe results

MX Master 3S over Bluetooth LE, macOS 26.5.1, measured with `mxprobe`
(2026-10-19). No Bolt receiver was available; the Bolt column above is from
the references and untested.

**Transport**

* One IOHIDDevice, PID `0xB034`, `Transport = "Bluetooth Low Energy"`, usage
  pairs `0001/0002 0001/0001 FF43/0202`. Vendor elements: one 19-byte
  output/input array (report ID `0x11`); no 6-byte short element.
* `IOHIDDeviceSetValue` on the 19-byte output element: answered in 16 ms.
  (lazy-mouse is right; the daemon never uses `SetReport` over BLE.)
* HID++ 4.5, 36 features. Relevant indices on this unit: `0x1D4B` @0x04,
  `0x1B04` v5 @0x09, `0x2110` @0x0E, `0x2121` @0x0F, `0x2150` @0x10.
  `0x2111` (enhanced SmartShift) is absent. Indices are still resolved at
  runtime.

**Controls (`0x1B04` getCidInfo)**

| CID | flags | note |
|---|---|---|
| `0x0050`/`0x0051` | `0x0401` mse,analytics | not divertable |
| `0x0052`,`0x0053`,`0x0056`,`0x00C3`,`0x00C4` | `0x0531` mse,reprog,divert,**rawxy**,analytics | |
| `0x00D7` | `0x03A0` divert,virtual,rawxy,forcerawxy | virtual |

lazy-mouse states `0x00C3` lacks rawXY; that reading used only the low flag
byte. The high byte (`0x05`) carries `0x100` rawXY, and `setCidReporting(0x00C3,
0x33)` is accepted (read back as `0x11`, i.e. temporary divert + rawXY).

**Options+ leaves only volatile state.** With Options+ running, `0x53`, `0x56`
and `0xC4` were diverted, `0xC3` diverted+rawXY, the HiRes wheel mode was `01`
(target = HID++), the thumb wheel diverted and analytics reporting on for every
control. After stopping Options+ and power-cycling the mouse everything read
stock (all reporting `00`, wheel mode `00`, thumb wheel `00`), and the SmartShift
threshold went from Options+'s 12 back to 10. No persistent device state needs
cleaning up.

**rawXY stream**

* Press/release: `FF 09 00 00C3…` / `FF 09 00 0000…`.
* rawXY: `FF 09 10 dx dy`, about every 7 ms while the control is held; the
  pointer does not move during the hold.
* **The first rawXY report after a press is stale.** It arrives 0–16 ms after
  the press notification carrying an accumulated value (logged up to −4358
  counts), unrelated to, and in 32 of 136 logged presses opposite to, the
  swipe that follows. The daemon always discards it. The real swipe is the
  following stream: 20–90 reports summing to 600–1900 counts; clicks stay
  under ~30.

**Actions**

* `CoreDockSendNotification` opens Mission Control / App Exposé without any
  permission.
* Posting the Space hotkeys needs Accessibility (`CGPreflightPostEventAccess`).
  The active Space ID (`CGSGetActiveSpace`) changes 0.4–1 s after the post,
  when the slide animation ends. Gestures that arrive meanwhile are dropped by
  the daemon rather than queued.
