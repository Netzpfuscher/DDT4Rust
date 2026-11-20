use indexmap::IndexMap;

use crate::error::{Error, Result};
use crate::hexutil;
use crate::model::{EcuFile, EcuRequest};

use super::session::EcuSession;

#[derive(Debug, Clone, Default)]
pub struct DtcField {
    pub name: String,
    pub value: Option<String>,
    pub raw_hex: Option<String>,
    pub unit: String,
}

#[derive(Debug, Clone, Default)]
pub struct DtcRecord {
    pub index: usize,
    pub code: Option<u32>,
    pub name: Option<String>,
    pub fields: Vec<DtcField>,
    pub raw: Vec<u8>,
}

impl DtcRecord {
    pub fn code_text(&self) -> String {
        self.code
            .map(|c| format!("{c:06X}"))
            .or_else(|| self.fields.iter().find_map(|f| f.raw_hex.clone()))
            .unwrap_or_else(|| "?".into())
    }
}

#[derive(Debug, Clone, Default)]
pub struct DtcReport {
    pub request: String,
    pub records: Vec<DtcRecord>,
    pub raw: Vec<u8>,
    pub count: usize,
}

impl DtcReport {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

fn find_request<'a>(ecu: &'a EcuFile, names: &[&str], contains: &str) -> Option<&'a EcuRequest> {
    for name in names {
        if let Some(r) = ecu.request(name) {
            return Some(r);
        }
    }
    ecu.requests.values().find(|r| {
        r.name.to_ascii_lowercase().contains(contains) && r.sentbytes.first() == Some(&0x19)
    })
}

fn read_request<'a>(ecu: &'a EcuFile) -> Option<&'a EcuRequest> {
    find_request(
        ecu,
        &[
            "ReadDTCInformation.ReportDTC",
            "ReadDTC",
            "ReadDTCInformation.ReportDTCByStatusMask",
        ],
        "readdtc",
    )
}

fn clear_request<'a>(ecu: &'a EcuFile) -> Option<&'a EcuRequest> {
    find_request(
        ecu,
        &[
            "ClearDiagnosticInformation.All",
            "ClearDTC",
            "Clear Diagnostic Information",
        ],
        "cleardtc",
    )
}

fn record_extent(req: &EcuRequest, ecu: &EcuFile) -> usize {
    let mut max = 0;
    for (name, item) in &req.receivebyte_dataitems {
        if let Some(def) = ecu.data.get(name) {
            max = max.max(item.byte_extent(def));
        }
    }
    max
}

pub fn read_dtc(session: &mut EcuSession) -> Result<DtcReport> {
    let req = read_request(&session.ecu)
        .ok_or_else(|| Error::Unsupported("this ECU has no ReadDTC request".into()))?
        .clone();
    let mut payload = req.sentbytes.clone();
    if payload.is_empty() {
        return Err(Error::Protocol("ReadDTC request has no sent bytes".into()));
    }
    let _ = session.ensure_session();
    let mut raw = session.send_payload(&payload, false)?;
    let mut more: Option<Vec<u8>> = None;

    if let Some(item) = req
        .sendbyte_dataitems
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("MoreDTC"))
        .map(|(_, v)| v)
    {
        if let Some(fb) = item.firstbyte.checked_sub(1) {
            if (fb as usize) < payload.len() {
                payload[fb as usize] = 0xFF;
                more = Some(payload.clone());
            }
        }
    }
    if let Some(more) = more {
        for _ in 0..50 {
            match session.send_payload(&more, false) {
                Ok(rsp) => {
                    if rsp.len() < 3 || rsp[0] == 0x7F {
                        break;
                    }
                    raw.extend_from_slice(&rsp[2..]);
                    if rsp.len() <= 2 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }

    let start = if raw.len() >= 2 && raw[0] == 0x59 && raw[1] == 0x02 {
        3
    } else {
        2
    };
    let mut shift = req.shiftbytescount as usize;
    if shift == 0 {
        let extent = record_extent(&req, &session.ecu);
        shift = extent.saturating_sub(start).max(1);
    }
    let count = if raw.len() >= 2 && raw[0] == 0x59 && raw[1] == 0x02 {
        if shift == 0 {
            0
        } else {
            raw.len().saturating_sub(3) / shift
        }
    } else if raw.len() >= 2 {
        raw[1] as usize
    } else {
        0
    };
    let count = count.min(100);

    let mut records = Vec::new();
    let mut slice = raw.clone();
    for index in 0..count {
        if slice.len() <= start {
            break;
        }
        let mut fields = Vec::new();
        let mut code = None;
        for (name, item) in &req.receivebyte_dataitems {
            if name.eq_ignore_ascii_case("NDTC") {
                continue;
            }
            let Some(def) = session.ecu.data.get(name).or_else(|| {
                session
                    .ecu
                    .data
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v)
            }) else {
                continue;
            };
            fields.push(DtcField {
                name: name.clone(),
                value: def.display_value(&slice, item, session.ecu.endian),
                raw_hex: def
                    .get_hex_value(&slice, item, session.ecu.endian)
                    .map(|s| s.to_ascii_uppercase()),
                unit: def.unit.clone(),
            });
            if code.is_none()
                && (name.eq_ignore_ascii_case("DTC") || name.to_ascii_uppercase().contains("DTC"))
            {
                if let Some(v) = def.int_value(&slice, item, session.ecu.endian) {
                    code = Some(v as u32);
                }
            }
        }
        if code.is_none() && slice.len() >= start + 3 {
            code = Some(
                ((slice[start] as u32) << 16)
                    | ((slice[start + 1] as u32) << 8)
                    | slice[start + 2] as u32,
            );
        }
        let name = code.and_then(|c| session.ecu.device_for_dtc(c).map(|d| d.name.clone()));
        let end = (start + shift).min(slice.len());
        let raw_record = slice[start..end].to_vec();
        records.push(DtcRecord {
            index,
            code,
            name,
            fields,
            raw: raw_record,
        });
        if slice.len() <= shift {
            break;
        }
        slice = slice[shift..].to_vec();
    }

    Ok(DtcReport {
        request: req.name,
        count,
        records,
        raw,
    })
}

pub fn clear_dtc(session: &mut EcuSession) -> Result<Vec<u8>> {
    let payload = match clear_request(&session.ecu) {
        Some(r) if !r.sentbytes.is_empty() => r.sentbytes.clone(),
        _ => vec![0x14, 0xFF, 0x00],
    };
    let _ = session.ensure_session();
    session.send_payload(&payload, false)
}

pub fn dtc_summary(report: &DtcReport) -> String {
    if report.records.is_empty() {
        return "No DTC".into();
    }
    let mut s = String::new();
    for r in &report.records {
        let fields: Vec<String> = r
            .fields
            .iter()
            .map(|f| {
                format!(
                    "{}={}{}",
                    f.name,
                    f.value.clone().unwrap_or_else(|| "?".into()),
                    if f.unit.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", f.unit)
                    }
                )
            })
            .collect();
        s.push_str(&format!(
            "#{} {} {} [{}]\n",
            r.index + 1,
            r.name.clone().unwrap_or_default(),
            r.code_text(),
            fields.join(", ")
        ));
    }
    s
}

pub fn parsed_fields_json(records: &[DtcRecord]) -> Vec<IndexMap<String, String>> {
    records
        .iter()
        .map(|r| {
            let mut m = IndexMap::new();
            m.insert("code".into(), r.code_text());
            if let Some(n) = &r.name {
                m.insert("name".into(), n.clone());
            }
            for f in &r.fields {
                m.insert(
                    f.name.clone(),
                    f.value.clone().unwrap_or_else(|| hexutil::to_hex(&r.raw)),
                );
            }
            m
        })
        .collect()
}
