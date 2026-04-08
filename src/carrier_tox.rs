//! FFI bindings to libcarrier and CarrierEvent → Turtle serialization.
use anyhow::{bail, Result};
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::mpsc::Sender;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Constants mirroring carrier.h
// ---------------------------------------------------------------------------

const MAX_NAME_LENGTH: usize = 128;
const MAX_MESSAGE_LENGTH: usize = 4096;
const MAX_ID_LENGTH: usize = 128;
const MAX_KEY_LENGTH: usize = 128;

// ---------------------------------------------------------------------------
// CarrierEventType — must match carrier.h enum order exactly
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CarrierEventType {
    Connected,
    Disconnected,
    SelfId,
    TextMessage,
    MessageSent,
    FriendRequest,
    FriendOnline,
    FriendOffline,
    Nick,
    Status,
    StatusMessage,
    GroupMessage,
    GroupPeerJoin,
    GroupPeerExit,
    GroupInvite,
    GroupSelfJoin,
    ConferenceMessage,
    ConferenceInvite,
    FileTransfer,
    FileProgress,
    FileComplete,
    Call,
    CallState,
    AudioFrame,
    VideoFrame,
    Pipe,
    PipeData,
    PipeEof,
    Error,
    System,
}

// ---------------------------------------------------------------------------
// Union variants — C-compatible layout
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ConnectedData {
    pub transport: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct SelfIdData {
    pub id: [u8; MAX_ID_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct TextMessageData {
    pub friend_id: u32,
    pub name: [u8; MAX_NAME_LENGTH],
    pub text: [u8; MAX_MESSAGE_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct MessageSentData {
    pub friend_id: u32,
    pub receipt: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct FriendRequestData {
    pub request_id: u32,
    pub key: [u8; MAX_KEY_LENGTH],
    pub message: [u8; MAX_MESSAGE_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct FriendOnlineData {
    pub friend_id: u32,
    pub name: [u8; MAX_NAME_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct FriendOfflineData {
    pub friend_id: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct NickData {
    pub friend_id: u32,
    pub name: [u8; MAX_NAME_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct StatusData {
    pub friend_id: u32,
    pub status: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct StatusMessageData {
    pub friend_id: u32,
    pub text: [u8; MAX_MESSAGE_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct GroupMessageData {
    pub group_id: u32,
    pub peer_id: u32,
    pub name: [u8; MAX_NAME_LENGTH],
    pub text: [u8; MAX_MESSAGE_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct GroupPeerJoinData {
    pub group_id: u32,
    pub peer_id: u32,
    pub name: [u8; MAX_NAME_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct GroupPeerExitData {
    pub group_id: u32,
    pub peer_id: u32,
    pub name: [u8; MAX_NAME_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct GroupInviteData {
    pub friend_id: u32,
    pub group_id: u32,
    pub name: [u8; MAX_NAME_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct GroupSelfJoinData {
    pub group_id: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct FileTransferData {
    pub friend_id: u32,
    pub file_id: u32,
    pub file_size: u64,
    pub filename: [u8; MAX_NAME_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct FileProgressData {
    pub friend_id: u32,
    pub file_id: u32,
    pub progress: f64,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct FileCompleteData {
    pub friend_id: u32,
    pub file_id: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct CallData {
    pub friend_id: u32,
    pub audio: bool,
    pub video: bool,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct CallStateData {
    pub friend_id: u32,
    pub state: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PipeEventData {
    pub friend_id: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct SystemData {
    pub text: [u8; MAX_MESSAGE_LENGTH],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ErrorData {
    pub cmd: [u8; 64],
    pub text: [u8; MAX_MESSAGE_LENGTH],
}

// The union — we use the largest variant to ensure correct size
#[repr(C)]
#[derive(Copy, Clone)]
pub union CarrierEventData {
    pub connected: ConnectedData,
    pub self_id: SelfIdData,
    pub text_message: TextMessageData,
    pub message_sent: MessageSentData,
    pub friend_request: FriendRequestData,
    pub friend_online: FriendOnlineData,
    pub friend_offline: FriendOfflineData,
    pub nick: NickData,
    pub status: StatusData,
    pub status_message: StatusMessageData,
    pub group_message: GroupMessageData,
    pub group_peer_join: GroupPeerJoinData,
    pub group_peer_exit: GroupPeerExitData,
    pub group_invite: GroupInviteData,
    pub group_self_join: GroupSelfJoinData,
    pub file_transfer: FileTransferData,
    pub file_progress: FileProgressData,
    pub file_complete: FileCompleteData,
    pub call: CallData,
    pub call_state: CallStateData,
    pub pipe: PipeEventData,
    pub pipe_eof: PipeEventData,
    pub system: SystemData,
    pub error: ErrorData,
}

#[repr(C)]
pub struct CarrierEvent {
    pub type_: CarrierEventType,
    pub timestamp: i64,
    pub data: CarrierEventData,
}

// ---------------------------------------------------------------------------
// Opaque Carrier handle
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct Carrier {
    _opaque: [u8; 0],
}

type CarrierEventCb = unsafe extern "C" fn(event: *const CarrierEvent, userdata: *mut c_void);

// ---------------------------------------------------------------------------
// FFI declarations
// ---------------------------------------------------------------------------

extern "C" {
    fn carrier_new(
        profile_path: *const c_char,
        config_path: *const c_char,
        nodes_path: *const c_char,
    ) -> *mut Carrier;
    fn carrier_free(c: *mut Carrier);
    fn carrier_iterate(c: *mut Carrier) -> c_int;
    fn carrier_iteration_interval(c: *mut Carrier) -> c_int;
    fn carrier_set_event_callback(c: *mut Carrier, cb: CarrierEventCb, userdata: *mut c_void);
    fn carrier_send_message(c: *mut Carrier, friend_id: u32, text: *const c_char) -> c_int;
    fn carrier_get_id(c: *mut Carrier) -> c_int;
    fn carrier_set_nick(c: *mut Carrier, nick: *const c_char) -> c_int;
    fn carrier_save(c: *mut Carrier) -> c_int;
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

fn is_turtle(text: &str) -> bool {
    text.trim_start().starts_with("[] a carrier:")
}

fn format_timestamp(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    // Simple UTC formatting without pulling in chrono
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days to Y-M-D (simplified Gregorian)
    let mut y = 1970i64;
    let mut remaining = days_since_epoch;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
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

// ---------------------------------------------------------------------------
// Event → Turtle serialization (mirrors turtle_emit.c)
// ---------------------------------------------------------------------------

pub fn event_to_turtle(ev: &CarrierEvent) -> Option<String> {
    let ts = format!(
        " ; carrier:at \"{}\"^^xsd:dateTime",
        format_timestamp(ev.timestamp)
    );

    // SAFETY: ev is a valid reference to a CarrierEvent. Accessing the union
    // fields is safe because we match on ev.type_ which determines which
    // variant is active — this mirrors the C API contract where the type tag
    // indicates which union member is valid. All accessed fields are Copy
    // types or fixed-size byte arrays.
    let line = unsafe {
        match ev.type_ {
            CarrierEventType::Connected => {
                let transport = if ev.data.connected.transport == 1 {
                    "TCP"
                } else {
                    "UDP"
                };
                format!(
                    "[] a carrier:Connected ; carrier:transport \"{}\"{} .",
                    transport, ts
                )
            }
            CarrierEventType::Disconnected => {
                format!("[] a carrier:Disconnected{} .", ts)
            }
            CarrierEventType::SelfId => {
                let id = cstr_from_buf(&ev.data.self_id.id);
                format!("[] a carrier:SelfId ; carrier:id \"{}\" .", id)
            }
            CarrierEventType::TextMessage => {
                let d = &ev.data.text_message;
                let text = cstr_from_buf(&d.text);
                let name = cstr_from_buf(&d.name);

                if is_turtle(text) {
                    // Passthrough: strip trailing dot, append metadata
                    let stripped = text.trim_end().trim_end_matches('.');
                    format!(
                        "{} ; carrier:friendId {} ; carrier:name \"{}\"{} .",
                        stripped,
                        d.friend_id,
                        turtle_escape(name),
                        ts
                    )
                } else {
                    format!(
                        "[] a carrier:TextMessage ; carrier:friendId {} ; carrier:name \"{}\" ; carrier:text \"{}\"{} .",
                        d.friend_id,
                        turtle_escape(name),
                        turtle_escape(text),
                        ts
                    )
                }
            }
            CarrierEventType::MessageSent => {
                let d = &ev.data.message_sent;
                format!(
                    "[] a carrier:MessageSent ; carrier:friendId {} ; carrier:receipt {} .",
                    d.friend_id, d.receipt
                )
            }
            CarrierEventType::FriendRequest => {
                let d = &ev.data.friend_request;
                let key = cstr_from_buf(&d.key);
                let message = cstr_from_buf(&d.message);

                if is_turtle(message) {
                    let stripped = message.trim_end().trim_end_matches('.');
                    format!(
                        "{} ; carrier:requestId {} ; carrier:key \"{}\" .",
                        stripped,
                        d.request_id,
                        turtle_escape(key)
                    )
                } else {
                    format!(
                        "[] a carrier:FriendRequest ; carrier:requestId {} ; carrier:key \"{}\" ; carrier:message \"{}\" .",
                        d.request_id,
                        turtle_escape(key),
                        turtle_escape(message)
                    )
                }
            }
            CarrierEventType::FriendOnline => {
                let d = &ev.data.friend_online;
                let name = cstr_from_buf(&d.name);
                format!(
                    "[] a carrier:FriendOnline ; carrier:friendId {} ; carrier:name \"{}\" .",
                    d.friend_id,
                    turtle_escape(name)
                )
            }
            CarrierEventType::FriendOffline => {
                format!(
                    "[] a carrier:FriendOffline ; carrier:friendId {} .",
                    ev.data.friend_offline.friend_id
                )
            }
            CarrierEventType::Nick => {
                let d = &ev.data.nick;
                let name = cstr_from_buf(&d.name);

                if is_turtle(name) {
                    let stripped = name.trim_end().trim_end_matches('.');
                    format!("{} ; carrier:friendId {} .", stripped, d.friend_id)
                } else {
                    format!(
                        "[] a carrier:Nick ; carrier:friendId {} ; carrier:nick \"{}\" .",
                        d.friend_id,
                        turtle_escape(name)
                    )
                }
            }
            CarrierEventType::Status => {
                let d = &ev.data.status;
                format!(
                    "[] a carrier:Status ; carrier:friendId {} ; carrier:status {} .",
                    d.friend_id, d.status
                )
            }
            CarrierEventType::StatusMessage => {
                let d = &ev.data.status_message;
                let text = cstr_from_buf(&d.text);

                if is_turtle(text) {
                    let stripped = text.trim_end().trim_end_matches('.');
                    format!("{} ; carrier:friendId {} .", stripped, d.friend_id)
                } else {
                    format!(
                        "[] a carrier:StatusMessage ; carrier:friendId {} ; carrier:text \"{}\" .",
                        d.friend_id,
                        turtle_escape(text)
                    )
                }
            }
            CarrierEventType::GroupMessage => {
                let d = &ev.data.group_message;
                let name = cstr_from_buf(&d.name);
                let text = cstr_from_buf(&d.text);

                if is_turtle(text) {
                    let stripped = text.trim_end().trim_end_matches('.');
                    format!(
                        "{} ; carrier:peerId {} ; carrier:name \"{}\"{} .",
                        stripped,
                        d.peer_id,
                        turtle_escape(name),
                        ts
                    )
                } else {
                    format!(
                        "[] a carrier:GroupMessage ; carrier:groupId {} ; carrier:peerId {} ; carrier:name \"{}\" ; carrier:text \"{}\" .",
                        d.group_id,
                        d.peer_id,
                        turtle_escape(name),
                        turtle_escape(text)
                    )
                }
            }
            CarrierEventType::GroupPeerJoin => {
                let d = &ev.data.group_peer_join;
                let name = cstr_from_buf(&d.name);
                format!(
                    "[] a carrier:GroupPeerJoin ; carrier:groupId {} ; carrier:peerId {} ; carrier:name \"{}\" .",
                    d.group_id,
                    d.peer_id,
                    turtle_escape(name)
                )
            }
            CarrierEventType::GroupPeerExit => {
                let d = &ev.data.group_peer_exit;
                format!(
                    "[] a carrier:GroupPeerExit ; carrier:groupId {} ; carrier:peerId {} .",
                    d.group_id, d.peer_id
                )
            }
            CarrierEventType::GroupInvite => {
                let d = &ev.data.group_invite;
                let name = cstr_from_buf(&d.name);
                format!(
                    "[] a carrier:GroupInvite ; carrier:friendId {} ; carrier:name \"{}\" .",
                    d.friend_id,
                    turtle_escape(name)
                )
            }
            CarrierEventType::GroupSelfJoin => {
                format!(
                    "[] a carrier:GroupSelfJoin ; carrier:groupId {} .",
                    ev.data.group_self_join.group_id
                )
            }
            CarrierEventType::FileTransfer => {
                let d = &ev.data.file_transfer;
                let filename = cstr_from_buf(&d.filename);
                format!(
                    "[] a carrier:FileTransfer ; carrier:friendId {} ; carrier:fileId {} ; carrier:size {} ; carrier:filename \"{}\" .",
                    d.friend_id, d.file_id, d.file_size, turtle_escape(filename)
                )
            }
            CarrierEventType::FileProgress => {
                let d = &ev.data.file_progress;
                format!(
                    "[] a carrier:FileProgress ; carrier:friendId {} ; carrier:fileId {} ; carrier:progress {:.4} .",
                    d.friend_id, d.file_id, d.progress
                )
            }
            CarrierEventType::FileComplete => {
                let d = &ev.data.file_complete;
                format!(
                    "[] a carrier:FileComplete ; carrier:friendId {} ; carrier:fileId {} .",
                    d.friend_id, d.file_id
                )
            }
            CarrierEventType::Call => {
                let d = &ev.data.call;
                format!(
                    "[] a carrier:Call ; carrier:friendId {} ; carrier:audio {} ; carrier:video {} .",
                    d.friend_id, d.audio, d.video
                )
            }
            CarrierEventType::CallState => {
                let d = &ev.data.call_state;
                format!(
                    "[] a carrier:CallState ; carrier:friendId {} ; carrier:state {} .",
                    d.friend_id, d.state
                )
            }
            CarrierEventType::Pipe => {
                format!(
                    "[] a carrier:Pipe ; carrier:friendId {} .",
                    ev.data.pipe.friend_id
                )
            }
            CarrierEventType::PipeEof => {
                format!(
                    "[] a carrier:PipeEof ; carrier:friendId {} .",
                    ev.data.pipe_eof.friend_id
                )
            }
            CarrierEventType::Error => {
                let d = &ev.data.error;
                let cmd = cstr_from_buf(&d.cmd);
                let text = cstr_from_buf(&d.text);
                format!(
                    "[] a carrier:Error ; carrier:cmd \"{}\" ; carrier:message \"{}\" .",
                    turtle_escape(cmd),
                    turtle_escape(text)
                )
            }
            CarrierEventType::System => {
                let text = cstr_from_buf(&ev.data.system.text);
                format!(
                    "[] a carrier:System ; carrier:message \"{}\" .",
                    turtle_escape(text)
                )
            }
            // Binary events — not serializable to Turtle
            CarrierEventType::PipeData
            | CarrierEventType::AudioFrame
            | CarrierEventType::VideoFrame
            | CarrierEventType::ConferenceMessage
            | CarrierEventType::ConferenceInvite => return None,
        }
    };

    Some(line)
}

// ---------------------------------------------------------------------------
// C callback that serializes events and sends them through a channel
// ---------------------------------------------------------------------------

/// # Safety
///
/// Called by libcarrier from the same thread that calls `carrier_iterate`.
/// `event` must point to a valid `CarrierEvent`. `userdata` must be the
/// pointer we registered in `ToxCarrier::new` — a `*const Sender<String>`
/// that remains valid because `ToxCarrier._sender` (a `Box<Sender>`) is
/// kept alive for the lifetime of the carrier.
unsafe extern "C" fn event_callback(event: *const CarrierEvent, userdata: *mut c_void) {
    if event.is_null() || userdata.is_null() {
        return;
    }
    // SAFETY: userdata was set to &*sender_box in ToxCarrier::new and the
    // Box outlives the carrier (dropped in ToxCarrier::drop after carrier_free).
    let sender = &*(userdata as *const Sender<String>);
    if let Some(turtle) = event_to_turtle(&*event) {
        let _ = sender.send(turtle);
    }
}

// ---------------------------------------------------------------------------
// Safe wrapper
// ---------------------------------------------------------------------------

pub struct ToxCarrier {
    ptr: *mut Carrier,
    // Box the sender so the pointer stays stable for the C callback
    _sender: Box<Sender<String>>,
    /// Thread that owns iteration — set on first iterate(), asserted thereafter.
    iterate_thread: std::sync::OnceLock<std::thread::ThreadId>,
}

// SAFETY: Carrier is single-threaded (caller must iterate from one thread),
// but we need Send to move it into AntennaContext after construction.
// The main loop calls iterate() from a single thread, satisfying the
// single-threaded requirement of libcarrier.
unsafe impl Send for ToxCarrier {}

impl ToxCarrier {
    pub fn new(profile: &str, nodes: Option<&str>, sender: Sender<String>) -> Result<Self> {
        let profile_c = CString::new(profile)?;
        let nodes_c = nodes.map(CString::new).transpose()?;

        // SAFETY: CStrings are valid for the duration of carrier_new.
        // carrier_new returns an opaque pointer or NULL on failure.
        let ptr = unsafe {
            carrier_new(
                profile_c.as_ptr(),
                std::ptr::null(),
                nodes_c.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            )
        };

        if ptr.is_null() {
            bail!("carrier_new returned NULL");
        }

        // SAFETY: We box the sender and pass a raw pointer to the callback.
        // The Box is stored in `_sender` and outlives `ptr` (carrier_free is
        // called in Drop before _sender is dropped).
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
        // Ensure single-threaded access: set owner on first call, assert on subsequent.
        let current = std::thread::current().id();
        let owner = self.iterate_thread.get_or_init(|| current);
        debug_assert_eq!(
            *owner, current,
            "ToxCarrier::iterate() called from wrong thread"
        );

        // SAFETY: self.ptr is a valid carrier handle (checked non-null in new()).
        let rc = unsafe { carrier_iterate(self.ptr) };
        if rc < 0 {
            bail!("carrier_iterate returned {}", rc);
        }
        Ok(())
    }

    pub fn iteration_interval(&self) -> Duration {
        // SAFETY: self.ptr is valid (see iterate).
        let ms = unsafe { carrier_iteration_interval(self.ptr) };
        Duration::from_millis(ms.max(1) as u64)
    }

    pub fn send_message(&self, friend_id: u32, text: &str) -> Result<()> {
        let text_c = CString::new(text)?;
        // SAFETY: self.ptr is valid; text_c is a valid null-terminated string.
        let rc = unsafe { carrier_send_message(self.ptr, friend_id, text_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_send_message failed: {}", rc);
        }
        Ok(())
    }

    pub fn get_id(&self) -> Result<()> {
        // SAFETY: self.ptr is valid.
        let rc = unsafe { carrier_get_id(self.ptr) };
        if rc < 0 {
            bail!("carrier_get_id failed: {}", rc);
        }
        Ok(())
    }

    pub fn set_nick(&self, nick: &str) -> Result<()> {
        let nick_c = CString::new(nick)?;
        // SAFETY: self.ptr is valid; nick_c is a valid null-terminated string.
        let rc = unsafe { carrier_set_nick(self.ptr, nick_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_set_nick failed: {}", rc);
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        // SAFETY: self.ptr is valid.
        let rc = unsafe { carrier_save(self.ptr) };
        if rc < 0 {
            bail!("carrier_save failed: {}", rc);
        }
        Ok(())
    }
}

impl Drop for ToxCarrier {
    fn drop(&mut self) {
        // SAFETY: carrier_free is called exactly once. After this, no more
        // callbacks will fire, so _sender (dropped after this) is safe to free.
        if !self.ptr.is_null() {
            unsafe { carrier_free(self.ptr) };
        }
    }
}

/// Standard Turtle prefixes prepended to every statement for parsing.
pub const TURTLE_PREFIXES: &str = "\
@prefix carrier: <http://resonator.network/v2/carrier#> .\n\
@prefix tox: <http://resonator.network/v2/carrier#> .\n\
@prefix antenna: <http://resonator.network/v2/antenna#> .\n\
@prefix sp: <http://spinrdf.org/sp#> .\n\
@prefix spin: <http://spinrdf.org/spin#> .\n\
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn set_buf(buf: &mut [u8], s: &str) {
        let bytes = s.as_bytes();
        buf[..bytes.len()].copy_from_slice(bytes);
        buf[bytes.len()] = 0;
    }

    fn make_event(type_: CarrierEventType, data: CarrierEventData) -> CarrierEvent {
        CarrierEvent {
            type_,
            timestamp: 1000,
            data,
        }
    }

    // -- event_to_turtle tests ------------------------------------------------

    #[test]
    fn event_to_turtle_connected_udp() {
        let data = CarrierEventData {
            connected: ConnectedData { transport: 0 },
        };
        let ev = make_event(CarrierEventType::Connected, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:Connected"));
        assert!(turtle.contains("carrier:transport \"UDP\""));
        assert!(turtle.contains("carrier:at \"1970-01-01T00:00:01\""));
    }

    #[test]
    fn event_to_turtle_connected_tcp() {
        let data = CarrierEventData {
            connected: ConnectedData { transport: 1 },
        };
        let ev = make_event(CarrierEventType::Connected, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("carrier:transport \"TCP\""));
    }

    #[test]
    fn event_to_turtle_disconnected() {
        let data = CarrierEventData {
            connected: ConnectedData { transport: 0 },
        };
        let ev = make_event(CarrierEventType::Disconnected, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:Disconnected"));
        assert!(turtle.ends_with('.'));
    }

    #[test]
    fn event_to_turtle_self_id() {
        let mut id_data = SelfIdData {
            id: [0u8; MAX_ID_LENGTH],
        };
        set_buf(&mut id_data.id, "ABC123DEF456");
        let data = CarrierEventData { self_id: id_data };
        let ev = make_event(CarrierEventType::SelfId, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:SelfId"));
        assert!(turtle.contains("carrier:id \"ABC123DEF456\""));
    }

    #[test]
    fn event_to_turtle_text_message() {
        let mut msg = TextMessageData {
            friend_id: 7,
            name: [0u8; MAX_NAME_LENGTH],
            text: [0u8; MAX_MESSAGE_LENGTH],
        };
        set_buf(&mut msg.name, "Alice");
        set_buf(&mut msg.text, "Hello world");
        let data = CarrierEventData { text_message: msg };
        let ev = make_event(CarrierEventType::TextMessage, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:TextMessage"));
        assert!(turtle.contains("carrier:friendId 7"));
        assert!(turtle.contains("carrier:name \"Alice\""));
        assert!(turtle.contains("carrier:text \"Hello world\""));
    }

    #[test]
    fn event_to_turtle_text_message_passthrough() {
        let mut msg = TextMessageData {
            friend_id: 3,
            name: [0u8; MAX_NAME_LENGTH],
            text: [0u8; MAX_MESSAGE_LENGTH],
        };
        set_buf(&mut msg.name, "Bob");
        set_buf(&mut msg.text, "[] a carrier:Custom ; carrier:foo \"bar\" .");
        let data = CarrierEventData { text_message: msg };
        let ev = make_event(CarrierEventType::TextMessage, data);
        let turtle = event_to_turtle(&ev).unwrap();
        // Passthrough: should NOT contain "a carrier:TextMessage"
        assert!(!turtle.contains("a carrier:TextMessage"));
        // Should start with the passthrough turtle
        assert!(turtle.starts_with("[] a carrier:Custom"));
        // Should append friendId and name
        assert!(turtle.contains("carrier:friendId 3"));
        assert!(turtle.contains("carrier:name \"Bob\""));
    }

    #[test]
    fn event_to_turtle_friend_online() {
        let mut d = FriendOnlineData {
            friend_id: 42,
            name: [0u8; MAX_NAME_LENGTH],
        };
        set_buf(&mut d.name, "Charlie");
        let data = CarrierEventData { friend_online: d };
        let ev = make_event(CarrierEventType::FriendOnline, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:FriendOnline"));
        assert!(turtle.contains("carrier:friendId 42"));
        assert!(turtle.contains("carrier:name \"Charlie\""));
    }

    #[test]
    fn event_to_turtle_friend_offline() {
        let data = CarrierEventData {
            friend_offline: FriendOfflineData { friend_id: 99 },
        };
        let ev = make_event(CarrierEventType::FriendOffline, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:FriendOffline"));
        assert!(turtle.contains("carrier:friendId 99"));
    }

    #[test]
    fn event_to_turtle_error() {
        let mut d = ErrorData {
            cmd: [0u8; 64],
            text: [0u8; MAX_MESSAGE_LENGTH],
        };
        set_buf(&mut d.cmd, "send");
        set_buf(&mut d.text, "timeout");
        let data = CarrierEventData { error: d };
        let ev = make_event(CarrierEventType::Error, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:Error"));
        assert!(turtle.contains("carrier:cmd \"send\""));
        assert!(turtle.contains("carrier:message \"timeout\""));
    }

    #[test]
    fn event_to_turtle_system() {
        let mut d = SystemData {
            text: [0u8; MAX_MESSAGE_LENGTH],
        };
        set_buf(&mut d.text, "bootstrap complete");
        let data = CarrierEventData { system: d };
        let ev = make_event(CarrierEventType::System, data);
        let turtle = event_to_turtle(&ev).unwrap();
        assert!(turtle.contains("a carrier:System"));
        assert!(turtle.contains("carrier:message \"bootstrap complete\""));
    }

    #[test]
    fn event_to_turtle_binary_returns_none() {
        // PipeData, AudioFrame, VideoFrame should all return None
        let data = CarrierEventData {
            connected: ConnectedData { transport: 0 },
        };
        for typ in [
            CarrierEventType::PipeData,
            CarrierEventType::AudioFrame,
            CarrierEventType::VideoFrame,
        ] {
            let ev = make_event(typ, data);
            assert!(
                event_to_turtle(&ev).is_none(),
                "{:?} should return None",
                typ
            );
        }
    }

    // -- format_timestamp tests -----------------------------------------------

    #[test]
    fn format_timestamp_epoch() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00");
    }

    #[test]
    fn format_timestamp_known_date() {
        // 86400 seconds = exactly 1 day after epoch
        assert_eq!(format_timestamp(86_400_000), "1970-01-02T00:00:00");
        // 90061 seconds = 1 day + 1 hour + 1 minute + 1 second
        assert_eq!(format_timestamp(90_061_000), "1970-01-02T01:01:01");
    }

    // -- is_turtle tests ------------------------------------------------------

    #[test]
    fn is_turtle_true() {
        assert!(is_turtle("[] a carrier:TextMessage ; carrier:text \"hi\" ."));
        assert!(is_turtle("  [] a carrier:Foo ."));
    }

    #[test]
    fn is_turtle_false() {
        assert!(!is_turtle("Hello world"));
        assert!(!is_turtle(""));
        assert!(!is_turtle("a carrier:Foo ."));
    }

    // -- cstr_from_buf tests --------------------------------------------------

    #[test]
    fn cstr_from_buf_basic() {
        let mut buf = [0u8; 32];
        set_buf(&mut buf, "hello");
        assert_eq!(cstr_from_buf(&buf), "hello");
    }

    #[test]
    fn cstr_from_buf_no_null() {
        let buf = [b'A'; 8]; // no null terminator
        assert_eq!(cstr_from_buf(&buf), "AAAAAAAA");
    }

    // -- turtle_escape --------------------------------------------------------

    #[test]
    fn turtle_escape_special_chars() {
        assert_eq!(turtle_escape(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(turtle_escape("a\\b"), "a\\\\b");
        assert_eq!(turtle_escape("line\nbreak"), "line\\nbreak");
        assert_eq!(turtle_escape("cr\rret"), "cr\\rret");
    }

    #[test]
    fn turtle_escape_plain() {
        assert_eq!(turtle_escape("nothing special"), "nothing special");
    }
}
