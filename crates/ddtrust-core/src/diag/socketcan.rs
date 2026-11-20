//! CandleLight / SocketCAN link for Linux.
//!
//! CandleLight firmware exposes the adapter as a Linux CAN interface (`can0`, `vcan0`, ...).
//! This link speaks ISO-TP (ISO 15765-2) itself: it sends the tester frames, handles ECU flow
//! control while transmitting long requests, and sends flow control for long responses.

use std::time::{Duration, Instant};

use socketcan::{
    CanFrame, CanSocket, EmbeddedFrame, ExtendedId, Frame, ShouldRetry, Socket, StandardId,
};

use super::link::{CanSpec, DiagnosticLink, KwpSpec, LinkInfo};
use crate::error::{Error, Result};

pub struct SocketCanLink {
    socket: CanSocket,
    interface: String,
    tx_id: u32,
    tx_ext: bool,
    rx_id: u32,
    rx_ext: bool,
    baud: u32,
    timeout: Duration,
    configured: bool,
}

impl SocketCanLink {
    pub fn new(interface: &str, timeout: Duration) -> Result<Self> {
        if let Ok(flags) = std::fs::read_to_string(format!("/sys/class/net/{interface}/flags")) {
            let up = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)
                .map(|f| f & 1 != 0)
                .unwrap_or(true);
            if !up {
                return Err(Error::Transport(format!(
                    "SocketCAN {interface} is down; run: sudo ip link set {interface} up type can bitrate 500000"
                )));
            }
        }
        let socket = CanSocket::open(interface)
            .map_err(|e| Error::Transport(format!("cannot open SocketCAN {interface}: {e}")))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(|e| Error::Transport(format!("SocketCAN timeout: {e}")))?;
        Ok(Self {
            socket,
            interface: interface.to_string(),
            tx_id: 0,
            tx_ext: false,
            rx_id: 0,
            rx_ext: false,
            baud: 500_000,
            timeout,
            configured: false,
        })
    }

    fn parse_id(text: &str) -> Result<(u32, bool)> {
        let clean = text.trim().replace(' ', "").to_ascii_uppercase();
        let id = u32::from_str_radix(&clean, 16)
            .map_err(|_| Error::Protocol(format!("invalid CAN id {text:?}")))?;
        match clean.len() {
            3 if id <= 0x7FF => Ok((id, false)),
            8 if id <= 0x1FFF_FFFF => Ok((id, true)),
            _ => Err(Error::Protocol(format!(
                "invalid CAN id {text:?}: expected 3 (11-bit) or 8 (29-bit) hex digits"
            ))),
        }
    }

    fn write(&mut self, id: u32, ext: bool, data: &[u8]) -> Result<()> {
        let frame = if ext {
            CanFrame::new(
                ExtendedId::new(id)
                    .ok_or_else(|| Error::Protocol(format!("bad extended id {id:X}")))?,
                data,
            )
        } else {
            CanFrame::new(
                StandardId::new(id as u16)
                    .ok_or_else(|| Error::Protocol(format!("bad standard id {id:X}")))?,
                data,
            )
        }
        .ok_or_else(|| Error::Protocol("cannot build CAN frame".into()))?;
        self.socket
            .write_frame(&frame)
            .map_err(|e| Error::Transport(format!("SocketCAN write: {e}")))
    }

    fn read_frame(&mut self, deadline: Instant) -> Result<Option<(u32, bool, Vec<u8>)>> {
        loop {
            if Instant::now() >= deadline {
                return Ok(None);
            }
            match self.socket.read_frame() {
                Ok(CanFrame::Data(frame)) => {
                    let data = frame.data().to_vec();
                    let id = frame.raw_id();
                    let ext = frame.is_extended();
                    return Ok(Some((id, ext, data)));
                }
                Ok(_) => continue,
                Err(e) if e.should_retry() => continue,
                Err(e) => return Err(Error::Transport(format!("SocketCAN read: {e}"))),
            }
        }
    }

    fn read_matching(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>> {
        loop {
            let Some((id, ext, data)) = self.read_frame(deadline)? else {
                return Ok(None);
            };
            if id == self.rx_id && ext == self.rx_ext {
                return Ok(Some(data));
            }
        }
    }

    fn send_flow_control(&mut self) -> Result<()> {
        // BS=0 (send everything), STmin=0.
        self.write(
            self.tx_id,
            self.tx_ext,
            &[0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        )
    }

    fn read_flow_control(&mut self, deadline: Instant) -> Result<(u8, u8)> {
        loop {
            let Some(data) = self.read_matching(deadline)? else {
                return Err(Error::Timeout(
                    "timed out waiting for ISO-TP flow control".into(),
                ));
            };
            if data.first().map(|b| b >> 4) != Some(3) {
                continue;
            }
            let fs = data[0] & 0x0F;
            match fs {
                0 => {
                    return Ok((
                        data.get(1).copied().unwrap_or(0),
                        data.get(2).copied().unwrap_or(0),
                    ))
                }
                1 => continue, // wait
                2 => return Err(Error::Protocol("ECU reported ISO-TP overflow".into())),
                _ => {
                    return Err(Error::Protocol(format!(
                        "unexpected ISO-TP flow status {fs}"
                    )))
                }
            }
        }
    }

    fn send_request_frames(&mut self, payload: &[u8]) -> Result<()> {
        if payload.is_empty() {
            return Err(Error::Protocol("empty request".into()));
        }
        if payload.len() <= 7 {
            let mut frame = Vec::with_capacity(payload.len() + 1);
            frame.push(payload.len() as u8);
            frame.extend_from_slice(payload);
            self.write(self.tx_id, self.tx_ext, &frame)?;
            return Ok(());
        }
        if payload.len() > 0xFFF {
            return Err(Error::Protocol("request exceeds ISO-TP maximum".into()));
        }
        let mut ff = Vec::with_capacity(8);
        ff.push(0x10 | ((payload.len() >> 8) as u8 & 0x0F));
        ff.push((payload.len() & 0xFF) as u8);
        ff.extend_from_slice(&payload[..6]);
        self.write(self.tx_id, self.tx_ext, &ff)?;
        let deadline = Instant::now() + self.timeout.min(Duration::from_secs(2));
        let (bs, stmin) = self.read_flow_control(deadline)?;

        let mut rest = &payload[6..];
        let mut seq = 1u8;
        let mut sent_since_fc = 0u8;
        let (mut bs, mut stmin) = (bs, stmin);
        while !rest.is_empty() {
            let take = rest.len().min(7);
            let mut cf = Vec::with_capacity(take + 1);
            cf.push(0x20 | (seq & 0x0F));
            cf.extend_from_slice(&rest[..take]);
            self.write(self.tx_id, self.tx_ext, &cf)?;
            rest = &rest[take..];
            seq = seq.wrapping_add(1);
            sent_since_fc = sent_since_fc.wrapping_add(1);
            if stmin > 0 {
                let delay = if stmin >= 0xF1 {
                    Duration::from_micros(((stmin - 0xF0) as u64) * 100)
                } else {
                    Duration::from_millis(stmin as u64)
                };
                std::thread::sleep(delay.min(Duration::from_millis(20)));
            }
            if bs > 0 && sent_since_fc >= bs && !rest.is_empty() {
                let deadline = Instant::now() + self.timeout.min(Duration::from_secs(2));
                let (new_bs, new_stmin) = self.read_flow_control(deadline)?;
                sent_since_fc = 0;
                bs = new_bs;
                stmin = new_stmin;
            }
        }
        Ok(())
    }

    fn receive_isotp(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + timeout;
        loop {
            let Some(data) = self.read_matching(deadline)? else {
                return Ok(None);
            };
            if data.is_empty() {
                continue;
            }
            match data[0] >> 4 {
                0 => {
                    let len = (data[0] & 0x0F) as usize;
                    if len == 0 || data.len() < 1 + len {
                        continue;
                    }
                    return Ok(Some(data[1..1 + len].to_vec()));
                }
                1 => {
                    if data.len() < 3 {
                        continue;
                    }
                    let len = (((data[0] & 0x0F) as usize) << 8) | data[1] as usize;
                    if len <= 6 {
                        let mut msg = data[2..].to_vec();
                        msg.truncate(len);
                        return Ok(Some(msg));
                    }
                    let mut msg = data[2..].to_vec();
                    self.send_flow_control()?;
                    let mut expected = 1u8;
                    while msg.len() < len {
                        let Some(cf) = self.read_matching(deadline)? else {
                            return Err(Error::Timeout("timed out during ISO-TP response".into()));
                        };
                        if cf.is_empty() || cf[0] >> 4 != 2 {
                            continue;
                        }
                        let seq = cf[0] & 0x0F;
                        if seq != (expected & 0x0F) {
                            return Err(Error::Protocol(format!(
                                "ISO-TP sequence error: expected {}, got {seq}",
                                expected & 0x0F
                            )));
                        }
                        expected = expected.wrapping_add(1);
                        msg.extend_from_slice(&cf[1..]);
                    }
                    msg.truncate(len);
                    return Ok(Some(msg));
                }
                2 | 3 => continue,
                _ => continue,
            }
        }
    }
}

impl DiagnosticLink for SocketCanLink {
    fn info(&self) -> LinkInfo {
        LinkInfo {
            description: format!("SocketCAN {}", self.interface),
            protocol: format!("CAN @{}k (CandleLight)", self.baud / 1000),
            tx_id: format_id(self.tx_id, self.tx_ext),
            rx_id: format_id(self.rx_id, self.rx_ext),
            baud: self.baud,
        }
    }

    fn open_can(&mut self, spec: &CanSpec) -> Result<()> {
        let (tx, tx_ext) = Self::parse_id(&spec.tx_id)?;
        let (rx, rx_ext) = Self::parse_id(&spec.rx_id)?;
        self.tx_id = tx;
        self.tx_ext = tx_ext;
        self.rx_id = rx;
        self.rx_ext = rx_ext;
        self.baud = if spec.baud == 0 { 500_000 } else { spec.baud };
        self.configured = true;
        Ok(())
    }

    fn open_kwp(&mut self, _spec: &KwpSpec) -> Result<()> {
        Err(Error::Unsupported(
            "CandleLight/SocketCAN is CAN-only; KWP2000 and ISO8 need an ELM327/K-line adapter"
                .into(),
        ))
    }

    fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if !self.configured {
            return Err(Error::Protocol("CAN ids are not configured".into()));
        }
        for attempt in 0..2 {
            self.send_request_frames(payload)?;
            if let Some(response) = self.receive_isotp(self.timeout)? {
                if response.len() >= 3 && response[0] == 0x7F && response[2] == 0x78 {
                    if attempt == 0 {
                        continue;
                    }
                }
                return Ok(response);
            }
            if attempt == 0 {
                continue;
            }
        }
        Err(Error::NoResponse)
    }

    fn raw_command(&mut self, _command: &str) -> Result<String> {
        Err(Error::Unsupported(
            "AT commands are not available on a SocketCAN/CandleLight adapter".into(),
        ))
    }

    fn close(&mut self) {
        self.configured = false;
    }
}

pub fn can_interfaces() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_can = name.starts_with("can")
                || name.starts_with("vcan")
                || name.starts_with("slcan")
                || name.starts_with("gs_usb");
            if is_can && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

pub fn interface_state(interface: &str) -> String {
    if let Ok(flags) = std::fs::read_to_string(format!("/sys/class/net/{interface}/flags")) {
        if let Ok(flags) = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16) {
            return if flags & 1 != 0 {
                "up".into()
            } else {
                "down".into()
            };
        }
    }
    std::fs::read_to_string(format!("/sys/class/net/{interface}/operstate"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::SocketCanLink;

    #[test]
    fn parses_can_ids() {
        assert_eq!(SocketCanLink::parse_id("7E0").unwrap(), (0x7E0, false));
        assert_eq!(
            SocketCanLink::parse_id("18DA2DF1").unwrap(),
            (0x18DA2DF1, true)
        );
        assert!(SocketCanLink::parse_id("7E").is_err());
        assert!(SocketCanLink::parse_id("18DA2DF1FF").is_err());
    }
}

fn format_id(id: u32, ext: bool) -> String {
    if ext {
        format!("{id:08X}")
    } else {
        format!("{id:03X}")
    }
}
