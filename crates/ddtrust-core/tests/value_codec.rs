use std::path::Path;

use ddtrust_core::model::data::EcuDataSpec;
use ddtrust_core::model::request::DataItemSpec;
use ddtrust_core::model::{DataItem, EcuData, Endian};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    decode: Vec<DecodeCase>,
    encode: Vec<EncodeCase>,
    requests: Vec<RequestCase>,
}

#[derive(Deserialize)]
struct DecodeCase {
    file: String,
    data_name: String,
    spec: serde_json::Value,
    item: serde_json::Value,
    endian: String,
    response: String,
    display: Option<String>,
    hex: Option<String>,
}

#[derive(Deserialize)]
struct EncodeCase {
    file: String,
    data_name: String,
    spec: serde_json::Value,
    item: serde_json::Value,
    endian: String,
    stream: Vec<String>,
    value: String,
    expected: Vec<String>,
}

#[derive(Deserialize)]
struct RequestCase {
    file: String,
    name: String,
    inputs: serde_json::Map<String, serde_json::Value>,
    expected: String,
}

fn fixture() -> Fixture {
    let text = include_str!("fixtures/value_vectors.json");
    serde_json::from_str(text).expect("fixture")
}

fn bytes(hex: &str) -> Vec<u8> {
    ddtrust_core::hexutil::parse_hex(hex).unwrap()
}

#[test]
fn golden_decode_vectors() {
    let f = fixture();
    let mut checked = 0;
    for c in &f.decode {
        let spec: EcuDataSpec = serde_json::from_value(c.spec.clone()).unwrap();
        let item: DataItemSpec = serde_json::from_value(c.item.clone()).unwrap();
        let data = EcuData::from_spec(&c.data_name, &spec);
        let item = DataItem::from_spec(&c.data_name, &item);
        let endian = Endian::parse(&c.endian);
        let resp = bytes(&c.response);
        let got_hex = data.get_hex_value(&resp, &item, endian);
        assert_eq!(
            got_hex.as_deref().map(|s| s.to_ascii_uppercase()),
            c.hex.as_deref().map(|s| s.to_ascii_uppercase()),
            "hex mismatch in {} / {}",
            c.file,
            c.data_name
        );
        let got_display = data.display_value(&resp, &item, endian);
        assert_eq!(
            got_display, c.display,
            "display mismatch in {} / {} (spec {:?}, item {:?}, resp {})",
            c.file, c.data_name, c.spec, c.item, c.response
        );
        checked += 1;
    }
    assert!(checked > 50, "too few vectors: {checked}");
}

#[test]
fn golden_encode_vectors() {
    let f = fixture();
    let mut checked = 0;
    for c in &f.encode {
        let spec: EcuDataSpec = serde_json::from_value(c.spec.clone()).unwrap();
        let item: DataItemSpec = serde_json::from_value(c.item.clone()).unwrap();
        let data = EcuData::from_spec(&c.data_name, &spec);
        let item = DataItem::from_spec(&c.data_name, &item);
        let endian = Endian::parse(&c.endian);
        let mut stream = bytes(&c.stream.join(""));
        data.set_value(&c.value, &mut stream, &item, endian)
            .unwrap_or_else(|e| panic!("{} / {}: {e}", c.file, c.data_name));
        let got = ddtrust_core::hexutil::to_hex_spaced(&stream);
        let expected = c.expected.join(" ").to_ascii_uppercase();
        assert_eq!(
            got, expected,
            "encode mismatch in {} / {} value {:?}",
            c.file, c.data_name, c.value
        );
        checked += 1;
    }
    assert!(checked > 30, "too few vectors: {checked}");
}

#[test]
fn golden_request_build_vectors() {
    let f = fixture();
    let dir = Path::new("../../data/ecu");
    if !dir.exists() {
        eprintln!("skipping: {dir:?} not present");
        return;
    }
    let store = ddtrust_core::DataStore::open(dir).unwrap();
    let mut checked = 0;
    for c in &f.requests {
        let Ok(ecu) = store.load_ecu(&c.file) else {
            continue;
        };
        let req = ecu
            .request(&c.name)
            .unwrap_or_else(|| panic!("request {}", c.name));
        let inputs: Vec<(String, String)> = c
            .inputs
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect();
        let stream = req.build_stream(&inputs, &ecu.data, ecu.endian).unwrap();
        assert_eq!(
            ddtrust_core::hexutil::to_hex(&stream),
            c.expected.to_ascii_uppercase(),
            "request build mismatch {} / {}",
            c.file,
            c.name
        );
        checked += 1;
    }
    assert!(checked > 0);
}
