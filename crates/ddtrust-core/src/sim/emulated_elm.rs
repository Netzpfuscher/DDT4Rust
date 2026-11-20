use std::collections::VecDeque;
use std::time::Duration;

use super::virtual_ecu::{response_frames, VirtualEcu};
use crate::elm::transport::Transport;
use crate::error::Result;
use crate::hexutil;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimProtocol {
    None,
    Can,
    Kwp,
}

/// A software ELM327 that speaks the ASCII protocol and talks to virtual ECUs.
/// It is used for offline demos and for integration-testing the real driver.
pub struct EmulatedElm {
    pub ecus: Vec<VirtualEcu>,
    header: String,
    cp_prefix: String,
    filter: String,
    protocol: SimProtocol,
    echo: bool,
    spaces: bool,
    linefeeds: bool,
    headers: bool,
    out: VecDeque<u8>,
    command: Vec<u8>,
    rx_len: Option<usize>,
    rx_buf: Vec<u8>,
    rx_seq: u8,
}

impl Default for EmulatedElm {
    fn default() -> Self {
        Self::new()
    }
}

impl EmulatedElm {
    pub fn new() -> Self {
        Self {
            ecus: Vec::new(),
            header: String::new(),
            cp_prefix: String::new(),
            filter: String::new(),
            protocol: SimProtocol::None,
            echo: false,
            spaces: false,
            linefeeds: false,
            headers: false,
            out: VecDeque::new(),
            command: Vec::new(),
            rx_len: None,
            rx_buf: Vec::new(),
            rx_seq: 1,
        }
    }

    pub fn with_ecus(ecus: Vec<VirtualEcu>) -> Self {
        let mut s = Self::new();
        s.ecus = ecus;
        s
    }

    pub fn add_ecu(&mut self, ecu: VirtualEcu) {
        self.ecus.push(ecu);
    }

    pub fn clear_ecus(&mut self) {
        self.ecus.clear();
    }

    fn emit(&mut self, line: &str) {
        self.out.extend(line.bytes());
        if self.linefeeds {
            self.out.push_back(b'\n');
        }
        self.out.push_back(b'\r');
    }

    fn finish(&mut self) {
        self.out.push_back(b'>');
    }

    fn respond_ok(&mut self) {
        self.emit("OK");
        self.finish();
    }

    fn find_ecu(&self) -> Option<&VirtualEcu> {
        let header = self.header.as_str();
        if header.is_empty() {
            return self.ecus.first();
        }
        if self.protocol == SimProtocol::Kwp && header.len() >= 6 && header.starts_with("81") {
            let addr = &header[2..4];
            return self
                .ecus
                .iter()
                .find(|e| e.address.eq_ignore_ascii_case(addr));
        }
        self.ecus
            .iter()
            .find(|e| e.tx_id.eq_ignore_ascii_case(header))
    }

    fn handle_can(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let pci = bytes[0] >> 4;
        match pci {
            0 => {
                let len = (bytes[0] & 0x0F) as usize;
                if len == 0 || bytes.len() < 1 + len {
                    self.emit("NO DATA");
                    self.finish();
                    return;
                }
                let payload = bytes[1..1 + len].to_vec();
                self.answer(&payload);
            }
            1 => {
                if bytes.len() < 3 {
                    self.emit("NO DATA");
                    self.finish();
                    return;
                }
                let len = (((bytes[0] & 0x0F) as usize) << 8) | bytes[1] as usize;
                self.rx_len = Some(len);
                self.rx_buf = bytes[2..].to_vec();
                self.rx_seq = 1;
                self.finish();
            }
            2 => {
                let Some(len) = self.rx_len else {
                    self.finish();
                    return;
                };
                let seq = bytes[0] & 0x0F;
                if seq != (self.rx_seq & 0x0F) {
                    self.rx_len = None;
                    self.emit("CAN ERROR");
                    self.finish();
                    return;
                }
                self.rx_seq = self.rx_seq.wrapping_add(1);
                self.rx_buf.extend_from_slice(&bytes[1..]);
                if self.rx_buf.len() >= len {
                    self.rx_buf.truncate(len);
                    let payload = std::mem::take(&mut self.rx_buf);
                    self.rx_len = None;
                    self.answer(&payload);
                } else {
                    self.finish();
                }
            }
            3 => self.finish(),
            _ => {
                self.emit("CAN ERROR");
                self.finish();
            }
        }
    }

    fn answer(&mut self, payload: &[u8]) {
        let Some(ecu) = self.find_ecu() else {
            self.emit("NO DATA");
            self.finish();
            return;
        };
        let Some(response) = ecu.respond(payload) else {
            self.emit("NO DATA");
            self.finish();
            return;
        };
        if self.protocol == SimProtocol::Kwp {
            let line = if self.spaces {
                hexutil::to_hex_spaced(&response)
            } else {
                hexutil::to_hex(&response)
            };
            self.emit(&line);
            self.finish();
            return;
        }
        for frame in response_frames(&response) {
            let line = if self.spaces {
                hexutil::to_hex_spaced(&frame)
            } else {
                hexutil::to_hex(&frame)
            };
            self.emit(&line);
        }
        self.finish();
    }

    fn handle_kwp(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(ecu) = self.find_ecu() {
            if let Some(resp) = ecu.respond(bytes) {
                let line = if self.spaces {
                    hexutil::to_hex_spaced(&resp)
                } else {
                    hexutil::to_hex(&resp)
                };
                self.emit(&line);
                self.finish();
                return;
            }
        }
        self.emit("NO DATA");
        self.finish();
    }

    fn handle_at(&mut self, cmd: &str) {
        let compact = cmd.replace(' ', "").to_ascii_uppercase();
        if compact == "ATZ" {
            self.header.clear();
            self.cp_prefix.clear();
            self.filter.clear();
            self.protocol = SimProtocol::None;
            self.emit("ELM327 v1.5");
            self.finish();
        } else if compact == "ATI" {
            self.emit("ELM327 v1.5");
            self.finish();
        } else if compact == "AT@1" {
            self.emit("DDTRust virtual ELM327");
            self.finish();
        } else if compact == "ATDPN" {
            self.emit("A6");
            self.finish();
        } else if compact == "ATDP" {
            self.emit("ISO 15765-4 (CAN 11/500)");
            self.finish();
        } else if compact == "ATE0" {
            self.echo = false;
            self.respond_ok();
        } else if compact == "ATE1" {
            self.echo = true;
            self.respond_ok();
        } else if compact == "ATS0" {
            self.spaces = false;
            self.respond_ok();
        } else if compact == "ATS1" {
            self.spaces = true;
            self.respond_ok();
        } else if compact == "ATL0" {
            self.linefeeds = false;
            self.respond_ok();
        } else if compact == "ATL1" {
            self.linefeeds = true;
            self.respond_ok();
        } else if compact == "ATH0" {
            self.headers = false;
            self.respond_ok();
        } else if compact == "ATH1" {
            self.headers = true;
            self.respond_ok();
        } else if compact == "ATPC" || compact == "ATWS" || compact == "ATD" {
            self.protocol = SimProtocol::None;
            self.respond_ok();
        } else if compact.starts_with("ATSH") {
            let sh = compact[4..].to_string();
            self.header = if self.cp_prefix.len() == 2 {
                format!("{}{}", self.cp_prefix, sh)
            } else {
                sh
            };
            self.respond_ok();
        } else if compact.starts_with("ATCP") {
            self.cp_prefix = compact[4..].to_string();
            self.respond_ok();
        } else if compact.starts_with("ATCRA") {
            self.filter = compact[5..].to_string();
            self.respond_ok();
        } else if compact.starts_with("ATSP") {
            self.protocol = match compact[4..].trim() {
                "4" | "5" => SimProtocol::Kwp,
                "6" | "7" | "8" | "9" => SimProtocol::Can,
                _ => SimProtocol::None,
            };
            self.respond_ok();
        } else if compact.starts_with("ATFCSH")
            || compact.starts_with("ATFCSD")
            || compact.starts_with("ATFCSM")
            || compact.starts_with("ATIB")
            || compact.starts_with("ATST")
            || compact.starts_with("ATSW")
            || compact.starts_with("ATWM")
            || compact.starts_with("ATAT")
            || compact.starts_with("ATAL")
            || compact.starts_with("ATCAF")
            || compact.starts_with("ATCFC")
            || compact == "ATSI"
            || compact == "ATFI"
            || compact.starts_with("ST")
        {
            if compact == "ATSI" || compact == "ATFI" {
                self.protocol = SimProtocol::Kwp;
            }
            self.respond_ok();
        } else {
            self.emit("?");
            self.finish();
        }
    }

    fn process(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        if self.echo {
            self.emit(cmd);
        }
        let upper = cmd.to_ascii_uppercase();
        if upper.starts_with("AT") || upper.starts_with("ST") {
            self.handle_at(cmd);
            return;
        }
        let clean: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
        if clean.is_empty() || !clean.chars().all(|c| c.is_ascii_hexdigit()) || clean.len() % 2 != 0
        {
            self.emit("?");
            self.finish();
            return;
        }
        let Ok(bytes) = hexutil::parse_hex(&clean) else {
            self.emit("?");
            self.finish();
            return;
        };
        match self.protocol {
            SimProtocol::Can => {
                let _ = &self.cp_prefix;
                self.handle_can(&bytes);
            }
            SimProtocol::Kwp => self.handle_kwp(&bytes),
            SimProtocol::None => {
                self.emit("CAN ERROR");
                self.finish();
            }
        }
    }
}

impl Transport for EmulatedElm {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        for &b in data {
            if b == b'\r' || b == b'\n' {
                let cmd = String::from_utf8_lossy(&self.command).to_string();
                self.command.clear();
                if !cmd.trim().is_empty() {
                    self.process(&cmd);
                }
            } else {
                self.command.push(b);
            }
        }
        Ok(())
    }

    fn read_byte(&mut self, _timeout: Duration) -> Result<Option<u8>> {
        Ok(self.out.pop_front())
    }

    fn flush(&mut self) -> Result<()> {
        self.out.clear();
        self.command.clear();
        Ok(())
    }

    fn set_timeout(&mut self, _timeout: Duration) -> Result<()> {
        Ok(())
    }

    fn description(&self) -> String {
        "simulated ELM327".into()
    }
}
