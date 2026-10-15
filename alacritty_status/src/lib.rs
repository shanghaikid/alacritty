//! OSC 7501 revision 0.4: bounded, allocation-free parsing and reusable records.
//!
//! The caller supplies an entire OSC body, excluding `ESC ] 7501 ;` and its
//! terminator. It MUST bound its streaming buffer before calling this module.
//! There are no clocks, timers, threads, subprocesses, or I/O in this library.

use std::cmp::Reverse;
use std::fmt::Write;
use std::str;

use base64::Engine;
use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
#[cfg(feature = "serde")]
use serde::Serialize;

pub const MAX_SEQUENCE: usize = 4096;
pub const MAX_RECORDS: usize = 64;
pub const MAX_MESSAGE: usize = 2048;
pub const MAX_TITLE: usize = 192;
const MAX_MESSAGE_ENCODED: usize = 2732;
const MAX_TITLE_ENCODED: usize = 256;
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

pub const REPLY_ST: &str =
    "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";
pub const REPLY_BEL: &str =
    "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminator {
    St,
    Bel,
}

impl Terminator {
    pub const fn overhead(self) -> usize {
        match self {
            Self::St => 9,
            Self::Bel => 8,
        }
    }
    pub const fn reply(self) -> &'static str {
        match self {
            Self::St => REPLY_ST,
            Self::Bel => REPLY_BEL,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize), serde(rename_all = "lowercase"))]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

impl State {
    fn parse(value: &[u8]) -> Option<Self> {
        Some(match value {
            b"idle" => Self::Idle,
            b"working" => Self::Working,
            b"done" => Self::Done,
            b"blocked" => Self::Blocked,
            b"error" => Self::Error,
            b"clear" => Self::Clear,
            _ => return None,
        })
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }
    fn priority(self) -> u8 {
        match self {
            Self::Blocked => 5,
            Self::Error => 4,
            Self::Done => 3,
            Self::Working => 2,
            Self::Idle => 1,
            Self::Clear => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize), serde(rename_all = "lowercase"))]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    fn parse(value: &[u8]) -> Option<Self> {
        match value {
            b"permission" => Some(Self::Permission),
            b"question" => Some(Self::Question),
            b"auth" => Some(Self::Auth),
            _ => None,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Unchanged,
    Changed,
    Query(Terminator),
    Rejected,
}

/// An immutable view. `app` is the record's own value; use `summary` for inheritance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Record<'a> {
    pub id: &'a str,
    pub state: State,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    pub app: Option<&'a str>,
    pub title: Option<&'a str>,
    pub msg: Option<&'a str>,
}

#[derive(Debug)]
struct Entry {
    id: String,
    state: State,
    kind: Option<Kind>,
    progress: Option<u8>,
    app: String,
    title: String,
    msg: String,
    has_title: bool,
    has_msg: bool,
    updated: u64,
}

impl Entry {
    fn view(&self) -> Record<'_> {
        Record {
            id: &self.id,
            state: self.state,
            kind: self.kind,
            progress: self.progress,
            app: (!self.app.is_empty()).then_some(self.app.as_str()),
            title: self.has_title.then_some(self.title.as_str()),
            msg: self.has_msg.then_some(self.msg.as_str()),
        }
    }
    fn replace(&mut self, p: &Parsed<'_>, title: Option<&str>, msg: Option<&str>) -> bool {
        let kind = (p.state == State::Blocked).then_some(p.kind).flatten();
        let progress =
            matches!(p.state, State::Working | State::Blocked).then_some(p.progress).flatten();
        let changed = self.state != p.state
            || self.kind != kind
            || self.progress != progress
            || self.app != p.app
            || self.has_title != title.is_some()
            || self.has_msg != msg.is_some()
            || self.title != title.unwrap_or_default()
            || self.msg != msg.unwrap_or_default();
        self.state = p.state;
        self.kind = kind;
        self.progress = progress;
        assign(&mut self.app, p.app);
        assign(&mut self.title, title.unwrap_or_default());
        assign(&mut self.msg, msg.unwrap_or_default());
        self.has_title = title.is_some();
        self.has_msg = msg.is_some();
        changed
    }
}

fn assign(destination: &mut String, value: &str) {
    if destination != value {
        destination.clear();
        destination.push_str(value);
    }
}

struct Parsed<'a> {
    id: &'a str,
    state: State,
    kind: Option<Kind>,
    progress: Option<u8>,
    app: &'a str,
    title_len: Option<usize>,
    msg_len: Option<usize>,
}

#[derive(Debug)]
struct Scratch {
    title: [u8; MAX_TITLE],
    msg: [u8; MAX_MESSAGE + 1],
}

impl Default for Scratch {
    fn default() -> Self {
        Self { title: [0; MAX_TITLE], msg: [0; MAX_MESSAGE + 1] }
    }
}

/// Lazily allocated, at most 64 records, with an exact last-packet fast path.
/// Updating existing fields reuses string capacity, including when a field disappears.
#[derive(Default, Debug)]
pub struct Store {
    records: Vec<Entry>,
    last_packet: Vec<u8>,
    last_index: Option<usize>,
    clock: u64,
    generation: u64,
    scratch: Scratch,
}

impl Store {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn records(&self) -> impl ExactSizeIterator<Item = Record<'_>> {
        self.records.iter().map(Entry::view)
    }

    pub fn apply(&mut self, payload: &[u8], terminator: Terminator) -> Outcome {
        if payload.len() > MAX_SEQUENCE - terminator.overhead() {
            return Outcome::Rejected;
        }
        if payload.trim_ascii() == b"?" {
            return Outcome::Query(terminator);
        }
        if let Some(index) = self.last_index.filter(|_| self.last_packet == payload) {
            self.clock = self.clock.wrapping_add(1);
            self.records[index].updated = self.clock;
            return Outcome::Unchanged;
        }

        // Scratch is reused, including for rejected input: no 2 KiB memset per update.
        let Some(parsed) = parse(payload, &mut self.scratch.title, &mut self.scratch.msg) else {
            return Outcome::Rejected;
        };
        if parsed.state == State::Clear {
            return if self.clear_subtree(parsed.id) {
                Outcome::Changed
            } else {
                Outcome::Unchanged
            };
        }

        let title = parsed.title_len.map(|len| str::from_utf8(&self.scratch.title[..len]).unwrap());
        let msg = parsed.msg_len.map(|len| str::from_utf8(&self.scratch.msg[..len]).unwrap());
        let index = match self
            .last_index
            .filter(|&i| self.records[i].id == parsed.id)
            .or_else(|| self.records.iter().position(|r| r.id == parsed.id))
        {
            Some(index) => index,
            None if self.records.len() < MAX_RECORDS => {
                self.records.push(Entry {
                    id: parsed.id.into(),
                    state: parsed.state,
                    kind: None,
                    progress: None,
                    app: String::new(),
                    title: String::new(),
                    msg: String::new(),
                    has_title: false,
                    has_msg: false,
                    updated: 0,
                });
                self.records.len() - 1
            },
            None => {
                let index = self
                    .records
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, r)| self.clock.wrapping_sub(r.updated))
                    .unwrap()
                    .0;
                assign(&mut self.records[index].id, parsed.id);
                self.records[index].has_title = false;
                self.records[index].has_msg = false;
                index
            },
        };
        let changed =
            self.records[index].replace(&parsed, title, msg) || self.last_index != Some(index);
        self.clock = self.clock.wrapping_add(1);
        self.records[index].updated = self.clock;
        self.last_index = Some(index);
        self.last_packet.clear();
        self.last_packet.extend_from_slice(payload);
        if changed {
            self.generation = self.generation.wrapping_add(1);
            Outcome::Changed
        } else {
            Outcome::Unchanged
        }
    }

    /// Remove a record and its descendants, with path-segment boundaries.
    pub fn clear_subtree(&mut self, id: &str) -> bool {
        let before = self.records.len();
        self.records.retain(|r| {
            !(id.is_empty()
                || r.id == id
                || r.id.strip_prefix(id).is_some_and(|s| s.starts_with('/')))
        });
        self.removed(before)
    }

    /// Called on a shell prompt (OSC 133 A) or attached-process exit, not on DECSTR.
    pub fn expire_transient(&mut self) -> bool {
        let before = self.records.len();
        self.records.retain(|r| matches!(r.state, State::Done | State::Error));
        self.removed(before)
    }

    /// Called on RIS. Primary/alternate screen switches do not affect this store.
    pub fn reset(&mut self) -> bool {
        self.clear_subtree("")
    }

    fn removed(&mut self, before: usize) -> bool {
        if before == self.records.len() {
            return false;
        }
        self.last_index = None;
        self.last_packet.clear();
        self.generation = self.generation.wrapping_add(1);
        true
    }

    pub fn inherited_app(&self, id: &str) -> Option<&str> {
        let mut ancestor = id;
        loop {
            if let Some(entry) = self.records.iter().find(|r| r.id == ancestor && !r.app.is_empty())
            {
                return Some(&entry.app);
            }
            if ancestor.is_empty() {
                return None;
            }
            ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }

    /// Most actionable state first; newest update breaks equal-priority ties.
    pub fn summary(&self) -> Option<Record<'_>> {
        let entry = self
            .records
            .iter()
            .max_by_key(|r| (r.state.priority(), Reverse(self.clock.wrapping_sub(r.updated))))?;
        let mut view = entry.view();
        view.app = self.inherited_app(view.id);
        Some(view)
    }

    /// Bounded plain text, suitable for a terminal's own title/status area.
    /// No markup is interpreted. The host must retain its window/pane identity.
    pub fn display_label(&self, output: &mut String) {
        output.clear();
        let Some(record) = self.summary() else {
            return;
        };
        let state = record.kind.map_or(record.state.name(), Kind::name);
        output.push('[');
        output.push_str(state);
        if let Some(progress) = record.progress {
            let _ = write!(output, " {progress}%");
        }
        output.push(']');
        if let Some(label) = record.title.or(record.app).or(record.msg) {
            output.push(' ');
            for c in label.chars().filter(|c| !invisible(*c)).take(96) {
                output.push(c);
            }
        }
    }
}

fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

fn identifier(value: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'))
}

fn valid_id(value: &[u8]) -> bool {
    value.len() <= 128
        && value.split(|b| *b == b'/').count() <= 8
        && value.split(|b| *b == b'/').all(identifier)
}

fn decode(value: &[u8], output: &mut [u8], limit: usize) -> Option<usize> {
    if value.contains(&b'=') && value.len() % 4 != 0 {
        return None;
    }
    let len = BASE64.decode_slice(value, output).ok()?;
    if len > limit || str::from_utf8(&output[..len]).ok()?.chars().any(char::is_control) {
        return None;
    }
    Some(len)
}

fn parse<'a>(payload: &'a [u8], title: &mut [u8], msg: &mut [u8]) -> Option<Parsed<'a>> {
    let mut result = Parsed {
        id: "",
        state: State::Idle,
        kind: None,
        progress: None,
        app: "",
        title_len: None,
        msg_len: None,
    };
    let mut state = None;
    let mut id = None;
    for pair in payload.split(|b| *b == b':') {
        let Some(equal) = pair.iter().position(|b| *b == b'=') else {
            continue;
        };
        let key = pair[..equal].trim_ascii();
        let value = pair[equal + 1..].trim_ascii();
        if key.len() > 16 {
            return None;
        }
        if key.is_empty() || !key.iter().all(u8::is_ascii_lowercase) {
            continue;
        }
        // Hard limits are checked even when an otherwise malformed pair would be skipped.
        match key {
            b"msg" if value.len() > MAX_MESSAGE_ENCODED => return None,
            b"title" if value.len() > MAX_TITLE_ENCODED => return None,
            b"app" if value.len() > 32 => return None,
            b"id"
                if value.len() > 128
                    || value.split(|b| *b == b'/').count() > 8
                    || value.split(|b| *b == b'/').any(|segment| segment.len() > 32) =>
            {
                return None;
            },
            _ => (),
        }
        if key == b"id" {
            id = Some(value);
            continue;
        }
        if !value.iter().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
        }) {
            continue;
        }
        match key {
            b"state" => state = State::parse(value),
            b"id" => id = Some(value),
            b"kind" => result.kind = Kind::parse(value),
            b"progress" => {
                result.progress =
                    str::from_utf8(value).ok()?.parse::<u8>().ok().filter(|v| *v <= 100);
                if !value.iter().all(u8::is_ascii_digit) {
                    result.progress = None;
                }
            },
            b"app" => {
                if value.len() > 32 {
                    return None;
                }
                result.app = if identifier(value) { str::from_utf8(value).ok()? } else { "" };
            },
            b"title" => {
                if value.len() > MAX_TITLE_ENCODED {
                    return None;
                }
                result.title_len = Some(decode(value, title, MAX_TITLE)?);
            },
            b"msg" => {
                if value.len() > MAX_MESSAGE_ENCODED {
                    return None;
                }
                result.msg_len = Some(decode(value, msg, MAX_MESSAGE)?);
            },
            _ => (),
        }
    }
    if let Some(value) = id {
        if !valid_id(value) {
            return None;
        }
        result.id = str::from_utf8(value).ok()?;
    }
    result.state = state?;
    Some(result)
}

#[cfg(test)]
mod tests;

mod ffi;
