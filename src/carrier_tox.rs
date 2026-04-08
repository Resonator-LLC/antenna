///! FFI bindings to libcarrier and CarrierEvent → Turtle serialization.

use anyhow::{bail, Result};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
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

type CarrierEventCb =
    unsafe extern "C" fn(event: *const CarrierEvent, userdata: *mut c_void);

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
    fn carrier_set_event_callback(
        c: *mut Carrier,
        cb: CarrierEventCb,
        userdata: *mut c_void,
    );
    fn carrier_send_message(
        c: *mut Carrier,
        friend_id: u32,
        text: *const c_char,
    ) -> c_int;
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
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
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
                        stripped, d.request_id, turtle_escape(key)
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
                    format!(
                        "{} ; carrier:friendId {} .",
                        stripped, d.friend_id
                    )
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
                    format!(
                        "{} ; carrier:friendId {} .",
                        stripped, d.friend_id
                    )
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

unsafe extern "C" fn event_callback(
    event: *const CarrierEvent,
    userdata: *mut c_void,
) {
    if event.is_null() || userdata.is_null() {
        return;
    }
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
}

// Carrier is single-threaded (caller must iterate from one thread),
// but we need Send to move it into the main struct.
unsafe impl Send for ToxCarrier {}

impl ToxCarrier {
    pub fn new(
        profile: &str,
        nodes: Option<&str>,
        sender: Sender<String>,
    ) -> Result<Self> {
        let profile_c = CString::new(profile)?;
        let nodes_c = nodes
            .map(|n| CString::new(n))
            .transpose()?;

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

        let sender_box = Box::new(sender);
        let sender_ptr = &*sender_box as *const Sender<String> as *mut c_void;

        unsafe {
            carrier_set_event_callback(ptr, event_callback, sender_ptr);
        }

        Ok(Self {
            ptr,
            _sender: sender_box,
        })
    }

    pub fn iterate(&self) -> Result<()> {
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

    pub fn send_message(&self, friend_id: u32, text: &str) -> Result<()> {
        let text_c = CString::new(text)?;
        let rc = unsafe { carrier_send_message(self.ptr, friend_id, text_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_send_message failed: {}", rc);
        }
        Ok(())
    }

    pub fn get_id(&self) -> Result<()> {
        let rc = unsafe { carrier_get_id(self.ptr) };
        if rc < 0 {
            bail!("carrier_get_id failed: {}", rc);
        }
        Ok(())
    }

    pub fn set_nick(&self, nick: &str) -> Result<()> {
        let nick_c = CString::new(nick)?;
        let rc = unsafe { carrier_set_nick(self.ptr, nick_c.as_ptr()) };
        if rc < 0 {
            bail!("carrier_set_nick failed: {}", rc);
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let rc = unsafe { carrier_save(self.ptr) };
        if rc < 0 {
            bail!("carrier_save failed: {}", rc);
        }
        Ok(())
    }
}

impl Drop for ToxCarrier {
    fn drop(&mut self) {
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
