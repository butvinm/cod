//! The wire format between Cod and the `cod-browser` helper process (issue #16).
//!
//! Every message is a little-endian `u32` length, a one-byte tag and the fields. A frame carries its pixels after a fixed header so the reader can fill a buffer of the exact size without copying.
//! Cod's `web_view` crate includes this file by path, so both sides always agree.

use std::io::{self, Read, Write};

/// The most bytes a reported address can carry on the wire (RC review #1): the engine truncates
/// any longer address to this many bytes on a character boundary, so Cod's side must compare
/// against the same truncation to recognize its own agent document after a long page loads.
pub const MAX_URL_BYTES: usize = 8192;

/// Every line an agent page's bootstrap script prints on Cod's behalf starts with this (RC
/// review B5), so the engine can tell a bridge call apart from the page's own console output and
/// give it a larger byte limit than ordinary diagnostics. Shared with `web_view::agent_bridge`,
/// which defines the rest of the bridge wire format (the JSON payload after this prefix). No real
/// page is expected to start a log line this way.
pub const BRIDGE_PREFIX: &str = "cod:bridge:";

/// The name of the sentinel bridge call the engine sends in place of a bridge console line it
/// cannot carry whole (RC review B5): `args` is always empty. Cod's side recognizes this name
/// specially and closes the page instead of leaving `codctl open --html --wait` blocked forever
/// on a value it will never receive.
pub const BRIDGE_OVERSIZED_ACTION: &str = "__bridge_oversized__";

/// The most bytes a bridge call's console line keeps (RC review B5): far more than ordinary
/// diagnostics need, so a real agent-page submission is never silently cut into invalid JSON and
/// lost, while still comfortably inside the protocol's own 1 MiB message budget alongside the
/// `Console` message's own tag, page id and length-prefix overhead.
pub const MAX_BRIDGE_BYTES: usize = 65536;

/// Cuts `text` to at most `limit` bytes on a character boundary, so no message can exceed what
/// the receiving side accepts. Shared by the engine (outgoing diagnostics) and Cod's side (RC
/// review #1), so both agree on exactly where a long address gets cut.
pub fn limited(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// What the engine's `on_console_message` should put on the wire for one console line (RC
/// review B5): ordinary diagnostics are cut to `console_limit` as before, but a bridge call
/// (`BRIDGE_PREFIX`) that still fits under `MAX_BRIDGE_BYTES` goes out whole, and one that does
/// not becomes the sentinel `BRIDGE_OVERSIZED_ACTION` call instead of silently truncated, invalid
/// JSON. A real browser engine is not required to exercise this from Rust, so it is a plain
/// function of its inputs and unit-tested directly.
pub fn console_payload(text: &str, console_limit: usize) -> String {
    if !text.starts_with(BRIDGE_PREFIX) {
        return limited(text, console_limit);
    }
    if text.len() <= MAX_BRIDGE_BYTES {
        return text.to_owned();
    }
    format!("{BRIDGE_PREFIX}{{\"name\":\"{BRIDGE_OVERSIZED_ACTION}\",\"args\":{{}}}}")
}

pub const KEY_RAW_DOWN: u8 = 0;
pub const KEY_UP: u8 = 2;
pub const KEY_CHAR: u8 = 3;

pub const BUTTON_LEFT: u8 = 0;
pub const BUTTON_MIDDLE: u8 = 1;
pub const BUTTON_RIGHT: u8 = 2;

pub const EVENT_SHIFT: u32 = 1 << 1;
pub const EVENT_CONTROL: u32 = 1 << 2;
pub const EVENT_ALT: u32 = 1 << 3;
pub const EVENT_LEFT_BUTTON: u32 = 1 << 4;
pub const EVENT_MIDDLE_BUTTON: u32 = 1 << 5;
pub const EVENT_RIGHT_BUTTON: u32 = 1 << 6;
pub const EVENT_COMMAND: u32 = 1 << 7;

/// The `action` of `ToBrowser::Navigate`.
pub const NAVIGATE_BACK: u8 = 0;
pub const NAVIGATE_FORWARD: u8 = 1;
pub const NAVIGATE_RELOAD: u8 = 2;

const MAX_MESSAGE: u32 = 1 << 20;
const MAX_FRAME_BYTES: usize = 1 << 28;

/// Bytes a `Create` message spends on everything but its `url` field: the tag, `page`, `width`,
/// `height`, `scale` and the URL's own length prefix (RC review B4).
const CREATE_FIXED_BYTES: usize = 1 + 4 + 4 + 4 + 4 + 4;

/// Whether a `Create` message carrying a URL this long would fit under the protocol's one
/// message size limit (RC review B4): a page large enough to fail this must be refused before it
/// is ever sent, instead of exceeding `MAX_MESSAGE` and taking down the helper's connection, and
/// every page tab with it.
pub fn create_message_fits(url_len: usize) -> bool {
    CREATE_FIXED_BYTES
        .checked_add(url_len)
        .is_some_and(|total| total <= MAX_MESSAGE as usize)
}

/// The largest `url` a `Create` message can carry under the protocol's message size limit (RC
/// review B4), for a clear error message that names the limit.
pub fn max_create_url_len() -> usize {
    MAX_MESSAGE as usize - CREATE_FIXED_BYTES
}

/// Messages from Cod to the helper.
#[derive(Debug, Clone, PartialEq)]
pub enum ToBrowser {
    Create {
        page: u32,
        width: i32,
        height: i32,
        scale: f32,
        url: String,
    },
    Resize {
        page: u32,
        width: i32,
        height: i32,
        scale: f32,
    },
    Hidden {
        page: u32,
        hidden: bool,
    },
    Focus {
        page: u32,
        focused: bool,
    },
    Load {
        page: u32,
        url: String,
    },
    Script {
        page: u32,
        script: String,
    },
    MouseMove {
        page: u32,
        x: i32,
        y: i32,
        modifiers: u32,
        leave: bool,
    },
    MouseButton {
        page: u32,
        x: i32,
        y: i32,
        modifiers: u32,
        button: u8,
        up: bool,
        clicks: i32,
    },
    Wheel {
        page: u32,
        x: i32,
        y: i32,
        modifiers: u32,
        delta_x: i32,
        delta_y: i32,
    },
    Key {
        page: u32,
        kind: u8,
        code: i32,
        character: u16,
        modifiers: u32,
    },
    Close {
        page: u32,
    },
    /// Back, forward or reload: one of the `NAVIGATE_*` constants.
    Navigate {
        page: u32,
        action: u8,
    },
}

/// Messages from the helper to Cod.
#[derive(Debug, Clone, PartialEq)]
pub enum FromBrowser {
    Ready,
    Failed { message: String },
    Created { page: u32 },
    Title { page: u32, title: String },
    Address { page: u32, url: String },
    Loading { page: u32, loading: bool },
    Console { page: u32, message: String },
    Closed { page: u32 },
    /// BGRA pixels, `width * height * 4` bytes.
    Frame {
        page: u32,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    },
    /// The main frame could not load `url`.
    LoadFailed {
        page: u32,
        url: String,
        error: String,
    },
    /// The page asked for a new window; the helper cancelled it and Cod routes the URL like a link.
    PopupRequested { page: u32, url: String },
}

struct Body(Vec<u8>);

impl Body {
    fn new(tag: u8) -> Self {
        Self(vec![0, 0, 0, 0, tag])
    }

    fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    fn u16(mut self, value: u16) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn i32(mut self, value: i32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn f32(mut self, value: f32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn bool(self, value: bool) -> Self {
        self.u8(value as u8)
    }

    fn string(mut self, value: &str) -> Self {
        self = self.u32(value.len() as u32);
        self.0.extend_from_slice(value.as_bytes());
        self
    }

    fn send(mut self, out: &mut impl Write) -> io::Result<()> {
        let length = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&length.to_le_bytes());
        out.write_all(&self.0)
    }
}

struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
    fn take(&mut self, count: usize) -> io::Result<&[u8]> {
        if self.0.len() < count {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short message"));
        }
        let (head, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut array = [0; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    fn bool(&mut self) -> io::Result<bool> {
        Ok(self.u8()? != 0)
    }

    fn string(&mut self) -> io::Result<String> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid text"))
    }
}

fn read_length(input: &mut impl Read) -> io::Result<usize> {
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    Ok(u32::from_le_bytes(length) as usize)
}

fn bad(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Drains an oversized message's bytes so the stream stays framed, then reports it as
/// `InvalidInput`, distinct from `bad`'s `InvalidData`: a caller can tell "one bad message,
/// still a live connection" apart from "the stream itself is corrupt" (RC review B4).
fn skip_oversized(input: &mut impl Read, length: usize) -> io::Result<io::Error> {
    let mut discard = vec![0u8; length];
    input.read_exact(&mut discard)?;
    Ok(io::Error::new(io::ErrorKind::InvalidInput, "oversized message refused"))
}

impl ToBrowser {
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Create {
                page,
                width,
                height,
                scale,
                url,
            } => Body::new(1)
                .u32(*page)
                .i32(*width)
                .i32(*height)
                .f32(*scale)
                .string(url),
            Self::Resize {
                page,
                width,
                height,
                scale,
            } => Body::new(2).u32(*page).i32(*width).i32(*height).f32(*scale),
            Self::Hidden { page, hidden } => Body::new(3).u32(*page).bool(*hidden),
            Self::Focus { page, focused } => Body::new(4).u32(*page).bool(*focused),
            Self::Load { page, url } => Body::new(5).u32(*page).string(url),
            Self::Script { page, script } => Body::new(6).u32(*page).string(script),
            Self::MouseMove {
                page,
                x,
                y,
                modifiers,
                leave,
            } => Body::new(7)
                .u32(*page)
                .i32(*x)
                .i32(*y)
                .u32(*modifiers)
                .bool(*leave),
            Self::MouseButton {
                page,
                x,
                y,
                modifiers,
                button,
                up,
                clicks,
            } => Body::new(8)
                .u32(*page)
                .i32(*x)
                .i32(*y)
                .u32(*modifiers)
                .u8(*button)
                .bool(*up)
                .i32(*clicks),
            Self::Wheel {
                page,
                x,
                y,
                modifiers,
                delta_x,
                delta_y,
            } => Body::new(9)
                .u32(*page)
                .i32(*x)
                .i32(*y)
                .u32(*modifiers)
                .i32(*delta_x)
                .i32(*delta_y),
            Self::Key {
                page,
                kind,
                code,
                character,
                modifiers,
            } => Body::new(10)
                .u32(*page)
                .u8(*kind)
                .i32(*code)
                .u16(*character)
                .u32(*modifiers),
            Self::Close { page } => Body::new(11).u32(*page),
            Self::Navigate { page, action } => Body::new(12).u32(*page).u8(*action),
        }
        .send(out)
    }

    pub fn read_from(input: &mut impl Read) -> io::Result<Self> {
        let length = read_length(input)?;
        if length == 0 {
            return Err(bad("bad message length"));
        }
        if length > MAX_MESSAGE as usize {
            // Cod itself refuses to send a `Create` this large (RC review B4), so this is
            // defensive: still consume the bytes so the stream stays framed, and let the caller
            // keep every other page tab alive instead of treating the connection as corrupt.
            return Err(skip_oversized(input, length)?);
        }
        let mut message = vec![0; length];
        input.read_exact(&mut message)?;
        let (tag, rest) = message.split_first().ok_or_else(|| bad("empty message"))?;
        let mut fields = Fields(rest);
        let page = fields.u32()?;
        Ok(match tag {
            1 => Self::Create {
                page,
                width: fields.i32()?,
                height: fields.i32()?,
                scale: fields.f32()?,
                url: fields.string()?,
            },
            2 => Self::Resize {
                page,
                width: fields.i32()?,
                height: fields.i32()?,
                scale: fields.f32()?,
            },
            3 => Self::Hidden {
                page,
                hidden: fields.bool()?,
            },
            4 => Self::Focus {
                page,
                focused: fields.bool()?,
            },
            5 => Self::Load {
                page,
                url: fields.string()?,
            },
            6 => Self::Script {
                page,
                script: fields.string()?,
            },
            7 => Self::MouseMove {
                page,
                x: fields.i32()?,
                y: fields.i32()?,
                modifiers: fields.u32()?,
                leave: fields.bool()?,
            },
            8 => Self::MouseButton {
                page,
                x: fields.i32()?,
                y: fields.i32()?,
                modifiers: fields.u32()?,
                button: fields.u8()?,
                up: fields.bool()?,
                clicks: fields.i32()?,
            },
            9 => Self::Wheel {
                page,
                x: fields.i32()?,
                y: fields.i32()?,
                modifiers: fields.u32()?,
                delta_x: fields.i32()?,
                delta_y: fields.i32()?,
            },
            10 => Self::Key {
                page,
                kind: fields.u8()?,
                code: fields.i32()?,
                character: fields.u16()?,
                modifiers: fields.u32()?,
            },
            11 => Self::Close { page },
            12 => Self::Navigate {
                page,
                action: fields.u8()?,
            },
            _ => return Err(bad("unknown message")),
        })
    }
}

impl FromBrowser {
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Ready => Body::new(1),
            Self::Failed { message } => Body::new(2).string(message),
            Self::Created { page } => Body::new(3).u32(*page),
            Self::Title { page, title } => Body::new(4).u32(*page).string(title),
            Self::Address { page, url } => Body::new(5).u32(*page).string(url),
            Self::Loading { page, loading } => Body::new(6).u32(*page).bool(*loading),
            Self::Console { page, message } => Body::new(7).u32(*page).string(message),
            Self::Closed { page } => Body::new(8).u32(*page),
            Self::LoadFailed { page, url, error } => {
                Body::new(10).u32(*page).string(url).string(error)
            }
            Self::PopupRequested { page, url } => Body::new(11).u32(*page).string(url),
            Self::Frame {
                page,
                width,
                height,
                bgra,
            } => return write_frame(out, *page, *width, *height, bgra),
        }
        .send(out)
    }

    pub fn read_from(input: &mut impl Read) -> io::Result<Self> {
        let length = read_length(input)?;
        if length == 0 {
            return Err(bad("bad message length"));
        }
        let mut tag = [0; 1];
        input.read_exact(&mut tag)?;
        if tag[0] == 9 {
            let mut header = [0; 12];
            input.read_exact(&mut header)?;
            let mut fields = Fields(&header);
            let (page, width, height) = (fields.u32()?, fields.u32()?, fields.u32()?);
            // The sizes come from the other process, so every step is checked before anything is allocated.
            let bytes = frame_byte_len(width, height)?;
            if bytes.checked_add(13) != Some(length) {
                return Err(bad("bad frame length"));
            }
            let mut bgra = vec![0; bytes];
            input.read_exact(&mut bgra)?;
            return Ok(Self::Frame {
                page,
                width,
                height,
                bgra,
            });
        }
        if length - 1 > MAX_MESSAGE as usize {
            return Err(bad("bad message length"));
        }
        let mut rest = vec![0; length - 1];
        input.read_exact(&mut rest)?;
        let mut fields = Fields(&rest);
        Ok(match tag[0] {
            1 => Self::Ready,
            2 => Self::Failed {
                message: fields.string()?,
            },
            3 => Self::Created { page: fields.u32()? },
            4 => Self::Title {
                page: fields.u32()?,
                title: fields.string()?,
            },
            5 => Self::Address {
                page: fields.u32()?,
                url: fields.string()?,
            },
            6 => Self::Loading {
                page: fields.u32()?,
                loading: fields.bool()?,
            },
            7 => Self::Console {
                page: fields.u32()?,
                message: fields.string()?,
            },
            8 => Self::Closed { page: fields.u32()? },
            10 => Self::LoadFailed {
                page: fields.u32()?,
                url: fields.string()?,
                error: fields.string()?,
            },
            11 => Self::PopupRequested {
                page: fields.u32()?,
                url: fields.string()?,
            },
            _ => return Err(bad("unknown message")),
        })
    }
}

/// The one place that turns frame dimensions into a byte count: BGRA, no overflow, not empty, within `MAX_FRAME_BYTES`.
/// The helper uses it before it copies pixels, the writer before it sends them and the reader before it allocates.
pub fn frame_byte_len(width: u32, height: u32) -> io::Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes > 0 && *bytes <= MAX_FRAME_BYTES)
        .ok_or_else(|| bad("bad frame size"))
}

/// The length field of a frame message (tag, 12 header bytes and the pixels), if it fits in a `u32`.
fn frame_message_len(bytes: usize) -> io::Result<u32> {
    bytes
        .checked_add(13)
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| bad("frame too large"))
}

/// Writes a frame without first copying the pixels into a message buffer. Refuses pixels that do not match the dimensions.
pub fn write_frame(
    out: &mut impl Write,
    page: u32,
    width: u32,
    height: u32,
    bgra: &[u8],
) -> io::Result<()> {
    if frame_byte_len(width, height)? != bgra.len() {
        return Err(bad("frame pixels do not match its size"));
    }
    let length = frame_message_len(bgra.len())?;
    let mut header = Vec::with_capacity(17);
    header.extend_from_slice(&length.to_le_bytes());
    header.push(9);
    header.extend_from_slice(&page.to_le_bytes());
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&height.to_le_bytes());
    out.write_all(&header)?;
    out.write_all(bgra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_messages_round_trip() {
        let messages = [
            ToBrowser::Create {
                page: 3,
                width: 600,
                height: 800,
                scale: 1.5,
                url: "file:///tmp/a.html".into(),
            },
            ToBrowser::Wheel {
                page: 3,
                x: 10,
                y: -20,
                modifiers: EVENT_SHIFT,
                delta_x: 0,
                delta_y: -120,
            },
            ToBrowser::Key {
                page: 3,
                kind: KEY_CHAR,
                code: 65,
                character: 0x41,
                modifiers: 0,
            },
            ToBrowser::Close { page: 3 },
            ToBrowser::Navigate {
                page: 3,
                action: NAVIGATE_BACK,
            },
            ToBrowser::Navigate {
                page: 3,
                action: NAVIGATE_RELOAD,
            },
        ];
        for message in messages {
            let mut bytes = Vec::new();
            message.write_to(&mut bytes).unwrap();
            let decoded = ToBrowser::read_from(&mut bytes.as_slice()).unwrap();
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn frames_and_events_round_trip() {
        let frame = FromBrowser::Frame {
            page: 2,
            width: 3,
            height: 2,
            bgra: (0..24).collect(),
        };
        let title = FromBrowser::Title {
            page: 2,
            title: "Cod".into(),
        };
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes).unwrap();
        title.write_to(&mut bytes).unwrap();
        let mut input = bytes.as_slice();
        assert_eq!(FromBrowser::read_from(&mut input).unwrap(), frame);
        assert_eq!(FromBrowser::read_from(&mut input).unwrap(), title);
    }

    #[test]
    fn load_failures_and_popups_round_trip() {
        let messages = [
            FromBrowser::LoadFailed {
                page: 4,
                url: "http://127.0.0.1:9/tracker/PAY-1234".into(),
                error: "ERR_CONNECTION_REFUSED".into(),
            },
            FromBrowser::PopupRequested {
                page: 4,
                url: "http://127.0.0.1:9/wiki/Home".into(),
            },
        ];
        for message in messages {
            let mut bytes = Vec::new();
            message.write_to(&mut bytes).unwrap();
            let decoded = FromBrowser::read_from(&mut bytes.as_slice()).unwrap();
            assert_eq!(decoded, message);
        }
    }

    fn frame_header(length: u32, page: u32, width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.push(9);
        bytes.extend_from_slice(&page.to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes
    }

    #[test]
    fn a_frame_header_with_overflowing_dimensions_is_rejected() {
        for (width, height) in [(u32::MAX, u32::MAX), (1 << 31, 4), (0, 10), (10, 0)] {
            let bytes = frame_header(13, 1, width, height);
            let error = FromBrowser::read_from(&mut bytes.as_slice()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{width}x{height}");
        }
    }

    #[test]
    fn a_frame_above_the_size_limit_is_rejected_before_allocating() {
        let bytes = frame_header(13 + (MAX_FRAME_BYTES as u32 + 4), 1, 1 << 14, (1 << 12) + 1);
        assert!(FromBrowser::read_from(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn frame_byte_len_is_checked_and_limited() {
        assert_eq!(frame_byte_len(3, 2).unwrap(), 24);
        assert!(frame_byte_len(0, 5).is_err());
        assert!(frame_byte_len(u32::MAX, u32::MAX).is_err());
        assert!(frame_byte_len(1 << 14, (1 << 12) + 1).is_err());
        assert_eq!(frame_byte_len(1 << 14, 1 << 12).unwrap(), MAX_FRAME_BYTES);
    }

    #[test]
    fn the_writer_refuses_frames_the_reader_would_reject() {
        let mut out = Vec::new();
        assert!(write_frame(&mut out, 1, 2, 2, &[0; 15]).is_err());
        assert!(write_frame(&mut out, 1, 0, 2, &[]).is_err());
        assert!(write_frame(&mut out, 1, u32::MAX, 2, &[0; 16]).is_err());
        assert!(out.is_empty(), "nothing is written for a refused frame");
    }

    #[test]
    fn frame_message_lengths_that_do_not_fit_are_refused() {
        assert_eq!(frame_message_len(16).unwrap(), 29);
        assert!(frame_message_len(usize::MAX).is_err());
        assert!(frame_message_len(u32::MAX as usize).is_err());
    }

    #[test]
    fn a_truncated_frame_payload_is_an_error() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, 1, 4, 4, &[7; 64]).unwrap();
        bytes.truncate(bytes.len() - 10);
        let error = FromBrowser::read_from(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    struct OneByteAtATime<'a>(&'a [u8]);

    impl Read for OneByteAtATime<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() || buffer.is_empty() {
                return Ok(0);
            }
            buffer[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    #[test]
    fn messages_survive_fragmented_reads() {
        let frame = FromBrowser::Frame {
            page: 5,
            width: 2,
            height: 2,
            bgra: (0..16).collect(),
        };
        let command = ToBrowser::Load {
            page: 5,
            url: "https://example.org/".into(),
        };
        let mut frame_bytes = Vec::new();
        frame.write_to(&mut frame_bytes).unwrap();
        let mut command_bytes = Vec::new();
        command.write_to(&mut command_bytes).unwrap();
        assert_eq!(
            FromBrowser::read_from(&mut OneByteAtATime(&frame_bytes)).unwrap(),
            frame
        );
        assert_eq!(
            ToBrowser::read_from(&mut OneByteAtATime(&command_bytes)).unwrap(),
            command
        );
    }

    #[test]
    fn a_frame_with_the_wrong_length_is_rejected() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, 1, 2, 2, &[0; 16]).unwrap();
        bytes[0] = bytes[0].wrapping_add(1);
        assert!(FromBrowser::read_from(&mut bytes.as_slice()).is_err());
    }

    // RC review B5: a bridge call well over the old 4096-byte diagnostic cap must still cross
    // the wire whole, not truncated into invalid JSON that `codctl open --html --wait` never
    // answers for.

    #[test]
    fn a_bridge_call_over_the_old_console_cap_still_crosses_whole() {
        let value = "x".repeat(20_000);
        let line = format!("{BRIDGE_PREFIX}{{\"name\":\"submit\",\"args\":{{\"value\":\"{value}\"}}}}");
        assert!(line.len() > 4096, "the fixture must exceed the old cap");
        assert!(line.len() <= MAX_BRIDGE_BYTES, "the fixture must fit the new one");
        assert_eq!(console_payload(&line, 4096), line);
    }

    #[test]
    fn ordinary_console_output_still_gets_the_small_cap() {
        let line = "x".repeat(20_000);
        assert!(!line.starts_with(BRIDGE_PREFIX));
        let payload = console_payload(&line, 4096);
        assert_eq!(payload.len(), 4096);
    }

    #[test]
    fn a_bridge_call_over_the_new_cap_becomes_the_oversized_sentinel_instead_of_garbled_json() {
        let value = "x".repeat(200_000);
        let line = format!("{BRIDGE_PREFIX}{{\"name\":\"submit\",\"args\":{{\"value\":\"{value}\"}}}}");
        assert!(line.len() > MAX_BRIDGE_BYTES);
        let payload = console_payload(&line, 4096);
        let expected = format!("{BRIDGE_PREFIX}{{\"name\":\"{BRIDGE_OVERSIZED_ACTION}\",\"args\":{{}}}}");
        assert_eq!(payload, expected);
    }
}
