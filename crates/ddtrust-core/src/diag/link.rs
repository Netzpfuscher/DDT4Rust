use crate::elm::{ActiveProtocol, Elm327};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct CanSpec {
    pub tx_id: String,
    pub rx_id: String,
    pub baud: u32,
}

#[derive(Debug, Clone, Default)]
pub struct KwpSpec {
    pub address: String,
    pub fast_init: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LinkInfo {
    pub description: String,
    pub protocol: String,
    pub tx_id: String,
    pub rx_id: String,
    pub baud: u32,
}

/// Transport-level diagnostic link. Both the real ELM327 and the simulator
/// implement this; higher layers are hardware independent.
pub trait DiagnosticLink: Send {
    fn info(&self) -> LinkInfo;
    fn open_can(&mut self, spec: &CanSpec) -> Result<()>;
    fn open_kwp(&mut self, spec: &KwpSpec) -> Result<()>;
    fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>>;
    fn raw_command(&mut self, command: &str) -> Result<String>;
    fn close(&mut self);

    fn start_session(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let rsp = self.request(payload)?;
        if rsp.is_empty() {
            return Err(Error::NoResponse);
        }
        if rsp[0] == 0x7F {
            let code = rsp.get(2).copied().unwrap_or(0);
            return Err(Error::negative(payload[0], code));
        }
        if rsp[0] == payload[0].wrapping_add(0x40) {
            Ok(rsp)
        } else {
            Err(Error::Protocol(format!(
                "unexpected session response {}",
                crate::hexutil::to_hex_spaced(&rsp)
            )))
        }
    }
}

impl DiagnosticLink for Elm327 {
    fn info(&self) -> LinkInfo {
        LinkInfo {
            description: self.description(),
            protocol: match self.protocol() {
                ActiveProtocol::Can => format!("CAN @{}k", self.baud() / 1000),
                ActiveProtocol::Kwp => "KWP2000".into(),
                ActiveProtocol::None => "unconfigured".into(),
            },
            tx_id: self.tx_id().to_string(),
            rx_id: self.rx_id().to_string(),
            baud: self.baud(),
        }
    }

    fn open_can(&mut self, spec: &CanSpec) -> Result<()> {
        let baud = if spec.baud == 0 { 500_000 } else { spec.baud };
        self.set_can(&spec.tx_id, &spec.rx_id, baud).map(|_| ())
    }

    fn open_kwp(&mut self, spec: &KwpSpec) -> Result<()> {
        self.set_kwp(&spec.address, spec.fast_init)
    }

    fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        Elm327::request(self, payload)
    }

    fn raw_command(&mut self, command: &str) -> Result<String> {
        self.raw(command)
    }

    fn close(&mut self) {
        let _ = Elm327::close(self);
    }
}
