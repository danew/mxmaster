//! IOKit HID transport: device enumeration/hotplug and raw HID++ report I/O.
//!
//! Everything runs on one thread. IOKit callbacks fire from inside
//! `CFRunLoopRunInMode` and only queue data; all processing happens after the
//! run loop returns, so requests never nest.

use crate::cf;
use crate::ffi::*;
use core_foundation_sys::base::{kCFAllocatorDefault, CFIndex, CFRelease, CFRetain, CFTypeRef};
use core_foundation_sys::runloop::{kCFRunLoopDefaultMode, CFRunLoopGetCurrent};
use core_foundation_sys::set::{CFSetGetCount, CFSetGetValues};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::raw::c_void;

pub const LOGITECH_VID: i64 = 0x046D;
pub const REPORT_SHORT: u8 = 0x10;
pub const REPORT_LONG: u8 = 0x11;
pub const SHORT_LEN: usize = 7;
pub const LONG_LEN: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMethod {
    /// `IOHIDDeviceSetValue` on the vendor output element (needed for BLE).
    SetValue,
    /// `IOHIDDeviceSetReport` (classic USB path).
    SetReport,
}

#[derive(Debug, Clone)]
pub struct ElementInfo {
    pub usage_page: u32,
    pub usage: u32,
    pub kind: u32,
    pub report_id: u32,
    pub report_size: u32,
    pub report_count: u32,
}

/// Static properties of an IOHIDDevice.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub product_id: u16,
    pub product: String,
    pub transport: String,
    pub location_id: i64,
    pub primary_usage: (i64, i64),
    pub usage_pairs: Vec<(i64, i64)>,
}

impl DeviceInfo {
    /// # Safety
    /// `raw` must be a valid IOHIDDevice reference.
    pub unsafe fn read(raw: IOHIDDeviceRef) -> DeviceInfo {
        let prop = |k: &str| {
            let key = cf::string(k);
            unsafe { IOHIDDeviceGetProperty(raw, key.0 as _) }
        };
        let usage_pairs = cf::array_items(prop("DeviceUsagePairs"))
            .into_iter()
            .map(|d| {
                (
                    cf::to_i64(cf::dict_get(d, "DeviceUsagePage")).unwrap_or(0),
                    cf::to_i64(cf::dict_get(d, "DeviceUsage")).unwrap_or(0),
                )
            })
            .collect();
        DeviceInfo {
            product_id: cf::to_i64(prop("ProductID")).unwrap_or(0) as u16,
            product: cf::to_string(prop("Product")).unwrap_or_default(),
            transport: cf::to_string(prop("Transport")).unwrap_or_default(),
            location_id: cf::to_i64(prop("LocationID")).unwrap_or(0),
            primary_usage: (
                cf::to_i64(prop("PrimaryUsagePage")).unwrap_or(0),
                cf::to_i64(prop("PrimaryUsage")).unwrap_or(0),
            ),
            usage_pairs,
        }
    }

    pub fn is_bluetooth(&self) -> bool {
        self.transport.to_ascii_lowercase().contains("bluetooth")
    }
}

// ── Manager ───────────────────────────────────────────────────────

#[derive(Default)]
struct HotplugQueue {
    arrived: Vec<IOHIDDeviceRef>,
    removed: Vec<IOHIDDeviceRef>,
}

pub enum Hotplug {
    /// Retained device reference; ownership passes to the receiver.
    Arrived(IOHIDDeviceRef),
    /// Pointer identity only; may already be released.
    Removed(IOHIDDeviceRef),
}

pub struct Manager {
    raw: IOHIDManagerRef,
    queue: Box<RefCell<HotplugQueue>>,
}

extern "C" fn on_match(ctx: *mut c_void, _r: IOReturn, _s: *mut c_void, device: IOHIDDeviceRef) {
    let q = unsafe { &*(ctx as *const RefCell<HotplugQueue>) };
    unsafe { CFRetain(device as CFTypeRef) };
    q.borrow_mut().arrived.push(device);
}

extern "C" fn on_remove(ctx: *mut c_void, _r: IOReturn, _s: *mut c_void, device: IOHIDDeviceRef) {
    let q = unsafe { &*(ctx as *const RefCell<HotplugQueue>) };
    q.borrow_mut().removed.push(device);
}

impl Manager {
    /// Opens an IOHIDManager matching every Logitech HID device. Fails with
    /// `kIOReturnNotPermitted` when Input Monitoring has not been granted.
    pub fn open() -> Result<Manager, IOReturn> {
        unsafe {
            let raw = IOHIDManagerCreate(kCFAllocatorDefault, kIOHIDOptionsTypeNone);
            let matching = cf::int_dict(&[("VendorID", LOGITECH_VID)]);
            IOHIDManagerSetDeviceMatching(raw, matching.0 as _);
            let res = IOHIDManagerOpen(raw, kIOHIDOptionsTypeNone);
            if res != kIOReturnSuccess {
                CFRelease(raw as CFTypeRef);
                return Err(res);
            }
            Ok(Manager {
                raw,
                queue: Box::new(RefCell::new(HotplugQueue::default())),
            })
        }
    }

    /// Delivers arrivals (including devices already present) and removals
    /// through `take_hotplug` once the run loop runs.
    pub fn watch(&mut self) {
        let ctx = &*self.queue as *const RefCell<HotplugQueue> as *mut c_void;
        unsafe {
            IOHIDManagerRegisterDeviceMatchingCallback(self.raw, on_match, ctx);
            IOHIDManagerRegisterDeviceRemovalCallback(self.raw, on_remove, ctx);
            IOHIDManagerScheduleWithRunLoop(self.raw, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
        }
    }

    pub fn take_hotplug(&self) -> Vec<Hotplug> {
        let mut q = self.queue.borrow_mut();
        let mut out: Vec<Hotplug> = q.removed.drain(..).map(Hotplug::Removed).collect();
        out.extend(q.arrived.drain(..).map(Hotplug::Arrived));
        out
    }

    /// Snapshot of currently attached Logitech devices (retained refs).
    pub fn devices(&self) -> Vec<IOHIDDeviceRef> {
        unsafe {
            let set = IOHIDManagerCopyDevices(self.raw);
            if set.is_null() {
                return Vec::new();
            }
            let n = CFSetGetCount(set) as usize;
            let mut vals: Vec<*const c_void> = vec![std::ptr::null(); n];
            CFSetGetValues(set, vals.as_mut_ptr());
            for v in &vals {
                CFRetain(*v);
            }
            CFRelease(set as CFTypeRef);
            vals.into_iter().map(|v| v as IOHIDDeviceRef).collect()
        }
    }
}

// ── Device ────────────────────────────────────────────────────────

pub struct Device {
    pub raw: IOHIDDeviceRef,
    pub info: DeviceInfo,
    pub elements: Vec<ElementInfo>,
    long_out: Option<IOHIDElementRef>,
    short_out: Option<IOHIDElementRef>,
    pub send_method: SendMethod,
    inbox: Box<RefCell<VecDeque<Vec<u8>>>>,
    buf: Box<[u8; 64]>,
    opened: bool,
}

extern "C" fn on_report(
    ctx: *mut c_void,
    result: IOReturn,
    _sender: *mut c_void,
    _ty: IOHIDReportType,
    report_id: u32,
    report: *mut u8,
    len: CFIndex,
) {
    if result != kIOReturnSuccess || len <= 0 || report.is_null() {
        return;
    }
    let id = report_id as u8;
    if id != REPORT_SHORT && id != REPORT_LONG {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(report, len as usize) };
    // IOKit includes the report ID as byte 0; normalise in case it does not.
    let mut v = Vec::with_capacity(LONG_LEN);
    if bytes[0] != id {
        v.push(id);
    }
    v.extend_from_slice(bytes);
    let q = unsafe { &*(ctx as *const RefCell<VecDeque<Vec<u8>>>) };
    q.borrow_mut().push_back(v);
}

impl Device {
    /// Takes ownership of a retained IOHIDDevice reference. Returns `None` if
    /// it carries no HID++ vendor output element.
    ///
    /// # Safety
    /// `raw` must be a valid, retained IOHIDDevice reference; it is released
    /// when the `Device` drops, or immediately when `None` is returned.
    pub unsafe fn new(raw: IOHIDDeviceRef) -> Option<Device> {
        let info = DeviceInfo::read(raw);
        let mut elements = Vec::new();
        let mut long_out = None;
        let mut short_out = None;
        unsafe {
            let arr = IOHIDDeviceCopyMatchingElements(raw, std::ptr::null(), kIOHIDOptionsTypeNone);
            for el in cf::array_items(arr as CFTypeRef) {
                let el = el as IOHIDElementRef;
                let e = ElementInfo {
                    usage_page: IOHIDElementGetUsagePage(el),
                    usage: IOHIDElementGetUsage(el),
                    kind: IOHIDElementGetType(el),
                    report_id: IOHIDElementGetReportID(el),
                    report_size: IOHIDElementGetReportSize(el),
                    report_count: IOHIDElementGetReportCount(el),
                };
                if e.usage_page >= 0xFF00 && e.kind == kIOHIDElementTypeOutput && e.report_size == 8
                {
                    let slot = match e.report_count as usize {
                        n if n == LONG_LEN - 1 => Some(&mut long_out),
                        n if n == SHORT_LEN - 1 => Some(&mut short_out),
                        _ => None,
                    };
                    if let Some(slot @ None) = slot {
                        CFRetain(el as CFTypeRef);
                        *slot = Some(el);
                    }
                }
                elements.push(e);
            }
            if !arr.is_null() {
                CFRelease(arr as CFTypeRef);
            }
        }
        if long_out.is_none() && short_out.is_none() {
            unsafe { CFRelease(raw as CFTypeRef) };
            return None;
        }
        Some(Device {
            raw,
            info,
            elements,
            long_out,
            short_out,
            send_method: SendMethod::SetValue,
            inbox: Box::new(RefCell::new(VecDeque::new())),
            buf: Box::new([0u8; 64]),
            opened: false,
        })
    }

    /// Opens the device non-exclusively (native pointer input keeps flowing to
    /// macOS) and starts receiving input reports on this thread's run loop.
    pub fn open(&mut self) -> Result<(), IOReturn> {
        unsafe {
            let res = IOHIDDeviceOpen(self.raw, kIOHIDOptionsTypeNone);
            if res != kIOReturnSuccess {
                return Err(res);
            }
            IOHIDDeviceScheduleWithRunLoop(self.raw, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
            let ctx = &*self.inbox as *const RefCell<VecDeque<Vec<u8>>> as *mut c_void;
            IOHIDDeviceRegisterInputReportCallback(
                self.raw,
                self.buf.as_mut_ptr(),
                64,
                Some(on_report),
                ctx,
            );
        }
        self.opened = true;
        Ok(())
    }

    pub fn has_long(&self) -> bool {
        self.long_out.is_some()
    }

    pub fn has_short(&self) -> bool {
        self.short_out.is_some()
    }

    /// Sends a complete report (report ID at byte 0).
    pub fn write(&self, report: &[u8]) -> Result<(), IOReturn> {
        let el = match report[0] {
            REPORT_LONG => self.long_out,
            _ => self.short_out,
        };
        let res = match (self.send_method, el) {
            (SendMethod::SetValue, Some(el)) => unsafe {
                let body = &report[1..];
                let value = IOHIDValueCreateWithBytes(
                    kCFAllocatorDefault,
                    el,
                    0,
                    body.as_ptr(),
                    body.len() as _,
                );
                if value.is_null() {
                    return Err(-1);
                }
                let r = IOHIDDeviceSetValue(self.raw, el, value);
                CFRelease(value as CFTypeRef);
                r
            },
            _ => unsafe {
                IOHIDDeviceSetReport(
                    self.raw,
                    kIOHIDReportTypeOutput,
                    report[0] as CFIndex,
                    report.as_ptr(),
                    report.len() as _,
                )
            },
        };
        if res == kIOReturnSuccess {
            Ok(())
        } else {
            Err(res)
        }
    }

    pub fn pop_report(&self) -> Option<Vec<u8>> {
        self.inbox.borrow_mut().pop_front()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            if self.opened {
                IOHIDDeviceRegisterInputReportCallback(
                    self.raw,
                    self.buf.as_mut_ptr(),
                    64,
                    None,
                    std::ptr::null_mut(),
                );
                IOHIDDeviceUnscheduleFromRunLoop(
                    self.raw,
                    CFRunLoopGetCurrent(),
                    kCFRunLoopDefaultMode,
                );
                IOHIDDeviceClose(self.raw, kIOHIDOptionsTypeNone);
            }
            for el in [self.long_out, self.short_out].into_iter().flatten() {
                CFRelease(el as CFTypeRef);
            }
            CFRelease(self.raw as CFTypeRef);
        }
    }
}

pub fn release(raw: IOHIDDeviceRef) {
    unsafe { CFRelease(raw as CFTypeRef) }
}
