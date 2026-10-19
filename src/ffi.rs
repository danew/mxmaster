//! Hand-written bindings for the small slice of IOKit HID, CoreGraphics and the
//! private Dock/CGS calls the daemon uses. CoreFoundation comes from
//! `core-foundation-sys`.

#![allow(non_upper_case_globals, non_camel_case_types, dead_code)]

use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::base::{CFAllocatorRef, CFIndex, CFTypeRef};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::runloop::CFRunLoopRef;
use core_foundation_sys::set::CFSetRef;
use core_foundation_sys::string::CFStringRef;
use std::os::raw::c_void;

pub type IOReturn = i32;
pub type IOOptionBits = u32;
pub type IOHIDManagerRef = *mut c_void;
pub type IOHIDDeviceRef = *mut c_void;
pub type IOHIDElementRef = *mut c_void;
pub type IOHIDValueRef = *mut c_void;
pub type IOHIDReportType = u32;
pub type IOHIDElementType = u32;

pub const kIOReturnSuccess: IOReturn = 0;
pub const kIOReturnNotPermitted: IOReturn = 0xE00002E2u32 as i32;
pub const kIOHIDOptionsTypeNone: IOOptionBits = 0;
pub const kIOHIDReportTypeInput: IOHIDReportType = 0;
pub const kIOHIDReportTypeOutput: IOHIDReportType = 1;
pub const kIOHIDElementTypeOutput: IOHIDElementType = 129;

pub type IOHIDDeviceCallback = extern "C" fn(
    context: *mut c_void,
    result: IOReturn,
    sender: *mut c_void,
    device: IOHIDDeviceRef,
);
pub type IOHIDReportCallback = extern "C" fn(
    context: *mut c_void,
    result: IOReturn,
    sender: *mut c_void,
    report_type: IOHIDReportType,
    report_id: u32,
    report: *mut u8,
    report_length: CFIndex,
);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IOHIDManagerCreate(allocator: CFAllocatorRef, options: IOOptionBits) -> IOHIDManagerRef;
    pub fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: CFDictionaryRef);
    pub fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: IOOptionBits) -> IOReturn;
    pub fn IOHIDManagerClose(manager: IOHIDManagerRef, options: IOOptionBits) -> IOReturn;
    pub fn IOHIDManagerCopyDevices(manager: IOHIDManagerRef) -> CFSetRef;
    pub fn IOHIDManagerScheduleWithRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    pub fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDDeviceCallback,
        context: *mut c_void,
    );
    pub fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDDeviceCallback,
        context: *mut c_void,
    );

    pub fn IOHIDDeviceGetProperty(device: IOHIDDeviceRef, key: CFStringRef) -> CFTypeRef;
    pub fn IOHIDDeviceOpen(device: IOHIDDeviceRef, options: IOOptionBits) -> IOReturn;
    pub fn IOHIDDeviceClose(device: IOHIDDeviceRef, options: IOOptionBits) -> IOReturn;
    pub fn IOHIDDeviceScheduleWithRunLoop(
        device: IOHIDDeviceRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    pub fn IOHIDDeviceUnscheduleFromRunLoop(
        device: IOHIDDeviceRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    pub fn IOHIDDeviceCopyMatchingElements(
        device: IOHIDDeviceRef,
        matching: CFDictionaryRef,
        options: IOOptionBits,
    ) -> CFArrayRef;
    pub fn IOHIDDeviceRegisterInputReportCallback(
        device: IOHIDDeviceRef,
        report: *mut u8,
        report_length: CFIndex,
        callback: Option<IOHIDReportCallback>,
        context: *mut c_void,
    );
    pub fn IOHIDDeviceSetReport(
        device: IOHIDDeviceRef,
        report_type: IOHIDReportType,
        report_id: CFIndex,
        report: *const u8,
        report_length: CFIndex,
    ) -> IOReturn;
    pub fn IOHIDDeviceSetValue(
        device: IOHIDDeviceRef,
        element: IOHIDElementRef,
        value: IOHIDValueRef,
    ) -> IOReturn;

    /// Prompts for (and lists the app under) Input Monitoring; type 1 = listen.
    pub fn IOHIDRequestAccess(request_type: u32) -> bool;

    pub fn IOHIDValueCreateWithBytes(
        allocator: CFAllocatorRef,
        element: IOHIDElementRef,
        timestamp: u64,
        bytes: *const u8,
        length: CFIndex,
    ) -> IOHIDValueRef;

    pub fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    pub fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
    pub fn IOHIDElementGetType(element: IOHIDElementRef) -> IOHIDElementType;
    pub fn IOHIDElementGetReportID(element: IOHIDElementRef) -> u32;
    pub fn IOHIDElementGetReportSize(element: IOHIDElementRef) -> u32;
    pub fn IOHIDElementGetReportCount(element: IOHIDElementRef) -> u32;
}

// ── CoreGraphics ──────────────────────────────────────────────────

pub type CGEventRef = *mut c_void;
pub type CGEventSourceRef = *mut c_void;
pub type CGEventFlags = u64;
pub type CGKeyCode = u16;
pub type CGEventTapLocation = u32;
pub const kCGHIDEventTap: CGEventTapLocation = 0;
pub const kCGSessionEventTap: CGEventTapLocation = 1;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        key: CGKeyCode,
        down: bool,
    ) -> CGEventRef;
    pub fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    pub fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
    pub fn CGPreflightPostEventAccess() -> bool;
    pub fn CGRequestPostEventAccess() -> bool;
    // Private, re-exported from SkyLight; used by mxprobe to verify Space switches.
    pub fn CGSMainConnectionID() -> i32;
    pub fn CGSGetActiveSpace(cid: i32) -> u64;
}

// ── Private SkyLight / Dock calls (exported by ApplicationServices) ──

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    /// Dock notifications such as "com.apple.expose.awake" (Mission Control).
    pub fn CoreDockSendNotification(notification: CFStringRef, unknown: i32) -> i32;
    /// Looks up the user's binding for a system hotkey (79 = Space left, 81 = right).
    pub fn CGSGetSymbolicHotKeyValue(
        hotkey: u32,
        key_equivalent: *mut u16,
        virtual_key: *mut u16,
        modifiers: *mut u32,
    ) -> i32;
    pub fn CGSIsSymbolicHotKeyEnabled(hotkey: u32) -> bool;
    pub fn CGSSetSymbolicHotKeyEnabled(hotkey: u32, enabled: bool) -> i32;
}

// ── IOKit power management (sleep/wake notifications) ─────────────

pub type io_connect_t = u32;
pub type io_object_t = u32;
pub type IONotificationPortRef = *mut c_void;
pub type IOServiceInterestCallback = extern "C" fn(
    refcon: *mut c_void,
    service: io_object_t,
    message_type: u32,
    message_argument: *mut c_void,
);

pub const kIOMessageCanSystemSleep: u32 = 0xE0000270;
pub const kIOMessageSystemWillSleep: u32 = 0xE0000280;
pub const kIOMessageSystemHasPoweredOn: u32 = 0xE0000300;

extern "C" {
    pub fn IORegisterForSystemPower(
        refcon: *mut c_void,
        port: *mut IONotificationPortRef,
        callback: IOServiceInterestCallback,
        notifier: *mut io_object_t,
    ) -> io_connect_t;
    pub fn IOAllowPowerChange(kernel_port: io_connect_t, notification_id: isize) -> IOReturn;
    pub fn IONotificationPortGetRunLoopSource(
        port: IONotificationPortRef,
    ) -> core_foundation_sys::runloop::CFRunLoopSourceRef;
}
