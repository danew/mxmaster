//! mxprobe — read-only HID++ inspection of attached Logitech devices.
//!
//!   mxprobe                          enumerate, find HID++ endpoints, dump features
//!   mxprobe --listen SECS [--cid C]  temporarily divert C (default 0x00C3) with
//!                                    rawXY, print notifications, then undivert
//!   mxprobe --watch SECS             print hotplug events and notifications only
//!
//! Nothing persistent is written: diverts use the temporary bits only.

use mxmaster::cf;
use mxmaster::ffi::kIOReturnNotPermitted;
use mxmaster::hid::{self, Device, Hotplug, Manager, SendMethod};
use mxmaster::hidpp::{self, hex, Error, Link, DIRECT_INDEX};
use std::time::{Duration, Instant};

struct Args {
    listen: Option<u64>,
    watch: Option<u64>,
    cid: u16,
    flags: u8,
    action: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        listen: None,
        watch: None,
        cid: 0x00C3,
        flags: 0x33,
        action: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let num = |it: &mut dyn Iterator<Item = String>| -> u64 {
            let v = it.next().expect("missing value");
            match v.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16).expect("bad hex"),
                None => v.parse().expect("bad number"),
            }
        };
        match arg.as_str() {
            "--listen" => a.listen = Some(num(&mut it)),
            "--watch" => a.watch = Some(num(&mut it)),
            "--cid" => a.cid = num(&mut it) as u16,
            "--flags" => a.flags = num(&mut it) as u8,
            "--action" => a.action = it.next(),
            _ => {
                eprintln!("usage: mxprobe [--listen SECS [--cid 0xC3] [--flags 0x33]] [--watch SECS] [--action mission-control|app-expose|space-left|space-right]");
                std::process::exit(2);
            }
        }
    }
    a
}

fn main() {
    let args = parse_args();
    let mut mgr = match Manager::open() {
        Ok(m) => m,
        Err(e) if e == kIOReturnNotPermitted => {
            eprintln!(
                "IOHIDManagerOpen: not permitted — grant Input Monitoring to this terminal/binary"
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("IOHIDManagerOpen failed: 0x{:08X}", e as u32);
            std::process::exit(1);
        }
    };

    if let Some(name) = &args.action {
        test_action(name);
        return;
    }

    if let Some(secs) = args.watch {
        watch(&mut mgr, secs);
        return;
    }

    let raws = mgr.devices();
    println!("== {} Logitech IOHIDDevice(s)", raws.len());
    let mut links = Vec::new();
    for raw in raws {
        let info = unsafe { hid::DeviceInfo::read(raw) };
        println!(
            "\n-- PID 0x{:04X} '{}' transport='{}' location=0x{:08X} primary={:04X}/{:04X}",
            info.product_id,
            info.product,
            info.transport,
            info.location_id,
            info.primary_usage.0,
            info.primary_usage.1
        );
        let pairs: Vec<String> = info
            .usage_pairs
            .iter()
            .map(|(p, u)| format!("{p:04X}/{u:04X}"))
            .collect();
        println!("   usage pairs: {}", pairs.join(" "));
        let Some(dev) = (unsafe { Device::new(raw) }) else {
            println!("   no HID++ vendor output element");
            continue;
        };
        for e in dev.elements.iter().filter(|e| e.usage_page >= 0xFF00) {
            println!(
                "   vendor element page={:04X} usage={:04X} type={} reportID=0x{:02X} size={} count={}",
                e.usage_page, e.usage, e.kind, e.report_id, e.report_size, e.report_count
            );
        }
        println!(
            "   long output: {}  short output: {}",
            dev.has_long(),
            dev.has_short()
        );
        if let Some(link) = open_link(dev) {
            links.push(link);
        }
    }

    for mut link in links {
        let receiver = !link.dev.info.is_bluetooth();
        let indices: Vec<u8> = if receiver {
            (1..=6).collect()
        } else {
            vec![DIRECT_INDEX]
        };
        if receiver {
            probe_receiver(&mut link);
        }
        for idx in indices {
            link.index = idx;
            match link.ping(Duration::from_millis(if receiver { 1500 } else { 1000 })) {
                Ok((maj, min)) => {
                    println!("\n== device index 0x{idx:02X}: HID++ {maj}.{min}");
                    dump_device(&mut link);
                    if let Some(secs) = args.listen {
                        listen(&mut link, args.cid, args.flags, secs);
                    }
                }
                Err(e) => println!("   index 0x{idx:02X}: {e}"),
            }
        }
    }
}

/// Opens the device and determines which send method gets an answer.
fn open_link(mut dev: Device) -> Option<Link> {
    if let Err(e) = dev.open() {
        println!("   IOHIDDeviceOpen failed: 0x{:08X}", e as u32);
        return None;
    }
    let bt = dev.info.is_bluetooth();
    let mut link = Link::new(dev, DIRECT_INDEX);
    for method in [SendMethod::SetValue, SendMethod::SetReport] {
        link.dev.send_method = method;
        let t0 = Instant::now();
        let res = if bt {
            link.ping(Duration::from_millis(1000))
                .map(|v| format!("HID++ {}.{}", v.0, v.1))
        } else {
            // Receiver: HID++ 1.0 register 0x02 (connection state) at index 0xFF.
            link.register(
                hidpp::SUB_GET_REGISTER,
                0x02,
                &[],
                Duration::from_millis(1000),
            )
            .map(|p| hex(&p))
        };
        let ms = t0.elapsed().as_millis();
        match res {
            Ok(v) => {
                println!("   send via {method:?}: answered in {ms} ms ({v})");
                for m in link.poll() {
                    println!("   (unsolicited) {m}");
                }
                return Some(link);
            }
            Err(Error::Hidpp20(c)) | Err(Error::Hidpp10(c)) => {
                println!("   send via {method:?}: answered with error 0x{c:02X} in {ms} ms");
                return Some(link);
            }
            Err(e) => println!("   send via {method:?}: {e}"),
        }
    }
    None
}

fn probe_receiver(link: &mut Link) {
    link.index = DIRECT_INDEX;
    let t = Duration::from_millis(1000);
    for (reg, name) in [(0x00u8, "notification flags"), (0x02, "connection state")] {
        match link.register(hidpp::SUB_GET_REGISTER, reg, &[], t) {
            Ok(p) => println!(
                "   receiver reg 0x{reg:02X} ({name}): {}",
                hex(&p[..3.min(p.len())])
            ),
            Err(e) => println!("   receiver reg 0x{reg:02X} ({name}): {e}"),
        }
    }
}

fn dump_device(link: &mut Link) {
    // Feature table
    let Ok(Some(fs)) = link.feature_index(hidpp::FEAT_FEATURE_SET) else {
        println!("   no FeatureSet");
        return;
    };
    let count = link.call(fs, 0, &[]).map(|p| p[0]).unwrap_or(0);
    println!("   {count} features (+root):");
    for i in 1..=count {
        if let Ok(p) = link.call(fs, 1, &[i]) {
            let id = u16::from_be_bytes([p[0], p[1]]);
            let ty = p[2];
            let mut tags = Vec::new();
            if ty & 0x80 != 0 {
                tags.push("obsolete");
            }
            if ty & 0x40 != 0 {
                tags.push("hidden");
            }
            if ty & 0x20 != 0 {
                tags.push("engineering");
            }
            println!("     [0x{i:02X}] 0x{id:04X} v{} {}", p[3], tags.join(","));
        }
    }

    // Name
    if let Ok(Some(n)) = link.feature_index(hidpp::FEAT_DEVICE_NAME) {
        let len = link.call(n, 0, &[]).map(|p| p[0] as usize).unwrap_or(0);
        let mut name = Vec::new();
        while name.len() < len {
            match link.call(n, 1, &[name.len() as u8]) {
                Ok(p) => name.extend(p.into_iter().take_while(|b| *b != 0)),
                Err(_) => break,
            }
            if name.is_empty() {
                break;
            }
        }
        name.truncate(len);
        println!("   name: '{}'", String::from_utf8_lossy(&name));
    }

    // Controls
    if let Ok(Some(r)) = link.feature_index(hidpp::FEAT_REPROG_V4) {
        let n = link.call(r, 0, &[]).map(|p| p[0]).unwrap_or(0);
        println!("   REPROG_V4 @0x{r:02X}: {n} controls");
        for i in 0..n {
            let Ok(p) = link.call(r, 1, &[i]) else {
                continue;
            };
            let cid = u16::from_be_bytes([p[0], p[1]]);
            let task = u16::from_be_bytes([p[2], p[3]]);
            let flags = p[4] as u16 | ((p[8] as u16) << 8);
            let rep = link
                .call(r, 2, &[p[0], p[1]])
                .map(|q| hex(&q[..q.len().min(6)]))
                .unwrap_or_else(|e| e.to_string());
            println!(
                "     cid 0x{cid:04X} task 0x{task:04X} flags 0x{flags:04X} [{}] pos {} group {} gmask 0x{:02X}  reporting: {rep}",
                key_flags(flags), p[5], p[6], p[7]
            );
        }
    }

    // SmartShift / wheels / wireless status
    for (id, name, func) in [
        (
            hidpp::FEAT_SMART_SHIFT,
            "SmartShift 0x2110 getRatchetControlMode",
            0u8,
        ),
        (
            hidpp::FEAT_SMART_SHIFT_ENH,
            "SmartShift 0x2111 getCapabilities",
            0,
        ),
        (
            hidpp::FEAT_SMART_SHIFT_ENH,
            "SmartShift 0x2111 getRatchetControlMode",
            1,
        ),
        (
            hidpp::FEAT_HIRES_WHEEL,
            "HiRes wheel 0x2121 getWheelCapability",
            0,
        ),
        (
            hidpp::FEAT_HIRES_WHEEL,
            "HiRes wheel 0x2121 getWheelMode",
            1,
        ),
        (hidpp::FEAT_THUMB_WHEEL, "Thumb wheel 0x2150 getInfo", 0),
        (hidpp::FEAT_THUMB_WHEEL, "Thumb wheel 0x2150 getStatus", 1),
    ] {
        match link.feature_index(id) {
            Ok(Some(i)) => match link.call(i, func, &[]) {
                Ok(p) => println!("   {name}: {}", hex(&p[..p.len().min(6)])),
                Err(e) => println!("   {name}: {e}"),
            },
            Ok(None) => println!("   {name}: feature absent"),
            Err(e) => println!("   {name}: {e}"),
        }
    }
    match link.feature(hidpp::FEAT_WIRELESS_STATUS) {
        Ok(Some((i, _, v))) => println!("   WirelessDeviceStatus 0x1D4B @0x{i:02X} v{v}"),
        other => println!("   WirelessDeviceStatus 0x1D4B: {other:?}"),
    }
    for m in link.poll() {
        println!("   (unsolicited) {m}");
    }
}

fn key_flags(f: u16) -> String {
    let names = [
        (0x01, "mse"),
        (0x02, "fn"),
        (0x04, "nonstd"),
        (0x08, "fnsens"),
        (0x10, "reprog"),
        (0x20, "divert"),
        (0x40, "pdivert"),
        (0x80, "virtual"),
        (0x100, "rawxy"),
        (0x200, "forcerawxy"),
        (0x400, "analytics"),
        (0x800, "rawwheel"),
    ];
    names
        .iter()
        .filter(|(b, _)| f & b != 0)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(",")
}

fn listen(link: &mut Link, cid: u16, flags: u8, secs: u64) {
    let Ok(Some(r)) = link.feature_index(hidpp::FEAT_REPROG_V4) else {
        return;
    };
    let [hi, lo] = cid.to_be_bytes();
    match link.call(r, 3, &[hi, lo, flags, 0, 0, 0]) {
        Ok(p) => println!(
            "\n   setCidReporting(0x{cid:04X}, 0x{flags:02X}) -> {}",
            hex(&p[..p.len().min(6)])
        ),
        Err(e) => {
            println!("\n   setCidReporting(0x{cid:04X}, 0x{flags:02X}) -> {e}");
            return;
        }
    }
    if let Ok(p) = link.call(r, 2, &[hi, lo]) {
        println!("   getCidReporting -> {}", hex(&p[..p.len().min(6)]));
    }
    println!(
        "   listening {secs}s — press/hold/move the control (Ctrl-C leaves divert until reconnect)"
    );
    let end = Instant::now() + Duration::from_secs(secs);
    let (mut moves, mut sx, mut sy) = (0u32, 0i32, 0i32);
    while Instant::now() < end {
        cf::run_loop_once(0.05);
        for m in link.poll() {
            if m.feat == r && m.func == 1 && m.is_notification() {
                let dx = i16::from_be_bytes([m.params[0], m.params[1]]) as i32;
                let dy = i16::from_be_bytes([m.params[2], m.params[3]]) as i32;
                moves += 1;
                sx += dx;
                sy += dy;
                if moves % 10 == 1 {
                    println!("   rawXY dx={dx:+} dy={dy:+} (sum {sx:+},{sy:+}, n={moves})");
                }
            } else {
                if m.feat == r && m.func == 0 {
                    println!(
                        "   buttons: {}  [sum since last {sx:+},{sy:+} n={moves}]",
                        hex(&m.params[..m.params.len().min(8)])
                    );
                    moves = 0;
                    sx = 0;
                    sy = 0;
                } else {
                    println!("   other: {m}");
                }
            }
        }
    }
    let undo = if flags & 0x20 != 0 { 0x22 } else { 0x02 };
    match link.call(r, 3, &[hi, lo, undo, 0, 0, 0]) {
        Ok(_) => println!("   reverted (0x{undo:02X})"),
        Err(e) => println!("   revert failed: {e}"),
    }
}

fn watch(mgr: &mut Manager, secs: u64) {
    mgr.watch();
    let mut links: Vec<Link> = Vec::new();
    let end = Instant::now() + Duration::from_secs(secs);
    let t0 = Instant::now();
    println!("watching {secs}s for hotplug and HID++ notifications");
    while Instant::now() < end {
        cf::run_loop_once(0.1);
        let ts = t0.elapsed().as_secs_f32();
        for ev in mgr.take_hotplug() {
            match ev {
                Hotplug::Arrived(raw) => {
                    let info = unsafe { hid::DeviceInfo::read(raw) };
                    println!(
                        "[{ts:7.2}] arrived PID 0x{:04X} '{}' {}",
                        info.product_id, info.product, info.transport
                    );
                    if let Some(mut d) = unsafe { Device::new(raw) } {
                        if d.open().is_ok() {
                            links.push(Link::new(d, DIRECT_INDEX));
                        }
                    }
                }
                Hotplug::Removed(raw) => {
                    println!("[{ts:7.2}] removed {raw:?}");
                    links.retain(|l| l.dev.raw != raw);
                }
            }
        }
        for l in &mut links {
            for m in l.poll() {
                println!("[{ts:7.2}] PID 0x{:04X}: {m}", l.dev.info.product_id);
            }
        }
    }
}

/// Performs one daemon action and reports the active Space before and after.
fn test_action(name: &str) {
    use mxmaster::actions::{perform, Action};
    use mxmaster::ffi::{CGPreflightPostEventAccess, CGSGetActiveSpace, CGSMainConnectionID};
    let action = match name {
        "mission-control" => Action::MissionControl,
        "app-expose" => Action::AppExpose,
        "space-left" => Action::SpaceLeft,
        "space-right" => Action::SpaceRight,
        _ => {
            eprintln!("unknown action {name}");
            std::process::exit(2);
        }
    };
    let space = || unsafe { CGSGetActiveSpace(CGSMainConnectionID()) };
    println!("post-event access: {}", unsafe {
        CGPreflightPostEventAccess()
    });
    println!("active space before: {}", space());
    perform(action);
    for ms in [100u64, 400, 1000] {
        std::thread::sleep(Duration::from_millis(ms));
        println!("active space after +{ms}ms: {}", space());
    }
}
