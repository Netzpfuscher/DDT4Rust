use std::path::Path;

use ddtrust_core::DataStore;

fn data_dir() -> Option<&'static Path> {
    let p = Path::new("../../data/ecu");
    p.is_dir().then_some(p)
}

#[test]
fn loads_database_and_samples() {
    let Some(dir) = data_dir() else {
        eprintln!("skipping: extracted data not available");
        return;
    };
    let store = DataStore::open(dir).unwrap();
    let db = store.load_database().unwrap();
    let (targets, vehicles, can) = db.stats();
    assert!(targets > 2000, "{targets}");
    assert!(vehicles >= 100, "{vehicles}");
    assert!(can > 1500, "{can}");
    assert!(db.vehicle("ALL").is_some());
    assert!(db.find_by_href_or_name("UCH_84_J84_05_00").is_some());

    for href in [
        "UCH_84_J84_05_00.json",
        "EDC16C36___S40___V4___(Diag_On_CAN)_20100927T101813.json",
    ] {
        let ecu = store.load_ecu(href).unwrap();
        assert!(!ecu.requests.is_empty(), "{href}");
        assert!(!ecu.data.is_empty(), "{href}");
        let layout = store.load_layout(href).unwrap();
        assert!(!layout.screens.is_empty(), "{href}");
    }
}

#[test]
#[ignore = "parses ~1.4 GB of ECU definitions"]
fn parses_entire_database() {
    let Some(dir) = data_dir() else { return };
    let store = DataStore::open(dir).unwrap();
    let db = store.load_database().unwrap();
    let mut ok = 0;
    let mut failures = Vec::new();
    for target in &db.targets {
        match store.load_ecu(&target.href) {
            Ok(ecu) => {
                if let Err(e) = store.load_layout(&target.href) {
                    failures.push(format!("layout {}: {e}", target.href));
                }
                ok += 1;
                let _ = ecu;
            }
            Err(e) => failures.push(format!("{}: {e}", target.href)),
        }
    }
    eprintln!("parsed {ok} ECUs, {} failures", failures.len());
    for f in failures.iter().take(50) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} failures", failures.len());
}
