// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! FFI bindings to libcarrier (Jami-backed) and CarrierEvent → Turtle
//! serialization for the v0.2 vocabulary.

use anyhow::{bail, Result};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::mpsc::Sender;
use std::time::Duration;

const CARRIER_URI_LEN: usize = 128;
const CARRIER_ACCOUNT_ID_LEN: usize = 64;
const CARRIER_CONVERSATION_ID_LEN: usize = 64;
const CARRIER_NAME_LEN: usize = 128;
const CARRIER_LOG_TAG_LEN: usize = 16;
const CARRIER_LOG_MESSAGE_LEN: usize = 512;
const CARRIER_ERROR_FIELD_LEN: usize = 64;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CarrierEventType {
    Connected = 0,
    Disconnected,
    AccountReady,
    AccountError,
    SelfId,
    TrustRequest,
    ContactOnline,
    ContactOffline,
    ContactName,
    TextMessage,
    MessageSent,
    Error,
    System,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct AccountReadyData {
    pub self_uri: [u8; CARRIER_URI_LEN],
    pub display_name: [u8; CARRIER_NAME_LEN],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct AccountErrorData {
    pub cause: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct SelfIdData {
    pub self_uri: [u8; CARRIER_URI_LEN],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct TrustRequestData {
    pub from_uri: [u8; CARRIER_URI_LEN],
    pub payload: *const c_char,
    pub payload_len: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ContactOnlineData {
    pub contact_uri: [u8; CARRIER_URI_LEN],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ContactOfflineData {
    pub contact_uri: [u8; CARRIER_URI_LEN],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ContactNameData {
    pub contact_uri: [u8; CARRIER_URI_LEN],
    pub display_name: [u8; CARRIER_NAME_LEN],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct TextMessageData {
    pub contact_uri: [u8; CARRIER_URI_LEN],
    pub conversation_id: [u8; CARRIER_CONVERSATION_ID_LEN],
    pub message_id: u64,
    pub text: *const c_char,
    pub text_len: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct MessageSentData {
    pub contact_uri: [u8; CARRIER_URI_LEN],
    pub conversation_id: [u8; CARRIER_CONVERSATION_ID_LEN],
    pub message_id: u64,
    pub status: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ErrorData {
    pub command: [u8; CARRIER_ERROR_FIELD_LEN],
    pub class_: [u8; CARRIER_ERROR_FIELD_LEN],
    pub text: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct SystemData {
    pub text: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union CarrierEventData {
    pub account_ready: AccountReadyData,
    pub account_error: AccountErrorData,
    pub self_id: SelfIdData,
    pub trust_request: TrustRequestData,
    pub contact_online: ContactOnlineData,
    pub contact_offline: ContactOfflineData,
    pub contact_name: ContactNameData,
    pub text_message: TextMessageData,
    pub message_sent: MessageSentData,
    pub error: ErrorData,
    pub system: SystemData,
}

#[repr(C)]
pub struct CarrierEvent {
    pub type_: CarrierEventType,
    pub timestamp: i64,
    pub account_id: [u8; CARRIER_ACCOUNT_ID_LEN],
    pub data: CarrierEventData,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CarrierLogLevel {
    Error = 0,
    Warn,
    Info,
    Debug,
}

const CARRIER_LOG_ERROR: c_int = 0;
const CARRIER_LOG_WARN: c_int = 1;
const CARRIER_LOG_INFO: c_int = 2;
const CARRIER_LOG_DEBUG: c_int = 3;

#[repr(C)]
pub struct CarrierLogRecord {
    pub level: c_int,
    pub timestamp_ms: i64,
    pub tag: [u8; CARRIER_LOG_TAG_LEN],
    pub message: [u8; CARRIER_LOG_MESSAGE_LEN],
}

#[repr(C)]
pub struct Carrier {
    _opaque: [u8; 0],
}

pub type CarrierEventCb =
    unsafe extern "C" fn(event: *const CarrierEvent, userdata: *mut c_void);
pub type CarrierLogCb =
    unsafe extern "C" fn(record: *const CarrierLogRecord, userdata: *mut c_void);

extern "C" {
    fn carrier_new(
        data_dir: *const c_char,
        log_cb: Option<CarrierLogCb>,
        log_userdata: *mut c_void,
    ) -> *mut Carrier;
    fn carrier_free(c: *mut Carrier);
    fn carrier_iterate(c: *mut Carrier) -> c_int;
    fn carrier_iteration_interval(c: *mut Carrier) -> c_int;
    fn carrier_clock_fd(c: *mut Carrier) -> c_int;
    fn carrier_set_event_callback(c: *mut Carrier, cb: CarrierEventCb, userdata: *mut c_void);
    fn carrier_set_log_callback(c: *mut Carrier, cb: Option<CarrierLogCb>, userdata: *mut c_void);
    fn carrier_set_log_level(c: *mut Carrier, level: c_int);
    fn carrier_create_account(
        c: *mut Carrier,
        display_name: *const c_char,
        out_account_id: *mut c_char,
    ) -> c_int;
    fn carrier_load_account(c: *mut Carrier, account_id: *const c_char) -> c_int;
    fn carrier_get_id(c: *mut Carrier, account_id: *const c_char) -> c_int;
    fn carrier_set_nick(
        c: *mut Carrier,
        account_id: *const c_char,
        nick: *const c_char,
    ) -> c_int;
    fn carrier_send_trust_request(
        c: *mut Carrier,
        account_id: *const c_char,
        contact_uri: *const c_char,
        message: *const c_char,
    ) -> c_int;
    fn carrier_accept_trust_request(
        c: *mut Carrier,
        account_id: *const c_char,
        contact_uri: *const c_char,
    ) -> c_int;
    fn carrier_discard_trust_request(
        c: *mut Carrier,
        account_id: *const c_char,
        contact_uri: *const c_char,
    ) -> c_int;
    fn carrier_remove_contact(
        c: *mut Carrier,
        account_id: *const c_char,
        contact_uri: *const c_char,
    ) -> c_int;
    fn carrier_send_message(
        c: *mut Carrier,
        account_id: *const c_char,
        contact_uri: *const c_char,
        text: *const c_char,
    ) -> c_int;
}

// ---------------------------------------------------------------------------
// Carrier → tracing bridge
//
// libcarrier emits two static tags ("JAMI", "SHIM"). Anything else falls
// through to "CARRIER". tracing's `target:` argument must be a string
// literal at the call site, so we explode known tags into one branch each.
// ---------------------------------------------------------------------------

macro_rules! carrier_emit_at_level {
    ($level:expr, $target:literal, $msg:expr) => {
        match $level {
            CARRIER_LOG_ERROR => tracing::error!(target: $target, "{}", $msg),
            CARRIER_LOG_WARN => tracing::warn!(target: $target, "{}", $msg),
            CARRIER_LOG_INFO => tracing::info!(target: $target, "{}", $msg),
            CARRIER_LOG_DEBUG => tracing::debug!(target: $target, "{}", $msg),
            _ => tracing::info!(target: $target, "{}", $msg),
        }
    };
}

/// # Safety
///
/// Called by libcarrier on the iterate thread with a record valid for the
/// duration of the call. `userdata` is unused.
unsafe extern "C" fn log_callback(record: *const CarrierLogRecord, _userdata: *mut c_void) {
    if record.is_null() {
        return;
    }
    // SAFETY: caller guarantees record points at a live CarrierLogRecord.
    let rec = &*record;

    let tag = cstr_from_buf(&rec.tag);
    let message = String::from_utf8_lossy(
        &rec.message[..rec
            .message
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(rec.message.len())],
    );

    match tag {
        "JAMI" => carrier_emit_at_level!(rec.level, "JAMI", message),
        "SHIM" => carrier_emit_at_level!(rec.level, "SHIM", message),
        "DISPATCH" => carrier_emit_at_level!(rec.level, "DISPATCH", message),
        "SPARQL" => carrier_emit_at_level!(rec.level, "SPARQL", message),
        "PIPELINE" => carrier_emit_at_level!(rec.level, "PIPELINE", message),
        "SCRIPT" => carrier_emit_at_level!(rec.level, "SCRIPT", message),
        "CHANNEL" => carrier_emit_at_level!(rec.level, "CHANNEL", message),
        "LLM" => carrier_emit_at_level!(rec.level, "LLM", message),
        "WS" => carrier_emit_at_level!(rec.level, "WS", message),
        "STATION" => carrier_emit_at_level!(rec.level, "STATION", message),
        _ => carrier_emit_at_level!(rec.level, "CARRIER", message),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cstr_from_buf(buf: &[u8]) -> &str {
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn turtle_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn format_timestamp(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let days_since_epoch = secs / 86400;
    let time_of_day = secs.rem_euclid(86400);
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let mut y = 1970i64;
    let mut remaining = days_since_epoch;
    loop {
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days_in_months: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0usize;
    for (i, &d) in days_in_months.iter().enumerate() {
        if remaining < d {
            m = i;
            break;
        }
        remaining -= d;
    }
    let day = remaining + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        y,
        m + 1,
        day,
        hours,
        minutes,
        seconds
    )
}

/// Render an event header: `[] a carrier:<Type> ; carrier:account "<id>"` (or
/// just the type for events without account scoping).
fn header(class: &str, account_id: &[u8]) -> String {
    let acct = cstr_from_buf(account_id);
    if acct.is_empty() {
        format!("[] a carrier:{class}")
    } else {
        format!(
            "[] a carrier:{class} ; carrier:account \"{}\"",
            turtle_escape(acct)
        )
    }
}

unsafe fn cstr_to_string(p: *const c_char, fallback: &str) -> String {
    if p.is_null() {
        return fallback.to_string();
    }
    // SAFETY: caller asserts pointer is a NUL-terminated C string valid for
    // the duration of the call.
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

unsafe fn bytes_to_string(p: *const c_char, len: usize) -> String {
    if p.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: caller asserts (p, len) is a valid byte slice for the call.
    let slice = std::slice::from_raw_parts(p as *const u8, len);
    String::from_utf8_lossy(slice).into_owned()
}

// ---------------------------------------------------------------------------
// Event → Turtle (mirrors carrier/src/turtle_emit.c at the wire layer)
// ---------------------------------------------------------------------------

pub fn event_to_turtle(ev: &CarrierEvent) -> Option<String> {
    let ts = format!(
        " ; carrier:at \"{}\"^^xsd:dateTime",
        format_timestamp(ev.timestamp)
    );

    // SAFETY: the union variant matches ev.type_ by C-side contract.
    let line = unsafe {
        match ev.type_ {
            CarrierEventType::Connected => format!("{}{} .", header("Connected", &ev.account_id), ts),
            CarrierEventType::Disconnected => {
                format!("{}{} .", header("Disconnected", &ev.account_id), ts)
            }
            CarrierEventType::AccountReady => {
                let d = ev.data.account_ready;
                let mut s = header("AccountReady", &ev.account_id);
                s.push_str(&format!(
                    " ; carrier:selfUri \"{}\"",
                    turtle_escape(cstr_from_buf(&d.self_uri))
                ));
                let dn = cstr_from_buf(&d.display_name);
                if !dn.is_empty() {
                    s.push_str(&format!(
                        " ; carrier:displayName \"{}\"",
                        turtle_escape(dn)
                    ));
                }
                s.push_str(&ts);
                s.push_str(" .");
                s
            }
            CarrierEventType::AccountError => {
                let d = ev.data.account_error;
                let cause = cstr_to_string(d.cause, "");
                format!(
                    "{} ; carrier:cause \"{}\"{} .",
                    header("AccountError", &ev.account_id),
                    turtle_escape(&cause),
                    ts
                )
            }
            CarrierEventType::SelfId => {
                let d = ev.data.self_id;
                format!(
                    "{} ; carrier:selfUri \"{}\"{} .",
                    header("SelfId", &ev.account_id),
                    turtle_escape(cstr_from_buf(&d.self_uri)),
                    ts
                )
            }
            CarrierEventType::TrustRequest => {
                let d = ev.data.trust_request;
                let mut s = header("TrustRequest", &ev.account_id);
                s.push_str(&format!(
                    " ; carrier:contactUri \"{}\"",
                    turtle_escape(cstr_from_buf(&d.from_uri))
                ));
                if !d.payload.is_null() && d.payload_len > 0 {
                    let p = bytes_to_string(d.payload, d.payload_len);
                    s.push_str(&format!(" ; carrier:payload \"{}\"", turtle_escape(&p)));
                }
                s.push_str(&ts);
                s.push_str(" .");
                s
            }
            CarrierEventType::ContactOnline => {
                let d = ev.data.contact_online;
                format!(
                    "{} ; carrier:contactUri \"{}\"{} .",
                    header("ContactOnline", &ev.account_id),
                    turtle_escape(cstr_from_buf(&d.contact_uri)),
                    ts
                )
            }
            CarrierEventType::ContactOffline => {
                let d = ev.data.contact_offline;
                format!(
                    "{} ; carrier:contactUri \"{}\"{} .",
                    header("ContactOffline", &ev.account_id),
                    turtle_escape(cstr_from_buf(&d.contact_uri)),
                    ts
                )
            }
            CarrierEventType::ContactName => {
                let d = ev.data.contact_name;
                format!(
                    "{} ; carrier:contactUri \"{}\" ; carrier:displayName \"{}\"{} .",
                    header("ContactName", &ev.account_id),
                    turtle_escape(cstr_from_buf(&d.contact_uri)),
                    turtle_escape(cstr_from_buf(&d.display_name)),
                    ts
                )
            }
            CarrierEventType::TextMessage => {
                let d = ev.data.text_message;
                let body = bytes_to_string(d.text, d.text_len);
                format!(
                    "{} ; carrier:contactUri \"{}\" ; carrier:conversationId \"{}\" ; carrier:messageId {} ; carrier:text \"{}\"{} .",
                    header("TextMessage", &ev.account_id),
                    turtle_escape(cstr_from_buf(&d.contact_uri)),
                    turtle_escape(cstr_from_buf(&d.conversation_id)),
                    d.message_id,
                    turtle_escape(&body),
                    ts
                )
            }
            CarrierEventType::MessageSent => {
                let d = ev.data.message_sent;
                format!(
                    "{} ; carrier:contactUri \"{}\" ; carrier:conversationId \"{}\" ; carrier:messageId {} ; carrier:status {}{} .",
                    header("MessageSent", &ev.account_id),
                    turtle_escape(cstr_from_buf(&d.contact_uri)),
                    turtle_escape(cstr_from_buf(&d.conversation_id)),
                    d.message_id,
                    d.status,
                    ts
                )
            }
            CarrierEventType::Error => {
                let d = ev.data.error;
                let text = cstr_to_string(d.text, "");
                format!(
                    "{} ; carrier:command \"{}\" ; carrier:class \"{}\" ; carrier:message \"{}\"{} .",
                    header("Error", &ev.account_id),
                    turtle_escape(cstr_from_buf(&d.command)),
                    turtle_escape(cstr_from_buf(&d.class_)),
                    turtle_escape(&text),
                    ts
                )
            }
            CarrierEventType::System => {
                let d = ev.data.system;
                let text = cstr_to_string(d.text, "");
                format!(
                    "{} ; carrier:message \"{}\"{} .",
                    header("System", &ev.account_id),
                    turtle_escape(&text),
                    ts
                )
            }
        }
    };

    Some(line)
}

// ---------------------------------------------------------------------------
// C event callback — serializes and forwards to the channel
// ---------------------------------------------------------------------------

/// # Safety
///
/// Called by libcarrier on the iterate thread. `userdata` must be the
/// `*const Sender<String>` passed to `Carrier::new`; the Box keeps it alive.
unsafe extern "C" fn event_callback(event: *const CarrierEvent, userdata: *mut c_void) {
    if event.is_null() || userdata.is_null() {
        return;
    }
    // SAFETY: userdata was set to a Box<Sender<String>> in Carrier::new and
    // outlives the Carrier handle.
    let sender = &*(userdata as *const Sender<String>);
    if let Some(turtle) = event_to_turtle(&*event) {
        let _ = sender.send(turtle);
    }
}

// ---------------------------------------------------------------------------
// Safe wrapper
// ---------------------------------------------------------------------------

pub struct CarrierClient {
    ptr: *mut Carrier,
    _sender: Box<Sender<String>>,
    iterate_thread: std::sync::OnceLock<std::thread::ThreadId>,
}

// SAFETY: Carrier is single-threaded for iterate(). Send is needed to move
// the wrapper into AntennaContext after construction; the OnceLock asserts
// that all subsequent iterate() calls come from the same thread.
unsafe impl Send for CarrierClient {}

impl CarrierClient {
    /// Construct a Carrier instance backed by `data_dir`. The instance
    /// holds no account yet; call `create_account` or `load_account`.
    pub fn new(data_dir: &str, sender: Sender<String>) -> Result<Self> {
        let dir_c = CString::new(data_dir)?;
        let ptr = unsafe {
            carrier_new(dir_c.as_ptr(), Some(log_callback), std::ptr::null_mut())
        };
        if ptr.is_null() {
            bail!("carrier_new returned NULL");
        }

        // Default level after construction is ERROR; widen to DEBUG and let
        // tracing's EnvFilter do the actual filtering.
        unsafe { carrier_set_log_level(ptr, CARRIER_LOG_DEBUG) };

        let sender_box = Box::new(sender);
        let sender_ptr = &*sender_box as *const Sender<String> as *mut c_void;

        unsafe {
            carrier_set_event_callback(ptr, event_callback, sender_ptr);
        }

        Ok(Self {
            ptr,
            _sender: sender_box,
            iterate_thread: std::sync::OnceLock::new(),
        })
    }

    pub fn iterate(&self) -> Result<()> {
        let current = std::thread::current().id();
        let owner = self.iterate_thread.get_or_init(|| current);
        debug_assert_eq!(*owner, current, "CarrierClient::iterate() from wrong thread");

        let rc = unsafe { carrier_iterate(self.ptr) };
        if rc < 0 {
            bail!("carrier_iterate returned {}", rc);
        }
        Ok(())
    }

    pub fn iteration_interval(&self) -> Duration {
        let ms = unsafe { carrier_iteration_interval(self.ptr) };
        Duration::from_millis(ms.max(1) as u64)
    }

    pub fn clock_fd(&self) -> Option<c_int> {
        let fd = unsafe { carrier_clock_fd(self.ptr) };
        if fd < 0 {
            None
        } else {
            Some(fd)
        }
    }

    pub fn create_account(&self, display_name: Option<&str>) -> Result<String> {
        let name_c = display_name.map(CString::new).transpose()?;
        let name_ptr = name_c.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
        let mut buf = [0u8; CARRIER_ACCOUNT_ID_LEN];
        let rc = unsafe {
            carrier_create_account(self.ptr, name_ptr, buf.as_mut_ptr() as *mut c_char)
        };
        if rc < 0 {
            bail!("carrier_create_account failed: {}", rc);
        }
        Ok(cstr_from_buf(&buf).to_string())
    }

    pub fn load_account(&self, account_id: &str) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let rc = unsafe { carrier_load_account(self.ptr, id_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_load_account failed: {}", rc);
        }
        Ok(())
    }

    pub fn get_id(&self, account_id: &str) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let rc = unsafe { carrier_get_id(self.ptr, id_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_get_id failed: {}", rc);
        }
        Ok(())
    }

    pub fn set_nick(&self, account_id: &str, nick: &str) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let nick_c = CString::new(nick)?;
        let rc = unsafe { carrier_set_nick(self.ptr, id_c.as_ptr(), nick_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_set_nick failed: {}", rc);
        }
        Ok(())
    }

    pub fn send_trust_request(
        &self,
        account_id: &str,
        contact_uri: &str,
        message: Option<&str>,
    ) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let uri_c = CString::new(contact_uri)?;
        let msg_c = message.map(CString::new).transpose()?;
        let msg_ptr = msg_c.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
        let rc = unsafe {
            carrier_send_trust_request(self.ptr, id_c.as_ptr(), uri_c.as_ptr(), msg_ptr)
        };
        if rc < 0 {
            bail!("carrier_send_trust_request failed: {}", rc);
        }
        Ok(())
    }

    pub fn accept_trust_request(&self, account_id: &str, contact_uri: &str) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let uri_c = CString::new(contact_uri)?;
        let rc = unsafe {
            carrier_accept_trust_request(self.ptr, id_c.as_ptr(), uri_c.as_ptr())
        };
        if rc < 0 {
            bail!("carrier_accept_trust_request failed: {}", rc);
        }
        Ok(())
    }

    pub fn discard_trust_request(&self, account_id: &str, contact_uri: &str) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let uri_c = CString::new(contact_uri)?;
        let rc = unsafe {
            carrier_discard_trust_request(self.ptr, id_c.as_ptr(), uri_c.as_ptr())
        };
        if rc < 0 {
            bail!("carrier_discard_trust_request failed: {}", rc);
        }
        Ok(())
    }

    pub fn remove_contact(&self, account_id: &str, contact_uri: &str) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let uri_c = CString::new(contact_uri)?;
        let rc = unsafe { carrier_remove_contact(self.ptr, id_c.as_ptr(), uri_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_remove_contact failed: {}", rc);
        }
        Ok(())
    }

    pub fn send_message(&self, account_id: &str, contact_uri: &str, text: &str) -> Result<()> {
        let id_c = CString::new(account_id)?;
        let uri_c = CString::new(contact_uri)?;
        let text_c = CString::new(text)?;
        let rc = unsafe {
            carrier_send_message(self.ptr, id_c.as_ptr(), uri_c.as_ptr(), text_c.as_ptr())
        };
        if rc < 0 {
            bail!("carrier_send_message failed: {}", rc);
        }
        Ok(())
    }
}

impl Drop for CarrierClient {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: ptr was obtained from carrier_new and not freed elsewhere.
            unsafe {
                carrier_set_event_callback(
                    self.ptr,
                    placeholder_event_cb,
                    std::ptr::null_mut(),
                );
                carrier_set_log_callback(self.ptr, None, std::ptr::null_mut());
                carrier_free(self.ptr);
            }
        }
    }
}

unsafe extern "C" fn placeholder_event_cb(_event: *const CarrierEvent, _userdata: *mut c_void) {}

/// Standard Turtle prefixes prepended to every statement for parsing.
pub const TURTLE_PREFIXES: &str = "\
@prefix carrier: <http://resonator.network/v2/carrier#> .\n\
@prefix antenna: <http://resonator.network/v2/antenna#> .\n\
@prefix sp: <http://spinrdf.org/sp#> .\n\
@prefix spin: <http://spinrdf.org/spin#> .\n\
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .";
