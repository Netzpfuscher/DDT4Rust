use std::fmt;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

use super::data::{EcuData, EcuDataSpec};
use super::request::{EcuRequest, EcuRequestSpec};
use super::Endian;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    Can,
    Kwp2000,
    Iso8,
    Doip,
    Unknown(String),
}

impl Protocol {
    pub fn parse(s: &str) -> Protocol {
        let u = s.trim().to_ascii_uppercase();
        if u.contains("CAN") && !u.contains("DOIP") {
            Protocol::Can
        } else if u.contains("KWP") || u == "ISO" {
            Protocol::Kwp2000
        } else if u.contains("ISO8") {
            Protocol::Iso8
        } else if u.contains("DOIP") {
            Protocol::Doip
        } else {
            Protocol::Unknown(s.to_string())
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Protocol::Can => "CAN",
            Protocol::Kwp2000 => "KWP2000",
            Protocol::Iso8 => "ISO8",
            Protocol::Doip => "DOIP",
            Protocol::Unknown(s) => s,
        }
    }

    pub fn is_can(&self) -> bool {
        matches!(self, Protocol::Can)
    }

    pub fn is_kwp(&self) -> bool {
        matches!(self, Protocol::Kwp2000 | Protocol::Iso8)
    }
}

impl Default for Protocol {
    fn default() -> Self {
        Protocol::Unknown(String::new())
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EcuIdentSpec {
    #[serde(default, alias = "diagnostic_version")]
    pub diagversion: String,
    #[serde(default, alias = "supplier_code")]
    pub supplier: String,
    #[serde(default, alias = "soft_version")]
    pub soft: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Default)]
pub struct EcuIdent {
    pub diagversion: String,
    pub supplier: String,
    pub soft: String,
    pub version: String,
}

#[derive(Debug, Clone, Default)]
pub struct EcuDevice {
    pub name: String,
    pub dtc: u64,
    pub dtctype: u64,
    pub devicedata: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ObdSpec {
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub send_id: String,
    #[serde(default)]
    pub recv_id: String,
    #[serde(default)]
    pub baudrate: Option<f64>,
    #[serde(default)]
    pub funcaddr: String,
    #[serde(default)]
    pub funcname: String,
    #[serde(default)]
    pub fastinit: bool,
    #[serde(default)]
    pub kw1: String,
    #[serde(default)]
    pub kw2: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeviceSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub dtc: u64,
    #[serde(default)]
    pub dtctype: u64,
    #[serde(default)]
    pub devicedata: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EcuFileSpec {
    #[serde(default)]
    pub obd: ObdSpec,
    #[serde(default)]
    pub endian: String,
    #[serde(default)]
    pub ecuname: String,
    #[serde(default)]
    pub autoidents: Vec<EcuIdentSpec>,
    #[serde(default)]
    pub data: IndexMap<String, EcuDataSpec>,
    #[serde(default)]
    pub requests: Vec<EcuRequestSpec>,
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
}

#[derive(Debug, Clone)]
pub struct EcuFile {
    pub href: String,
    pub ecuname: String,
    pub protocol: Protocol,
    pub endian: Endian,
    pub endian_str: String,
    pub send_id: String,
    pub recv_id: String,
    pub funcaddr: String,
    pub funcname: String,
    pub baudrate: u32,
    pub fastinit: bool,
    pub kw1: String,
    pub kw2: String,
    pub data: IndexMap<String, EcuData>,
    pub requests: IndexMap<String, EcuRequest>,
    pub devices: Vec<EcuDevice>,
    pub autoidents: Vec<EcuIdent>,
}

impl EcuFile {
    pub fn from_json(href: impl Into<String>, text: &str) -> Result<Self> {
        let spec: EcuFileSpec = serde_json::from_str(text)?;
        let protocol = Protocol::parse(&spec.obd.protocol);
        let endian = Endian::parse(&spec.endian);
        let mut requests = IndexMap::new();
        for r in &spec.requests {
            if r.name.is_empty() {
                continue;
            }
            requests.insert(r.name.clone(), EcuRequest::from_spec(r)?);
        }
        let data = spec
            .data
            .iter()
            .map(|(k, v)| (k.clone(), EcuData::from_spec(k, v)))
            .collect();
        let devices = spec
            .devices
            .iter()
            .map(|d| EcuDevice {
                name: d.name.clone(),
                dtc: d.dtc,
                dtctype: d.dtctype,
                devicedata: d.devicedata.clone(),
            })
            .collect();
        let autoidents = spec
            .autoidents
            .iter()
            .map(|a| EcuIdent {
                diagversion: a.diagversion.clone(),
                supplier: a.supplier.clone(),
                soft: a.soft.clone(),
                version: a.version.clone(),
            })
            .collect();
        Ok(Self {
            href: href.into(),
            ecuname: spec.ecuname,
            protocol,
            endian,
            endian_str: spec.endian,
            send_id: spec.obd.send_id.trim().to_ascii_uppercase(),
            recv_id: spec.obd.recv_id.trim().to_ascii_uppercase(),
            funcaddr: spec.obd.funcaddr.trim().to_ascii_uppercase(),
            funcname: spec.obd.funcname,
            baudrate: spec.obd.baudrate.unwrap_or(500000.0) as u32,
            fastinit: spec.obd.fastinit,
            kw1: spec.obd.kw1,
            kw2: spec.obd.kw2,
            data,
            requests,
            devices,
            autoidents,
        })
    }

    pub fn request(&self, name: &str) -> Option<&EcuRequest> {
        self.requests.get(name).or_else(|| {
            self.requests
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    pub fn request_name(&self, name: &str) -> Option<&String> {
        self.requests
            .get_key_value(name)
            .map(|(k, _)| k)
            .or_else(|| self.requests.keys().find(|k| k.eq_ignore_ascii_case(name)))
    }

    pub fn data(&self, name: &str) -> Option<&EcuData> {
        self.data.get(name).or_else(|| {
            self.data
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    pub fn is_extended_id(&self) -> bool {
        self.send_id.len() == 8
    }

    /// A request that ddt4all uses to keep the diagnostic session alive.
    pub fn tester_present(&self) -> Option<&EcuRequest> {
        self.requests
            .iter()
            .find(|(k, _)| {
                k.to_lowercase().contains("tester") && k.to_lowercase().contains("present")
            })
            .map(|(_, v)| v)
    }

    pub fn session_requests(&self) -> Vec<&EcuRequest> {
        self.requests
            .iter()
            .filter(|(k, r)| {
                let upper = k.to_uppercase();
                upper.contains("START")
                    && upper.contains("DIAG")
                    && upper.contains("SESSION")
                    && !r.sentbytes.is_empty()
            })
            .map(|(_, r)| r)
            .collect()
    }

    pub fn device_for_dtc(&self, code: u32) -> Option<&EcuDevice> {
        self.devices.iter().find(|d| {
            d.dtc == code as u64
                || d.devicedata.get("DTC").and_then(|v| v.as_u64()) == Some(code as u64)
        })
    }

    pub fn summary(&self) -> String {
        format!(
            "{} [{}] {} @ {} ({} requests, {} data)",
            self.ecuname,
            self.href,
            self.protocol,
            self.funcaddr,
            self.requests.len(),
            self.data.len()
        )
    }
}
