//! Small CoreFoundation conveniences.

use core_foundation_sys::array::{
    CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef,
};
use core_foundation_sys::base::{kCFAllocatorDefault, CFGetTypeID, CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::{
    kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreate,
    CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef,
};
use core_foundation_sys::number::{
    kCFNumberSInt64Type, CFNumberCreate, CFNumberGetTypeID, CFNumberGetValue, CFNumberRef,
};
use core_foundation_sys::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringCreateWithBytes, CFStringGetCString, CFStringGetTypeID,
    CFStringRef,
};
use std::os::raw::c_void;

/// Owned CF object, released on drop.
pub struct Owned(pub CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

pub fn string(s: &str) -> Owned {
    Owned(unsafe {
        CFStringCreateWithBytes(
            kCFAllocatorDefault,
            s.as_ptr(),
            s.len() as _,
            kCFStringEncodingUTF8,
            0,
        ) as CFTypeRef
    })
}

pub fn number(n: i64) -> Owned {
    Owned(unsafe {
        CFNumberCreate(
            kCFAllocatorDefault,
            kCFNumberSInt64Type,
            &n as *const i64 as *const c_void,
        ) as CFTypeRef
    })
}

/// Builds a CFDictionary of string keys → integer values (IOKit matching dictionaries).
pub fn int_dict(pairs: &[(&str, i64)]) -> Owned {
    let keys: Vec<Owned> = pairs.iter().map(|(k, _)| string(k)).collect();
    let vals: Vec<Owned> = pairs.iter().map(|(_, v)| number(*v)).collect();
    let kp: Vec<*const c_void> = keys.iter().map(|o| o.0).collect();
    let vp: Vec<*const c_void> = vals.iter().map(|o| o.0).collect();
    Owned(unsafe {
        CFDictionaryCreate(
            kCFAllocatorDefault,
            kp.as_ptr(),
            vp.as_ptr(),
            pairs.len() as _,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ) as CFTypeRef
    })
}

pub(crate) fn to_i64(v: CFTypeRef) -> Option<i64> {
    if v.is_null() || unsafe { CFGetTypeID(v) } != unsafe { CFNumberGetTypeID() } {
        return None;
    }
    let mut out: i64 = 0;
    let ok = unsafe {
        CFNumberGetValue(
            v as CFNumberRef,
            kCFNumberSInt64Type,
            &mut out as *mut i64 as *mut c_void,
        )
    };
    ok.then_some(out)
}

pub(crate) fn to_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || unsafe { CFGetTypeID(v) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0i8; 512];
    let ok = unsafe {
        CFStringGetCString(
            v as CFStringRef,
            buf.as_mut_ptr(),
            buf.len() as _,
            kCFStringEncodingUTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(cstr.to_string_lossy().into_owned())
}

/// Elements of a CFArray (borrowed).
pub(crate) fn array_items(v: CFTypeRef) -> Vec<CFTypeRef> {
    if v.is_null() || unsafe { CFGetTypeID(v) } != unsafe { CFArrayGetTypeID() } {
        return Vec::new();
    }
    let arr = v as CFArrayRef;
    let n = unsafe { CFArrayGetCount(arr) };
    (0..n)
        .map(|i| unsafe { CFArrayGetValueAtIndex(arr, i) })
        .collect()
}

pub(crate) fn dict_get(dict: CFTypeRef, key: &str) -> CFTypeRef {
    if dict.is_null() || unsafe { CFGetTypeID(dict) } != unsafe { CFDictionaryGetTypeID() } {
        return std::ptr::null();
    }
    let k = string(key);
    unsafe { CFDictionaryGetValue(dict as CFDictionaryRef, k.0) }
}

/// Runs the current thread's run loop once for up to `seconds`.
pub fn run_loop_once(seconds: f64) {
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 1);
    }
}
