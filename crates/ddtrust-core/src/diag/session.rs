use std::sync::Arc;

use crate::error::{Error, Result};
use crate::hexutil;
use crate::model::{EcuFile, EcuRequest, Protocol};

use super::dtc::{clear_dtc, read_dtc, DtcReport};
use super::link::{CanSpec, DiagnosticLink, DoipSpec, KwpSpec, LinkInfo};
use super::safety::Safety;
use crate::model::request::DecodedValue;

#[derive(Debug, Clone, Default)]
pub struct SessionOpen {
    pub info: LinkInfo,
    pub session_response: Option<Vec<u8>>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ReadResult {
    pub request: String,
    pub raw: Vec<u8>,
    pub values: Vec<DecodedValue>,
}

pub struct EcuSession {
    pub link: Box<dyn DiagnosticLink>,
    pub ecu: Arc<EcuFile>,
    pub safety: Safety,
    pub open: SessionOpen,
    session_payload: Option<Vec<u8>>,
}

impl EcuSession {
    pub fn open(
        mut link: Box<dyn DiagnosticLink>,
        ecu: Arc<EcuFile>,
        safety: Safety,
    ) -> Result<Self> {
        let mut warnings = Vec::new();
        if link.is_doip() {
            if matches!(ecu.protocol, Protocol::Kwp2000 | Protocol::Iso8) {
                return Err(Error::Unsupported(
                    "KWP2000/K-line ECUs cannot be reached through DoIP".into(),
                ));
            }
            let target_address = doip_logical_address(&ecu.funcaddr)
                .or_else(|| doip_logical_address(&ecu.send_id))
                .ok_or_else(|| {
                    Error::Protocol(format!(
                        "{} has no DoIP logical address (funcaddr is empty)",
                        ecu.ecuname
                    ))
                })?;
            link.open_doip(&DoipSpec {
                target_address,
                tester_address: None,
                timeout: None,
            })?;
        } else {
            match ecu.protocol {
                Protocol::Can => {
                    if ecu.send_id.is_empty() {
                        return Err(Error::Protocol(format!(
                            "{} has no CAN send id",
                            ecu.ecuname
                        )));
                    }
                    link.open_can(&CanSpec {
                        tx_id: ecu.send_id.clone(),
                        rx_id: ecu.recv_id.clone(),
                        baud: if ecu.baudrate == 0 {
                            500_000
                        } else {
                            ecu.baudrate
                        },
                    })?;
                }
                Protocol::Kwp2000 | Protocol::Iso8 => {
                    if ecu.funcaddr.is_empty() {
                        return Err(Error::Protocol(format!(
                            "{} has no KWP address",
                            ecu.ecuname
                        )));
                    }
                    link.open_kwp(&KwpSpec {
                        address: ecu.funcaddr.clone(),
                        fast_init: ecu.fastinit,
                    })?;
                }
                Protocol::Doip => {
                    return Err(Error::Unsupported(
                        "this ECU requires a DoIP adapter".into(),
                    ))
                }
                ref p => {
                    return Err(Error::Unsupported(format!(
                        "protocol {p} is not supported yet (CAN, KWP2000, ISO8 and DoIP are)"
                    )))
                }
            }
        }
        let session_payload = vec![0x10, 0xC0];
        let session_response = match link.start_session(&session_payload) {
            Ok(r) => Some(r),
            Err(e) => {
                warnings.push(format!("could not open 10C0 session: {e}"));
                None
            }
        };
        let info = link.info();
        Ok(Self {
            link,
            ecu,
            safety,
            open: SessionOpen {
                info,
                session_response,
                warnings,
            },
            session_payload: Some(session_payload),
        })
    }

    pub fn info(&self) -> &LinkInfo {
        &self.open.info
    }

    pub fn session_payload(&self) -> Option<&[u8]> {
        self.session_payload.as_deref()
    }

    pub fn ensure_session(&mut self) -> Result<()> {
        if let Some(p) = self.session_payload.clone() {
            self.link.start_session(&p)?;
        }
        Ok(())
    }

    /// Send a raw payload. Negative responses are converted into errors.
    pub fn send_payload(&mut self, payload: &[u8], write: bool) -> Result<Vec<u8>> {
        if !write {
            self.safety.check(payload)?;
        } else if !self.safety.expert {
            return Err(Error::Safety(
                "writing/actuator requests require expert mode".into(),
            ));
        }
        let rsp = self.link.request(payload)?;
        self.check_negative(payload, &rsp)?;
        Ok(rsp)
    }

    pub fn check_negative(&self, request: &[u8], response: &[u8]) -> Result<()> {
        if response.len() >= 3 && response[0] == 0x7F {
            let code = response[2];
            return Err(Error::negative(request.first().copied().unwrap_or(0), code));
        }
        Ok(())
    }

    pub fn read_request(
        &mut self,
        request: &EcuRequest,
        inputs: &[(String, String)],
    ) -> Result<ReadResult> {
        if request.sentbytes.is_empty() {
            return Err(Error::Protocol(format!(
                "request {} has no sent bytes",
                request.name
            )));
        }
        let stream = request.build_stream(inputs, &self.ecu.data, self.ecu.endian)?;
        self.safety.check(&stream)?;
        let rsp = self.link.request(&stream)?;
        self.check_negative(&stream, &rsp)?;
        let values = request.decode(&rsp, &self.ecu.data, self.ecu.endian);
        Ok(ReadResult {
            request: request.name.clone(),
            raw: rsp,
            values,
        })
    }

    pub fn read_named(&mut self, name: &str, inputs: &[(String, String)]) -> Result<ReadResult> {
        let req = self
            .ecu
            .request(name)
            .ok_or_else(|| Error::Missing(format!("request {name}")))?
            .clone();
        self.read_request(&req, inputs)
    }

    pub fn send_named(&mut self, name: &str, inputs: &[(String, String)]) -> Result<Vec<u8>> {
        let req = self
            .ecu
            .request(name)
            .ok_or_else(|| Error::Missing(format!("request {name}")))?
            .clone();
        let stream = req.build_stream(inputs, &self.ecu.data, self.ecu.endian)?;
        self.send_payload(&stream, true)
    }

    pub fn read_dtc(&mut self) -> Result<DtcReport> {
        read_dtc(self)
    }

    pub fn clear_dtc(&mut self) -> Result<Vec<u8>> {
        clear_dtc(self)
    }

    pub fn raw_command(&mut self, cmd: &str) -> Result<String> {
        self.link.raw_command(cmd)
    }

    pub fn last_exchange_hex(&self) -> String {
        hexutil::to_hex_spaced(&self.open.session_response.clone().unwrap_or_default())
    }
}

fn doip_logical_address(value: &str) -> Option<u16> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        return u16::from_str_radix(hex, 16).ok();
    }
    u16::from_str_radix(v, 16).ok()
}
