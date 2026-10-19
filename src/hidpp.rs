//! HID++ framing and synchronous request/response over a [`Device`].

use crate::cf;
use crate::hid::{Device, LONG_LEN, REPORT_LONG, REPORT_SHORT, SHORT_LEN};
use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

pub const DIRECT_INDEX: u8 = 0xFF;

pub const FEAT_ROOT: u16 = 0x0000;
pub const FEAT_FEATURE_SET: u16 = 0x0001;
pub const FEAT_DEVICE_NAME: u16 = 0x0005;
pub const FEAT_WIRELESS_STATUS: u16 = 0x1D4B;
pub const FEAT_REPROG_V4: u16 = 0x1B04;
pub const FEAT_SMART_SHIFT: u16 = 0x2110;
pub const FEAT_SMART_SHIFT_ENH: u16 = 0x2111;
pub const FEAT_HIRES_WHEEL: u16 = 0x2121;
pub const FEAT_THUMB_WHEEL: u16 = 0x2150;

/// HID++ 1.0 sub-IDs.
pub const SUB_SET_REGISTER: u8 = 0x80;
pub const SUB_GET_REGISTER: u8 = 0x81;
pub const SUB_ERROR_10: u8 = 0x8F;
pub const SUB_DEVICE_CONNECTION: u8 = 0x41;

#[derive(Debug, Clone)]
pub struct Msg {
    pub report_id: u8,
    pub dev: u8,
    pub feat: u8,
    pub func: u8,
    pub sw: u8,
    pub params: Vec<u8>,
}

impl Msg {
    pub fn parse(r: &[u8]) -> Option<Msg> {
        if r.len() < 4 || (r[0] != REPORT_SHORT && r[0] != REPORT_LONG) {
            return None;
        }
        Some(Msg {
            report_id: r[0],
            dev: r[1],
            feat: r[2],
            func: r[3] >> 4,
            sw: r[3] & 0x0F,
            params: r[4..].to_vec(),
        })
    }

    /// The raw address byte (function << 4 | software ID); HID++ 1.0 register number.
    pub fn addr(&self) -> u8 {
        (self.func << 4) | self.sw
    }

    pub fn is_notification(&self) -> bool {
        self.sw == 0
    }
}

impl fmt::Display for Msg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:02X} {:02X} {:02X} {:02X} | {}",
            self.report_id,
            self.dev,
            self.feat,
            self.addr(),
            hex(&self.params)
        )
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Send(i32),
    Timeout,
    /// HID++ 2.0 error code.
    Hidpp20(u8),
    /// HID++ 1.0 error code (receiver).
    Hidpp10(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Send(r) => write!(f, "send failed (IOReturn 0x{:08X})", *r as u32),
            Error::Timeout => write!(f, "timeout"),
            Error::Hidpp20(c) => write!(f, "HID++ 2.0 error 0x{c:02X} ({})", error_name(*c)),
            Error::Hidpp10(c) => write!(f, "HID++ 1.0 error 0x{c:02X}"),
        }
    }
}

pub fn error_name(code: u8) -> &'static str {
    match code {
        1 => "UNKNOWN",
        2 => "INVALID_ARGUMENT",
        3 => "OUT_OF_RANGE",
        4 => "HARDWARE_ERROR",
        5 => "LOGITECH_INTERNAL",
        6 => "INVALID_FEATURE_INDEX",
        7 => "INVALID_FUNCTION",
        8 => "BUSY",
        9 => "UNSUPPORTED",
        _ => "?",
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A HID++ conversation with one device index on one IOHIDDevice.
pub struct Link {
    pub dev: Device,
    pub index: u8,
    sw: u8,
    /// Reports that arrived while waiting for a response and did not match it.
    unsolicited: VecDeque<Msg>,
}

impl Link {
    pub fn new(dev: Device, index: u8) -> Link {
        Link {
            dev,
            index,
            sw: 1,
            unsolicited: VecDeque::new(),
        }
    }

    fn next_sw(&mut self) -> u8 {
        self.sw = if self.sw >= 0x0F { 2 } else { self.sw + 1 };
        self.sw
    }

    fn frame(&self, report_id: u8, b2: u8, b3: u8, params: &[u8]) -> Vec<u8> {
        let len = if report_id == REPORT_LONG {
            LONG_LEN
        } else {
            SHORT_LEN
        };
        let mut r = vec![0u8; len];
        r[0] = report_id;
        r[1] = self.index;
        r[2] = b2;
        r[3] = b3;
        for (i, p) in params.iter().take(len - 4).enumerate() {
            r[4 + i] = *p;
        }
        r
    }

    /// Waits for a report satisfying `matches`; everything else is stashed.
    fn wait<F>(&mut self, timeout: Duration, mut matches: F) -> Result<Msg>
    where
        F: FnMut(&Msg) -> Option<Result<()>>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(raw) = self.dev.pop_report() {
                let Some(m) = Msg::parse(&raw) else { continue };
                match matches(&m) {
                    Some(Ok(())) => return Ok(m),
                    Some(Err(e)) => return Err(e),
                    None => self.unsolicited.push_back(m),
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Timeout);
            }
            cf::run_loop_once((deadline - now).as_secs_f64().min(0.05));
        }
    }

    /// HID++ 2.0 request to feature index `feat`, function `func`.
    pub fn request(
        &mut self,
        feat: u8,
        func: u8,
        params: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let sw = self.next_sw();
        let addr = (func << 4) | sw;
        let report_id = if self.dev.has_long() {
            REPORT_LONG
        } else {
            REPORT_SHORT
        };
        let report = self.frame(report_id, feat, addr, params);
        self.dev.write(&report).map_err(Error::Send)?;
        let index = self.index;
        let m = self.wait(timeout, |m| {
            // Direct devices sometimes answer with index 0x00 instead of 0xFF.
            let dev_ok = m.dev == index || (index == DIRECT_INDEX && m.dev == 0);
            if !dev_ok {
                return None;
            }
            if m.feat == 0xFF && m.params.len() >= 3 && m.params[0] == feat && m.params[1] == addr {
                return Some(Err(Error::Hidpp20(m.params[2])));
            }
            if m.feat == SUB_ERROR_10
                && m.params.len() >= 3
                && m.params[0] == feat
                && m.params[1] == addr
            {
                return Some(Err(Error::Hidpp10(m.params[2])));
            }
            (m.feat == feat && m.addr() == addr).then_some(Ok(()))
        })?;
        Ok(m.params)
    }

    pub fn call(&mut self, feat: u8, func: u8, params: &[u8]) -> Result<Vec<u8>> {
        self.request(feat, func, params, Duration::from_millis(2000))
    }

    /// IRoot.ping. Returns the HID++ protocol version (major, minor).
    pub fn ping(&mut self, timeout: Duration) -> Result<(u8, u8)> {
        let p = self.request(0, 1, &[0, 0, 0x5A], timeout)?;
        Ok((p[0], p[1]))
    }

    /// IRoot.getFeature → (index, type, version); `None` if absent.
    pub fn feature(&mut self, id: u16) -> Result<Option<(u8, u8, u8)>> {
        let p = self.call(0, 0, &[(id >> 8) as u8, id as u8])?;
        Ok((p[0] != 0).then_some((p[0], p[1], p[2])))
    }

    pub fn feature_index(&mut self, id: u16) -> Result<Option<u8>> {
        Ok(self.feature(id)?.map(|f| f.0))
    }

    /// HID++ 1.0 register access (receivers). `sub` is SUB_GET/SET_REGISTER.
    pub fn register(
        &mut self,
        sub: u8,
        reg: u8,
        params: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let report = self.frame(REPORT_SHORT, sub, reg, params);
        self.dev.write(&report).map_err(Error::Send)?;
        let index = self.index;
        let m = self.wait(timeout, |m| {
            if m.dev != index {
                return None;
            }
            if m.feat == SUB_ERROR_10
                && m.params.len() >= 3
                && m.params[0] == sub
                && m.params[1] == reg
            {
                return Some(Err(Error::Hidpp10(m.params[2])));
            }
            (m.feat == sub && m.addr() == reg).then_some(Ok(()))
        })?;
        Ok(m.params)
    }

    /// Drains everything received outside of a request.
    pub fn poll(&mut self) -> Vec<Msg> {
        let mut out: Vec<Msg> = self.unsolicited.drain(..).collect();
        while let Some(raw) = self.dev.pop_report() {
            if let Some(m) = Msg::parse(&raw) {
                out.push(m);
            }
        }
        out
    }
}
