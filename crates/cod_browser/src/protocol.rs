//! The wire format between Cod and the `cod-browser` helper process (issue #16).
//!
//! Every message is a little-endian `u32` length, a one-byte tag and the fields. A frame carries its pixels after a fixed header so the reader can fill a buffer of the exact size without copying.
//! Cod's `web_view` crate includes this file by path, so both sides always agree.

use std::io::{self, Read, Write};

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

const MAX_MESSAGE: u32 = 1 << 20;
const MAX_FRAME_BYTES: usize = 1 << 28;

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
        }
        .send(out)
    }

    pub fn read_from(input: &mut impl Read) -> io::Result<Self> {
        let length = read_length(input)?;
        if length == 0 || length > MAX_MESSAGE as usize {
            return Err(bad("bad message length"));
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
            let bytes = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .filter(|bytes| *bytes > 0 && *bytes <= MAX_FRAME_BYTES)
                .ok_or_else(|| bad("bad frame size"))?;
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
            _ => return Err(bad("unknown message")),
        })
    }
}

/// Writes a frame without first copying the pixels into a message buffer.
pub fn write_frame(
    out: &mut impl Write,
    page: u32,
    width: u32,
    height: u32,
    bgra: &[u8],
) -> io::Result<()> {
    let length = (1 + 12 + bgra.len()) as u32;
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
}
