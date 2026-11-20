use std::thread::sleep;
use std::time::Duration;

use super::isotp::{self, Frame};
use super::transport::{read_prompt, SerialTransport, TcpTransport, Transport};
use crate::error::{Error, Result};
use crate::hexutil;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveProtocol {
    None,
    Can,
    Kwp,
}

#[derive(Debug, Clone)]
pub struct ElmConfig {
    pub timeout: Duration,
    pub reset_timeout: Duration,
    pub request_retries: u32,
    pub pending_delay: Duration,
}

impl Default for ElmConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            reset_timeout: Duration::from_secs(2),
            request_retries: 4,
            pending_delay: Duration::from_millis(30),
        }
    }
}

pub struct Elm327 {
    io: Box<dyn Transport>,
    cfg: ElmConfig,
    protocol: ActiveProtocol,
    tx_id: String,
    rx_id: String,
    baud: u32,
    version: String,
    last_raw: String,
}

impl Elm327 {
    pub fn new(io: Box<dyn Transport>, cfg: ElmConfig) -> Self {
        Self {
            io,
            cfg,
            protocol: ActiveProtocol::None,
            tx_id: String::new(),
            rx_id: String::new(),
            baud: 500_000,
            version: String::new(),
            last_raw: String::new(),
        }
    }

    pub fn serial(port: &str, baud: u32, cfg: ElmConfig) -> Result<Self> {
        let io = SerialTransport::open(port, baud, cfg.timeout)?;
        Ok(Self::new(Box::new(io), cfg))
    }

    pub fn tcp(addr: &str, cfg: ElmConfig) -> Result<Self> {
        let io = TcpTransport::open(addr, cfg.timeout)?;
        Ok(Self::new(Box::new(io), cfg))
    }

    pub fn protocol(&self) -> ActiveProtocol {
        self.protocol
    }

    pub fn description(&self) -> String {
        self.io.description()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn last_raw(&self) -> &str {
        &self.last_raw
    }

    pub fn tx_id(&self) -> &str {
        &self.tx_id
    }

    pub fn rx_id(&self) -> &str {
        &self.rx_id
    }

    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// Reset the adapter and put it into a predictable state.
    pub fn initialize(&mut self) -> Result<()> {
        let _ = self.io.flush();
        let saved = self.cfg.timeout;
        self.cfg.timeout = saved.max(Duration::from_secs(2));
        let _ = self.raw("ATZ");
        std::thread::sleep(Duration::from_millis(150));
        self.version = self.raw("ATI").unwrap_or_default().trim().to_string();
        self.cfg.timeout = saved;
        self.raw("ATE0")?;
        self.raw("ATL0")?;
        self.raw("ATS0")?;
        self.raw("ATH0")?;
        self.raw("ATAL")?;
        self.raw("ATAT1")?;
        self.raw("ATSTFF")?;
        self.raw("ATCAF0")?;
        self.raw("ATCFC1")?;
        Ok(())
    }

    /// Send an AT/ST command and return the cleaned multi-line answer.
    pub fn raw(&mut self, command: &str) -> Result<String> {
        let command = command.trim();
        self.io.write_bytes(format!("{command}\r").as_bytes())?;
        let raw = read_prompt(self.io.as_mut(), self.cfg.timeout)?;
        let text = String::from_utf8_lossy(&raw).replace('\r', "\n");
        self.last_raw = text.clone();
        let mut lines = Vec::new();
        for line in text.split('\n') {
            let line = line.trim();
            if line.is_empty() || line == ">" {
                continue;
            }
            if line.eq_ignore_ascii_case(command) {
                continue;
            }
            if line.starts_with('?') {
                return Err(Error::Adapter(format!(
                    "adapter rejected command {command:?}"
                )));
            }
            lines.push(line.to_string());
        }
        Ok(lines.join("\n"))
    }

    fn command_ok(&mut self, command: &str) -> Result<()> {
        let r = self.raw(command)?;
        if r.to_ascii_uppercase().contains("OK") || r.is_empty() {
            Ok(())
        } else {
            Err(Error::Adapter(format!(
                "{command}: unexpected answer {r:?}"
            )))
        }
    }

    fn can_protocol_number(&self, extended: bool) -> u8 {
        match (self.baud, extended) {
            (250_000, false) => 8,
            (250_000, true) => 9,
            (_, false) => 6,
            (_, true) => 7,
        }
    }

    /// Configure the adapter for an 11-bit or 29-bit CAN ECU.
    pub fn set_can(&mut self, tx_id: &str, rx_id: &str, baud: u32) -> Result<(String, String)> {
        let tx = tx_id.trim().replace(' ', "").to_ascii_uppercase();
        let rx = rx_id.trim().replace(' ', "").to_ascii_uppercase();
        if tx.len() != 3 && tx.len() != 8 {
            return Err(Error::Protocol(format!("invalid CAN TX id {tx:?}")));
        }
        if !rx.is_empty() && rx.len() != 3 && rx.len() != 8 {
            return Err(Error::Protocol(format!("invalid CAN RX id {rx:?}")));
        }
        self.baud = baud;
        self.tx_id = tx.clone();
        self.rx_id = rx.clone();
        let extended = tx.len() == 8;
        let sp = self.can_protocol_number(extended);
        self.raw(&format!("ATSP{sp}"))?;
        self.command_ok("ATE0")?;
        self.command_ok("ATL0")?;
        self.command_ok("ATS0")?;
        self.command_ok("ATH0")?;
        self.command_ok("ATAL")?;
        self.command_ok("ATCAF0")?;
        self.command_ok("ATCFC1")?;
        self.command_ok("ATAT1")?;
        self.command_ok("ATSTFF")?;
        if extended {
            self.command_ok(&format!("ATCP{}", &tx[..2]))?;
            self.command_ok(&format!("ATSH{}", &tx[2..]))?;
        } else {
            self.command_ok(&format!("ATSH{tx}"))?;
        }
        if !rx.is_empty() {
            self.command_ok(&format!("ATCRA{rx}"))?;
        }
        self.command_ok(&format!("ATFCSH{tx}"))?;
        self.command_ok("ATFCSD300000")?;
        self.command_ok("ATFCSM1")?;
        self.protocol = ActiveProtocol::Can;
        Ok((tx, rx))
    }

    /// Configure the adapter for KWP2000 on K-line.
    pub fn set_kwp(&mut self, addr: &str, fast_init: bool) -> Result<()> {
        let addr = addr.trim().to_ascii_uppercase();
        if addr.len() != 2 {
            return Err(Error::Protocol(format!("invalid KWP address {addr:?}")));
        }
        self.command_ok("ATE0")?;
        self.command_ok("ATL0")?;
        self.command_ok("ATS0")?;
        self.command_ok("ATH0")?;
        self.command_ok("ATIB10")?;
        self.command_ok(&format!("ATSH81{addr}F1"))?;
        self.command_ok("ATSW96")?;
        self.command_ok(&format!("ATWM81{addr}F13E"))?;
        self.command_ok("ATSTFF")?;
        self.command_ok("ATAT1")?;
        if fast_init {
            self.command_ok("ATSP5")?;
            let _ = self.raw("ATFI");
        } else {
            self.command_ok("ATSP4")?;
            let _ = self.raw("ATSI");
        }
        self.protocol = ActiveProtocol::Kwp;
        // Start communication (KWP StartCommunication service).
        let _ = self.raw("81");
        Ok(())
    }

    pub fn send_session(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        self.request(payload)
    }

    pub fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if payload.is_empty() {
            return Err(Error::Protocol("empty request".into()));
        }
        match self.protocol {
            ActiveProtocol::Can => self.can_request(payload),
            ActiveProtocol::Kwp => self.kwp_request(payload),
            ActiveProtocol::None => Err(Error::Protocol(
                "adapter is not configured for an ECU".into(),
            )),
        }
    }

    fn can_request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        for attempt in 0..=self.cfg.request_retries {
            let frames = isotp::frame(payload)?;
            let mut response_frames = Vec::new();
            let mut saw_no_data = false;
            let mut last_text = String::new();
            for (idx, f) in frames.iter().enumerate() {
                let echo = hexutil::to_hex(f);
                let rsp = self.raw(&echo)?;
                last_text.push_str(&rsp);
                last_text.push('\n');
                let _ = idx;
                let parsed = parse_can_frames(&rsp, &echo);
                saw_no_data |= rsp.to_ascii_uppercase().contains("NO DATA");
                response_frames.extend(parsed);
            }
            // Some adapters only deliver the ECU answer after the last CF and may need a moment.
            if response_frames.is_empty() && !saw_no_data {
                sleep(Duration::from_millis(10));
            }
            let messages = isotp::reassemble_checked(&response_frames);
            if messages.is_empty() {
                if saw_no_data || last_text.to_ascii_uppercase().contains("NO DATA") {
                    if attempt < self.cfg.request_retries {
                        sleep(self.cfg.pending_delay);
                        continue;
                    }
                    return Err(Error::NoResponse);
                }
                return Err(Error::NoResponse);
            }
            let expected = payload[0].wrapping_add(0x40);
            let chosen = messages
                .iter()
                .rev()
                .find(|m| m.len() >= 1 && m[0] == expected)
                .or_else(|| messages.iter().rev().find(|m| m.len() >= 1 && m[0] == 0x7F))
                .or_else(|| messages.last());
            let Some(msg) = chosen.cloned() else {
                return Err(Error::NoResponse);
            };
            if msg.len() >= 3 && msg[0] == 0x7F && msg[2] == 0x78 {
                if attempt < self.cfg.request_retries {
                    sleep(self.cfg.pending_delay);
                    continue;
                }
            }
            return Ok(msg);
        }
        Err(Error::NoResponse)
    }

    fn kwp_request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let sent = hexutil::to_hex(payload);
        let rsp = self.raw(&sent)?;
        let mut hex = String::new();
        let mut no_data = false;
        for line in rsp.split('\n') {
            let trimmed = line.trim();
            let upper = trimmed.to_ascii_uppercase();
            if upper.contains("NO DATA") {
                no_data = true;
                continue;
            }
            if upper.is_empty() {
                continue;
            }
            // Only accept lines that consist solely of hex digits and spaces.
            if !trimmed.chars().all(|c| c.is_ascii_hexdigit() || c == ' ') {
                continue;
            }
            let cleaned: String = trimmed.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            if cleaned.len() >= 2 && cleaned.len() % 2 == 0 && !cleaned.eq_ignore_ascii_case(&sent)
            {
                hex.push_str(&cleaned);
            }
        }
        if hex.is_empty() {
            return if no_data {
                Err(Error::NoResponse)
            } else {
                Err(Error::NoResponse)
            };
        }
        hexutil::parse_hex(&hex)
    }

    pub fn close(&mut self) -> Result<()> {
        let _ = self.raw("ATPC");
        self.protocol = ActiveProtocol::None;
        Ok(())
    }
}

fn parse_can_frames(text: &str, echo: &str) -> Vec<Frame> {
    let mut out = Vec::new();
    for line in text.split(['\r', '\n']) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut payload = line;
        if let Some((_, after)) = line.rsplit_once(':') {
            payload = after;
        }
        let clean: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
        if clean.is_empty() || !clean.chars().all(|c| c.is_ascii_hexdigit()) || clean.len() % 2 != 0
        {
            continue;
        }
        let clean = clean.to_ascii_uppercase();
        if clean == echo.to_ascii_uppercase() {
            continue;
        }
        if let Ok(bytes) = hexutil::parse_hex(&clean) {
            if bytes.is_empty() {
                continue;
            }
            match bytes[0] >> 4 {
                0..=3 => out.push(Frame::parse(&bytes)),
                _ => {}
            }
        }
    }
    out
}
