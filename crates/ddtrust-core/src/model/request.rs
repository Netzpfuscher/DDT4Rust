use indexmap::IndexMap;
use serde::Deserialize;

use super::{EcuData, Endian};
use crate::error::{Error, Result};
use crate::hexutil;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DataItemSpec {
    #[serde(default)]
    pub firstbyte: u32,
    #[serde(default)]
    pub bitoffset: u32,
    #[serde(default)]
    pub r#ref: bool,
    #[serde(default)]
    pub endian: String,
}

#[derive(Debug, Clone)]
pub struct DataItem {
    pub name: String,
    pub firstbyte: u32,
    pub bitoffset: u32,
    pub r#ref: bool,
    pub endian: Option<Endian>,
}

impl DataItem {
    pub fn from_spec(name: impl Into<String>, spec: &DataItemSpec) -> Self {
        Self {
            name: name.into(),
            firstbyte: spec.firstbyte,
            bitoffset: spec.bitoffset,
            r#ref: spec.r#ref,
            endian: match spec.endian.as_str() {
                "Little" | "little" => Some(Endian::Little),
                "Big" | "big" => Some(Endian::Big),
                _ => None,
            },
        }
    }

    pub fn byte_extent(&self, data: &EcuData) -> usize {
        let start = self.firstbyte.saturating_sub(1) as usize;
        start + (data.bitscount as usize + self.bitoffset as usize).div_ceil(8)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EcuRequestSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub minbytes: Option<f64>,
    #[serde(default)]
    pub shiftbytescount: Option<f64>,
    #[serde(default)]
    pub replybytes: Option<String>,
    #[serde(default)]
    pub manualsend: bool,
    #[serde(default)]
    pub sentbytes: Option<String>,
    #[serde(default)]
    pub deny_sds: Vec<String>,
    #[serde(default)]
    pub sendbyte_dataitems: IndexMap<String, DataItemSpec>,
    #[serde(default)]
    pub receivebyte_dataitems: IndexMap<String, DataItemSpec>,
}

#[derive(Debug, Clone)]
pub struct EcuRequest {
    pub name: String,
    pub minbytes: u32,
    pub shiftbytescount: u32,
    pub replybytes: Vec<u8>,
    pub manualsend: bool,
    pub sentbytes: Vec<u8>,
    pub deny_sds: Vec<String>,
    pub sendbyte_dataitems: IndexMap<String, DataItem>,
    pub receivebyte_dataitems: IndexMap<String, DataItem>,
}

#[derive(Debug, Clone, Default)]
pub struct DecodedValue {
    pub name: String,
    pub display: Option<String>,
    pub raw_hex: Option<String>,
    pub unit: String,
}

impl EcuRequest {
    pub fn from_spec(spec: &EcuRequestSpec) -> Result<Self> {
        let sentbytes = match spec.sentbytes.as_deref() {
            Some(s) if !s.trim().is_empty() => hexutil::parse_hex(s)?,
            _ => Vec::new(),
        };
        let replybytes = match spec.replybytes.as_deref() {
            Some(s) if !s.trim().is_empty() => hexutil::parse_hex(s).unwrap_or_default(),
            _ => Vec::new(),
        };
        Ok(Self {
            name: spec.name.clone(),
            minbytes: spec.minbytes.unwrap_or(0.0) as u32,
            shiftbytescount: spec.shiftbytescount.unwrap_or(0.0) as u32,
            replybytes,
            manualsend: spec.manualsend,
            sentbytes,
            deny_sds: spec.deny_sds.clone(),
            sendbyte_dataitems: spec
                .sendbyte_dataitems
                .iter()
                .map(|(k, v)| (k.clone(), DataItem::from_spec(k, v)))
                .collect(),
            receivebyte_dataitems: spec
                .receivebyte_dataitems
                .iter()
                .map(|(k, v)| (k.clone(), DataItem::from_spec(k, v)))
                .collect(),
        })
    }

    pub fn sent_hex(&self) -> String {
        hexutil::to_hex_spaced(&self.sentbytes)
    }

    pub fn data_item(&self, name: &str) -> Option<&DataItem> {
        self.sendbyte_dataitems.get(name).or_else(|| {
            self.sendbyte_dataitems
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    pub fn receive_item(&self, name: &str) -> Option<&DataItem> {
        self.receivebyte_dataitems.get(name).or_else(|| {
            self.receivebyte_dataitems
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    /// Build the byte stream to send, applying the given user inputs in order.
    /// Values may be display labels from EcuData::items.
    pub fn build_stream(
        &self,
        inputs: &[(String, String)],
        data: &IndexMap<String, EcuData>,
        endian: Endian,
    ) -> Result<Vec<u8>> {
        let mut stream = self.sentbytes.clone();
        for (name, value) in inputs {
            let item = self.data_item(name).ok_or_else(|| {
                Error::Invalid(format!("request {}: unknown data item {name}", self.name))
            })?;
            let definition = data
                .get(name)
                .or_else(|| data.get(&item.name))
                .or_else(|| {
                    data.iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(name))
                        .map(|(_, v)| v)
                })
                .ok_or_else(|| {
                    Error::Invalid(format!("request {}: no data definition {name}", self.name))
                })?;
            if let Some(raw) = definition.items.get(value) {
                // A display label was selected: encode its raw integer directly.
                definition.set_raw_value(
                    &num_bigint::BigUint::from(*raw as u64),
                    &mut stream,
                    item,
                    endian,
                )?;
            } else {
                definition.set_value(value, &mut stream, item, endian)?;
            }
        }
        Ok(stream)
    }

    pub fn decode(
        &self,
        response: &[u8],
        data: &IndexMap<String, EcuData>,
        endian: Endian,
    ) -> Vec<DecodedValue> {
        let mut out = Vec::with_capacity(self.receivebyte_dataitems.len());
        for (name, item) in &self.receivebyte_dataitems {
            let definition = data.get(name).or_else(|| data.get(&item.name)).or_else(|| {
                data.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v)
            });
            match definition {
                Some(def) => {
                    let display = def.display_value(response, item, endian);
                    let raw_hex = def
                        .get_hex_value(response, item, endian)
                        .map(|h| h.to_ascii_uppercase());
                    out.push(DecodedValue {
                        name: name.clone(),
                        display,
                        raw_hex,
                        unit: def.unit.clone(),
                    });
                }
                None => out.push(DecodedValue {
                    name: name.clone(),
                    display: None,
                    raw_hex: None,
                    unit: String::new(),
                }),
            }
        }
        out
    }

    pub fn expected_response_len(&self, data: &IndexMap<String, EcuData>) -> usize {
        let mut len = self.minbytes as usize;
        for (name, item) in &self.receivebyte_dataitems {
            if let Some(def) = data.get(name) {
                len = len.max(item.byte_extent(def));
            }
        }
        len
    }

    pub fn is_read_only(&self) -> bool {
        matches!(
            self.sentbytes.first().copied(),
            Some(0x10 | 0x12 | 0x14 | 0x17 | 0x19 | 0x1A | 0x21 | 0x22 | 0x23 | 0x3E)
        )
    }
}
