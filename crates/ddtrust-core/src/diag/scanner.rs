use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};
use crate::hexutil;
use crate::model::{Database, Protocol, TargetMatch, Vehicle};

use super::link::{CanSpec, DiagnosticLink, DoipSpec, KwpSpec};

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Attempt {
        address: String,
        group: String,
    },
    Found {
        address: String,
        name: String,
        href: Option<String>,
        exact: bool,
        group: String,
        protocol: Protocol,
        diagversion: String,
        supplier: String,
        soft: String,
        version: String,
        legacy: bool,
    },
    NoEcu {
        address: String,
        group: String,
    },
    Error {
        address: String,
        message: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub address: String,
    pub protocol: Protocol,
    pub diagversion: String,
    pub supplier: String,
    pub soft: String,
    pub version: String,
    pub matched: Option<TargetMatch>,
    pub legacy: bool,
    pub group: String,
}

impl ScanResult {
    pub fn label(&self) -> String {
        match &self.matched {
            Some(m) => format!("{} [{}] @ {}", m.name, m.href, self.address),
            None => format!(
                "Unknown ECU @ {} diag={} supplier={} soft={} version={}",
                self.address, self.diagversion, self.supplier, self.soft, self.version
            ),
        }
    }
}

pub fn scan_can<F>(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    vehicle: Option<&Vehicle>,
    addresses: &[String],
    baud: u32,
    on_event: &mut F,
) -> Vec<ScanResult>
where
    F: FnMut(ScanEvent),
{
    scan_can_with_cancel(
        link,
        db,
        vehicle,
        addresses,
        baud,
        on_event,
        &AtomicBool::new(false),
    )
}

pub fn scan_can_with_cancel<F>(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    vehicle: Option<&Vehicle>,
    addresses: &[String],
    baud: u32,
    on_event: &mut F,
    cancel: &AtomicBool,
) -> Vec<ScanResult>
where
    F: FnMut(ScanEvent),
{
    let mut found = Vec::new();
    for addr in addresses {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let group = db.address_info(vehicle, addr).long;
        on_event(ScanEvent::Attempt {
            address: addr.clone(),
            group: group.clone(),
        });
        let Some((tx, rx)) = db.can_ids(vehicle, addr) else {
            on_event(ScanEvent::Error {
                address: addr.clone(),
                message: "no CAN id mapping for this vehicle".into(),
            });
            continue;
        };
        if let Err(e) = link.open_can(&CanSpec {
            tx_id: tx,
            rx_id: rx,
            baud,
        }) {
            on_event(ScanEvent::Error {
                address: addr.clone(),
                message: e.to_string(),
            });
            continue;
        }
        match identify_can(link, db, addr, &group, baud) {
            Ok(Some(mut r)) => {
                r.group = group;
                on_event(found_event(&r));
                found.push(r);
            }
            Ok(None) => {
                on_event(ScanEvent::NoEcu {
                    address: addr.clone(),
                    group: group.clone(),
                });
            }
            Err(e) if matches!(e, Error::NoResponse) => {
                on_event(ScanEvent::NoEcu {
                    address: addr.clone(),
                    group: group.clone(),
                });
            }
            Err(e) => on_event(ScanEvent::Error {
                address: addr.clone(),
                message: e.to_string(),
            }),
        }
    }
    link.close();
    found
}

pub fn scan_kwp<F>(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    vehicle: Option<&Vehicle>,
    addresses: &[String],
    on_event: &mut F,
) -> Vec<ScanResult>
where
    F: FnMut(ScanEvent),
{
    scan_kwp_with_cancel(
        link,
        db,
        vehicle,
        addresses,
        on_event,
        &AtomicBool::new(false),
    )
}

pub fn scan_kwp_with_cancel<F>(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    vehicle: Option<&Vehicle>,
    addresses: &[String],
    on_event: &mut F,
    cancel: &AtomicBool,
) -> Vec<ScanResult>
where
    F: FnMut(ScanEvent),
{
    let mut found = Vec::new();
    for addr in addresses {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let group = db.address_info(vehicle, addr).long;
        on_event(ScanEvent::Attempt {
            address: addr.clone(),
            group: group.clone(),
        });
        let mut result = None;
        for fast in [false, true] {
            if let Err(e) = link.open_kwp(&KwpSpec {
                address: addr.clone(),
                fast_init: fast,
            }) {
                on_event(ScanEvent::Error {
                    address: addr.clone(),
                    message: e.to_string(),
                });
                break;
            }
            let _ = link.start_session(&[0x10, 0xC0]);
            match link.request(&[0x21, 0x80]) {
                Ok(rsp) => {
                    if let Some(mut r) = parse_legacy(db, addr, &rsp, Protocol::Kwp2000) {
                        r.group = group.clone();
                        result = Some(r);
                        break;
                    }
                }
                Err(_) => continue,
            }
        }
        if let Some(r) = result {
            on_event(found_event(&r));
            found.push(r);
        } else {
            on_event(ScanEvent::NoEcu {
                address: addr.clone(),
                group: group.clone(),
            });
        }
    }
    link.close();
    found
}

pub fn scan_doip<F>(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    vehicle: Option<&Vehicle>,
    addresses: &[String],
    on_event: &mut F,
) -> Vec<ScanResult>
where
    F: FnMut(ScanEvent),
{
    scan_doip_with_cancel(
        link,
        db,
        vehicle,
        addresses,
        on_event,
        &AtomicBool::new(false),
    )
}

pub fn scan_doip_with_cancel<F>(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    vehicle: Option<&Vehicle>,
    addresses: &[String],
    on_event: &mut F,
    cancel: &AtomicBool,
) -> Vec<ScanResult>
where
    F: FnMut(ScanEvent),
{
    let names: std::collections::HashMap<String, String> =
        db.doip_addresses(vehicle).into_iter().collect();
    let mut found = Vec::new();
    for addr in addresses {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let group = names
            .get(&addr.trim().to_ascii_uppercase())
            .cloned()
            .unwrap_or_else(|| db.address_info(vehicle, addr).long);
        on_event(ScanEvent::Attempt {
            address: addr.clone(),
            group: group.clone(),
        });
        let Some(target) = parse_doip_address(addr) else {
            on_event(ScanEvent::Error {
                address: addr.clone(),
                message: format!("invalid DoIP logical address '{addr}'"),
            });
            continue;
        };
        match identify_doip(link, db, addr, target, &group) {
            Ok(Some(mut r)) => {
                r.group = group;
                on_event(found_event(&r));
                found.push(r);
            }
            Ok(None) => {
                on_event(ScanEvent::NoEcu {
                    address: addr.clone(),
                    group: group.clone(),
                });
            }
            Err(e) if matches!(e, Error::NoResponse) => {
                on_event(ScanEvent::NoEcu {
                    address: addr.clone(),
                    group: group.clone(),
                });
            }
            Err(e) => on_event(ScanEvent::Error {
                address: addr.clone(),
                message: e.to_string(),
            }),
        }
    }
    link.close();
    found
}

/// Parse a DoIP logical address. Project maps use decimal ("239"); hex forms ("0xEF", "EF")
/// are accepted as well.
pub fn parse_doip_address(value: &str) -> Option<u16> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        return u16::from_str_radix(hex, 16).ok();
    }
    if v.chars().all(|c| c.is_ascii_digit()) {
        return v.parse::<u16>().ok();
    }
    u16::from_str_radix(v, 16).ok()
}

fn found_event(r: &ScanResult) -> ScanEvent {
    ScanEvent::Found {
        address: r.address.clone(),
        name: r
            .matched
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Unknown ECU".into()),
        href: r.matched.as_ref().map(|m| m.href.clone()),
        exact: r.matched.as_ref().map(|m| m.exact).unwrap_or(false),
        group: r.group.clone(),
        protocol: r.protocol.clone(),
        diagversion: r.diagversion.clone(),
        supplier: r.supplier.clone(),
        soft: r.soft.clone(),
        version: r.version.clone(),
        legacy: r.legacy,
    }
}

fn identify_can(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    addr: &str,
    group: &str,
    _baud: u32,
) -> Result<Option<ScanResult>> {
    identify_opened(link, db, addr, group, Protocol::Can)
}

fn identify_doip(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    addr: &str,
    target_address: u16,
    group: &str,
) -> Result<Option<ScanResult>> {
    link.open_doip(&DoipSpec {
        target_address,
        tester_address: None,
        timeout: None,
    })?;
    identify_opened(link, db, addr, group, Protocol::Doip)
}

fn identify_opened(
    link: &mut dyn DiagnosticLink,
    db: &Database,
    addr: &str,
    group: &str,
    protocol: Protocol,
) -> Result<Option<ScanResult>> {
    // New UDS identification (diag session 1003 + DIDs F1A0/F18A/F194/F195).
    let opened = link.start_session(&[0x10, 0x03]).is_ok();
    if opened {
        let diag = read_did(link, 0xF1A0).ok();
        let supplier = read_did(link, 0xF18A).ok();
        let soft = read_did(link, 0xF194).ok();
        let version = read_did(link, 0xF195).ok();
        let _ = link.start_session(&[0x10, 0x01]);
        if let (Some(diag), Some(supplier), Some(soft), Some(version)) =
            (diag, supplier, soft, version)
        {
            let diagversions = diag_candidates(diag.first().copied().unwrap_or(0));
            let supplier = clean_ascii(&supplier);
            let soft = clean_ascii(&soft);
            let version = clean_ascii(&version);
            let matched = match_ident(
                db,
                &diagversions,
                &supplier,
                &soft,
                &version,
                addr,
                protocol.clone(),
            );
            return Ok(Some(ScanResult {
                address: addr.to_string(),
                protocol: protocol.clone(),
                diagversion: diagversions.first().cloned().unwrap_or_default(),
                supplier,
                soft,
                version,
                matched,
                legacy: false,
                group: group.to_string(),
            }));
        }
    }

    // Legacy identification: 10C0 + 2180.
    let _ = link.start_session(&[0x10, 0xC0]);
    let rsp = link.request(&[0x21, 0x80])?;
    let mut result = parse_legacy(db, addr, &rsp, protocol.clone());
    if let Some(r) = &mut result {
        r.group = group.to_string();
    }
    let _ = link.start_session(&[0x10, 0x81]);
    Ok(result)
}

fn read_did(link: &mut dyn DiagnosticLink, did: u16) -> Result<Vec<u8>> {
    let payload = [0x22, (did >> 8) as u8, did as u8];
    let rsp = link.request(&payload)?;
    if rsp.len() >= 3 && rsp[0] == 0x62 && rsp[1] == payload[1] && rsp[2] == payload[2] {
        Ok(rsp[3..].to_vec())
    } else if rsp.len() >= 3 && rsp[0] == 0x7F {
        Err(Error::negative(0x22, rsp[2]))
    } else {
        Err(Error::Protocol(
            "unexpected ReadDataByIdentifier response".into(),
        ))
    }
}

fn parse_legacy(db: &Database, addr: &str, rsp: &[u8], protocol: Protocol) -> Option<ScanResult> {
    if rsp.len() < 15 {
        return None;
    }
    let (diag_byte, supplier, soft, version) = if rsp.len() >= 20 {
        (
            rsp[7],
            clean_ascii(&rsp[8..12]),
            hexutil::to_hex(&rsp[16..18]),
            hexutil::to_hex(&rsp[18..20]),
        )
    } else {
        (
            rsp[2],
            clean_ascii(&rsp[3..7]),
            hexutil::to_hex(&rsp[7..11]),
            hexutil::to_hex(&rsp[11..15]),
        )
    };
    let diagversions = diag_candidates(diag_byte);
    let matched = match_ident(
        db,
        &diagversions,
        &supplier,
        &soft,
        &version,
        addr,
        protocol.clone(),
    );
    Some(ScanResult {
        address: addr.to_string(),
        protocol,
        diagversion: diagversions.first().cloned().unwrap_or_default(),
        supplier,
        soft,
        version,
        matched,
        legacy: true,
        group: String::new(),
    })
}

/// ddt4all decodes the diagnostic version byte as a decimal string. Some database
/// entries store it as the raw hex representation, so try both.
fn diag_candidates(byte: u8) -> Vec<String> {
    let dec = byte.to_string();
    let hex = format!("{byte:X}");
    if dec == hex {
        vec![dec]
    } else {
        vec![dec, hex]
    }
}

fn match_ident(
    db: &Database,
    diagversions: &[String],
    supplier: &str,
    soft: &str,
    version: &str,
    addr: &str,
    protocol: Protocol,
) -> Option<TargetMatch> {
    for diag in diagversions {
        let exact = db.identify_exact(diag, supplier, soft, version, addr, protocol.clone());
        if let Some(m) = exact.into_iter().next() {
            return Some(m);
        }
    }
    for diag in diagversions {
        if let Some(m) =
            db.identify_approximate(diag, supplier, soft, version, addr, protocol.clone())
        {
            return Some(m);
        }
    }
    None
}

fn clean_ascii(bytes: &[u8]) -> String {
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], 0x00 | 0xFF | b' ') {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end])
        .replace('\u{FFFD}', "")
        .trim()
        .to_string()
}
