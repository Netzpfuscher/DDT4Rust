use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use num_bigint::BigUint;

use crate::elm::isotp;
use crate::model::{EcuFile, Endian, Target};

#[derive(Debug, Clone, Default)]
pub struct SimIdent {
    pub diagversion: String,
    pub supplier: String,
    pub soft: String,
    pub version: String,
}

/// A virtual ECU that answers Renault diagnostic requests with synthetic data.
pub struct VirtualEcu {
    pub tx_id: String,
    pub address: String,
    pub name: String,
    pub ecu: Option<Arc<EcuFile>>,
    pub ident: SimIdent,
    pub dtcs: Vec<u32>,
    start: Instant,
}

impl VirtualEcu {
    pub fn from_target(target: &Target, tx_id: impl Into<String>) -> Self {
        let ident = target
            .autoidents
            .first()
            .map(|a| SimIdent {
                diagversion: a.diagnostic_version.clone(),
                supplier: a.supplier_code.clone(),
                soft: a.soft_version.clone(),
                version: a.version.clone(),
            })
            .unwrap_or_default();
        Self {
            tx_id: tx_id.into().to_ascii_uppercase(),
            address: target.address.clone(),
            name: target.ecuname.clone(),
            ecu: None,
            ident,
            dtcs: Vec::new(),
            start: Instant::now(),
        }
    }

    pub fn from_file(target: &Target, ecu: Arc<EcuFile>, tx_id: impl Into<String>) -> Self {
        let ident = ecu
            .autoidents
            .first()
            .map(|a| SimIdent {
                diagversion: a.diagversion.clone(),
                supplier: a.supplier.clone(),
                soft: a.soft.clone(),
                version: a.version.clone(),
            })
            .unwrap_or_else(|| {
                target
                    .autoidents
                    .first()
                    .map(|a| SimIdent {
                        diagversion: a.diagnostic_version.clone(),
                        supplier: a.supplier_code.clone(),
                        soft: a.soft_version.clone(),
                        version: a.version.clone(),
                    })
                    .unwrap_or_default()
            });
        let mut dtcs: Vec<u32> = ecu
            .devices
            .iter()
            .map(|d| d.dtc as u32)
            .filter(|c| *c != 0)
            .collect();
        dtcs.sort_unstable();
        dtcs.dedup();
        if dtcs.is_empty() {
            dtcs = vec![0x123456, 0x234567, 0x345678];
        }
        Self {
            tx_id: tx_id.into().to_ascii_uppercase(),
            address: target.address.clone(),
            name: ecu.ecuname.clone(),
            ecu: Some(ecu),
            ident,
            dtcs,
            start: Instant::now(),
        }
    }

    fn tick(&self) -> u64 {
        (self.start.elapsed().as_millis() / 500) as u64
    }

    pub fn respond(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.is_empty() {
            return None;
        }
        match payload[0] {
            0x10 => {
                let session = payload.get(1).copied().unwrap_or(0xC0);
                Some(vec![0x50, session])
            }
            0x3E => Some(vec![0x7E, 0x00]),
            0x81 => Some(vec![0xC1, 0x00, 0x00]),
            0x14 => Some(vec![0x54]),
            0x17 => self.read_kwp_dtc(payload),
            0x19 => self.read_dtc(payload),
            0x22 => self.read_did(payload),
            0x21 => self.read_local_id(payload),
            sid => {
                if let Some(resp) = self.generic_request(payload) {
                    Some(resp)
                } else if sid >= 0x10 {
                    Some(vec![0x7F, sid, 0x11])
                } else {
                    None
                }
            }
        }
    }

    fn read_did(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 3 {
            return Some(vec![0x7F, 0x22, 0x13]);
        }
        let did = ((payload[1] as u16) << 8) | payload[2] as u16;
        let data = match did {
            0xF1A0 => {
                let d = u8::from_str_radix(self.ident.diagversion.trim(), 16).unwrap_or(0);
                vec![d]
            }
            0xF18A => ascii_field(&self.ident.supplier, 63),
            0xF194 => ascii_field(&self.ident.soft, 32),
            0xF195 => ascii_field(&self.ident.version, 32),
            0xF190 => ascii_field("VF1SIMULATED000000", 17),
            _ => {
                let matched = self.match_request(payload);
                match matched {
                    Some(req) => {
                        let len = req.expected_response_len(&self.ecu.as_ref()?.data).max(3);
                        let mut resp = payload.to_vec();
                        resp[0] = 0x62;
                        resp.resize(len.max(payload.len()), 0);
                        self.fill_values(
                            &mut resp,
                            req,
                            &self.ecu.as_ref()?.data,
                            &self.ecu.as_ref()?.endian,
                        );
                        return Some(resp);
                    }
                    None => return Some(vec![0x7F, 0x22, 0x31]),
                }
            }
        };
        let mut resp = vec![0x62, payload[1], payload[2]];
        resp.extend_from_slice(&data);
        Some(resp)
    }

    fn read_local_id(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 2 {
            return Some(vec![0x7F, 0x21, 0x13]);
        }
        let Some(ecu) = &self.ecu else {
            if payload[1] == 0x80 {
                return Some(self.legacy_ident());
            }
            return Some(vec![0x7F, 0x21, 0x31]);
        };
        if payload[1] == 0x80 {
            return Some(self.legacy_ident());
        }
        let matched = self.match_request(payload);
        let Some(req) = matched else {
            return Some(vec![0x7F, 0x21, 0x31]);
        };
        let len = req.expected_response_len(&ecu.data).max(payload.len());
        let mut resp = payload.to_vec();
        resp[0] = 0x61;
        resp.resize(len, 0);
        self.fill_values(&mut resp, req, &ecu.data, &ecu.endian);
        Some(resp)
    }

    fn legacy_ident(&self) -> Vec<u8> {
        let diag = u8::from_str_radix(self.ident.diagversion.trim(), 16).unwrap_or(0);
        let mut supplier = [b' '; 4];
        for (i, b) in self.ident.supplier.trim().bytes().take(4).enumerate() {
            supplier[i] = b;
        }
        let soft = hex_pair(&self.ident.soft);
        let version = hex_pair(&self.ident.version);
        // 20-byte legacy 2180 record layout (ddt4all's >=20 branch):
        // [61 80] + 5 filler + diag + supplier[4] + 4 filler + soft[2] + version[2]
        let mut out = vec![0x61, 0x80, 0, 0, 0, 0, 0, diag];
        out.extend_from_slice(&supplier);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&soft);
        out.extend_from_slice(&version);
        out
    }

    fn read_kwp_dtc(&self, _payload: &[u8]) -> Option<Vec<u8>> {
        let count = self.dtcs.len().min(0x7F);
        let mut resp = vec![0x57, count as u8];
        for code in self.dtcs.iter().take(count) {
            resp.push(((code >> 8) & 0xFF) as u8);
            resp.push((code & 0xFF) as u8);
            resp.push(0x01);
        }
        Some(resp)
    }

    fn read_dtc(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 2 {
            return Some(vec![0x7F, 0x19, 0x13]);
        }
        let sub = payload[1];
        if sub == 0x02 {
            let mut resp = vec![0x59, 0x02, 0xFF];
            for code in self.dtcs.iter().take(8) {
                resp.push(((code >> 16) & 0xFF) as u8);
                resp.push(((code >> 8) & 0xFF) as u8);
                resp.push((code & 0xFF) as u8);
                resp.push(0x24);
            }
            Some(resp)
        } else if sub == 0x01 {
            Some(vec![0x59, 0x01, 0x01, 0x01])
        } else {
            Some(vec![0x7F, 0x19, 0x12])
        }
    }

    fn match_request<'a>(&'a self, payload: &[u8]) -> Option<&'a crate::model::EcuRequest> {
        let ecu = self.ecu.as_ref()?;
        let exact = ecu
            .requests
            .values()
            .find(|r| !r.sentbytes.is_empty() && payload.starts_with(&r.sentbytes));
        if exact.is_some() {
            return exact;
        }
        ecu.requests.values().find(|r| {
            !r.sentbytes.is_empty()
                && r.sentbytes.len() >= 2
                && r.sentbytes[0] == payload[0]
                && (r.sentbytes.get(1) == payload.get(1) || payload.len() == 1)
        })
    }

    fn generic_request(&self, payload: &[u8]) -> Option<Vec<u8>> {
        let ecu = self.ecu.as_ref()?;
        let req = self.match_request(payload)?;
        if req.manualsend && req.sentbytes.is_empty() {
            return None;
        }
        let mut resp = payload.to_vec();
        resp[0] = payload[0].wrapping_add(0x40);
        let len = req.expected_response_len(&ecu.data).max(resp.len());
        resp.resize(len, 0);
        self.fill_values(&mut resp, req, &ecu.data, &ecu.endian);
        Some(resp)
    }

    fn fill_values(
        &self,
        resp: &mut [u8],
        req: &crate::model::EcuRequest,
        data: &indexmap::IndexMap<String, crate::model::EcuData>,
        endian: &Endian,
    ) {
        let tick = self.tick();
        // Fill with deterministic pseudo-data first, so unmapped bytes look alive.
        let mut hasher = DefaultHasher::new();
        self.name.hash(&mut hasher);
        req.name.hash(&mut hasher);
        tick.hash(&mut hasher);
        let seed = hasher.finish();
        for (i, b) in resp.iter_mut().enumerate().skip(2) {
            let x =
                seed.rotate_left((i as u32 % 63) + 1) ^ (i as u64).wrapping_mul(0x9E3779B97F4A7C15);
            *b = (x >> 24) as u8;
        }
        for (name, item) in &req.receivebyte_dataitems {
            let Some(def) = data.get(name) else { continue };
            if item.firstbyte == 0 || def.bitscount > 32 || def.bitscount == 0 {
                continue;
            }
            let extent = item.byte_extent(def);
            if extent > resp.len() {
                continue;
            }
            let raw = if !def.lists.is_empty() {
                let keys: Vec<&i64> = def.lists.keys().collect();
                let k = keys[(tick as usize) % keys.len()];
                BigUint::from(*k as u64)
            } else if def.bytesascii {
                if def.bytescount > 32 {
                    continue;
                }
                let mut bytes = vec![b' '; def.bytescount as usize];
                for (i, b) in b"SIM".iter().take(bytes.len()).enumerate() {
                    bytes[i] = *b;
                }
                BigUint::from_bytes_be(&bytes)
            } else {
                let mask = (1u64 << def.bitscount.min(32)) - 1;
                let v = seed
                    .wrapping_add(tick.wrapping_mul(37))
                    .wrapping_add(name.len() as u64);
                BigUint::from(v & mask)
            };
            let _ = def.set_raw_value(&raw, resp, item, *endian);
        }
    }
}

fn ascii_field(s: &str, len: usize) -> Vec<u8> {
    let mut out = vec![b' '; len];
    for (i, b) in s.trim().bytes().take(len).enumerate() {
        out[i] = b;
    }
    out
}

fn hex_pair(s: &str) -> [u8; 2] {
    // Soft/version in ddt4all's legacy 2180 frame are two raw bytes each.
    let clean: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let v: u16 = u16::from_str_radix(&clean.chars().take(4).collect::<String>(), 16).unwrap_or(0);
    [(v >> 8) as u8, v as u8]
}

/// ISO-TP frame helper used by the emulated ELM and tests.
pub fn response_frames(payload: &[u8]) -> Vec<Vec<u8>> {
    isotp::frame(payload).unwrap_or_default()
}
