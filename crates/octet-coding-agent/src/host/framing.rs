//! Bounded newline-delimited framing for the host transport.
//!
//! Framing is deliberately independent of request policy and agent execution:
//! an oversized or incomplete line is represented without deserializing it.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::protocol::MAX_FRAME_BYTES;

pub(crate) fn serialize_bounded<T: serde::Serialize>(
    value: &T,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, serde_json::Error> {
    let mut writer = BoundedFrame::new(max_bytes);
    let result = serde_json::to_writer(&mut writer, value);
    if writer.overflowed {
        return Ok(None);
    }
    result?;
    Ok(Some(writer.bytes))
}

struct BoundedFrame {
    bytes: Vec<u8>,
    max_bytes: usize,
    overflowed: bool,
}

impl BoundedFrame {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(max_bytes.min(8 * 1024)),
            max_bytes,
            overflowed: false,
        }
    }
}

impl std::io::Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
            self.overflowed = true;
            return Err(std::io::Error::other("protocol frame exceeds limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) enum Frame {
    Data(Vec<u8>),
    Incomplete,
    Oversized,
}

pub(crate) async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Frame>> {
    let mut frame = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() && !oversized {
                Ok(None)
            } else {
                Ok(Some(Frame::Incomplete))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if !oversized {
            if frame.len().saturating_add(consumed) > MAX_FRAME_BYTES {
                oversized = true;
                frame.clear();
            } else {
                frame.extend_from_slice(&available[..consumed]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if oversized {
                return Ok(Some(Frame::Oversized));
            }
            while matches!(frame.last(), Some(b'\n' | b'\r')) {
                frame.pop();
            }
            return Ok(Some(Frame::Data(frame)));
        }
    }
}

#[cfg(test)]
mod tests;
