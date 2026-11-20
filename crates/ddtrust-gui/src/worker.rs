use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use ddtrust_core::diag::link::{DiagnosticLink, LinkInfo};
use ddtrust_core::diag::DtcReport;
use ddtrust_core::diag::{
    scan_can_with_cancel, scan_kwp_with_cancel, EcuSession, Safety, ScanEvent, ScanResult,
};
use ddtrust_core::elm::{Elm327, ElmConfig};
use ddtrust_core::model::{Database, EcuFile, Protocol, Target, Vehicle};
use ddtrust_core::sim::{EmulatedElm, VirtualEcu};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterChoice {
    Simulator,
    ElmSerial,
    ElmTcp,
    SocketCan,
}

impl AdapterChoice {
    pub fn label(self) -> &'static str {
        match self {
            AdapterChoice::Simulator => "Simulator",
            AdapterChoice::ElmSerial => "ELM327 serial",
            AdapterChoice::ElmTcp => "ELM327 Wi-Fi/TCP",
            AdapterChoice::SocketCan => "CandleLight / SocketCAN",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            AdapterChoice::Simulator => "Built-in virtual ELM327 and ECU — no hardware required.",
            AdapterChoice::ElmSerial => "USB or Bluetooth ELM327-compatible serial adapter.",
            AdapterChoice::ElmTcp => "Wi-Fi ELM327-compatible adapter with a TCP socket.",
            AdapterChoice::SocketCan => {
                "CandleLight gs_usb adapter exposed as a Linux CAN interface."
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct AdapterSettings {
    pub kind: AdapterChoice,
    pub port: String,
    pub serial_baud: u32,
    pub tcp: String,
    pub can_interface: String,
    pub can_baud: u32,
}

impl Default for AdapterSettings {
    fn default() -> Self {
        Self {
            kind: AdapterChoice::ElmSerial,
            port: String::new(),
            serial_baud: 38400,
            tcp: String::new(),
            can_interface: "can0".into(),
            can_baud: 500_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectRequest {
    pub target: Target,
    pub ecu: Arc<EcuFile>,
    pub settings: AdapterSettings,
    pub expert: bool,
    pub sim_tx: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PollItem {
    pub tag: String,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct SeqItem {
    pub payload: Vec<u8>,
    pub delay_ms: u64,
}

#[derive(Debug)]
pub enum Command {
    Connect(ConnectRequest),
    Disconnect,
    SetExpert(bool),
    Poll {
        generation: u64,
        items: Vec<PollItem>,
    },
    PollSync {
        items: Vec<PollItem>,
        reply: Sender<Vec<PollResult>>,
    },
    Sequence {
        items: Vec<SeqItem>,
    },
    ReadDtc {
        generation: u64,
    },
    ReadDtcSync {
        reply: Sender<std::result::Result<DtcReport, String>>,
    },
    ClearDtc,
    RawAt(String),
    RawHex(String),
    Scan {
        vehicle: Option<Vehicle>,
        protocol: Protocol,
        addresses: Vec<String>,
        settings: AdapterSettings,
        cancel: Arc<AtomicBool>,
    },
}

#[derive(Clone, Debug)]
pub struct PollResult {
    pub tag: String,
    pub result: std::result::Result<Vec<u8>, String>,
}

#[derive(Debug)]
pub enum Event {
    Connected {
        info: LinkInfo,
        warnings: Vec<String>,
    },
    Disconnected,
    Error {
        context: String,
        message: String,
    },
    Poll {
        generation: u64,
        results: Vec<PollResult>,
    },
    SequenceDone {
        errors: Vec<String>,
    },
    Dtc {
        generation: u64,
        result: std::result::Result<DtcReport, String>,
    },
    Cleared {
        result: std::result::Result<String, String>,
    },
    Raw {
        response: std::result::Result<String, String>,
    },
    Scan(ScanEvent),
    ScanDone(Vec<ScanResult>),
    Log(String),
}

pub struct WorkerHandle {
    pub tx: Sender<Command>,
    pub rx: Receiver<Event>,
}

pub fn spawn(db: Arc<Database>) -> WorkerHandle {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
    thread::Builder::new()
        .name("ddtrust-diag".into())
        .spawn(move || run(db, cmd_rx, ev_tx))
        .expect("spawn diag worker");
    WorkerHandle {
        tx: cmd_tx,
        rx: ev_rx,
    }
}

fn run(db: Arc<Database>, rx: Receiver<Command>, tx: Sender<Event>) {
    let mut session: Option<EcuSession> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Disconnect => {
                if let Some(mut s) = session.take() {
                    s.link.close();
                }
                let _ = tx.send(Event::Disconnected);
            }
            Command::SetExpert(expert) => {
                if let Some(s) = session.as_mut() {
                    s.safety = Safety { expert };
                }
            }
            Command::Connect(req) => {
                if let Some(mut s) = session.take() {
                    s.link.close();
                }
                let _ = tx.send(Event::Log(format!(
                    "Opening {} ({}){}",
                    req.target.ecuname,
                    req.target.href,
                    if req.settings.kind == AdapterChoice::Simulator {
                        " [simulator]"
                    } else {
                        ""
                    }
                )));
                match build_session(&req, &tx) {
                    Ok(s) => {
                        let info = s.info().clone();
                        let warnings = s.open.warnings.clone();
                        session = Some(s);
                        let _ = tx.send(Event::Connected { info, warnings });
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error {
                            context: "connect".into(),
                            message: e,
                        });
                    }
                }
            }
            Command::Poll { generation, items } => {
                let Some(s) = session.as_mut() else {
                    let _ = tx.send(Event::Error {
                        context: "poll".into(),
                        message: "not connected".into(),
                    });
                    continue;
                };
                let mut results = Vec::with_capacity(items.len());
                for item in items {
                    let r = s
                        .send_payload(&item.payload, false)
                        .map_err(|e| e.to_string());
                    results.push(PollResult {
                        tag: item.tag,
                        result: r,
                    });
                }
                let _ = tx.send(Event::Poll {
                    generation,
                    results,
                });
            }
            Command::Sequence { items, .. } => {
                let Some(s) = session.as_mut() else {
                    let _ = tx.send(Event::Error {
                        context: "sequence".into(),
                        message: "not connected".into(),
                    });
                    continue;
                };
                let mut errors = Vec::new();
                for item in items {
                    if item.delay_ms > 0 {
                        thread::sleep(Duration::from_millis(item.delay_ms));
                    }
                    let safe = Safety::is_safe(&item.payload);
                    if let Err(e) = s.send_payload(&item.payload, !safe) {
                        errors.push(e.to_string());
                        break;
                    }
                }
                let _ = tx.send(Event::SequenceDone { errors });
            }
            Command::PollSync { items, reply } => {
                let Some(s) = session.as_mut() else {
                    let _ = reply.send(Vec::new());
                    continue;
                };
                let mut results = Vec::with_capacity(items.len());
                for item in items {
                    let r = s
                        .send_payload(&item.payload, false)
                        .map_err(|e| e.to_string());
                    results.push(PollResult {
                        tag: item.tag,
                        result: r,
                    });
                }
                let _ = reply.send(results);
            }
            Command::ReadDtcSync { reply } => {
                let Some(s) = session.as_mut() else {
                    let _ = reply.send(Err("not connected".into()));
                    continue;
                };
                let _ = reply.send(s.read_dtc().map_err(|e| e.to_string()));
            }
            Command::ReadDtc { generation } => {
                let Some(s) = session.as_mut() else {
                    let _ = tx.send(Event::Error {
                        context: "dtc".into(),
                        message: "not connected".into(),
                    });
                    continue;
                };
                let result = s.read_dtc().map_err(|e| e.to_string());
                let _ = tx.send(Event::Dtc { generation, result });
            }
            Command::ClearDtc { .. } => {
                let Some(s) = session.as_mut() else {
                    let _ = tx.send(Event::Error {
                        context: "dtc".into(),
                        message: "not connected".into(),
                    });
                    continue;
                };
                let result = s
                    .clear_dtc()
                    .map(|r| ddtrust_core::hexutil::to_hex_spaced(&r))
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::Cleared { result });
            }
            Command::RawAt(cmd) => {
                let Some(s) = session.as_mut() else {
                    let _ = tx.send(Event::Error {
                        context: "terminal".into(),
                        message: "not connected".into(),
                    });
                    continue;
                };
                let r = s.raw_command(&cmd).map_err(|e| e.to_string());
                let _ = tx.send(Event::Raw { response: r });
            }
            Command::RawHex(hex) => {
                let Some(s) = session.as_mut() else {
                    let _ = tx.send(Event::Error {
                        context: "terminal".into(),
                        message: "not connected".into(),
                    });
                    continue;
                };
                let r = ddtrust_core::hexutil::parse_hex(&hex)
                    .map_err(|e| e.to_string())
                    .and_then(|payload| {
                        s.send_payload(&payload, false)
                            .map(|rsp| ddtrust_core::hexutil::to_hex_spaced(&rsp))
                            .map_err(|e| e.to_string())
                    });
                let _ = tx.send(Event::Raw { response: r });
            }
            Command::Scan {
                vehicle,
                protocol,
                addresses,
                settings,
                cancel,
            } => {
                let link = if settings.kind == AdapterChoice::Simulator {
                    build_sim_scan_link(&db, vehicle.as_ref(), &protocol, tx.clone())
                } else {
                    build_link(&settings, tx.clone(), None)
                };
                match link {
                    Err(e) => {
                        let _ = tx.send(Event::Error {
                            context: "scan".into(),
                            message: e,
                        });
                        let _ = tx.send(Event::ScanDone(Vec::new()));
                    }
                    Ok(mut link) => {
                        let tx2 = tx.clone();
                        let mut on_event = |ev: ScanEvent| {
                            let _ = tx2.send(Event::Scan(ev));
                        };
                        let results = match protocol {
                            Protocol::Can => scan_can_with_cancel(
                                link.as_mut(),
                                &db,
                                vehicle.as_ref(),
                                &addresses,
                                settings.can_baud,
                                &mut on_event,
                                cancel.as_ref(),
                            ),
                            _ => scan_kwp_with_cancel(
                                link.as_mut(),
                                &db,
                                vehicle.as_ref(),
                                &addresses,
                                &mut on_event,
                                cancel.as_ref(),
                            ),
                        };
                        link.close();
                        let _ = tx.send(Event::ScanDone(results));
                    }
                }
            }
        }
    }
}

fn build_sim_scan_link(
    db: &Database,
    vehicle: Option<&Vehicle>,
    protocol: &Protocol,
    tx: Sender<Event>,
) -> std::result::Result<Box<dyn DiagnosticLink>, String> {
    let mut ecus = Vec::new();
    for t in db.vehicle_targets(vehicle) {
        if !t.supports(protocol.clone()) {
            continue;
        }
        let txid = db
            .can_ids(vehicle, &t.address)
            .map(|(tx, _)| tx)
            .unwrap_or_else(|| t.address.clone());
        ecus.push(VirtualEcu::from_target(t, txid));
    }
    let mut elm = Elm327::new(
        Box::new(EmulatedElm::with_ecus(ecus)),
        ElmConfig {
            timeout: Duration::from_secs(2),
            ..Default::default()
        },
    );
    elm.initialize().map_err(|e| e.to_string())?;
    let _ = tx.send(Event::Log(format!(
        "simulator scan ready ({} virtual ECUs)",
        db.vehicle_targets(vehicle).len()
    )));
    Ok(Box::new(elm))
}

fn build_link(
    settings: &AdapterSettings,
    tx: Sender<Event>,
    sim: Option<(&Target, Arc<EcuFile>, Option<String>)>,
) -> std::result::Result<Box<dyn DiagnosticLink>, String> {
    match settings.kind {
        AdapterChoice::Simulator => {
            let Some((target, ecu, sim_tx)) = sim else {
                return Err("simulator requires an ECU".into());
            };
            let tx_id = sim_tx
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| ecu.send_id.clone());
            let v = VirtualEcu::from_file(target, ecu, tx_id);
            let mut elm = Elm327::new(
                Box::new(EmulatedElm::with_ecus(vec![v])),
                ElmConfig::default(),
            );
            elm.initialize().map_err(|e| e.to_string())?;
            let _ = tx.send(Event::Log("simulated ELM327 ready".into()));
            Ok(Box::new(elm))
        }
        AdapterChoice::ElmSerial => {
            let port = settings.port.trim();
            if port.is_empty() {
                return Err("enter a serial port for the ELM327".into());
            }
            let mut elm = Elm327::serial(port, settings.serial_baud, ElmConfig::default())
                .map_err(|e| e.to_string())?;
            elm.initialize().map_err(|e| e.to_string())?;
            Ok(Box::new(elm))
        }
        AdapterChoice::ElmTcp => {
            let addr = settings.tcp.trim();
            if addr.is_empty() {
                return Err("enter a TCP address for the ELM327".into());
            }
            let mut elm = Elm327::tcp(addr, ElmConfig::default()).map_err(|e| e.to_string())?;
            elm.initialize().map_err(|e| e.to_string())?;
            Ok(Box::new(elm))
        }
        AdapterChoice::SocketCan => {
            let interface = settings.can_interface.trim();
            if interface.is_empty() {
                return Err("select a CAN interface (e.g. can0)".into());
            }
            let _ = tx.send(Event::Log(format!(
                "opening SocketCAN {interface} — make sure it is up at {} kbit/s",
                settings.can_baud / 1000
            )));
            ddtrust_core::diag::open_socketcan(interface, Duration::from_secs(5))
                .map_err(|e| e.to_string())
        }
    }
}

fn build_session(
    req: &ConnectRequest,
    tx: &Sender<Event>,
) -> std::result::Result<EcuSession, String> {
    let link = build_link(
        &req.settings,
        tx.clone(),
        Some((&req.target, req.ecu.clone(), req.sim_tx.clone())),
    )?;
    let safety = Safety { expert: req.expert };
    EcuSession::open(link, req.ecu.clone(), safety).map_err(|e| e.to_string())
}
