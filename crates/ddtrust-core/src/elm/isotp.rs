//! Minimal ISO-TP (ISO 15765-2) framing used with ELM327 CAF0 adapters.

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Single(Vec<u8>),
    First(Vec<u8>, usize),
    Consecutive(u8, Vec<u8>),
    FlowControl,
    Invalid(String),
}

impl Frame {
    pub fn parse(bytes: &[u8]) -> Frame {
        if bytes.is_empty() {
            return Frame::Invalid("empty frame".into());
        }
        match bytes[0] >> 4 {
            0 => Frame::Single(bytes[1..].to_vec()),
            1 => {
                if bytes.len() < 3 {
                    return Frame::Invalid("short first frame".into());
                }
                let len = (((bytes[0] & 0x0F) as usize) << 8) | bytes[1] as usize;
                Frame::First(bytes[2..].to_vec(), len)
            }
            2 => Frame::Consecutive(bytes[0] & 0x0F, bytes.get(1..).unwrap_or_default().to_vec()),
            3 => Frame::FlowControl,
            n => Frame::Invalid(format!("unknown PCI 0x{n:X}")),
        }
    }
}

/// Split a diagnostic payload into ISO-TP frames (without flow control).
pub fn frame(payload: &[u8]) -> Result<Vec<Vec<u8>>> {
    if payload.is_empty() {
        return Err(Error::Protocol("empty ISO-TP payload".into()));
    }
    if payload.len() > 0xFFF {
        return Err(Error::Protocol(
            "payload exceeds ISO-TP maximum (4095 bytes)".into(),
        ));
    }
    let mut out = Vec::new();
    if payload.len() <= 7 {
        let mut f = Vec::with_capacity(payload.len() + 1);
        f.push(payload.len() as u8);
        f.extend_from_slice(payload);
        out.push(f);
    } else {
        let mut f = Vec::with_capacity(8);
        f.push(0x10 | ((payload.len() >> 8) as u8 & 0x0F));
        f.push((payload.len() & 0xFF) as u8);
        f.extend_from_slice(&payload[..6]);
        out.push(f);
        let mut seq = 1u8;
        for chunk in payload[6..].chunks(7) {
            let mut cf = Vec::with_capacity(chunk.len() + 1);
            cf.push(0x20 | (seq & 0x0F));
            cf.extend_from_slice(chunk);
            out.push(cf);
            seq = seq.wrapping_add(1);
        }
    }
    Ok(out)
}

/// Reassemble complete ISO-TP messages from decoded frames.
pub fn reassemble(frames: &[Frame]) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let mut current: Option<(usize, Vec<u8>)> = None;
    for f in frames {
        match f {
            Frame::Single(d) => messages.push(d.clone()),
            Frame::First(data, len) => {
                if let Some((_, buf)) = current.take() {
                    let _ = buf;
                }
                current = Some((*len, data.clone()));
            }
            Frame::Consecutive(_, data) => {
                let mut complete = false;
                if let Some((len, buf)) = &mut current {
                    buf.extend_from_slice(data);
                    complete = buf.len() >= *len;
                }
                if complete {
                    let (_, mut done) = current.take().unwrap();
                    done.truncate(done.len().min(usize::MAX));
                    let len = done.len();
                    done.truncate(len);
                    messages.push(done);
                }
            }
            Frame::FlowControl | Frame::Invalid(_) => {}
        }
    }
    messages
}

/// Sequence-checked reassembly with a stable order, used by the driver.
pub fn reassemble_checked(frames: &[Frame]) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let mut current: Option<(usize, Vec<u8>, u8)> = None;
    for f in frames {
        match f {
            Frame::Single(d) => messages.push(d.clone()),
            Frame::First(data, len) => {
                current = Some((*len, data.clone(), 1));
            }
            Frame::Consecutive(seq, data) => {
                let mut complete = false;
                let mut expected_len = 0usize;
                if let Some((len, buf, expected)) = current.as_mut() {
                    if (*seq & 0x0F) != (*expected & 0x0F) {
                        current = None;
                        continue;
                    }
                    buf.extend_from_slice(data);
                    *expected = expected.wrapping_add(1);
                    expected_len = *len;
                    complete = buf.len() >= *len;
                }
                if complete {
                    if let Some((_, mut done, _)) = current.take() {
                        done.truncate(expected_len);
                        messages.push(done);
                    }
                }
            }
            Frame::FlowControl | Frame::Invalid(_) => {}
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_short_and_long() {
        for len in [1usize, 6, 7, 8, 20, 100] {
            let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let frames: Vec<Frame> = frame(&payload)
                .unwrap()
                .iter()
                .map(|f| Frame::parse(f))
                .collect();
            let msgs = reassemble_checked(&frames);
            assert_eq!(msgs, vec![payload], "len {len}");
        }
    }
}
