//! ISO 13400 (DoIP) diagnostic link over TCP.
//!
//! DoIP carries UDS messages between a tester and an ECU logical address through a vehicle
//! gateway. This implementation performs the required routing activation and frames/validates
//! DoIP diagnostic messages, acknowledgements and alive checks.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::{Error, Result};

use super::link::{DiagnosticLink, DoipSpec, LinkInfo};

const HEADER_LEN: usize = 8;
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
const DOIP_VERSION: u8 = 0x02;

const VEHICLE_IDENTIFICATION_REQUEST: u16 = 0x0001;
const VEHICLE_IDENTIFICATION_RESPONSE: u16 = 0x0004;
const ROUTING_ACTIVATION_REQUEST: u16 = 0x0005;
const ROUTING_ACTIVATION_RESPONSE: u16 = 0x0006;
const ALIVE_CHECK_REQUEST: u16 = 0x0007;
const ALIVE_CHECK_RESPONSE: u16 = 0x0008;
const DIAGNOSTIC_MESSAGE: u16 = 0x8001;
const DIAGNOSTIC_MESSAGE_ACK: u16 = 0x8002;
const DIAGNOSTIC_MESSAGE_NACK: u16 = 0x8003;
const GENERIC_HEADER_NACK: u16 = 0x0000;

pub struct DoipLink {
    stream: TcpStream,
    host: String,
    port: u16,
    tester_address: u16,
    target_address: Option<u16>,
    timeout: Duration,
    routing_activated: bool,
    closed: bool,
}

impl DoipLink {
    pub fn connect(host: &str, port: u16, tester_address: u16, timeout: Duration) -> Result<Self> {
        let host = host.trim();
        if host.is_empty() {
            return Err(Error::Protocol("DoIP gateway host is empty".into()));
        }
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| Error::Transport(format!("DoIP resolve {host}:{port}: {e}")))?
            .next()
            .ok_or_else(|| Error::Transport(format!("DoIP cannot resolve {host}:{port}")))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)
            .map_err(|e| Error::Transport(format!("DoIP connect {host}:{port}: {e}")))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true).ok();
        let mut link = Self {
            stream,
            host: host.to_string(),
            port,
            tester_address,
            target_address: None,
            timeout,
            routing_activated: false,
            closed: false,
        };
        link.routing_activation()?;
        Ok(link)
    }

    fn routing_activation(&mut self) -> Result<()> {
        let mut payload = Vec::with_capacity(7);
        payload.extend_from_slice(&self.tester_address.to_be_bytes());
        payload.push(0x00); // default activation type
        payload.extend_from_slice(&[0, 0, 0, 0]);
        self.send_message(ROUTING_ACTIVATION_REQUEST, &payload)?;
        loop {
            let (kind, response) = self.receive_message()?;
            match kind {
                ROUTING_ACTIVATION_RESPONSE => {
                    if response.len() < 3 {
                        return Err(Error::Protocol(
                            "DoIP routing-activation response is incomplete".into(),
                        ));
                    }
                    let tester = u16::from_be_bytes([response[0], response[1]]);
                    let code = response[2];
                    if tester != self.tester_address {
                        return Err(Error::Protocol(format!(
                            "DoIP routing activation returned tester 0x{tester:04X}"
                        )));
                    }
                    if code != 0 {
                        return Err(Error::Protocol(format!(
                            "DoIP routing activation rejected (code 0x{code:02X})"
                        )));
                    }
                    self.routing_activated = true;
                    return Ok(());
                }
                ALIVE_CHECK_REQUEST => {
                    self.answer_alive_check()?;
                }
                VEHICLE_IDENTIFICATION_RESPONSE => continue,
                GENERIC_HEADER_NACK => {
                    return Err(Error::Protocol("DoIP generic header NACK".into()))
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected DoIP routing response 0x{other:04X}"
                    )))
                }
            }
        }
    }

    fn answer_alive_check(&mut self) -> Result<()> {
        self.send_message(ALIVE_CHECK_RESPONSE, &self.tester_address.to_be_bytes())
    }

    fn send_message(&mut self, kind: u16, payload: &[u8]) -> Result<()> {
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::Protocol("DoIP payload exceeds maximum".into()));
        }
        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
        frame.push(DOIP_VERSION);
        frame.push(!DOIP_VERSION);
        frame.extend_from_slice(&kind.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        self.stream
            .write_all(&frame)
            .map_err(|e| Error::Transport(format!("DoIP write: {e}")))?;
        Ok(())
    }

    fn receive_message(&mut self) -> Result<(u16, Vec<u8>)> {
        let mut header = [0u8; HEADER_LEN];
        self.read_exact(&mut header)?;
        let version = header[0];
        let inverse = header[1];
        if inverse != !version {
            return Err(Error::Protocol("invalid DoIP version complement".into()));
        }
        if version != 0x02 && version != 0x03 {
            return Err(Error::Protocol(format!(
                "unsupported DoIP version 0x{version:02X}"
            )));
        }
        let kind = u16::from_be_bytes([header[2], header[3]]);
        let len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if len > MAX_PAYLOAD {
            return Err(Error::Protocol("DoIP payload exceeds maximum".into()));
        }
        let mut payload = vec![0u8; len];
        if len > 0 {
            self.read_exact(&mut payload)?;
        }
        Ok((kind, payload))
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        match self.stream.read_exact(buf) {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Err(Error::Timeout("DoIP response timed out".into()))
            }
            Err(e) => Err(Error::Transport(format!("DoIP read: {e}"))),
        }
    }
}

impl DiagnosticLink for DoipLink {
    fn info(&self) -> LinkInfo {
        LinkInfo {
            description: format!("DoIP {}:{}", self.host, self.port),
            protocol: format!("DoIP / UDS (tester 0x{:04X})", self.tester_address),
            tx_id: self
                .target_address
                .map(|a| format!("{a:04X}"))
                .unwrap_or_default(),
            rx_id: format!("{:04X}", self.tester_address),
            baud: 0,
        }
    }

    fn open_can(&mut self, _spec: &super::link::CanSpec) -> Result<()> {
        Err(Error::Unsupported(
            "DoIP uses logical addresses, not CAN identifiers".into(),
        ))
    }

    fn open_kwp(&mut self, _spec: &super::link::KwpSpec) -> Result<()> {
        Err(Error::Unsupported(
            "DoIP is not available for KWP2000/K-line ECUs".into(),
        ))
    }

    fn open_doip(&mut self, spec: &DoipSpec) -> Result<()> {
        if !self.routing_activated {
            return Err(Error::Protocol("DoIP routing is not activated".into()));
        }
        self.target_address = Some(spec.target_address);
        self.timeout = spec.timeout.unwrap_or(self.timeout);
        self.stream.set_read_timeout(Some(self.timeout))?;
        Ok(())
    }

    fn is_doip(&self) -> bool {
        true
    }

    fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if !self.routing_activated {
            return Err(Error::Protocol("DoIP routing is not activated".into()));
        }
        let target = self
            .target_address
            .ok_or_else(|| Error::Protocol("no DoIP target address selected".into()))?;
        if payload.is_empty() {
            return Err(Error::Protocol("empty DoIP diagnostic request".into()));
        }
        let mut request = Vec::with_capacity(4 + payload.len());
        request.extend_from_slice(&self.tester_address.to_be_bytes());
        request.extend_from_slice(&target.to_be_bytes());
        request.extend_from_slice(payload);
        self.send_message(DIAGNOSTIC_MESSAGE, &request)?;

        loop {
            let (kind, response) = self.receive_message()?;
            match kind {
                DIAGNOSTIC_MESSAGE => {
                    if response.len() < 4 {
                        return Err(Error::Protocol(
                            "DoIP diagnostic response is incomplete".into(),
                        ));
                    }
                    let source = u16::from_be_bytes([response[0], response[1]]);
                    let dest = u16::from_be_bytes([response[2], response[3]]);
                    if source != target {
                        return Err(Error::Protocol(format!(
                            "DoIP response came from 0x{source:04X}, expected 0x{target:04X}"
                        )));
                    }
                    if dest != self.tester_address {
                        return Err(Error::Protocol(format!(
                            "DoIP response was sent to 0x{dest:04X}, expected tester 0x{:04X}",
                            self.tester_address
                        )));
                    }
                    let data = response[4..].to_vec();
                    if data.len() >= 3 && data[0] == 0x7F && data[2] == 0x78 {
                        // Response pending: wait for the final UDS response.
                        continue;
                    }
                    return Ok(data);
                }
                DIAGNOSTIC_MESSAGE_ACK => {
                    if response.len() < 5 {
                        return Err(Error::Protocol("DoIP acknowledgement incomplete".into()));
                    }
                    let source = u16::from_be_bytes([response[0], response[1]]);
                    let dest = u16::from_be_bytes([response[2], response[3]]);
                    let code = response[4];
                    if source != target || dest != self.tester_address {
                        return Err(Error::Protocol(
                            "DoIP acknowledgement has wrong addresses".into(),
                        ));
                    }
                    if code != 0 {
                        return Err(Error::Protocol(format!(
                            "DoIP diagnostic message rejected (code 0x{code:02X})"
                        )));
                    }
                }
                DIAGNOSTIC_MESSAGE_NACK => {
                    let code = response.get(4).copied().unwrap_or(0);
                    return Err(Error::Protocol(format!(
                        "DoIP diagnostic message NACK (code 0x{code:02X})"
                    )));
                }
                ALIVE_CHECK_REQUEST => self.answer_alive_check()?,
                VEHICLE_IDENTIFICATION_RESPONSE => continue,
                GENERIC_HEADER_NACK => {
                    return Err(Error::Protocol("DoIP generic header NACK".into()))
                }
                _ => continue,
            }
        }
    }

    fn raw_command(&mut self, _command: &str) -> Result<String> {
        Err(Error::Unsupported(
            "AT/ELM commands are not available over DoIP".into(),
        ))
    }

    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for DoipLink {
    fn drop(&mut self) {
        self.close();
    }
}

/// UDP vehicle announcement request (ISO 13400-2). Returns VIN and logical addresses.
pub fn discover_vehicles(host: &str, timeout: Duration) -> Result<Vec<DoipVehicle>> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| Error::Transport(format!("DoIP discovery socket: {e}")))?;
    socket
        .set_broadcast(true)
        .map_err(|e| Error::Transport(format!("DoIP broadcast: {e}")))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| Error::Transport(format!("DoIP discovery timeout: {e}")))?;
    let request = encode_message(VEHICLE_IDENTIFICATION_REQUEST, &[], 0xFF);
    socket
        .send_to(&request, (host, 13400))
        .map_err(|e| Error::Transport(format!("DoIP announcement: {e}")))?;
    let deadline = std::time::Instant::now() + timeout;
    let mut out = Vec::new();
    while std::time::Instant::now() < deadline {
        let mut buf = [0u8; 4096];
        match socket.recv_from(&mut buf) {
            Ok((n, addr)) => {
                if n < HEADER_LEN {
                    continue;
                }
                if let Ok((kind, payload)) = decode_message(&buf[..n]) {
                    if kind == VEHICLE_IDENTIFICATION_RESPONSE && payload.len() >= 31 {
                        let vin = String::from_utf8_lossy(&payload[..17]).trim().to_string();
                        let logical = u16::from_be_bytes([payload[17], payload[18]]);
                        out.push(DoipVehicle {
                            vin,
                            logical_address: logical,
                            address: addr.ip().to_string(),
                        });
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                break
            }
            Err(e) => return Err(Error::Transport(format!("DoIP discovery read: {e}"))),
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct DoipVehicle {
    pub vin: String,
    pub logical_address: u16,
    pub address: String,
}

fn encode_message(kind: u16, payload: &[u8], version: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(version);
    out.push(!version);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn decode_message(data: &[u8]) -> Result<(u16, Vec<u8>)> {
    if data.len() < HEADER_LEN {
        return Err(Error::Protocol("short DoIP message".into()));
    }
    let kind = u16::from_be_bytes([data[2], data[3]]);
    let len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    if data.len() < HEADER_LEN + len {
        return Err(Error::Protocol("truncated DoIP message".into()));
    }
    Ok((kind, data[HEADER_LEN..HEADER_LEN + len].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn encodes_and_decodes_messages() {
        let msg = encode_message(DIAGNOSTIC_MESSAGE, &[1, 2, 3], DOIP_VERSION);
        assert_eq!(msg[0], 0x02);
        assert_eq!(msg[1], 0xFD);
        assert_eq!(&msg[2..4], &[0x80, 0x01]);
        assert_eq!(u32::from_be_bytes([msg[4], msg[5], msg[6], msg[7]]), 3);
        let (kind, payload) = decode_message(&msg).unwrap();
        assert_eq!(kind, DIAGNOSTIC_MESSAGE);
        assert_eq!(payload, vec![1, 2, 3]);
    }

    #[test]
    fn performs_routing_activation_and_diagnostic_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            // Routing activation request
            let (kind, payload) = read_test_message(&mut stream);
            assert_eq!(kind, ROUTING_ACTIVATION_REQUEST);
            assert_eq!(&payload[..2], &0x0E00u16.to_be_bytes());
            stream
                .write_all(&encode_message(
                    ROUTING_ACTIVATION_RESPONSE,
                    &[0x0E, 0x00, 0x00],
                    DOIP_VERSION,
                ))
                .unwrap();
            // Diagnostic request
            let (kind, payload) = read_test_message(&mut stream);
            assert_eq!(kind, DIAGNOSTIC_MESSAGE);
            assert_eq!(&payload[4..], &[0x22, 0xF1, 0x90]);
            // Positive acknowledgement then response
            stream
                .write_all(&encode_message(
                    DIAGNOSTIC_MESSAGE_ACK,
                    &[0x00, 0xEF, 0x0E, 0x00, 0x00],
                    DOIP_VERSION,
                ))
                .unwrap();
            stream
                .write_all(&encode_message(
                    DIAGNOSTIC_MESSAGE,
                    &[0x00, 0xEF, 0x0E, 0x00, 0x62, 0xF1, 0x90, 0x01],
                    DOIP_VERSION,
                ))
                .unwrap();
        });

        let mut link =
            DoipLink::connect("127.0.0.1", addr.port(), 0x0E00, Duration::from_secs(2)).unwrap();
        link.open_doip(&DoipSpec {
            target_address: 0x00EF,
            tester_address: None,
            timeout: None,
        })
        .unwrap();
        let response = link.request(&[0x22, 0xF1, 0x90]).unwrap();
        assert_eq!(response, vec![0x62, 0xF1, 0x90, 0x01]);
        server.join().unwrap();
    }

    fn read_test_message(stream: &mut TcpStream) -> (u16, Vec<u8>) {
        let mut header = [0u8; HEADER_LEN];
        stream.read_exact(&mut header).unwrap();
        let kind = u16::from_be_bytes([header[2], header[3]]);
        let len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).unwrap();
        (kind, payload)
    }
}
