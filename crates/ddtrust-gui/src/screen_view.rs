use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use ddtrust_core::diag::link::LinkInfo;
use ddtrust_core::hexutil;
use ddtrust_core::model::{DisplayDef, EcuFile, InputDef, LabelDef, Screen};

pub const KEY_SEP: char = '\u{1f}';

pub fn key(request: &str, item: &str) -> String {
    format!("{request}{KEY_SEP}{item}")
}

#[derive(Clone)]
pub struct ResolvedDisplay {
    pub def: DisplayDef,
    pub request: String,
    pub item: String,
    pub unit: String,
}

#[derive(Clone)]
pub struct ResolvedInput {
    pub def: InputDef,
    pub request: String,
    pub item: String,
}

pub struct ScreenRuntime {
    pub name: String,
    pub screen: Arc<Screen>,
    pub displays: Vec<ResolvedDisplay>,
    pub inputs: Vec<ResolvedInput>,
    pub labels: Vec<LabelDef>,
    pub values: HashMap<String, Option<String>>,
    pub history: HashMap<String, VecDeque<f32>>,
    pub updated: HashMap<String, Instant>,
    pub input_values: HashMap<String, String>,
    pub request_payloads: Vec<(String, Vec<u8>)>,
    pub errors: Vec<String>,
}

#[derive(Clone)]
pub struct OpenedEcu {
    pub target: ddtrust_core::Target,
    pub ecu: Arc<EcuFile>,
    pub layout: Arc<ddtrust_core::EcuLayout>,
    pub info: Option<LinkInfo>,
    pub warnings: Vec<String>,
}

impl ScreenRuntime {
    pub fn new(name: String, screen: Arc<Screen>, ecu: &EcuFile) -> Self {
        let mut displays = Vec::new();
        let mut inputs = Vec::new();
        let mut errors = Vec::new();
        let mut request_payloads: Vec<(String, Vec<u8>)> = Vec::new();

        for def in &screen.displays {
            let Some(req_name) = ecu.request_name(&def.request).cloned() else {
                errors.push(format!(
                    "display {}: unknown request {}",
                    def.text, def.request
                ));
                continue;
            };
            let Some(req) = ecu.request(&req_name) else {
                continue;
            };
            let Some((item_name, _)) = req
                .receivebyte_dataitems
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&def.text))
            else {
                errors.push(format!(
                    "display {}: no data item in request {}",
                    def.text, req_name
                ));
                continue;
            };
            let unit = ecu
                .data(&item_name)
                .map(|d| d.unit.clone())
                .unwrap_or_default();
            displays.push(ResolvedDisplay {
                def: def.clone(),
                request: req_name.clone(),
                item: item_name.clone(),
                unit,
            });
            if !req.manualsend
                && !req.sentbytes.is_empty()
                && !request_payloads.iter().any(|(n, _)| n == &req_name)
            {
                request_payloads.push((req_name.clone(), req.sentbytes.clone()));
            }
        }
        for def in &screen.inputs {
            let Some(req_name) = ecu.request_name(&def.request).cloned() else {
                errors.push(format!(
                    "input {}: unknown request {}",
                    def.text, def.request
                ));
                continue;
            };
            let Some(req) = ecu.request(&req_name) else {
                continue;
            };
            let Some((item_name, _)) = req
                .sendbyte_dataitems
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&def.text))
            else {
                errors.push(format!(
                    "input {}: no send data item in request {}",
                    def.text, req_name
                ));
                continue;
            };
            inputs.push(ResolvedInput {
                def: def.clone(),
                request: req_name,
                item: item_name.clone(),
            });
        }
        let labels = screen.labels.clone();
        Self {
            name,
            screen,
            displays,
            inputs,
            labels,
            values: HashMap::new(),
            history: HashMap::new(),
            updated: HashMap::new(),
            input_values: HashMap::new(),
            request_payloads,
            errors,
        }
    }

    pub fn poll_items(&self) -> Vec<(String, Vec<u8>)> {
        self.request_payloads.clone()
    }

    pub fn apply_response(&mut self, ecu: &EcuFile, request: &str, raw: &[u8]) {
        for d in &self.displays {
            if d.request != request {
                continue;
            }
            let Some(def) = ecu.data(&d.item) else {
                continue;
            };
            let Some(item) = ecu.request(request).and_then(|r| r.receive_item(&d.item)) else {
                continue;
            };
            let value = def.display_value(raw, item, ecu.endian);
            let k = key(&d.request, &d.item);
            self.updated.insert(k.clone(), Instant::now());
            if let Some(text) = value.as_deref() {
                if let Some(num) = parse_display_number(text) {
                    let entry = self.history.entry(k.clone()).or_default();
                    entry.push_back(num);
                    while entry.len() > 48 {
                        entry.pop_front();
                    }
                }
            }
            self.values.insert(k, value);
        }
    }

    pub fn input_value(&self, request: &str, item: &str) -> String {
        self.input_values
            .get(&key(request, item))
            .cloned()
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn input_value_mut(&mut self, request: &str, item: &str) -> &mut String {
        self.input_values.entry(key(request, item)).or_default()
    }

    /// Inputs relevant to a request, in the request's send-byte order.
    pub fn inputs_for(&self, ecu: &EcuFile, request: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(req) = ecu.request(request) {
            for item_name in req.sendbyte_dataitems.keys() {
                for i in &self.inputs {
                    if i.request == request && i.item.eq_ignore_ascii_case(item_name) {
                        let v = self.input_value(&i.request, &i.item);
                        if !v.is_empty() {
                            out.push((item_name.clone(), v));
                        }
                    }
                }
            }
        }
        out
    }

    pub fn build_button_payloads(
        &self,
        ecu: &EcuFile,
        button_index: usize,
    ) -> Result<Vec<(Vec<u8>, u64)>, String> {
        let Some(button) = self.screen.buttons.get(button_index) else {
            return Err("button not found".into());
        };
        let mut out = Vec::new();
        for action in &button.send {
            let Some(req_name) = ecu.request_name(&action.request) else {
                return Err(format!("unknown request {}", action.request));
            };
            let Some(req) = ecu.request(req_name) else {
                return Err(format!("unknown request {}", action.request));
            };
            let inputs = self.inputs_for(ecu, req_name);
            let stream = req
                .build_stream(&inputs, &ecu.data, ecu.endian)
                .map_err(|e| e.to_string())?;
            out.push((stream, action.delay.0.max(0.0) as u64));
        }
        Ok(out)
    }

    pub fn build_presend(&self, ecu: &EcuFile) -> Result<Vec<(Vec<u8>, u64)>, String> {
        let mut out = Vec::new();
        for p in &self.screen.presend {
            let Some(req_name) = ecu.request_name(&p.request) else {
                return Err(format!("unknown presend request {}", p.request));
            };
            let Some(req) = ecu.request(req_name) else {
                continue;
            };
            let inputs = self.inputs_for(ecu, req_name);
            let stream = req
                .build_stream(&inputs, &ecu.data, ecu.endian)
                .map_err(|e| e.to_string())?;
            out.push((stream, p.delay.0.max(0.0) as u64));
        }
        Ok(out)
    }

    #[allow(dead_code)]
    pub fn raw_request(&self, ecu: &EcuFile, request: &str) -> Option<Vec<u8>> {
        ecu.request(request).map(|r| r.sentbytes.clone())
    }

    pub fn display_value(&self, request: &str, item: &str) -> Option<String> {
        self.values.get(&key(request, item)).cloned().flatten()
    }

    pub fn history(&self, request: &str, item: &str) -> Option<&VecDeque<f32>> {
        self.history.get(&key(request, item))
    }

    pub fn is_fresh(&self, request: &str, item: &str) -> bool {
        self.updated
            .get(&key(request, item))
            .map(|t| t.elapsed().as_millis() < 450)
            .unwrap_or(false)
    }
}

#[allow(dead_code)]
pub fn decode_request(ecu: &EcuFile, request: &str, raw: &[u8]) -> Vec<(String, Option<String>)> {
    let Some(req) = ecu.request(request) else {
        return Vec::new();
    };
    req.decode(raw, &ecu.data, ecu.endian)
        .into_iter()
        .map(|v| (v.name, v.display))
        .collect()
}

#[allow(dead_code)]
/// Parse a displayed numeric value ("12.5", "12,5", "1 234") for sparklines.
pub fn parse_display_number(text: &str) -> Option<f32> {
    let cleaned: String = text
        .trim()
        .replace(',', ".")
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    cleaned.parse::<f32>().ok()
}

#[allow(dead_code)]
pub fn request_hex(ecu: &EcuFile, request: &str) -> String {
    ecu.request(request)
        .map(|r| hexutil::to_hex_spaced(&r.sentbytes))
        .unwrap_or_default()
}
