//! mxmasterd — gesture button support for the MX Master 3S without Options+.
//!
//! Only the gesture button (CID 0x00C3) is diverted, temporarily, with rawXY.
//! Pointer, clicks, wheels, back/forward and the SmartShift button stay
//! native. Diversion is volatile on the mouse, so it is re-applied after every
//! reconnect: new IOHIDDevice (BLE), WirelessDeviceStatus 0x1D4B, Bolt
//! receiver connection notification 0x41, and system wake.

use mxmaster::actions::{self, Busy, GestureTracker};
use mxmaster::ffi::*;
use mxmaster::hid::{self, Device, Hotplug, Manager, SendMethod};
use mxmaster::hidpp::{self, Error, Link, Msg, DIRECT_INDEX};
use mxmaster::{cf, log};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PID_MX_MASTER_3S_BLE: u16 = 0xB034;
const PID_MX_MASTER_3S_BUSINESS_BLE: u16 = 0xB043;
const PID_BOLT_RECEIVER: u16 = 0xC548;

const CID_GESTURE: u16 = 0x00C3;
const DIVERT_RAWXY: u8 = 0x33; // divert + rawXY, temporary (valid bits set)
const DIVERT_BUTTON: u8 = 0x03; // divert only, temporary
const UNDIVERT_RAWXY: u8 = 0x22;
const UNDIVERT_BUTTON: u8 = 0x02;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

static STOP: AtomicBool = AtomicBool::new(false);
static WOKE: AtomicBool = AtomicBool::new(false);
static POWER_PORT: AtomicU32 = AtomicU32::new(0);
static DEBUG: AtomicBool = AtomicBool::new(false);

macro_rules! debug {
    ($($arg:tt)*) => {
        if DEBUG.load(Ordering::Relaxed) {
            log!($($arg)*);
        }
    };
}

struct Config {
    reprog: u8,
    wireless: Option<u8>,
    rawxy: bool,
}

struct Session {
    link: Link,
    bolt: bool,
    config: Option<Config>,
    next_attempt: Instant,
    backoff: Duration,
    tracker: GestureTracker,
    button_down: bool,
    busy: Busy,
}

impl Session {
    fn schedule(&mut self, delay: Duration) {
        self.config = None;
        self.tracker.cancel();
        self.button_down = false;
        self.next_attempt = Instant::now() + delay;
    }

    fn fail(&mut self, why: &str) {
        log!(
            "configure failed: {why}; retrying in {}s",
            self.backoff.as_secs()
        );
        self.config = None;
        self.next_attempt = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
    }
}

fn main() {
    DEBUG.store(
        std::env::var_os("MXMASTER_DEBUG").is_some(),
        Ordering::Relaxed,
    );
    install_signal_handlers();
    install_power_notifications();

    // Accessibility (posting the Space-switch hotkeys). Prompting at startup
    // lists the app in System Settings straight away.
    if !unsafe { CGPreflightPostEventAccess() } {
        log!("Accessibility not granted; requesting (needed for Space switching)");
        unsafe { CGRequestPostEventAccess() };
    }

    let mut requested = false;
    let mut mgr = loop {
        match Manager::open() {
            Ok(m) => break m,
            Err(e) if e == kIOReturnNotPermitted => {
                if !std::mem::replace(&mut requested, true) {
                    log!("Input Monitoring not granted; requesting and waiting for it");
                    unsafe { IOHIDRequestAccess(1) };
                }
            }
            Err(e) => log!("IOHIDManagerOpen failed: 0x{:08X}; waiting", e as u32),
        }
        for _ in 0..100 {
            if STOP.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    mgr.watch();
    log!("mxmasterd started");

    let mut session: Option<Session> = None;
    while !STOP.load(Ordering::Relaxed) {
        // Poll quickly while an action is running so the gate reopens promptly.
        let mut wait = 0.25f64;
        if let Some(s) = session.as_mut() {
            if s.busy.is_busy() {
                wait = 0.02;
            }
            if let Some(d) = s.tracker.deadline() {
                wait = wait.min(d.saturating_duration_since(Instant::now()).as_secs_f64());
            }
        }
        cf::run_loop_once(wait);

        for ev in mgr.take_hotplug() {
            match ev {
                Hotplug::Removed(raw) => {
                    if session.as_ref().is_some_and(|s| s.link.dev.raw == raw) {
                        log!("device removed");
                        session = None;
                    }
                }
                Hotplug::Arrived(raw) => {
                    if session.is_some() {
                        hid::release(raw);
                    } else {
                        session = adopt(raw);
                    }
                }
            }
        }

        let Some(s) = session.as_mut() else { continue };

        if WOKE.swap(false, Ordering::Relaxed) {
            log!("system woke; re-applying configuration");
            s.schedule(Duration::from_secs(2));
        }

        if s.config.is_none() && Instant::now() >= s.next_attempt {
            configure(s);
        }

        for m in s.link.poll() {
            handle(s, &m);
        }
        if let Some(g) = s.tracker.tick(Instant::now()) {
            fire(s, g);
        }
    }

    if let Some(s) = session.as_mut() {
        if let Some(cfg) = &s.config {
            let flags = if cfg.rawxy {
                UNDIVERT_RAWXY
            } else {
                UNDIVERT_BUTTON
            };
            let [hi, lo] = CID_GESTURE.to_be_bytes();
            let _ = s.link.request(
                cfg.reprog,
                3,
                &[hi, lo, flags, 0, 0, 0],
                Duration::from_millis(500),
            );
        }
    }
    log!("mxmasterd stopped");
}

/// Wraps a newly arrived IOHIDDevice if it is an MX Master 3S over BLE or a
/// Bolt receiver.
fn adopt(raw: IOHIDDeviceRef) -> Option<Session> {
    let pid = unsafe { hid::DeviceInfo::read(raw) }.product_id;
    let bolt = match pid {
        PID_MX_MASTER_3S_BLE | PID_MX_MASTER_3S_BUSINESS_BLE => false,
        PID_BOLT_RECEIVER => true,
        _ => {
            hid::release(raw);
            return None;
        }
    };
    let mut dev = unsafe { Device::new(raw) }?; // releases raw itself when not HID++
    if !bolt && !dev.info.is_bluetooth() {
        return None;
    }
    if let Err(e) = dev.open() {
        log!("IOHIDDeviceOpen failed: 0x{:08X}", e as u32);
        return None;
    }
    log!(
        "adopted '{}' PID 0x{pid:04X} via {}",
        dev.info.product,
        dev.info.transport
    );
    // BLE needs SetValue (SetReport is silently dropped). The receiver's send
    // method is determined during configure.
    dev.send_method = SendMethod::SetValue;
    Some(Session {
        link: Link::new(dev, DIRECT_INDEX),
        bolt,
        config: None,
        // Give a freshly (re)connected BLE link a moment before the first request.
        next_attempt: Instant::now() + Duration::from_millis(500),
        backoff: BACKOFF_MIN,
        tracker: GestureTracker::new(),
        button_down: false,
        busy: Busy::default(),
    })
}

fn configure(s: &mut Session) {
    if s.bolt {
        if let Err(e) = prepare_receiver(s) {
            return s.fail(&e);
        }
    }
    let link = &mut s.link;
    let reprog = match link.feature_index(hidpp::FEAT_REPROG_V4) {
        Ok(Some(i)) => i,
        Ok(None) => return s.fail("device has no REPROG_CONTROLS_V4"),
        Err(e) => return s.fail(&format!("feature lookup: {e}")),
    };
    let wireless = link
        .feature_index(hidpp::FEAT_WIRELESS_STATUS)
        .ok()
        .flatten();
    let [hi, lo] = CID_GESTURE.to_be_bytes();
    let rawxy = match link.call(reprog, 3, &[hi, lo, DIVERT_RAWXY, 0, 0, 0]) {
        Ok(_) => true,
        Err(Error::Hidpp20(c)) => {
            log!(
                "rawXY divert rejected ({}); falling back to tap only",
                hidpp::error_name(c)
            );
            if let Err(e) = link.call(reprog, 3, &[hi, lo, DIVERT_BUTTON, 0, 0, 0]) {
                return s.fail(&format!("divert: {e}"));
            }
            false
        }
        Err(e) => return s.fail(&format!("divert: {e}")),
    };
    log!(
        "configured: device index 0x{:02X}, gesture button diverted{}",
        link.index,
        if rawxy {
            " with rawXY"
        } else {
            " (button only)"
        }
    );
    s.config = Some(Config {
        reprog,
        wireless,
        rawxy,
    });
    s.backoff = BACKOFF_MIN;
}

/// Bolt: pick the send method, enable connection notifications, and find the
/// slot the mouse is paired to.
fn prepare_receiver(s: &mut Session) -> Result<(), String> {
    let link = &mut s.link;
    let rx_index = |l: &mut Link| l.index = DIRECT_INDEX;
    let t = Duration::from_millis(500);
    rx_index(link);
    let mut reachable = false;
    for method in [SendMethod::SetValue, SendMethod::SetReport] {
        link.dev.send_method = method;
        if let Ok(flags) = link.register(hidpp::SUB_GET_REGISTER, 0x00, &[], t) {
            reachable = true;
            // Wireless (link) notifications: bit 0x000100. Volatile receiver setting.
            if flags.len() >= 3 && flags[1] & 0x01 == 0 {
                let new = [flags[0], flags[1] | 0x01, flags[2]];
                if let Err(e) = link.register(hidpp::SUB_SET_REGISTER, 0x00, &new, t) {
                    log!("enabling receiver notifications failed: {e}");
                }
            }
            break;
        }
    }
    if !reachable {
        return Err("receiver does not answer".into());
    }
    for idx in 1..=6u8 {
        link.index = idx;
        if link.ping(Duration::from_millis(1500)).is_err() {
            continue;
        }
        if let Ok(Some(r)) = link.feature_index(hidpp::FEAT_REPROG_V4) {
            if has_control(link, r, CID_GESTURE) {
                return Ok(());
            }
        }
    }
    rx_index(link);
    Err("no paired device with a gesture button is connected".into())
}

fn has_control(link: &mut Link, reprog: u8, cid: u16) -> bool {
    let n = link.call(reprog, 0, &[]).map(|p| p[0]).unwrap_or(0);
    (0..n).any(|i| {
        link.call(reprog, 1, &[i])
            .is_ok_and(|p| u16::from_be_bytes([p[0], p[1]]) == cid)
    })
}

fn handle(s: &mut Session, m: &Msg) {
    // Bolt receiver: device (dis)connected on a slot.
    if s.bolt && m.feat == hidpp::SUB_DEVICE_CONNECTION && m.sw == 0 {
        let up = m.params.first().is_some_and(|f| f & 0x40 == 0);
        if s.config.is_none() || m.dev == s.link.index {
            if up {
                log!("receiver: device 0x{:02X} connected", m.dev);
                s.backoff = BACKOFF_MIN;
                s.schedule(Duration::from_millis(300));
            } else {
                log!("receiver: device 0x{:02X} disconnected", m.dev);
                s.tracker.cancel();
            }
        }
        return;
    }
    let Some(cfg) = &s.config else { return };
    if !m.is_notification()
        || (m.dev != s.link.index && !(s.link.index == DIRECT_INDEX && m.dev == 0))
    {
        return;
    }
    if Some(m.feat) == cfg.wireless && m.func == 0 {
        // status 1 = reconnection: volatile settings (our divert) are gone.
        if m.params.first() == Some(&1) {
            log!("device reports reconnection; re-applying divert");
            s.schedule(Duration::from_millis(200));
        }
        return;
    }
    if m.feat != cfg.reprog {
        return;
    }
    match m.func {
        0 => {
            let pressed = m
                .params
                .chunks(2)
                .take(4)
                .any(|c| c.len() == 2 && u16::from_be_bytes([c[0], c[1]]) == CID_GESTURE);
            if pressed && !s.button_down {
                s.button_down = true;
                if s.busy.is_busy() {
                    debug!("gesture button down (ignored: previous action still running)");
                } else {
                    debug!("gesture button down");
                    s.tracker.press(Instant::now());
                }
            } else if !pressed && s.button_down {
                s.button_down = false;
                debug!("gesture button up");
                if let Some(g) = s.tracker.release() {
                    fire(s, g);
                }
            }
        }
        1 if m.params.len() >= 4 => {
            let dx = i16::from_be_bytes([m.params[0], m.params[1]]) as i32;
            let dy = i16::from_be_bytes([m.params[2], m.params[3]]) as i32;
            debug!("rawXY {dx:+} {dy:+}");
            if let Some(g) = s.tracker.motion(Instant::now(), dx, dy) {
                fire(s, g);
            }
        }
        _ => {}
    }
}

fn fire(s: &mut Session, g: actions::Gesture) {
    let a = actions::action_for(g);
    log!("gesture {g:?} -> {a:?}");
    if actions::perform(a) {
        s.busy.start(a);
    }
}

// ── signals and power ─────────────────────────────────────────────

extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn on_signal(_sig: i32) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    unsafe {
        signal(2, on_signal); // SIGINT
        signal(15, on_signal); // SIGTERM
    }
}

#[allow(non_upper_case_globals)]
extern "C" fn on_power(_refcon: *mut c_void, _service: io_object_t, msg: u32, arg: *mut c_void) {
    match msg {
        kIOMessageCanSystemSleep | kIOMessageSystemWillSleep => unsafe {
            IOAllowPowerChange(POWER_PORT.load(Ordering::Relaxed), arg as isize);
        },
        kIOMessageSystemHasPoweredOn => WOKE.store(true, Ordering::Relaxed),
        _ => {}
    }
}

fn install_power_notifications() {
    unsafe {
        let mut port: IONotificationPortRef = std::ptr::null_mut();
        let mut notifier: io_object_t = 0;
        let root =
            IORegisterForSystemPower(std::ptr::null_mut(), &mut port, on_power, &mut notifier);
        if root == 0 {
            log!("IORegisterForSystemPower failed; relying on device notifications only");
            return;
        }
        POWER_PORT.store(root, Ordering::Relaxed);
        core_foundation_sys::runloop::CFRunLoopAddSource(
            core_foundation_sys::runloop::CFRunLoopGetCurrent(),
            IONotificationPortGetRunLoopSource(port),
            core_foundation_sys::runloop::kCFRunLoopDefaultMode,
        );
    }
}
