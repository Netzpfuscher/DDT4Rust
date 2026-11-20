use std::path::Path;
use std::sync::Arc;

use ddtrust_core::diag::link::{CanSpec, DiagnosticLink};
use ddtrust_core::diag::{scan_can, scan_kwp, EcuSession, Safety};
use ddtrust_core::elm::{Elm327, ElmConfig};
use ddtrust_core::sim::{EmulatedElm, VirtualEcu};
use ddtrust_core::{DataStore, Protocol};

fn store() -> Option<DataStore> {
    let p = Path::new("../../data/ecu");
    p.is_dir().then(|| DataStore::open(p).unwrap())
}

fn elm_with(ecus: Vec<VirtualEcu>) -> Elm327 {
    let mut elm = Elm327::new(Box::new(EmulatedElm::with_ecus(ecus)), ElmConfig::default());
    elm.initialize().unwrap();
    elm
}

#[test]
fn elm_can_session_and_multiframe() {
    let Some(store) = store() else { return };
    let db = store.load_database().unwrap();
    let target = db.target("UCH_84_J84_05_00.json").unwrap();
    let ecu = Arc::new(store.load_ecu(&target.href).unwrap());
    let all = db.vehicle("ALL").unwrap();
    let (tx, rx) = db.can_ids(Some(all), &target.address).unwrap();
    let v = VirtualEcu::from_file(target, ecu, tx.clone());
    let mut elm = elm_with(vec![v]);
    elm.open_can(&CanSpec {
        tx_id: tx,
        rx_id: rx,
        baud: 500_000,
    })
    .unwrap();
    let rsp = elm.start_session(&[0x10, 0x03]).unwrap();
    assert_eq!(rsp[0], 0x50);

    // Multi-frame answer (supplier string is longer than 7 bytes).
    let supplier = elm.request(&[0x22, 0xF1, 0x8A]).unwrap();
    assert_eq!(&supplier[..3], &[0x62, 0xF1, 0x8A]);
    assert!(supplier.len() > 7);
    assert_eq!(String::from_utf8_lossy(&supplier[3..]).trim(), "001");

    // Negative response for an unsupported DID.
    let neg = elm.request(&[0x22, 0x12, 0x34]).unwrap();
    assert_eq!(neg[0], 0x7F);
    assert_eq!(neg[2], 0x31);
}

#[test]
fn scans_virtual_vehicle() {
    let Some(store) = store() else { return };
    let db = store.load_database().unwrap();
    let all = db.vehicle("ALL").unwrap();
    let targets: Vec<_> = db
        .targets
        .iter()
        .filter(|t| t.protocol == Protocol::Can && !t.autoidents.is_empty())
        .take(6)
        .collect();
    let mut ecus = Vec::new();
    let mut addresses = Vec::new();
    for t in &targets {
        if let Some((tx, _)) = db.can_ids(Some(all), &t.address) {
            addresses.push(t.address.clone());
            ecus.push(VirtualEcu::from_target(t, tx));
        }
    }
    assert!(!ecus.is_empty());
    let mut elm = elm_with(ecus);
    let mut events = 0;
    let found = scan_can(&mut elm, &db, Some(all), &addresses, 500_000, &mut |_| {
        events += 1;
    });
    eprintln!("scan events={events} found={}", found.len());
    for f in &found {
        eprintln!("  {}", f.label());
    }
    assert!(!found.is_empty());
    assert!(found.iter().any(|r| r.matched.is_some()));
}

#[test]
fn scans_virtual_kwp() {
    let Some(store) = store() else { return };
    let db = store.load_database().unwrap();
    let target = db
        .targets
        .iter()
        .find(|t| t.protocol == Protocol::Kwp2000 && !t.autoidents.is_empty())
        .unwrap();
    let v = VirtualEcu::from_target(target, "00");
    let mut elm = elm_with(vec![v]);
    let mut events = 0;
    let found = scan_kwp(&mut elm, &db, None, &[target.address.clone()], &mut |_| {
        events += 1;
    });
    eprintln!("KWP events={events} found={}", found.len());
    for f in &found {
        eprintln!(
            "  {} diag={} sup={:?} soft={:?} ver={:?}",
            f.label(),
            f.diagversion,
            f.supplier,
            f.soft,
            f.version
        );
    }
    assert!(!found.is_empty());
    assert!(found[0].matched.is_some());
}

#[test]
fn session_reads_values_and_dtc() {
    let Some(store) = store() else { return };
    let db = store.load_database().unwrap();
    let target = db.target("UCH_84_J84_05_00.json").unwrap();
    let ecu = Arc::new(store.load_ecu(&target.href).unwrap());
    let layout = store.load_layout(&target.href).unwrap();
    let all = db.vehicle("ALL").unwrap();
    let (tx, rx) = db.can_ids(Some(all), &target.address).unwrap();
    let v = VirtualEcu::from_file(target, ecu.clone(), tx.clone());
    let mut elm = elm_with(vec![v]);
    elm.open_can(&CanSpec {
        tx_id: tx,
        rx_id: rx,
        baud: 500_000,
    })
    .unwrap();
    let mut session = EcuSession::open(Box::new(elm), ecu.clone(), Safety::default()).unwrap();
    assert!(session.open.session_response.is_some());

    let screen = layout.screens.values().next().unwrap();
    let mut read_ok = 0;
    for display in &screen.displays {
        let Some(req) = ecu.request(&display.request) else {
            continue;
        };
        if req.manualsend || req.sentbytes.is_empty() {
            continue;
        }
        if let Ok(result) = session.read_request(req, &[]) {
            assert!(!result.raw.is_empty());
            assert_eq!(result.values.len(), req.receivebyte_dataitems.len());
            read_ok += 1;
            if read_ok >= 3 {
                break;
            }
        }
    }
    assert!(read_ok > 0, "no display request could be read");

    let report = session.read_dtc().unwrap();
    eprintln!(
        "DTC raw: {:?}, records: {}",
        report.raw,
        report.records.len()
    );
    assert!(!report.records.is_empty());
    assert!(report.records[0].code.is_some());
}

#[test]
fn safety_blocks_writes() {
    let Some(store) = store() else { return };
    let db = store.load_database().unwrap();
    let target = db.target("UCH_84_J84_05_00.json").unwrap();
    let ecu = Arc::new(store.load_ecu(&target.href).unwrap());
    let all = db.vehicle("ALL").unwrap();
    let (tx, rx) = db.can_ids(Some(all), &target.address).unwrap();
    let v = VirtualEcu::from_file(target, ecu.clone(), tx.clone());
    let mut elm = elm_with(vec![v]);
    elm.open_can(&CanSpec {
        tx_id: tx,
        rx_id: rx,
        baud: 500_000,
    })
    .unwrap();
    let mut session = EcuSession::open(Box::new(elm), ecu.clone(), Safety::default()).unwrap();
    let err = session
        .send_payload(&[0x2E, 0xF1, 0x00, 0x01], true)
        .unwrap_err();
    assert!(err.to_string().contains("expert"));
    // Safe read still works.
    session.send_payload(&[0x22, 0xF1, 0x94], false).unwrap();
}
