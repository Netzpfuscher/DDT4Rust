use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use ddtrust_core::diag::{DtcReport, ScanResult};
use ddtrust_core::model::layout::{graphic_name, Font};
use ddtrust_core::model::{LabelDef, Protocol, Screen};
use ddtrust_core::DataStore;
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, TextStyle, Ui, Vec2,
};
use egui::{pos2, vec2};

use crate::agent::{self, AgentAction, AgentConfig, AgentContext, AgentEvent};
use crate::assets::AssetCache;
use crate::screen_view::{key, OpenedEcu, ResolvedDisplay, ScreenRuntime};
use crate::worker::{
    self, AdapterChoice, AdapterSettings, Command, ConnectRequest, Event as WorkerEvent, PollItem,
    SeqItem, WorkerHandle,
};
use serde_json::json;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Dashboard,
    Layout,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Screens,
    Dtc,
    Terminal,
    Info,
    Agent,
}

#[derive(Clone)]
struct ConfirmAction {
    title: String,
    message: String,
    payloads: Vec<(Vec<u8>, u64)>,
    after: AfterAction,
}

#[derive(Clone, Copy)]
enum AfterAction {
    None,
    ClearDtc,
}

struct DtcState {
    report: Option<DtcReport>,
    loading: bool,
    error: Option<String>,
    generation: u64,
}

struct ScanState {
    open: bool,
    protocol: Protocol,
    results: Vec<ScanResult>,
    live: Vec<ScanHit>,
    log: Vec<ScanLog>,
    scanning: bool,
    stop_requested: bool,
    attempted: usize,
    total: usize,
    found: usize,
    errors: usize,
    started: Option<Instant>,
    finished: Option<Instant>,
    cancel: Option<Arc<AtomicBool>>,
    error: Option<String>,
    current_address: String,
    current_group: String,
}

#[derive(Clone)]
struct ScanHit {
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
}

#[derive(Clone)]
struct ScanLog {
    at: Instant,
    address: String,
    group: String,
    text: String,
    kind: ScanLogKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanLogKind {
    Info,
    Found,
    Error,
}

impl ScanState {
    fn new() -> Self {
        Self {
            open: false,
            protocol: Protocol::Can,
            results: Vec::new(),
            live: Vec::new(),
            log: Vec::new(),
            scanning: false,
            stop_requested: false,
            attempted: 0,
            total: 0,
            found: 0,
            errors: 0,
            started: None,
            finished: None,
            cancel: None,
            error: None,
            current_address: String::new(),
            current_group: String::new(),
        }
    }

    fn elapsed(&self) -> Option<Duration> {
        self.started.map(|start| {
            self.finished
                .unwrap_or_else(Instant::now)
                .duration_since(start)
        })
    }

    fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.attempted as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
}

struct TerminalState {
    input: String,
    at_mode: bool,
    output: Vec<String>,
    pending: bool,
}

struct AgentState {
    config: AgentConfig,
    messages: Vec<AgentBubble>,
    history: Vec<serde_json::Value>,
    input: String,
    running: bool,
    show_settings: bool,
    include_context: bool,
    error: Option<String>,
    auto_prompt: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentBubbleRole {
    User,
    Assistant,
    Tool,
    Error,
}

struct AgentBubble {
    role: AgentBubbleRole,
    text: String,
}

impl AgentState {
    fn new() -> Self {
        Self {
            config: AgentConfig::from_env(),
            messages: Vec::new(),
            history: Vec::new(),
            input: String::new(),
            running: false,
            show_settings: false,
            include_context: true,
            error: None,
            auto_prompt: std::env::var("DDTRUST_AGENT_PROMPT").ok(),
        }
    }
}

pub struct App {
    store: Arc<DataStore>,
    db: Arc<ddtrust_core::Database>,
    assets: AssetCache,
    worker: WorkerHandle,
    startup_error: Option<String>,

    selected_vehicle: Option<usize>,
    search: String,
    filtered: Vec<usize>,
    target_search: Vec<String>,
    filter_dirty: bool,
    selected_target: Option<usize>,
    last_ecu_click: Option<(usize, Instant)>,
    opened: Option<OpenedEcu>,
    screen_list: Vec<(Option<String>, String)>,
    screen_categories: Vec<String>,
    category_filter: Option<String>,
    screen_filter: String,
    param_filter: String,
    selected_screen: Option<String>,
    runtime: Option<ScreenRuntime>,
    render_mode: RenderMode,
    fit_requested: bool,

    tab: Tab,
    connected: bool,
    adapter: AdapterChoice,
    expert: bool,
    port: String,
    tcp: String,
    baud: String,
    can_interface: String,
    can_baud: u32,
    available_ports: Vec<String>,
    can_interfaces: Vec<ddtrust_core::diag::CanInterfaceInfo>,
    auto_refresh: bool,
    refresh_ms: u64,
    zoom: f32,

    generation: u64,
    polling: bool,
    last_refresh: Instant,
    toast: Option<(String, Instant)>,
    logs: Vec<String>,

    show_settings: bool,
    show_scan: bool,
    scan: ScanState,
    dtc: DtcState,
    terminal: TerminalState,
    confirm: Option<ConfirmAction>,
    pending_button: Option<(usize, Vec<(Vec<u8>, u64)>)>,
    dark: bool,

    agent: AgentState,
    agent_events_tx: Sender<AgentEvent>,
    agent_events_rx: Receiver<AgentEvent>,
    agent_actions_tx: Sender<AgentAction>,
    agent_actions_rx: Receiver<AgentAction>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        data_path: Option<PathBuf>,
        simulate: bool,
        open_ecu: Option<String>,
        open_screen: Option<String>,
        open_tab: Option<String>,
    ) -> Self {
        setup_style(&cc.egui_ctx, true);
        let (agent_events_tx, agent_events_rx) = crossbeam_channel::unbounded();
        let (agent_actions_tx, agent_actions_rx) = crossbeam_channel::unbounded();
        let loaded = (|| -> anyhow::Result<(DataStore, ddtrust_core::Database)> {
            let store = DataStore::discover(data_path.as_deref())?;
            let db = store.load_database()?;
            Ok((store, db))
        })();
        match loaded {
            Ok((store, db)) => {
                let store = Arc::new(store);
                let assets = AssetCache::new(store.clone());
                let db = Arc::new(db);
                let worker = worker::spawn(db.clone());
                let target_search = build_target_search(&db);
                let mut app = Self {
                    assets,
                    store,
                    db,
                    worker,
                    startup_error: None,
                    selected_vehicle: None,
                    search: String::new(),
                    filtered: Vec::new(),
                    target_search,
                    filter_dirty: true,
                    selected_target: None,
                    last_ecu_click: None,
                    opened: None,
                    screen_list: Vec::new(),
                    screen_categories: Vec::new(),
                    category_filter: None,
                    screen_filter: String::new(),
                    param_filter: String::new(),
                    selected_screen: None,
                    runtime: None,
                    render_mode: RenderMode::Dashboard,
                    fit_requested: false,
                    tab: Tab::Screens,
                    connected: false,
                    adapter: if simulate {
                        AdapterChoice::Simulator
                    } else {
                        AdapterChoice::ElmSerial
                    },
                    expert: false,
                    port: String::new(),
                    tcp: String::new(),
                    baud: "38400".into(),
                    can_interface: "can0".into(),
                    can_baud: 500_000,
                    available_ports: ddtrust_core::elm::transport::try_ports(),
                    can_interfaces: ddtrust_core::diag::can_interfaces(),
                    auto_refresh: true,
                    refresh_ms: 700,
                    zoom: 1.0,
                    generation: 0,
                    polling: false,
                    last_refresh: Instant::now(),
                    toast: None,
                    logs: Vec::new(),
                    show_settings: std::env::var("DDTRUST_SETTINGS").is_ok(),
                    show_scan: false,
                    scan: ScanState::new(),
                    dtc: DtcState {
                        report: None,
                        loading: false,
                        error: None,
                        generation: 0,
                    },
                    terminal: TerminalState {
                        input: String::new(),
                        at_mode: false,
                        output: Vec::new(),
                        pending: false,
                    },
                    confirm: None,
                    pending_button: None,
                    dark: true,
                    agent: AgentState::new(),
                    agent_events_tx,
                    agent_events_rx,
                    agent_actions_tx,
                    agent_actions_rx,
                };
                if let Ok(adapter) = std::env::var("DDTRUST_ADAPTER") {
                    app.adapter = match adapter.to_ascii_lowercase().as_str() {
                        "simulator" | "sim" => AdapterChoice::Simulator,
                        "serial" | "elm" => AdapterChoice::ElmSerial,
                        "tcp" | "wifi" => AdapterChoice::ElmTcp,
                        "socketcan" | "canable" | "candlelight" => AdapterChoice::SocketCan,
                        _ => app.adapter,
                    };
                }
                if let Ok(search) = std::env::var("DDTRUST_SEARCH") {
                    app.search = search;
                    app.filter_dirty = true;
                }
                app.log(format!(
                    "Loaded {} ECU definitions for {} vehicles from {}",
                    app.db.targets.len(),
                    app.db.vehicles.len(),
                    app.store.location()
                ));
                if let Some(name) = open_ecu {
                    let target = app.db.find_by_href_or_name(&name).cloned();
                    if let Some(t) = target {
                        if let Some(idx) = app.db.targets.iter().position(|x| x.href == t.href) {
                            app.open_target(idx);
                            if let Some(screen) = open_screen {
                                app.start_screen(&screen);
                            }
                        }
                    } else {
                        app.toast(format!("Unknown ECU {name}"));
                    }
                }
                if std::env::var("DDTRUST_VIEW")
                    .map(|v| v.eq_ignore_ascii_case("layout"))
                    .unwrap_or(false)
                {
                    app.render_mode = RenderMode::Layout;
                    app.fit_requested = true;
                }
                if std::env::var("DDTRUST_SCAN").is_ok() {
                    app.show_scan = true;
                    app.start_scan();
                }
                if let Some(tab) = open_tab {
                    app.tab = match tab.to_ascii_lowercase().as_str() {
                        "dtc" | "faults" | "fault-codes" => Tab::Dtc,
                        "terminal" => Tab::Terminal,
                        "info" => Tab::Info,
                        "agent" | "ai" => Tab::Agent,
                        _ => Tab::Screens,
                    };
                }
                app
            }
            Err(e) => {
                let (store, db) = fallback_store_and_db();
                let store = Arc::new(store);
                let assets = AssetCache::new(store.clone());
                let db = Arc::new(db);
                let worker = worker::spawn(db.clone());
                let target_search = build_target_search(&db);
                Self {
                    assets,
                    store,
                    db,
                    worker,
                    startup_error: Some(format!("{e:#}")),
                    selected_vehicle: None,
                    search: String::new(),
                    filtered: Vec::new(),
                    target_search,
                    filter_dirty: true,
                    selected_target: None,
                    last_ecu_click: None,
                    opened: None,
                    screen_list: Vec::new(),
                    screen_categories: Vec::new(),
                    category_filter: None,
                    screen_filter: String::new(),
                    param_filter: String::new(),
                    selected_screen: None,
                    runtime: None,
                    render_mode: RenderMode::Dashboard,
                    fit_requested: false,
                    tab: Tab::Screens,
                    connected: false,
                    adapter: if simulate {
                        AdapterChoice::Simulator
                    } else {
                        AdapterChoice::ElmSerial
                    },
                    expert: false,
                    port: String::new(),
                    tcp: String::new(),
                    baud: "38400".into(),
                    can_interface: "can0".into(),
                    can_baud: 500_000,
                    available_ports: ddtrust_core::elm::transport::try_ports(),
                    can_interfaces: ddtrust_core::diag::can_interfaces(),
                    auto_refresh: true,
                    refresh_ms: 700,
                    zoom: 1.0,
                    generation: 0,
                    polling: false,
                    last_refresh: Instant::now(),
                    toast: None,
                    logs: Vec::new(),
                    show_settings: std::env::var("DDTRUST_SETTINGS").is_ok(),
                    show_scan: false,
                    scan: ScanState::new(),
                    dtc: DtcState {
                        report: None,
                        loading: false,
                        error: None,
                        generation: 0,
                    },
                    terminal: TerminalState {
                        input: String::new(),
                        at_mode: false,
                        output: Vec::new(),
                        pending: false,
                    },
                    confirm: None,
                    pending_button: None,
                    dark: true,
                    agent: AgentState::new(),
                    agent_events_tx,
                    agent_events_rx,
                    agent_actions_tx,
                    agent_actions_rx,
                }
            }
        }
    }

    fn log(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.logs.len() > 400 {
            self.logs.drain(..100);
        }
        self.logs.push(msg);
    }

    fn toast(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.log(msg.clone());
        self.toast = Some((msg, Instant::now() + Duration::from_secs(4)));
    }

    fn refresh_filter(&mut self) {
        if !self.filter_dirty {
            return;
        }
        self.filter_dirty = false;
        let vehicle = self.selected_vehicle.and_then(|i| self.db.vehicles.get(i));
        let needle = self.search.trim().to_ascii_lowercase();
        self.filtered = self
            .db
            .targets
            .iter()
            .enumerate()
            .filter(|(i, t)| {
                if let Some(v) = vehicle {
                    if !v.code.eq_ignore_ascii_case("ALL")
                        && t.protocol != Protocol::Doip
                        && !t.projects.iter().any(|p| p.eq_ignore_ascii_case(&v.code))
                    {
                        return false;
                    }
                }
                if needle.is_empty() {
                    return true;
                }
                self.target_search
                    .get(*i)
                    .map(|hay| hay.contains(&needle))
                    .unwrap_or_else(|| {
                        format!("{} {} {} {}", t.href, t.ecuname, t.group, t.address)
                            .to_ascii_lowercase()
                            .contains(&needle)
                    })
            })
            .map(|(i, _)| i)
            .collect();
    }

    fn filtered_screens(&self) -> Vec<(Option<String>, String)> {
        self.screen_list
            .iter()
            .filter(|(cat, screen)| {
                if let Some(filter) = &self.category_filter {
                    if cat.as_deref() != Some(filter.as_str()) {
                        return false;
                    }
                }
                if !self.screen_filter.trim().is_empty() {
                    let needle = self.screen_filter.to_ascii_lowercase();
                    if !screen.to_ascii_lowercase().contains(&needle) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    fn open_target(&mut self, index: usize) {
        let Some(target) = self.db.targets.get(index).cloned() else {
            return;
        };
        match self.store.load_ecu(&target.href) {
            Ok(ecu) => {
                let layout = self.store.load_layout(&target.href).unwrap_or_default();
                let ecu = Arc::new(ecu);
                let layout = Arc::new(layout);
                self.screen_list = layout.ordered_screens();
                self.screen_categories = {
                    let mut cats: Vec<String> = Vec::new();
                    for (cat, _) in &self.screen_list {
                        if let Some(c) = cat {
                            if !cats.iter().any(|x| x == c) {
                                cats.push(c.clone());
                            }
                        }
                    }
                    cats
                };
                self.category_filter = None;
                self.screen_filter.clear();
                self.param_filter.clear();
                self.selected_screen = self.screen_list.first().map(|(_, s)| s.clone());
                self.opened = Some(OpenedEcu {
                    target: target.clone(),
                    ecu: ecu.clone(),
                    layout: layout.clone(),
                    info: None,
                    warnings: Vec::new(),
                });
                self.runtime = None;
                self.dtc = DtcState {
                    report: None,
                    loading: false,
                    error: None,
                    generation: 0,
                };
                self.tab = Tab::Screens;
                self.connected = false;
                self.selected_target = Some(index);
                self.log(format!(
                    "Opened {} — {} requests, {} screens",
                    ecu.ecuname,
                    ecu.requests.len(),
                    layout.screens.len()
                ));
                if let Some(screen) = self.selected_screen.clone() {
                    self.start_screen(&screen);
                }
                if self.adapter == AdapterChoice::Simulator {
                    self.connect();
                }
            }
            Err(e) => self.toast(format!("Cannot load ECU: {e}")),
        }
    }

    fn start_screen(&mut self, name: &str) {
        let Some(opened) = self.opened.clone() else {
            return;
        };
        let Some(screen) = opened.layout.screen(name).cloned() else {
            self.toast(format!("Unknown screen {name}"));
            return;
        };
        let rt = ScreenRuntime::new(name.to_string(), Arc::new(screen), &opened.ecu);
        for e in &rt.errors {
            self.log(e.clone());
        }
        self.selected_screen = Some(name.to_string());
        self.runtime = Some(rt);
        if self.render_mode == RenderMode::Layout {
            self.fit_requested = true;
        }
        self.send_presend();
        self.poll_now();
    }

    fn adapter_settings(&self) -> AdapterSettings {
        AdapterSettings {
            kind: self.adapter,
            port: self.port.trim().to_string(),
            serial_baud: self.baud.trim().parse().unwrap_or(38400),
            tcp: self.tcp.trim().to_string(),
            can_interface: self.can_interface.trim().to_string(),
            can_baud: self.can_baud,
        }
    }

    fn adapter_ready(&self) -> bool {
        match self.adapter {
            AdapterChoice::Simulator => true,
            AdapterChoice::ElmSerial => !self.port.trim().is_empty(),
            AdapterChoice::ElmTcp => !self.tcp.trim().is_empty(),
            AdapterChoice::SocketCan => !self.can_interface.trim().is_empty(),
        }
    }

    fn connect(&mut self) {
        let Some(opened) = self.opened.clone() else {
            self.toast("Open an ECU first");
            return;
        };
        if !self.adapter_ready() {
            self.toast("Configure the selected adapter first");
            self.show_settings = true;
            return;
        }
        let sim_tx = if !opened.ecu.send_id.is_empty() {
            Some(opened.ecu.send_id.clone())
        } else {
            let vehicle = self.selected_vehicle.and_then(|i| self.db.vehicles.get(i));
            self.db
                .can_ids(vehicle, &opened.target.address)
                .map(|(tx, _)| tx)
        };
        let req = ConnectRequest {
            target: opened.target.clone(),
            ecu: opened.ecu.clone(),
            settings: self.adapter_settings(),
            expert: self.expert,
            sim_tx,
        };
        if self.worker.tx.send(Command::Connect(req)).is_err() {
            self.toast("Diagnostic worker stopped");
            return;
        }
        self.connected = false;
        self.polling = false;
        self.log(format!("Connecting to {}…", opened.ecu.ecuname));
    }

    fn disconnect(&mut self) {
        let _ = self.worker.tx.send(Command::Disconnect);
    }

    fn send_presend(&mut self) {
        if !self.connected {
            return;
        }
        let Some(opened) = self.opened.clone() else {
            return;
        };
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        let Ok(presend) = rt.build_presend(&opened.ecu) else {
            return;
        };
        if presend.is_empty() {
            return;
        }
        if !self.expert
            && presend
                .iter()
                .any(|(p, _)| !ddtrust_core::diag::Safety::is_safe(p))
        {
            self.log("Screen pre-send commands blocked in read-only mode");
            return;
        }
        let items: Vec<SeqItem> = presend
            .into_iter()
            .map(|(payload, delay_ms)| SeqItem { payload, delay_ms })
            .collect();
        let _ = self.worker.tx.send(Command::Sequence { items });
    }

    fn poll_now(&mut self) {
        if !self.connected {
            return;
        }
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        let items: Vec<PollItem> = rt
            .poll_items()
            .into_iter()
            .map(|(tag, payload)| PollItem { tag, payload })
            .collect();
        if items.is_empty() {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        if self
            .worker
            .tx
            .send(Command::Poll { generation, items })
            .is_ok()
        {
            self.polling = true;
        }
    }

    fn read_dtc(&mut self) {
        if !self.connected {
            self.toast("Connect first");
            return;
        }
        self.dtc.loading = true;
        self.dtc.error = None;
        self.dtc.generation = self.dtc.generation.wrapping_add(1);
        let _ = self.worker.tx.send(Command::ReadDtc {
            generation: self.dtc.generation,
        });
    }

    fn clear_dtc(&mut self) {
        if !self.connected {
            self.toast("Connect first");
            return;
        }
        self.confirm = Some(ConfirmAction {
            title: "Clear diagnostic trouble codes".into(),
            message: "This will clear all DTCs stored in the ECU. Continue?".into(),
            payloads: Vec::new(),
            after: AfterAction::ClearDtc,
        });
    }

    fn execute_sequence(&mut self, payloads: Vec<(Vec<u8>, u64)>, after: AfterAction) {
        if !self.connected {
            self.toast("Connect first");
            return;
        }
        if matches!(after, AfterAction::ClearDtc) && payloads.is_empty() {
            self.dtc.loading = true;
            self.dtc.generation = self.dtc.generation.wrapping_add(1);
            let _ = self.worker.tx.send(Command::ClearDtc);
            return;
        }
        if !self.expert
            && payloads
                .iter()
                .any(|(p, _)| !ddtrust_core::diag::Safety::is_safe(p))
        {
            self.toast("This action writes to the ECU — enable Expert mode in Settings");
            return;
        }
        let items: Vec<SeqItem> = payloads
            .into_iter()
            .map(|(payload, delay_ms)| SeqItem { payload, delay_ms })
            .collect();
        let _ = self.worker.tx.send(Command::Sequence { items });
        match after {
            AfterAction::ClearDtc => {}
            AfterAction::None => {
                self.last_refresh = Instant::now() - Duration::from_secs(60);
            }
        }
    }

    fn handle_worker_events(&mut self) {
        while let Ok(ev) = self.worker.rx.try_recv() {
            match ev {
                WorkerEvent::Connected { info, warnings } => {
                    self.connected = true;
                    if let Some(opened) = &mut self.opened {
                        opened.info = Some(info.clone());
                        opened.warnings = warnings.clone();
                    }
                    for w in warnings {
                        self.log(format!("Warning: {w}"));
                    }
                    self.toast(format!(
                        "Connected: {} {}",
                        info.description,
                        if info.protocol.is_empty() {
                            String::new()
                        } else {
                            format!("({})", info.protocol)
                        }
                    ));
                    self.last_refresh = Instant::now() - Duration::from_secs(60);
                    self.send_presend();
                    self.poll_now();
                }
                WorkerEvent::Disconnected => {
                    self.connected = false;
                    self.polling = false;
                    self.toast("Disconnected");
                }
                WorkerEvent::Error { context, message } => {
                    self.log(format!("[{context}] {message}"));
                    self.toast(format!("{context}: {message}"));
                    if context == "connect" || context == "scan" {
                        self.connected = false;
                    }
                    if context == "dtc" {
                        self.dtc.loading = false;
                        self.dtc.error = Some(message);
                    }
                }
                WorkerEvent::Poll {
                    generation,
                    results,
                } => {
                    self.polling = false;
                    self.last_refresh = Instant::now();
                    if generation != self.generation {
                        continue;
                    }
                    let Some(opened) = self.opened.clone() else {
                        continue;
                    };
                    let mut errors = Vec::new();
                    if let Some(rt) = &mut self.runtime {
                        for r in results {
                            match r.result {
                                Ok(raw) => rt.apply_response(&opened.ecu, &r.tag, &raw),
                                Err(e) => {
                                    let pairs: Vec<(String, String)> = rt
                                        .displays
                                        .iter()
                                        .filter(|d| d.request == r.tag)
                                        .map(|d| (d.request.clone(), d.item.clone()))
                                        .collect();
                                    for (req, item) in pairs {
                                        rt.values.insert(key(&req, &item), None);
                                    }
                                    errors.push(format!("{}: {e}", r.tag));
                                }
                            }
                        }
                    }
                    for e in errors {
                        self.log(e);
                    }
                }
                WorkerEvent::SequenceDone { errors } => {
                    self.polling = false;
                    for e in errors {
                        self.log(format!("Sequence error: {e}"));
                        self.toast(format!("Sequence error: {e}"));
                    }
                    self.last_refresh = Instant::now() - Duration::from_secs(60);
                }
                WorkerEvent::Dtc { generation, result } => {
                    if generation != self.dtc.generation {
                        continue;
                    }
                    self.dtc.loading = false;
                    match result {
                        Ok(report) => {
                            if report.records.is_empty() {
                                self.toast("No DTC stored");
                            }
                            self.dtc.report = Some(report);
                            self.dtc.error = None;
                        }
                        Err(e) => {
                            self.dtc.error = Some(e.clone());
                            self.log(format!("DTC read failed: {e}"));
                            self.toast(format!("DTC read failed: {e}"));
                        }
                    }
                }
                WorkerEvent::Cleared { result } => {
                    match result {
                        Ok(rsp) => self.toast(format!("DTC cleared ({rsp})")),
                        Err(e) => self.toast(format!("Clear failed: {e}")),
                    }
                    self.read_dtc();
                }
                WorkerEvent::Raw { response } => {
                    self.terminal.pending = false;
                    match response {
                        Ok(rsp) => self.terminal.output.push(format!("< {rsp}")),
                        Err(e) => self.terminal.output.push(format!("! {e}")),
                    }
                    if self.terminal.output.len() > 500 {
                        self.terminal.output.drain(..100);
                    }
                }
                WorkerEvent::Scan(ev) => {
                    match ev {
                        ddtrust_core::diag::ScanEvent::Attempt { address, group } => {
                            self.scan.attempted += 1;
                            self.scan.current_address = address.clone();
                            self.scan.current_group = group.clone();
                            self.scan.log.push(ScanLog {
                                at: Instant::now(),
                                address: address.clone(),
                                group: group.clone(),
                                text: format!("Probing {address} · {group}"),
                                kind: ScanLogKind::Info,
                            });
                        }
                        ddtrust_core::diag::ScanEvent::Found {
                            address,
                            name,
                            href,
                            exact,
                            group,
                            protocol,
                            diagversion,
                            supplier,
                            soft,
                            version,
                            ..
                        } => {
                            self.scan.found += 1;
                            self.scan.live.push(ScanHit {
                                address: address.clone(),
                                name: name.clone(),
                                href,
                                exact,
                                group: group.clone(),
                                protocol,
                                diagversion: diagversion.clone(),
                                supplier: supplier.clone(),
                                soft: soft.clone(),
                                version,
                            });
                            self.scan.log.push(ScanLog {
                                at: Instant::now(),
                                address: address.clone(),
                                group,
                                text: format!("Found {name}"),
                                kind: ScanLogKind::Found,
                            });
                        }
                        ddtrust_core::diag::ScanEvent::NoEcu { address, group } => {
                            self.scan.log.push(ScanLog {
                                at: Instant::now(),
                                address,
                                group,
                                text: "No ECU responded".into(),
                                kind: ScanLogKind::Info,
                            });
                        }
                        ddtrust_core::diag::ScanEvent::Error { address, message } => {
                            self.scan.errors += 1;
                            self.scan.error = Some(message.clone());
                            self.scan.log.push(ScanLog {
                                at: Instant::now(),
                                address,
                                group: self.scan.current_group.clone(),
                                text: message,
                                kind: ScanLogKind::Error,
                            });
                        }
                    }
                    if self.scan.log.len() > 500 {
                        self.scan.log.drain(..100);
                    }
                }
                WorkerEvent::ScanDone(results) => {
                    self.scan.scanning = false;
                    self.scan.finished = Some(Instant::now());
                    self.scan.found = results.len();
                    self.scan.results = results;
                    self.scan.stop_requested = false;
                    self.scan.cancel = None;
                    self.scan.log.push(ScanLog {
                        at: Instant::now(),
                        address: String::new(),
                        group: String::new(),
                        text: format!(
                            "Scan complete: {} ECU(s), {} error(s)",
                            self.scan.found, self.scan.errors
                        ),
                        kind: ScanLogKind::Info,
                    });
                }
                WorkerEvent::Log(msg) => self.log(msg),
            }
        }
    }

    fn draw_top(&mut self, ui: &mut Ui) {
        egui::Panel::top("topbar").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("DDTRust").size(24.0).strong().color(accent()));
                ui.label(
                    RichText::new("Renault / Dacia diagnostic suite")
                        .small()
                        .weak(),
                );
                ui.add_space(16.0);
                let (dot, color) = if self.connected {
                    ("connected", Color32::from_rgb(90, 220, 140))
                } else {
                    ("offline", Color32::from_rgb(160, 160, 170))
                };
                ui.label(RichText::new(dot).color(color));
                let badge_text = match self.adapter {
                    AdapterChoice::Simulator => "SIMULATOR".to_string(),
                    other => other.label().to_uppercase(),
                };
                ui.label(RichText::new(badge_text).small().strong().color(
                    if self.adapter == AdapterChoice::Simulator {
                        Color32::from_rgb(255, 190, 70)
                    } else {
                        muted(self.dark)
                    },
                ));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Settings").clicked() {
                        self.show_settings = true;
                    }
                    let theme = if self.dark { "Light" } else { "Dark" };
                    if ui.button(theme).clicked() {
                        self.dark = !self.dark;
                        setup_style(ui.ctx(), self.dark);
                    }
                    if ui.button("AI Agent").clicked() {
                        self.tab = Tab::Agent;
                    }
                    if ui.button("Scan").clicked() {
                        self.show_scan = true;
                        self.scan.open = true;
                    }
                    if self.connected {
                        if ui.button("Disconnect").clicked() {
                            self.disconnect();
                        }
                    } else if ui.button("Connect").clicked() {
                        self.connect();
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn draw_left(&mut self, ui: &mut Ui) {
        egui::Panel::left("ecus")
            .resizable(true)
            .default_size(340.0)
            .min_size(280.0)
            .max_size(400.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.heading("ECU library");
                let vehicles = &self.db.vehicles;
                let current = self
                    .selected_vehicle
                    .and_then(|i| vehicles.get(i))
                    .map(|v| v.key.clone())
                    .unwrap_or_else(|| "All vehicles".to_string());
                egui::ComboBox::from_id_salt("vehicle")
                    .width(ui.available_width() - 10.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.selected_vehicle.is_none(), "All vehicles")
                            .clicked()
                        {
                            self.selected_vehicle = None;
                            self.filter_dirty = true;
                        }
                        for (i, v) in vehicles.iter().enumerate() {
                            if v.code.eq_ignore_ascii_case("ALL") {
                                continue;
                            }
                            if ui
                                .selectable_label(self.selected_vehicle == Some(i), &v.key)
                                .clicked()
                            {
                                self.selected_vehicle = Some(i);
                                self.filter_dirty = true;
                            }
                        }
                    });
                ui.horizontal(|ui| {
                    let clear_w = if self.search.is_empty() { 0.0 } else { 26.0 };
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search ECU, group, car model, address…")
                            .desired_width((ui.available_width() - clear_w).max(80.0)),
                    );
                    if resp.changed() {
                        self.filter_dirty = true;
                    }
                    if !self.search.is_empty()
                        && ui.small_button("x").on_hover_text("Clear search").clicked()
                    {
                        self.search.clear();
                        self.filter_dirty = true;
                    }
                });
                self.refresh_filter();
                ui.label(
                    RichText::new(format!("{} definitions", self.filtered.len()))
                        .small()
                        .weak(),
                );
                ui.separator();
                let mut open_index = None;
                let mut last_click = self.last_ecu_click;
                ui.spacing_mut().item_spacing.y = 4.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, 46.0, self.filtered.len(), |ui, range| {
                        for &idx in &self.filtered[range] {
                            let t = &self.db.targets[idx];
                            let selected = self.selected_target == Some(idx);
                            let title = if t.group.is_empty() {
                                t.ecuname.clone()
                            } else {
                                format!("[{}] {}", t.group, t.ecuname)
                            };
                            let row_w = ui.available_width().max(120.0);
                            let (rect, resp) =
                                ui.allocate_exact_size(vec2(row_w, 44.0), Sense::click());
                            let dark = ui.visuals().dark_mode;
                            if selected {
                                ui.painter()
                                    .rect_filled(rect, 8.0, accent().gamma_multiply(0.16));
                                ui.painter().rect_filled(
                                    Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                                    2.0,
                                    accent(),
                                );
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 8.0, surface_alt(dark));
                            }
                            if !selected {
                                let y = rect.bottom() - 0.5;
                                ui.painter().line_segment(
                                    [pos2(rect.left() + 10.0, y), pos2(rect.right() - 10.0, y)],
                                    Stroke::new(1.0, border_color(dark).gamma_multiply(0.55)),
                                );
                            }
                            let inner = rect.shrink2(vec2(10.0, 6.0));
                            ui.scope_builder(
                                egui::UiBuilder::new()
                                    .max_rect(inner)
                                    .layout(egui::Layout::top_down(Align::Min)),
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.set_max_width(inner.width());
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&title).size(12.8).color(fg_color(dark)),
                                        )
                                        .truncate()
                                        .halign(Align::Min)
                                        .selectable(false),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!(
                                                "{} · @{}",
                                                t.protocol, t.address
                                            ))
                                            .size(10.5)
                                            .color(muted(dark)),
                                        )
                                        .truncate()
                                        .halign(Align::Min)
                                        .selectable(false),
                                    );
                                },
                            );
                            let resp = resp.on_hover_text(title);
                            let double = resp.double_clicked()
                                || last_click
                                    .map(|(i, t)| {
                                        i == idx && t.elapsed() < Duration::from_millis(450)
                                    })
                                    .unwrap_or(false);
                            if resp.clicked() || double {
                                self.selected_target = Some(idx);
                                if double {
                                    open_index = Some(idx);
                                    last_click = None;
                                } else {
                                    last_click = Some((idx, Instant::now()));
                                }
                            }
                        }
                    });
                self.last_ecu_click = last_click;
                ui.separator();
                ui.horizontal(|ui| {
                    let can_open = self.selected_target.is_some();
                    if ui
                        .add_enabled(can_open, egui::Button::new("Open ECU"))
                        .clicked()
                    {
                        open_index = self.selected_target;
                    }
                    if let Some(idx) = open_index {
                        self.open_target(idx);
                    }
                });
            });
    }

    fn draw_central(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(err) = &self.startup_error {
                ui.add_space(40.0);
                ui.heading("ECU database not found");
                ui.label(RichText::new(err).color(Color32::from_rgb(255, 120, 120)));
                ui.add_space(8.0);
                ui.label("Place ddt4all's ecu.zip next to the executable, or start DDTRust with a data directory:");
                ui.code("ddtrust-gui /path/to/ecu.zip");
                ui.code("ddtrust-gui /path/to/extracted/ecus");
                return;
            }
            let Some(opened) = self.opened.clone() else {
                if self.tab == Tab::Agent {
                    self.draw_agent_tab(ui);
                } else {
                    draw_welcome(ui, &self.db, &self.store);
                }
                return;
            };
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(&opened.ecu.ecuname);
                ui.label(RichText::new(format!("{} · {} · @{}", opened.ecu.protocol, opened.target.group, opened.target.address)).weak());
                if self.connected {
                    ui.label(RichText::new("live").color(Color32::from_rgb(90, 220, 140)));
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(self.connected, egui::Button::new("Refresh")).clicked() {
                        self.poll_now();
                    }
                    ui.checkbox(&mut self.auto_refresh, "Auto refresh");
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Screens, "Screens");
                ui.selectable_value(&mut self.tab, Tab::Dtc, "Fault codes");
                ui.selectable_value(&mut self.tab, Tab::Terminal, "Terminal");
                ui.selectable_value(&mut self.tab, Tab::Info, "ECU info");
                ui.selectable_value(&mut self.tab, Tab::Agent, "AI Agent");
                if let Some(info) = &opened.info {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("{} · {}", info.description, info.protocol)).small().weak());
                    });
                }
            });
            ui.separator();
            if self.tab == Tab::Dtc
                && self.connected
                && self.dtc.report.is_none()
                && !self.dtc.loading
                && self.dtc.error.is_none()
            {
                self.read_dtc();
            }
            match self.tab {
                Tab::Screens => self.draw_screens_tab(ui, &opened),
                Tab::Dtc => self.draw_dtc_tab(ui),
                Tab::Terminal => self.draw_terminal_tab(ui),
                Tab::Info => self.draw_info_tab(ui, &opened),
                Tab::Agent => self.draw_agent_tab(ui),
            }
        });
    }

    fn draw_screens_tab(&mut self, ui: &mut Ui, opened: &OpenedEcu) {
        let screens = self.filtered_screens();
        let current = self.selected_screen.clone().unwrap_or_else(|| "—".into());
        let mut chosen_screen: Option<String> = None;
        let mut cycle: i32 = 0;

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!screens.is_empty(), egui::Button::new("<").frame(false))
                .on_hover_text("Previous screen")
                .clicked()
            {
                cycle = -1;
            }
            egui::ComboBox::from_id_salt("screen")
                .width(360.0)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (cat, screen) in &screens {
                        let label = match cat {
                            Some(c) => format!("[{c}] {screen}"),
                            None => screen.clone(),
                        };
                        if ui
                            .selectable_label(
                                self.selected_screen.as_deref() == Some(screen.as_str()),
                                label,
                            )
                            .clicked()
                        {
                            chosen_screen = Some(screen.clone());
                        }
                    }
                });
            if ui
                .add_enabled(!screens.is_empty(), egui::Button::new(">").frame(false))
                .on_hover_text("Next screen")
                .clicked()
            {
                cycle = 1;
            }
            ui.label(
                RichText::new(format!("{} of {}", screens.len(), self.screen_list.len()))
                    .small()
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let previous = self.render_mode;
                ui.selectable_value(&mut self.render_mode, RenderMode::Layout, "Layout");
                ui.selectable_value(&mut self.render_mode, RenderMode::Dashboard, "Dashboard");
                if self.render_mode != previous && self.render_mode == RenderMode::Layout {
                    self.fit_requested = true;
                }
                ui.label(RichText::new("View").small().weak());
            });
        });

        if let Some(screen) = chosen_screen {
            self.pending_button = None;
            self.start_screen(&screen);
        } else if cycle != 0 && !screens.is_empty() {
            let pos = self
                .selected_screen
                .as_ref()
                .and_then(|s| screens.iter().position(|(_, x)| x == s))
                .unwrap_or(0);
            let next = (pos as i32 + cycle).rem_euclid(screens.len() as i32) as usize;
            let screen = screens[next].1.clone();
            self.pending_button = None;
            self.start_screen(&screen);
        }

        ui.horizontal_wrapped(|ui| {
            if chip(ui, "All screens", self.category_filter.is_none()).clicked() {
                self.category_filter = None;
            }
            for cat in self.screen_categories.clone() {
                let selected = self.category_filter.as_deref() == Some(cat.as_str());
                if chip(ui, &cat, selected).clicked() {
                    self.category_filter = if selected { None } else { Some(cat.clone()) };
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.screen_filter)
                    .hint_text("Find screen…")
                    .desired_width(200.0),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if self.render_mode == RenderMode::Layout {
                    if ui
                        .button("Fit width")
                        .on_hover_text("Fit the layout width to the window (vertical scrolling stays available)")
                        .clicked()
                    {
                        self.fit_requested = true;
                    }
                    if ui.button("1:1").clicked() {
                        self.zoom = 1.0;
                    }
                    ui.add(
                        egui::Slider::new(&mut self.zoom, 0.1..=4.0)
                            .fixed_decimals(2)
                            .text("zoom"),
                    );
                }
            });
        });

        ui.add_space(6.0);
        if let Some(mut rt) = self.runtime.take() {
            let screen = rt.screen.clone();
            match self.render_mode {
                RenderMode::Dashboard => self.draw_dashboard(ui, opened, &screen, &mut rt),
                RenderMode::Layout => self.draw_screen_canvas(ui, opened, &screen, &mut rt),
            }
            self.runtime = Some(rt);
        } else {
            ui.add_space(24.0);
            ui.label("Choose a screen above. Screens are generated from the ECU definition.");
        }
    }

    fn draw_dashboard(
        &mut self,
        ui: &mut Ui,
        opened: &OpenedEcu,
        screen: &Screen,
        rt: &mut ScreenRuntime,
    ) {
        let dark = ui.visuals().dark_mode;
        let mut clicked: Option<usize> = None;
        let mut refresh = false;
        let needle = self.param_filter.trim().to_ascii_lowercase();
        let total = rt.displays.len();
        let shown = rt
            .displays
            .iter()
            .filter(|d| needle.is_empty() || d.def.text.to_ascii_lowercase().contains(&needle))
            .count();

        egui::ScrollArea::vertical()
            .id_salt("dashboard_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width().max(320.0));
                // Header card
                egui::Frame::new()
                    .fill(card_fill(dark))
                    .corner_radius(16.0)
                    .stroke(Stroke::new(1.0, border_color(dark)))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(if dark { 90 } else { 28 }),
                    })
                    .inner_margin(18.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&rt.name)
                                            .size(23.0)
                                            .strong()
                                            .color(fg_color(dark)),
                                    );
                                    if let Some(cat) =
                                        opened.layout.categories_for(&rt.name).first()
                                    {
                                        badge(ui, cat, accent(), accent().gamma_multiply(0.16));
                                    }
                                });
                                ui.add_space(2.0);
                                ui.horizontal(|ui| {
                                    badge(
                                        ui,
                                        &opened.ecu.protocol.to_string(),
                                        muted(dark),
                                        surface_alt(dark),
                                    );
                                    badge(
                                        ui,
                                        &format!("{} parameters", total),
                                        muted(dark),
                                        surface_alt(dark),
                                    );
                                    if !rt.screen.presend.is_empty() {
                                        badge(
                                            ui,
                                            &format!("{} pre-send", rt.screen.presend.len()),
                                            muted(dark),
                                            surface_alt(dark),
                                        );
                                    }
                                    if let Some(info) = &opened.info {
                                        ui.label(
                                            RichText::new(&info.protocol)
                                                .small()
                                                .color(muted(dark)),
                                        );
                                    }
                                });
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if self.connected {
                                    let t = ui.input(|i| i.time) as f32;
                                    let pulse = 0.55 + 0.45 * (t * 2.2).sin().abs();
                                    ui.label(RichText::new("LIVE").strong().color(
                                        Color32::from_rgb(92, 221, 145).gamma_multiply(pulse),
                                    ));
                                } else {
                                    ui.label(RichText::new("OFFLINE").small().color(muted(dark)));
                                }
                                if ui.button("Refresh").on_hover_text("Refresh").clicked() {
                                    refresh = true;
                                }
                                if self.polling {
                                    ui.spinner();
                                }
                            });
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.param_filter)
                                    .hint_text("Filter parameters…")
                                    .desired_width(240.0),
                            );
                            if !self.param_filter.is_empty() && ui.small_button("x").clicked() {
                                self.param_filter.clear();
                            }
                            ui.label(
                                RichText::new(format!("{shown} / {total} shown"))
                                    .small()
                                    .color(muted(dark)),
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                let age = self.last_refresh.elapsed().as_secs_f32();
                                let text = if !self.connected {
                                    "not connected".to_string()
                                } else if age < 1.0 {
                                    "updated just now".to_string()
                                } else {
                                    format!("updated {:.0}s ago", age)
                                };
                                ui.label(RichText::new(text).small().color(muted(dark)));
                            });
                        });
                    });

                // Context chips from labels (helpful for screens whose layout carries captions).
                let mut seen: HashSet<String> = HashSet::new();
                let context: Vec<String> = rt
                    .labels
                    .iter()
                    .filter(|l| !l.text.trim().is_empty() && graphic_name(&l.text).is_none())
                    .map(|l| l.text.trim().to_string())
                    .filter(|t| t.len() < 48 && seen.insert(t.clone()))
                    .take(10)
                    .collect();
                if !context.is_empty() {
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        for c in context {
                            badge(ui, &c, muted(dark), surface_alt(dark));
                        }
                    });
                }

                // Inputs
                let mut input_keys = HashSet::new();
                let inputs: Vec<crate::screen_view::ResolvedInput> = rt
                    .inputs
                    .iter()
                    .filter(|i| input_keys.insert((i.request.clone(), i.item.clone())))
                    .cloned()
                    .collect();
                if !inputs.is_empty() {
                    ui.add_space(16.0);
                    section_header(
                        ui,
                        "Configuration",
                        "Values sent with the action buttons",
                        dark,
                    );
                    egui::Frame::new()
                        .fill(card_fill(dark))
                        .corner_radius(14.0)
                        .stroke(Stroke::new(1.0, border_color(dark)))
                        .inner_margin(16.0)
                        .show(ui, |ui| {
                            egui::Grid::new(("modern_inputs", &rt.name))
                                .num_columns(2)
                                .spacing([28.0, 12.0])
                                .show(ui, |ui| {
                                    for i in &inputs {
                                        ui.vertical(|ui| {
                                            ui.set_max_width(320.0);
                                            ui.label(
                                                RichText::new(&i.def.text)
                                                    .size(13.0)
                                                    .strong()
                                                    .color(fg_color(dark)),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} · {}",
                                                    i.request, i.item
                                                ))
                                                .size(10.5)
                                                .color(muted(dark)),
                                            );
                                        });
                                        let value = rt.input_value(&i.request, &i.item);
                                        let def = opened.ecu.data(&i.item);
                                        let labels: Vec<String> = def
                                            .map(|d| {
                                                let mut items: Vec<(i64, String)> = d
                                                    .lists
                                                    .iter()
                                                    .map(|(k, v)| (*k, v.clone()))
                                                    .collect();
                                                items.sort_by_key(|(k, _)| *k);
                                                items.into_iter().map(|(_, v)| v).collect()
                                            })
                                            .unwrap_or_default();
                                        if labels.is_empty() {
                                            let mut v = value;
                                            let resp = ui.add(
                                                egui::TextEdit::singleline(&mut v)
                                                    .hint_text("value")
                                                    .desired_width(260.0)
                                                    .margin(vec2(8.0, 5.0)),
                                            );
                                            if resp.changed() {
                                                rt.input_values.insert(key(&i.request, &i.item), v);
                                            }
                                        } else {
                                            let mut v = if value.is_empty() {
                                                labels[0].clone()
                                            } else {
                                                value
                                            };
                                            egui::ComboBox::from_id_salt((
                                                "modern_input",
                                                &i.request,
                                                &i.item,
                                            ))
                                            .width(260.0)
                                            .selected_text(&v)
                                            .show_ui(
                                                ui,
                                                |ui| {
                                                    for l in &labels {
                                                        ui.selectable_value(&mut v, l.clone(), l);
                                                    }
                                                },
                                            );
                                            rt.input_values.insert(key(&i.request, &i.item), v);
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                }

                // Value groups, one section per request.
                let mut groups: Vec<(String, Vec<&ResolvedDisplay>)> = Vec::new();
                for d in &rt.displays {
                    if !needle.is_empty() && !d.def.text.to_ascii_lowercase().contains(&needle) {
                        continue;
                    }
                    if let Some((_, items)) = groups.iter_mut().find(|(r, _)| r == &d.request) {
                        items.push(d);
                    } else {
                        groups.push((d.request.clone(), vec![d]));
                    }
                }

                if groups.is_empty() && !rt.displays.is_empty() {
                    ui.add_space(20.0);
                    ui.label(RichText::new("No parameter matches the filter.").color(muted(dark)));
                }

                for (request, items) in &groups {
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(request)
                                .size(16.0)
                                .strong()
                                .color(fg_color(dark)),
                        );
                        badge(
                            ui,
                            &format!("{}", items.len()),
                            accent(),
                            accent().gamma_multiply(0.16),
                        );
                        if let Some(req) = opened.ecu.request(request) {
                            let hex = req.sent_hex();
                            if !hex.is_empty() {
                                ui.label(
                                    RichText::new(hex).monospace().size(10.5).color(muted(dark)),
                                );
                            }
                        }
                    });
                    ui.add_space(6.0);
                    let avail = (ui.available_width() - 42.0).max(200.0);
                    let cols = (((avail + 12.0) / 248.0).floor() as usize).clamp(1, 6);
                    let card_w =
                        ((avail - 12.0 * (cols.saturating_sub(1)) as f32) / cols as f32).max(180.0);
                    for row in items.chunks(cols) {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            for d in row {
                                ui.allocate_ui_with_layout(
                                    vec2(card_w, 0.0),
                                    egui::Layout::top_down(Align::Min),
                                    |ui| value_card(ui, rt, d, card_w, dark),
                                );
                            }
                        });
                        ui.add_space(10.0);
                    }
                }

                // Actions
                if !screen.buttons.is_empty() {
                    ui.add_space(6.0);
                    section_header(
                        ui,
                        "Actions",
                        "Requests / coding commands for this screen",
                        dark,
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                        for (idx, button) in screen.buttons.iter().enumerate() {
                            let label = if button.text.trim().is_empty() {
                                format!("Action {}", idx + 1)
                            } else {
                                button.text.clone()
                            };
                            let tip = if button.send.is_empty() {
                                "No request attached".to_string()
                            } else {
                                button
                                    .send
                                    .iter()
                                    .map(|s| format!("{} ({} ms)", s.request, s.delay.0 as u64))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            };
                            let resp = if let Some(name) = graphic_name(&button.text) {
                                self.assets.ensure(ui.ctx(), &name);
                                let size = vec2(130.0, 46.0);
                                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                                let fill = if resp.hovered() {
                                    surface_hover(dark)
                                } else {
                                    surface_alt(dark)
                                };
                                ui.painter().rect_filled(rect, 10.0, fill);
                                ui.painter().rect_stroke(
                                    rect,
                                    10.0,
                                    Stroke::new(1.0, border_color(dark)),
                                    egui::StrokeKind::Inside,
                                );
                                if let Some(anim) = self.assets.get(&name) {
                                    if let Some(tex) = anim.frame_at(ui.input(|i| i.time)) {
                                        let img_rect = fit_rect(rect.shrink(5.0), anim.size);
                                        ui.painter().image(
                                            tex.id(),
                                            img_rect,
                                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                            Color32::WHITE,
                                        );
                                    }
                                }
                                resp
                            } else {
                                let text = if button.messages.is_empty() {
                                    label
                                } else {
                                    format!("! {label}")
                                };
                                ui.add(
                                    egui::Button::new(
                                        RichText::new(text)
                                            .strong()
                                            .size(13.5)
                                            .color(Color32::from_rgb(30, 26, 12)),
                                    )
                                    .fill(accent())
                                    .corner_radius(10.0)
                                    .min_size(vec2(150.0, 42.0)),
                                )
                            };
                            let resp = resp.on_hover_text(tip);
                            if resp.clicked() {
                                clicked = Some(idx);
                            }
                        }
                    });
                }

                // Graphics gallery
                let mut pics: Vec<(String, String)> = Vec::new();
                let mut seen_pics = HashSet::new();
                for l in &rt.labels {
                    if let Some(name) = graphic_name(&l.text) {
                        if seen_pics.insert(name.clone()) {
                            let caption =
                                name.rsplit('/').next().unwrap_or(&name).replace('_', " ");
                            pics.push((name, caption));
                        }
                    }
                }
                if !pics.is_empty() {
                    ui.add_space(14.0);
                    egui::CollapsingHeader::new(
                        RichText::new(format!("Screen graphics ({})", pics.len()))
                            .size(13.0)
                            .color(muted(dark)),
                    )
                    .id_salt(("graphics", &rt.name))
                    .default_open(false)
                    .show(ui, |ui| {
                        egui::ScrollArea::horizontal().show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (name, caption) in pics.iter().take(16) {
                                    self.assets.ensure(ui.ctx(), &name);
                                    ui.vertical(|ui| {
                                        let size = vec2(150.0, 92.0);
                                        let (rect, _) =
                                            ui.allocate_exact_size(size, Sense::hover());
                                        ui.painter().rect_filled(rect, 8.0, surface_alt(dark));
                                        if let Some(anim) = self.assets.get(name) {
                                            if let Some(tex) = anim.frame_at(ui.input(|i| i.time)) {
                                                let img_rect =
                                                    fit_rect(rect.shrink(4.0), anim.size);
                                                ui.painter().image(
                                                    tex.id(),
                                                    img_rect,
                                                    Rect::from_min_max(
                                                        pos2(0.0, 0.0),
                                                        pos2(1.0, 1.0),
                                                    ),
                                                    Color32::WHITE,
                                                );
                                            }
                                        }
                                        ui.label(
                                            RichText::new(caption).size(10.0).color(muted(dark)),
                                        );
                                    });
                                }
                                if pics.len() > 16 {
                                    ui.label(
                                        RichText::new(format!(
                                            "+{} more — use Layout view",
                                            pics.len() - 16
                                        ))
                                        .small()
                                        .color(muted(dark)),
                                    );
                                }
                            });
                        });
                    });
                }

                if !rt.errors.is_empty() {
                    ui.add_space(14.0);
                    egui::Frame::new()
                        .fill(Color32::from_rgb(80, 45, 22).gamma_multiply(if dark {
                            1.0
                        } else {
                            0.15
                        }))
                        .corner_radius(12.0)
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("Some widgets could not be resolved")
                                    .strong()
                                    .color(Color32::from_rgb(255, 190, 110)),
                            );
                            for e in rt.errors.iter().take(5) {
                                ui.label(RichText::new(e).small().color(muted(dark)));
                            }
                        });
                }
                ui.add_space(20.0);
            });

        if refresh {
            self.poll_now();
        }
        if let Some(idx) = clicked {
            self.on_button_clicked(opened, rt, idx);
        }
    }

    fn draw_screen_canvas(
        &mut self,
        ui: &mut Ui,
        opened: &OpenedEcu,
        screen: &Screen,
        rt: &mut ScreenRuntime,
    ) {
        if self.fit_requested {
            let avail = ui.available_size();
            if avail.x > 80.0 && avail.y > 80.0 {
                let base_w = (screen.width() as f32 / 8.0).max(1.0);
                // Layout view fits the width only; tall screens scroll vertically.
                self.zoom = ((avail.x - 22.0).max(40.0) / base_w).clamp(0.1, 4.0);
                self.fit_requested = false;
            }
        }
        let s = self.zoom / 8.0;
        let canvas_size = vec2(
            (screen.width() as f32 * s).max(200.0),
            (screen.height() as f32 * s).max(200.0),
        );
        let mut clicked: Option<usize> = None;
        egui::ScrollArea::both()
            .id_salt("layout_canvas")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (canvas_rect, _) = ui.allocate_exact_size(canvas_size, Sense::hover());
                let origin = canvas_rect.min;
                let bg = screen_color(&screen.color, Color32::from_rgb(0, 0, 30));
                ui.painter().rect_filled(canvas_rect, 0.0, bg);

                // Background labels first (large to small), then values, then interactive widgets.
                let mut label_order: Vec<&LabelDef> = rt.labels.iter().collect();
                label_order
                    .sort_by_key(|l| std::cmp::Reverse((l.bbox.width.0 * l.bbox.height.0) as i64));
                for label in &label_order {
                    let rect = layout_rect(origin, &label.bbox, s);
                    self.draw_label(ui, label, rect, s, opened);
                }
                for d in &rt.displays {
                    let rect = layout_rect(origin, &d.def.rect, s);
                    let name_w = (d.def.width.0 as f32 * s).clamp(0.0, rect.width());
                    let color = screen_color(&d.def.color, Color32::from_rgb(205, 205, 205));
                    let fg = screen_color(&d.def.fontcolor, Color32::from_rgb(10, 10, 10));
                    ui.painter().rect_filled(rect, 2.0, color);
                    ui.painter().rect_stroke(
                        rect,
                        2.0,
                        Stroke::new(1.0, Color32::from_black_alpha(90)),
                        egui::StrokeKind::Inside,
                    );
                    let font = font_for(&d.def.font, s);
                    let name_rect = Rect::from_min_size(rect.min, vec2(name_w, rect.height()));
                    draw_text_clipped(
                        ui,
                        name_rect.shrink2(vec2(3.0, 1.0)),
                        &d.def.text,
                        font.clone(),
                        fg,
                        Align2::LEFT_CENTER,
                    );
                    let mut value = rt.display_value(&d.request, &d.item);
                    if let Some(rt_value) = rt.values.get(&key(&d.request, &d.item)) {
                        if rt_value.is_none() {
                            value = Some("N/A".into());
                        }
                    }
                    let text = match value {
                        Some(v) if !v.is_empty() => format!("{} {}", v, d.unit),
                        _ => "—".to_string(),
                    };
                    let value_rect =
                        Rect::from_min_max(pos2(rect.left() + name_w, rect.top()), rect.max);
                    let vfg = if text == "N/A" {
                        Color32::from_rgb(160, 20, 20)
                    } else {
                        fg
                    };
                    draw_text_clipped(
                        ui,
                        value_rect.shrink2(vec2(3.0, 1.0)),
                        &text,
                        font.clone(),
                        vfg,
                        Align2::LEFT_CENTER,
                    );
                }
                // Inputs
                for i in &rt.inputs {
                    let rect = layout_rect(origin, &i.def.rect, s);
                    let name_w = (i.def.width.0 as f32 * s).clamp(0.0, rect.width());
                    let color = screen_color(&i.def.color, Color32::from_rgb(170, 170, 170));
                    let fg = screen_color(&i.def.fontcolor, Color32::from_rgb(10, 10, 10));
                    ui.painter().rect_filled(rect, 2.0, color);
                    let font = font_for(&i.def.font, s);
                    let name_rect = Rect::from_min_size(rect.min, vec2(name_w, rect.height()));
                    draw_text_clipped(
                        ui,
                        name_rect.shrink2(vec2(3.0, 1.0)),
                        &i.def.text,
                        font.clone(),
                        fg,
                        Align2::LEFT_CENTER,
                    );
                    let widget_rect = Rect::from_min_max(
                        pos2(rect.left() + name_w + 2.0, rect.top() + 1.0),
                        pos2(rect.right() - 1.0, rect.bottom() - 1.0),
                    );
                    let def = opened.ecu.data(&i.item);
                    let labels: Vec<String> = def
                        .map(|d| {
                            let mut items: Vec<(i64, String)> =
                                d.lists.iter().map(|(k, v)| (*k, v.clone())).collect();
                            items.sort_by_key(|(k, _)| *k);
                            items.into_iter().map(|(_, v)| v).collect()
                        })
                        .unwrap_or_default();
                    let value = rt.input_value(&i.request, &i.item);
                    if labels.is_empty() {
                        let mut v = value;
                        let resp = ui.put(
                            widget_rect,
                            egui::TextEdit::singleline(&mut v)
                                .font(font.clone())
                                .desired_width(widget_rect.width())
                                .margin(vec2(2.0, 1.0)),
                        );
                        if resp.changed() {
                            rt.input_values.insert(key(&i.request, &i.item), v);
                        }
                    } else {
                        let mut v = if value.is_empty() {
                            labels[0].clone()
                        } else {
                            value
                        };
                        ui.scope_builder(egui::UiBuilder::new().max_rect(widget_rect), |ui| {
                            egui::ComboBox::from_id_salt(("input", &i.request, &i.item))
                                .width(widget_rect.width())
                                .selected_text(&v)
                                .show_ui(ui, |ui| {
                                    for l in &labels {
                                        ui.selectable_value(&mut v, l.clone(), l);
                                    }
                                });
                        });
                        rt.input_values.insert(key(&i.request, &i.item), v);
                    }
                    if !i.def.text.is_empty() {
                        let _ = def;
                    }
                }
                // Buttons
                for (idx, button) in screen.buttons.iter().enumerate().rev() {
                    let rect = layout_rect(origin, &button.rect, s);
                    let resp = ui.interact(
                        rect,
                        ui.make_persistent_id(("button", &rt.name, idx)),
                        Sense::click(),
                    );
                    let fill = if resp.is_pointer_button_down_on() {
                        Color32::from_rgb(232, 186, 52)
                    } else if resp.hovered() {
                        Color32::from_rgb(255, 215, 110)
                    } else {
                        Color32::from_rgb(247, 199, 72)
                    };
                    ui.painter().rect_filled(rect, 7.0, fill);
                    ui.painter().rect_stroke(
                        rect,
                        7.0,
                        Stroke::new(1.0, Color32::from_black_alpha(100)),
                        egui::StrokeKind::Inside,
                    );
                    let font = font_for(&button.font, s);
                    if let Some(name) = graphic_name(&button.text) {
                        self.assets.ensure(ui.ctx(), &name);
                        if let Some(anim) = self.assets.get(&name) {
                            if let Some(tex) = anim.frame_at(ui.input(|i| i.time)) {
                                let img_rect = fit_rect(rect.shrink(2.0), anim.size);
                                ui.painter().image(
                                    tex.id(),
                                    img_rect,
                                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                        }
                    } else {
                        draw_text_clipped(
                            ui,
                            rect.shrink2(vec2(4.0, 2.0)),
                            &button.text,
                            font,
                            Color32::from_rgb(20, 20, 20),
                            Align2::CENTER_CENTER,
                        );
                    }
                    let tip = if button.send.is_empty() {
                        "no request attached".to_string()
                    } else {
                        button
                            .send
                            .iter()
                            .map(|s| format!("{} ({} ms)", s.request, s.delay.0 as u64))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    let resp = resp.on_hover_text(tip);
                    if resp.clicked() {
                        clicked = Some(idx);
                    }
                }
                if let Some(events) = self.runtime.as_ref().map(|_| &rt.errors) {
                    if !events.is_empty() {
                        let mut y = 4.0;
                        for e in events.iter().take(6) {
                            ui.painter().text(
                                pos2(origin.x + 8.0, origin.y + y),
                                Align2::LEFT_TOP,
                                format!("! {e}"),
                                FontId::proportional(11.0),
                                Color32::from_rgb(255, 120, 120),
                            );
                            y += 14.0;
                        }
                    }
                }
            });
        if let Some(idx) = clicked {
            self.on_button_clicked(opened, rt, idx);
        }
    }

    fn draw_label(
        &mut self,
        ui: &mut Ui,
        label: &LabelDef,
        rect: Rect,
        _s: f32,
        opened: &OpenedEcu,
    ) {
        let color = screen_color(&label.color, Color32::TRANSPARENT);
        let fg = screen_color(&label.fontcolor, Color32::from_rgb(10, 10, 10));
        if color != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, 0.0, color);
        }
        if let Some(name) = graphic_name(&label.text) {
            self.assets.ensure(ui.ctx(), &name);
            if let Some(anim) = self.assets.get(&name) {
                if let Some(tex) = anim.frame_at(ui.input(|i| i.time)) {
                    let r = fit_rect(rect, anim.size);
                    ui.painter().image(
                        tex.id(),
                        r,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            return;
        }
        if label.text.trim().is_empty() {
            return;
        }
        let font = font_for(&label.font, _s);
        let align = match label.alignment.trim() {
            "1" => Align2::RIGHT_CENTER,
            "2" => Align2::CENTER_CENTER,
            _ => Align2::LEFT_TOP,
        };
        draw_text_clipped(
            ui,
            rect.shrink2(vec2(2.0, 1.0)),
            &label.text,
            font,
            fg,
            align,
        );
        let _ = opened;
    }

    fn on_button_clicked(&mut self, opened: &OpenedEcu, rt: &ScreenRuntime, idx: usize) {
        let Some(button) = rt.screen.buttons.get(idx).cloned() else {
            return;
        };
        match rt.build_button_payloads(&opened.ecu, idx) {
            Ok(payloads) => {
                if payloads.is_empty() {
                    self.toast("Button has no request");
                    return;
                }
                if !button.messages.is_empty() {
                    self.confirm = Some(ConfirmAction {
                        title: button.text.clone(),
                        message: button.messages.join("\n"),
                        payloads,
                        after: AfterAction::None,
                    });
                } else {
                    self.execute_sequence(payloads, AfterAction::None);
                }
            }
            Err(e) => self.toast(format!("Cannot build request: {e}")),
        }
    }

    fn draw_dtc_tab(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.dtc.loading, egui::Button::new("Read DTC"))
                .clicked()
            {
                self.read_dtc();
            }
            if ui
                .add_enabled(!self.dtc.loading, egui::Button::new("Clear all DTC"))
                .clicked()
            {
                self.clear_dtc();
            }
            if self.dtc.loading {
                ui.spinner();
                ui.label("Reading…");
            }
            if let Some(report) = &self.dtc.report {
                ui.label(RichText::new(format!("{} record(s)", report.records.len())).weak());
            }
        });
        if let Some(err) = &self.dtc.error {
            ui.label(RichText::new(err).color(Color32::from_rgb(255, 120, 120)));
        }
        ui.separator();
        let Some(report) = &self.dtc.report else {
            ui.label("Read the fault memory to list diagnostic trouble codes.");
            return;
        };
        if report.records.is_empty() {
            ui.label("No DTC stored.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for r in &report.records {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(r.code_text()).strong().monospace());
                        if let Some(name) = &r.name {
                            ui.label(RichText::new(name).strong());
                        }
                        ui.label(RichText::new(format!("#{}", r.index + 1)).small().weak());
                    });
                    egui::Grid::new(("dtc", r.index))
                        .num_columns(2)
                        .spacing([12.0, 2.0])
                        .show(ui, |ui| {
                            for f in &r.fields {
                                ui.label(RichText::new(&f.name).small().weak());
                                let v = f.value.clone().unwrap_or_else(|| "?".into());
                                ui.label(format!("{v} {}", f.unit));
                                ui.end_row();
                            }
                        });
                });
            }
        });
    }

    fn draw_terminal_tab(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.terminal.at_mode, false, "Diagnostic bytes");
            ui.selectable_value(&mut self.terminal.at_mode, true, "Adapter command");
            if ui.button("Clear").clicked() {
                self.terminal.output.clear();
            }
        });
        ui.horizontal(|ui| {
            let hint = if self.terminal.at_mode {
                "ATI, ATSP6, ATSH…"
            } else {
                "21 80 / 22 F1 90"
            };
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.terminal.input)
                    .hint_text(hint)
                    .desired_width(ui.available_width() - 90.0),
            );
            let send = ui.button("Send").clicked()
                || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if send && !self.terminal.input.trim().is_empty() {
                let cmd = self.terminal.input.trim().to_string();
                self.terminal.output.push(format!("> {cmd}"));
                self.terminal.pending = true;
                if self.terminal.at_mode {
                    let _ = self.worker.tx.send(Command::RawAt(cmd));
                } else {
                    let _ = self.worker.tx.send(Command::RawHex(cmd));
                }
                self.terminal.input.clear();
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.terminal.output {
                    ui.label(RichText::new(line).monospace().size(12.0));
                }
            });
    }

    fn draw_info_tab(&mut self, ui: &mut Ui, opened: &OpenedEcu) {
        let ecu = &opened.ecu;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("ecuinfo")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for (k, v) in [
                        ("File", ecu.href.clone()),
                        ("Protocol", ecu.protocol.to_string()),
                        ("Address", opened.target.address.clone()),
                        ("CAN TX", ecu.send_id.clone()),
                        ("CAN RX", ecu.recv_id.clone()),
                        ("Baudrate", format!("{} kbit/s", ecu.baudrate / 1000)),
                        ("Endian", ecu.endian_str.clone()),
                        ("Requests", ecu.requests.len().to_string()),
                        ("Parameters", ecu.data.len().to_string()),
                        ("Screens", opened.layout.screens.len().to_string()),
                        ("Groups", opened.layout.categories.len().to_string()),
                    ] {
                        ui.label(RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    }
                    if let Some(info) = &opened.info {
                        ui.label(RichText::new("Adapter").weak());
                        ui.label(format!("{} ({})", info.description, info.protocol));
                        ui.end_row();
                    }
                });
            if !opened.warnings.is_empty() {
                ui.separator();
                for w in &opened.warnings {
                    ui.label(
                        RichText::new(format!("! {w}")).color(Color32::from_rgb(255, 180, 90)),
                    );
                }
            }
            ui.separator();
            ui.heading("Identification records");
            egui::Grid::new("idents")
                .num_columns(4)
                .striped(true)
                .spacing([14.0, 3.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Diag").strong());
                    ui.label(RichText::new("Supplier").strong());
                    ui.label(RichText::new("Soft").strong());
                    ui.label(RichText::new("Version").strong());
                    ui.end_row();
                    for a in &ecu.autoidents {
                        ui.label(&a.diagversion);
                        ui.label(&a.supplier);
                        ui.label(&a.soft);
                        ui.label(&a.version);
                        ui.end_row();
                    }
                });
            ui.separator();
            ui.heading("Diagnostic sessions");
            for req in ecu.session_requests() {
                ui.label(format!("{}  [{}]", req.name, req.sent_hex()));
            }
        });
        let _ = opened;
    }

    fn agent_context(&self) -> AgentContext {
        let opened = self.opened.as_ref();
        let mut values = Vec::new();
        if self.agent.include_context {
            if let Some(rt) = &self.runtime {
                for d in rt.displays.iter().take(120) {
                    values.push(agent::ContextValue {
                        name: d.def.text.clone(),
                        request: d.request.clone(),
                        value: rt.display_value(&d.request, &d.item),
                        unit: d.unit.clone(),
                    });
                }
            }
        }
        AgentContext {
            db: self.db.clone(),
            vehicle: self
                .selected_vehicle
                .and_then(|i| self.db.vehicles.get(i))
                .map(|v| v.key.clone()),
            connected: self.connected,
            ecu: opened.map(|o| o.ecu.clone()),
            address: opened.map(|o| o.target.address.clone()).unwrap_or_default(),
            group: opened.map(|o| o.target.group.clone()).unwrap_or_default(),
            screen: self.selected_screen.clone(),
            screens: if self.agent.include_context {
                self.screen_list.clone()
            } else {
                Vec::new()
            },
            values,
            dtc: if self.agent.include_context {
                self.dtc.report.clone()
            } else {
                None
            },
        }
    }

    fn send_agent_message(&mut self) {
        if self.agent.running {
            return;
        }
        let text = self.agent.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if !self.agent.config.is_configured() {
            self.agent.error = Some(
                "Configure an OpenAI-compatible endpoint and API key first (or use a local endpoint)."
                    .into(),
            );
            self.agent.show_settings = true;
            return;
        }
        self.agent.messages.push(AgentBubble {
            role: AgentBubbleRole::User,
            text: text.clone(),
        });
        self.agent.input.clear();
        self.agent.running = true;
        self.agent.error = None;
        let history = self.agent.history.clone();
        self.agent
            .history
            .push(json!({"role": "user", "content": text.clone()}));
        if self.agent.history.len() > 30 {
            let drop = self.agent.history.len() - 30;
            self.agent.history.drain(..drop);
        }
        let context = self.agent_context();
        agent::start(
            self.agent.config.clone(),
            history,
            text,
            context,
            self.worker.tx.clone(),
            self.agent_actions_tx.clone(),
            self.agent_events_tx.clone(),
        );
    }

    fn handle_agent_events(&mut self) {
        while let Ok(event) = self.agent_events_rx.try_recv() {
            match event {
                AgentEvent::Tool(text) => {
                    self.agent.messages.push(AgentBubble {
                        role: AgentBubbleRole::Tool,
                        text,
                    });
                }
                AgentEvent::Assistant(text) => {
                    self.agent.messages.push(AgentBubble {
                        role: AgentBubbleRole::Assistant,
                        text: text.clone(),
                    });
                    self.agent
                        .history
                        .push(json!({"role": "assistant", "content": text}));
                    if self.agent.history.len() > 30 {
                        let drop = self.agent.history.len() - 30;
                        self.agent.history.drain(..drop);
                    }
                    self.agent.running = false;
                }
                AgentEvent::Error(text) => {
                    self.agent.messages.push(AgentBubble {
                        role: AgentBubbleRole::Error,
                        text: text.clone(),
                    });
                    self.agent.error = Some(text);
                    self.agent.running = false;
                }
            }
            if self.agent.messages.len() > 200 {
                self.agent.messages.drain(..50);
            }
        }
    }

    fn handle_agent_actions(&mut self) {
        while let Ok(action) = self.agent_actions_rx.try_recv() {
            match action {
                AgentAction::OpenEcu(href) => {
                    let idx = self
                        .db
                        .targets
                        .iter()
                        .position(|t| t.href.eq_ignore_ascii_case(&href))
                        .or_else(|| {
                            self.db
                                .find_by_href_or_name(&href)
                                .and_then(|t| self.db.targets.iter().position(|x| x.href == t.href))
                        });
                    match idx {
                        Some(idx) => {
                            self.open_target(idx);
                            self.toast(format!("Agent opened {href}"));
                        }
                        None => {
                            self.agent.messages.push(AgentBubble {
                                role: AgentBubbleRole::Error,
                                text: format!("Agent could not find ECU {href}"),
                            });
                        }
                    }
                }
                AgentAction::OpenScreen(name) => {
                    let canonical = self
                        .screen_list
                        .iter()
                        .find(|(_, s)| s.eq_ignore_ascii_case(&name))
                        .map(|(_, s)| s.clone());
                    if let Some(screen) = canonical {
                        self.start_screen(&screen);
                        self.tab = Tab::Screens;
                        self.toast(format!("Agent opened screen {screen}"));
                    } else {
                        self.agent.messages.push(AgentBubble {
                            role: AgentBubbleRole::Error,
                            text: format!("Agent could not find screen {name}"),
                        });
                    }
                }
            }
        }
    }

    fn draw_agent_tab(&mut self, ui: &mut Ui) {
        let dark = ui.visuals().dark_mode;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("AI Agent")
                    .size(22.0)
                    .strong()
                    .color(fg_color(dark)),
            );
            badge(
                ui,
                "READ-ONLY",
                Color32::from_rgb(120, 230, 160),
                Color32::from_rgb(30, 70, 48),
            );
            ui.label(
                RichText::new("Diagnostic copilot for the current session")
                    .small()
                    .color(muted(dark)),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button("Clear")
                    .on_hover_text("Clear conversation")
                    .clicked()
                {
                    self.agent.messages.clear();
                    self.agent.history.clear();
                    self.agent.error = None;
                }
                if ui
                    .selectable_label(self.agent.show_settings, "Settings")
                    .clicked()
                {
                    self.agent.show_settings = !self.agent.show_settings;
                }
            });
        });
        ui.label(
            RichText::new(
                "The agent can search ECUs, read live values and DTCs, and navigate screens. \
                 It cannot write coding, clear faults or run actuators.",
            )
            .small()
            .color(muted(dark)),
        );

        if self.agent.show_settings {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(card_fill(dark))
                .corner_radius(12.0)
                .stroke(Stroke::new(1.0, border_color(dark)))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.label(RichText::new("OpenAI-compatible endpoint").strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.agent.config.endpoint)
                            .hint_text("https://api.deepseek.com/chat/completions")
                            .desired_width(ui.available_width()),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Model");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.agent.config.model)
                                .desired_width(180.0),
                        );
                        ui.label("API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.agent.config.api_key)
                                .password(true)
                                .hint_text("stays in memory")
                                .desired_width(240.0),
                        );
                    });
                    ui.checkbox(
                        &mut self.agent.include_context,
                        "Send the current ECU/screen/value context with each prompt",
                    );
                    ui.label(
                        RichText::new(
                            "Defaults to DeepSeek (deepseek-chat). Any OpenAI-compatible endpoint works, \
                             including OpenAI and local Ollama: http://localhost:11434/v1/chat/completions",
                        )
                        .small()
                        .color(muted(dark)),
                    );
                    if !self.agent.config.is_configured() {
                        ui.label(
                            RichText::new(
                                "An API key is required for remote endpoints; local endpoints may omit it.",
                            )
                            .small()
                            .color(Color32::from_rgb(240, 180, 90)),
                        );
                    }
                });
        }
        if let Some(err) = &self.agent.error {
            if !self.agent.show_settings {
                ui.label(
                    RichText::new(err)
                        .small()
                        .color(Color32::from_rgb(240, 150, 100)),
                );
            }
        }

        if self.agent.messages.is_empty() {
            ui.add_space(18.0);
            egui::Frame::new()
                .fill(card_fill(dark))
                .corner_radius(16.0)
                .stroke(Stroke::new(1.0, border_color(dark)))
                .inner_margin(18.0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Ask your diagnostic copilot")
                            .size(18.0)
                            .strong()
                            .color(fg_color(dark)),
                    );
                    ui.label(
                        RichText::new(if self.opened.is_some() {
                            "It knows the current ECU, screen and latest values. Ask it to explain DTCs, \
                             interpret parameters or find the right screen."
                        } else {
                            "Open an ECU first, or ask it to find one for a specific car model."
                        })
                        .small()
                        .color(muted(dark)),
                    );
                    ui.add_space(10.0);
                    let mut suggestion: Option<String> = None;
                    ui.horizontal_wrapped(|ui| {
                        for text in [
                            "Find ECUs for a Clio IV",
                            "Summarize the current ECU",
                            "Read the fault codes and explain them",
                            "Which values on this screen look abnormal?",
                        ] {
                            if chip(ui, text, false).clicked() {
                                suggestion = Some(text.to_string());
                            }
                        }
                    });
                    if let Some(text) = suggestion {
                        self.agent.input = text;
                        self.send_agent_message();
                    }
                });
        }

        let input_height = 96.0;
        let chat_height = (ui.available_height() - input_height).max(160.0);
        egui::ScrollArea::vertical()
            .id_salt("agent_chat")
            .stick_to_bottom(true)
            .max_height(chat_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for bubble in &self.agent.messages {
                    ui.add_space(4.0);
                    match bubble.role {
                        AgentBubbleRole::User => {
                            ui.with_layout(egui::Layout::right_to_left(Align::TOP), |ui| {
                                let max_w = (ui.available_width() * 0.78).max(220.0);
                                ui.allocate_ui_with_layout(
                                    vec2(max_w, 0.0),
                                    egui::Layout::top_down(Align::Max),
                                    |ui| {
                                        egui::Frame::new()
                                            .fill(accent().gamma_multiply(0.18))
                                            .corner_radius(12.0)
                                            .inner_margin(10.0)
                                            .show(ui, |ui| {
                                                ui.set_max_width(max_w - 20.0);
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(strip_markdown(&bubble.text))
                                                            .size(13.0)
                                                            .color(fg_color(dark)),
                                                    )
                                                    .wrap(),
                                                );
                                            });
                                    },
                                );
                            });
                        }
                        AgentBubbleRole::Assistant => {
                            ui.with_layout(egui::Layout::left_to_right(Align::TOP), |ui| {
                                let max_w = (ui.available_width() * 0.9).max(260.0);
                                ui.allocate_ui_with_layout(
                                    vec2(max_w, 0.0),
                                    egui::Layout::top_down(Align::Min),
                                    |ui| {
                                        egui::Frame::new()
                                            .fill(card_fill(dark))
                                            .corner_radius(12.0)
                                            .stroke(Stroke::new(1.0, border_color(dark)))
                                            .inner_margin(12.0)
                                            .show(ui, |ui| {
                                                ui.set_max_width(max_w - 24.0);
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(strip_markdown(&bubble.text))
                                                            .size(13.0)
                                                            .color(fg_color(dark)),
                                                    )
                                                    .wrap(),
                                                );
                                            });
                                    },
                                );
                            });
                        }
                        AgentBubbleRole::Tool => {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("tool")
                                        .monospace()
                                        .size(9.5)
                                        .color(muted(dark)),
                                );
                                ui.label(RichText::new(&bubble.text).size(10.5).color(muted(dark)));
                            });
                        }
                        AgentBubbleRole::Error => {
                            egui::Frame::new()
                                .fill(Color32::from_rgb(70, 36, 24).gamma_multiply(if dark {
                                    1.0
                                } else {
                                    0.2
                                }))
                                .corner_radius(10.0)
                                .inner_margin(10.0)
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("! {}", bubble.text))
                                            .size(12.0)
                                            .color(Color32::from_rgb(250, 170, 120)),
                                    );
                                });
                        }
                    }
                }
                if self.agent.running {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Agent is working…")
                                .small()
                                .color(muted(dark)),
                        );
                    });
                }
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let send_w = 84.0;
            let resp = ui.add(
                egui::TextEdit::multiline(&mut self.agent.input)
                    .hint_text("Ask about the car, ECU, DTCs or values… (Ctrl+Enter to send)")
                    .desired_rows(2)
                    .desired_width((ui.available_width() - send_w - 22.0).max(160.0)),
            );
            let enter = resp.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl);
            let can_send = !self.agent.running && !self.agent.input.trim().is_empty();
            if ui
                .add_enabled(
                    can_send,
                    egui::Button::new(
                        RichText::new(if self.agent.running { "…" } else { "Send" }).strong(),
                    )
                    .fill(accent())
                    .corner_radius(9.0)
                    .min_size(vec2(send_w, 42.0)),
                )
                .clicked()
                || (enter && can_send)
            {
                self.send_agent_message();
            }
        });
        ui.label(
            RichText::new(
                "Prompts and selected diagnostic context are sent to the configured endpoint. \
                 API keys stay in memory.",
            )
            .size(10.0)
            .color(muted(dark)),
        );
    }

    fn draw_settings(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        let mut close_requested = false;
        let mut reconnect_requested = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(700.0)
            .default_height(780.0)
            .min_width(620.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let scroll_h = (ui.available_height() - 48.0).max(220.0);
                egui::ScrollArea::vertical().max_height(scroll_h).show(ui, |ui| {
                    ui.heading("Adapter");
                    ui.label(
                        RichText::new(
                            "Choose how DDTRust talks to the car. Adapter changes apply on the next Connect.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.add_space(6.0);
                    let socketcan = ddtrust_core::diag::socketcan_supported();
                    let choices = [
                        (AdapterChoice::Simulator, true),
                        (AdapterChoice::ElmSerial, true),
                        (AdapterChoice::ElmTcp, true),
                        (AdapterChoice::SocketCan, socketcan),
                    ];
                    for (choice, enabled) in choices {
                        let selected = self.adapter == choice;
                        let resp = adapter_card(
                            ui,
                            selected,
                            choice.label(),
                            choice.description(),
                            enabled,
                            self.dark,
                        );
                        if resp.clicked() && enabled {
                            self.adapter = choice;
                        }
                        ui.add_space(4.0);
                    }

                    ui.add_space(8.0);
                    egui::Frame::new()
                        .fill(card_fill(self.dark))
                        .corner_radius(12.0)
                        .stroke(Stroke::new(1.0, border_color(self.dark)))
                        .inner_margin(14.0)
                        .show(ui, |ui| match self.adapter {
                            AdapterChoice::Simulator => {
                                ui.label(RichText::new("Simulator selected").strong());
                                ui.label(
                                    RichText::new(
                                        "Uses the built-in virtual ELM327 and ECU model. No hardware or \
                                         vehicle access is required; values are synthetic.",
                                    )
                                    .small()
                                    .color(muted(self.dark)),
                                );
                            }
                            AdapterChoice::ElmSerial => {
                                ui.label(RichText::new("Serial port").strong());
                                ui.horizontal(|ui| {
                                    if self.available_ports.is_empty() {
                                        ui.label(
                                            RichText::new("No serial ports detected")
                                                .small()
                                                .color(Color32::from_rgb(240, 180, 90)),
                                        );
                                    } else {
                                        egui::ComboBox::from_id_salt("serial_port")
                                            .width(260.0)
                                            .selected_text(if self.port.is_empty() {
                                                "Select port…".to_string()
                                            } else {
                                                self.port.clone()
                                            })
                                            .show_ui(ui, |ui| {
                                                for p in &self.available_ports {
                                                    ui.selectable_value(
                                                        &mut self.port,
                                                        p.clone(),
                                                        p,
                                                    );
                                                }
                                            });
                                    }
                                    if ui.button("Refresh").clicked() {
                                        self.available_ports =
                                            ddtrust_core::elm::transport::try_ports();
                                    }
                                });
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.port)
                                        .hint_text("/dev/ttyUSB0 or COM3")
                                        .desired_width(ui.available_width()),
                                );
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.label("Adapter baud");
                                    for baud in [9600, 38400, 57600, 115200, 230400, 500000] {
                                        let text = baud.to_string();
                                        if ui
                                            .selectable_label(self.baud == text, text)
                                            .clicked()
                                        {
                                            self.baud = baud.to_string();
                                        }
                                    }
                                });
                                ui.label(
                                    RichText::new(
                                        "Most ELM327 clones use 38400 or 115200. The app tries the \
                                         configured speed first, then common fallbacks.",
                                    )
                                    .small()
                                    .color(muted(self.dark)),
                                );
                            }
                            AdapterChoice::ElmTcp => {
                                ui.label(RichText::new("TCP address").strong());
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.tcp)
                                        .hint_text("192.168.0.10:35000")
                                        .desired_width(ui.available_width()),
                                );
                                ui.label(
                                    RichText::new(
                                        "For Wi-Fi ELM327 adapters. Put the adapter in TCP server mode.",
                                    )
                                    .small()
                                    .color(muted(self.dark)),
                                );
                            }
                            AdapterChoice::SocketCan => {
                                if !ddtrust_core::diag::socketcan_supported() {
                                    ui.label(
                                        RichText::new(
                                            "CandleLight / SocketCAN is currently supported on Linux only.",
                                        )
                                        .color(Color32::from_rgb(240, 180, 90)),
                                    );
                                    return;
                                }
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new("CAN interface").strong());
                                        if self.can_interfaces.is_empty() {
                                            ui.label(
                                                RichText::new("No CAN interfaces detected")
                                                    .small()
                                                    .color(Color32::from_rgb(240, 180, 90)),
                                            );
                                        } else {
                                            egui::ComboBox::from_id_salt("can_iface")
                                                .width(220.0)
                                                .selected_text(&self.can_interface)
                                                .show_ui(ui, |ui| {
                                                    for iface in &self.can_interfaces {
                                                        ui.selectable_value(
                                                            &mut self.can_interface,
                                                            iface.name.clone(),
                                                            format!("{} ({})", iface.name, iface.state),
                                                        );
                                                    }
                                                });
                                        }
                                    });
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new("CAN bitrate").strong());
                                        egui::ComboBox::from_id_salt("can_bitrate")
                                            .width(150.0)
                                            .selected_text(format!("{} kbit/s", self.can_baud / 1000))
                                            .show_ui(ui, |ui| {
                                                for baud in [125_000, 250_000, 500_000, 1_000_000] {
                                                    ui.selectable_value(
                                                        &mut self.can_baud,
                                                        baud,
                                                        format!("{} kbit/s", baud / 1000),
                                                    );
                                                }
                                            });
                                    });
                                });
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.can_interface)
                                        .hint_text("can0")
                                        .desired_width(200.0),
                                );
                                if ui.button("Refresh interfaces").clicked() {
                                    self.can_interfaces = ddtrust_core::diag::can_interfaces();
                                }
                                let state = self
                                    .can_interfaces
                                    .iter()
                                    .find(|i| i.name == self.can_interface)
                                    .map(|i| i.state.clone())
                                    .unwrap_or_else(|| {
                                        ddtrust_core::diag::can_interface_state(
                                            &self.can_interface,
                                        )
                                    });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("State").small().weak());
                                    ui.label(
                                        RichText::new(state)
                                            .small()
                                            .color(if self
                                                .can_interfaces
                                                .iter()
                                                .find(|i| i.name == self.can_interface)
                                                .map(|i| i.state == "up")
                                                .unwrap_or(false)
                                            {
                                                Color32::from_rgb(92, 221, 145)
                                            } else {
                                                Color32::from_rgb(240, 180, 90)
                                            }),
                                    );
                                });
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(
                                        "Bring the interface up before connecting, for example:",
                                    )
                                    .small()
                                    .color(muted(self.dark)),
                                );
                                let cmd = format!(
                                    "sudo ip link set {} up type can bitrate {}",
                                    self.can_interface, self.can_baud
                                );
                                ui.code(&cmd);
                                ui.label(
                                    RichText::new(
                                        "CandleLight firmware appears as can0 via the gs_usb driver. \
                                         SocketCAN is CAN-only: KWP2000/ISO8 (K-line) ECUs are not accessible.",
                                    )
                                    .small()
                                    .color(muted(self.dark)),
                                );
                            }
                        });

                    ui.add_space(12.0);
                    ui.heading("Safety & behaviour");
                    if ui
                        .checkbox(
                            &mut self.expert,
                            "Expert mode (allow writes, coding, actuators)",
                        )
                        .changed()
                    {
                        let _ = self.worker.tx.send(Command::SetExpert(self.expert));
                    }
                    ui.label(
                        RichText::new("Read-only services: 10, 12, 14, 17, 19, 1A, 21, 22, 23, 3E")
                            .small()
                            .weak(),
                    );
                    ui.checkbox(&mut self.auto_refresh, "Auto refresh screens");
                    ui.add(
                        egui::Slider::new(&mut self.refresh_ms, 100..=5000)
                            .text("refresh interval (ms)"),
                    );
                    ui.separator();
                    ui.heading("Data");
                    ui.label(self.store.location());
                    ui.label(
                        RichText::new(
                            "ddt4all's ecu.zip or an extracted ECU directory is supported.",
                        )
                        .small()
                        .weak(),
                    );
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Close").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Reconnect").clicked() && self.connected {
                        reconnect_requested = true;
                        close_requested = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("Active: {}", self.adapter.label()))
                                .small()
                                .color(muted(self.dark)),
                        );
                    });
                });
            });
        self.show_settings = open && !close_requested;
        if reconnect_requested {
            self.disconnect();
            self.show_settings = false;
            self.connect();
        }
    }

    fn start_scan(&mut self) {
        if self.scan.scanning {
            return;
        }
        if !self.adapter_ready() {
            self.scan.error = Some("Configure the selected adapter first".into());
            self.show_settings = true;
            return;
        }
        let vehicle = self
            .selected_vehicle
            .and_then(|i| self.db.vehicles.get(i))
            .cloned();
        let addresses = self
            .db
            .scan_addresses(vehicle.as_ref(), self.scan.protocol.clone());
        if addresses.is_empty() {
            self.scan.error = Some("No scan addresses for this vehicle/protocol".into());
            return;
        }
        self.scan.results.clear();
        self.scan.live.clear();
        self.scan.log.clear();
        self.scan.attempted = 0;
        self.scan.total = addresses.len();
        self.scan.found = 0;
        self.scan.errors = 0;
        self.scan.error = None;
        self.scan.scanning = true;
        self.scan.stop_requested = false;
        self.scan.started = Some(Instant::now());
        self.scan.finished = None;
        self.scan.current_address.clear();
        self.scan.current_group.clear();
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan.cancel = Some(cancel.clone());
        self.scan.log.push(ScanLog {
            at: Instant::now(),
            address: String::new(),
            group: String::new(),
            text: format!(
                "Starting {} scan of {} addresses using {}",
                self.scan.protocol,
                addresses.len(),
                self.adapter.label()
            ),
            kind: ScanLogKind::Info,
        });
        let _ = self.worker.tx.send(Command::Scan {
            vehicle,
            protocol: self.scan.protocol.clone(),
            addresses,
            settings: self.adapter_settings(),
            cancel,
        });
    }

    fn stop_scan(&mut self) {
        if let Some(cancel) = &self.scan.cancel {
            cancel.store(true, Ordering::Relaxed);
            self.scan.stop_requested = true;
            self.scan.log.push(ScanLog {
                at: Instant::now(),
                address: String::new(),
                group: String::new(),
                text: "Cancel requested — finishing the current address".into(),
                kind: ScanLogKind::Info,
            });
        }
    }

    fn draw_scan_window(&mut self, ctx: &egui::Context) {
        if !self.show_scan {
            return;
        }
        let dark = self.dark;
        let vehicle_label = self
            .selected_vehicle
            .and_then(|i| self.db.vehicles.get(i))
            .map(|v| v.key.clone())
            .unwrap_or_else(|| "All vehicles".into());
        let available_addresses = {
            let vehicle = self.selected_vehicle.and_then(|i| self.db.vehicles.get(i));
            self.db.scan_addresses(vehicle, self.scan.protocol.clone())
        };
        let can_start =
            !self.scan.scanning && self.adapter_ready() && !available_addresses.is_empty();
        let mut open = self.show_scan;
        let mut start_scan = false;
        let mut stop_scan = false;
        let mut open_href: Option<String> = None;

        egui::Window::new("Scan the bus")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([880.0, 680.0])
            .min_width(700.0)
            .max_width((ctx.content_rect().width() - 80.0).max(700.0))
            .max_height((ctx.content_rect().height() - 60.0).max(480.0))
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Bus scanner")
                            .size(22.0)
                            .strong()
                            .color(fg_color(dark)),
                    );
                    badge(
                        ui,
                        self.adapter.label(),
                        accent(),
                        accent().gamma_multiply(0.15),
                    );
                    if self.scan.scanning {
                        let t = ui.input(|i| i.time) as f32;
                        let pulse = 0.55 + 0.45 * (t * 2.2).sin().abs();
                        status_dot(
                            ui,
                            Color32::from_rgb(92, 221, 145).gamma_multiply(pulse),
                            7.0,
                        );
                        ui.label(
                            RichText::new(if self.scan.stop_requested {
                                "CANCELLING"
                            } else {
                                "LIVE"
                            })
                            .strong()
                            .color(Color32::from_rgb(92, 221, 145)),
                        );
                    } else if self.scan.finished.is_some() {
                        badge(
                            ui,
                            "COMPLETE",
                            Color32::from_rgb(92, 221, 145),
                            Color32::from_rgb(28, 70, 46),
                        );
                    } else {
                        badge(ui, "READY", muted(dark), surface_alt(dark));
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if self.scan.scanning {
                            if ui
                                .add_enabled(
                                    !self.scan.stop_requested,
                                    egui::Button::new("Stop scan"),
                                )
                                .clicked()
                            {
                                stop_scan = true;
                            }
                        } else if ui
                            .add_enabled(
                                can_start,
                                egui::Button::new(RichText::new("Start scan").strong())
                                    .fill(accent())
                                    .corner_radius(9.0),
                            )
                            .clicked()
                        {
                            start_scan = true;
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Probe every diagnostic address for the selected vehicle. Results are \
                         streamed live; you can stop the scan at any time.",
                    )
                    .small()
                    .color(muted(dark)),
                );
                ui.add_space(8.0);

                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Protocol").small().color(muted(dark)));
                    let can = self.scan.protocol == Protocol::Can;
                    if ui
                        .add_enabled(!self.scan.scanning, egui::RadioButton::new(can, "CAN"))
                        .clicked()
                    {
                        self.scan.protocol = Protocol::Can;
                    }
                    let kwp = self.scan.protocol == Protocol::Kwp2000;
                    let kwp_enabled =
                        !self.scan.scanning && self.adapter != AdapterChoice::SocketCan;
                    if ui
                        .add_enabled(kwp_enabled, egui::RadioButton::new(kwp, "KWP2000 / K-line"))
                        .clicked()
                    {
                        self.scan.protocol = Protocol::Kwp2000;
                    }
                    if self.adapter == AdapterChoice::SocketCan {
                        self.scan.protocol = Protocol::Can;
                    }
                    ui.separator();
                    ui.label(RichText::new("Vehicle").small().color(muted(dark)));
                    ui.label(vehicle_label.clone());
                    ui.separator();
                    ui.label(RichText::new("Addresses").small().color(muted(dark)));
                    ui.label(
                        (if self.scan.scanning {
                            self.scan.total
                        } else {
                            available_addresses.len()
                        })
                        .to_string(),
                    );
                });
                if !self.scan.scanning && available_addresses.is_empty() {
                    ui.label(
                        RichText::new("No scan addresses for this vehicle/protocol.")
                            .color(Color32::from_rgb(240, 180, 90)),
                    );
                }
                if !self.scan.scanning && !self.adapter_ready() {
                    ui.label(
                        RichText::new("The selected adapter is not configured yet.")
                            .color(Color32::from_rgb(240, 180, 90)),
                    );
                }

                ui.add_space(8.0);
                let elapsed = self.scan.elapsed().unwrap_or_default().as_secs_f32();
                let speed = if elapsed > 0.0 {
                    self.scan.attempted as f32 / elapsed
                } else {
                    0.0
                };
                let eta = if self.scan.scanning && speed > 0.01 {
                    let remaining = self.scan.total.saturating_sub(self.scan.attempted);
                    Some(remaining as f32 / speed)
                } else {
                    None
                };
                egui::Frame::new()
                    .fill(card_fill(dark))
                    .corner_radius(12.0)
                    .stroke(Stroke::new(1.0, border_color(dark)))
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(if self.scan.scanning {
                                    "Scanning…"
                                } else if self.scan.finished.is_some() {
                                    if self.scan.attempted < self.scan.total {
                                        "Scan cancelled"
                                    } else {
                                        "Scan complete"
                                    }
                                } else {
                                    "Ready to scan"
                                })
                                .strong()
                                .color(fg_color(dark)),
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if let Some(eta) = eta {
                                    ui.label(
                                        RichText::new(format!("ETA {:.0}s", eta))
                                            .small()
                                            .color(muted(dark)),
                                    );
                                }
                                ui.label(
                                    RichText::new(format!("{:.1} addr/s", speed))
                                        .small()
                                        .color(muted(dark)),
                                );
                            });
                        });
                        ui.add_space(6.0);
                        let fraction = if self.scan.scanning || self.scan.finished.is_some() {
                            self.scan.fraction()
                        } else {
                            0.0
                        };
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .desired_width(ui.available_width())
                                .text(format!(
                                    "{} / {} addresses · {} found · {} errors",
                                    self.scan.attempted,
                                    self.scan.total.max(available_addresses.len()),
                                    self.scan.found,
                                    self.scan.errors
                                )),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Elapsed {:.1}s", elapsed))
                                    .small()
                                    .color(muted(dark)),
                            );
                            ui.separator();
                            if self.scan.current_address.is_empty() {
                                ui.label(
                                    RichText::new("Waiting for first address")
                                        .small()
                                        .color(muted(dark)),
                                );
                            } else {
                                ui.label(
                                    RichText::new(format!(
                                        "Probing {} · {}",
                                        self.scan.current_address, self.scan.current_group
                                    ))
                                    .small()
                                    .color(muted(dark)),
                                );
                            }
                        });
                    });

                if let Some(err) = &self.scan.error {
                    ui.label(
                        RichText::new(format!("! {err}"))
                            .small()
                            .color(Color32::from_rgb(240, 150, 100)),
                    );
                }

                ui.add_space(8.0);
                ui.columns(2, |cols| {
                    // Live activity
                    egui::Frame::new()
                        .fill(card_fill(dark))
                        .corner_radius(12.0)
                        .stroke(Stroke::new(1.0, border_color(dark)))
                        .inner_margin(12.0)
                        .show(&mut cols[0], |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Live activity").strong());
                                badge(
                                    ui,
                                    &self.scan.log.len().to_string(),
                                    muted(dark),
                                    surface_alt(dark),
                                );
                            });
                            ui.add_space(4.0);
                            egui::ScrollArea::vertical()
                                .id_salt("scan_log")
                                .stick_to_bottom(true)
                                .max_height(280.0)
                                .show(ui, |ui| {
                                    if self.scan.log.is_empty() {
                                        ui.label(
                                            RichText::new("Scan events will appear here.")
                                                .small()
                                                .color(muted(dark)),
                                        );
                                    }
                                    for entry in &self.scan.log {
                                        ui.horizontal(|ui| {
                                            let color = match entry.kind {
                                                ScanLogKind::Found => {
                                                    Color32::from_rgb(92, 221, 145)
                                                }
                                                ScanLogKind::Error => {
                                                    Color32::from_rgb(240, 150, 100)
                                                }
                                                ScanLogKind::Info => muted(dark),
                                            };
                                            let at = self
                                                .scan
                                                .started
                                                .map(|s| entry.at.duration_since(s).as_secs_f32())
                                                .unwrap_or(0.0);
                                            ui.label(
                                                RichText::new(format!("{at:>5.1}s"))
                                                    .monospace()
                                                    .size(9.5)
                                                    .color(muted(dark)),
                                            );
                                            status_dot(ui, color, 6.0);
                                            if !entry.address.is_empty() {
                                                ui.label(
                                                    RichText::new(&entry.address)
                                                        .monospace()
                                                        .size(10.5)
                                                        .color(muted(dark)),
                                                );
                                            }
                                            if !entry.group.is_empty() {
                                                ui.label(
                                                    RichText::new(format!("· {}", entry.group))
                                                        .size(9.5)
                                                        .color(muted(dark)),
                                                );
                                            }
                                            let text_w = ui.available_width().max(60.0);
                                            ui.allocate_ui_with_layout(
                                                vec2(text_w, 16.0),
                                                egui::Layout::left_to_right(Align::Center),
                                                |ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&entry.text)
                                                                .size(11.5)
                                                                .color(fg_color(dark)),
                                                        )
                                                        .truncate()
                                                        .halign(Align::Min),
                                                    )
                                                    .on_hover_text(&entry.text);
                                                },
                                            );
                                        });
                                    }
                                });
                        });
                    // Found ECUs
                    egui::Frame::new()
                        .fill(card_fill(dark))
                        .corner_radius(12.0)
                        .stroke(Stroke::new(1.0, border_color(dark)))
                        .inner_margin(12.0)
                        .show(&mut cols[1], |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Found ECUs").strong());
                                badge(
                                    ui,
                                    &self.scan.found.to_string(),
                                    Color32::from_rgb(92, 221, 145),
                                    Color32::from_rgb(28, 70, 46),
                                );
                            });
                            ui.add_space(4.0);
                            egui::ScrollArea::vertical()
                                .id_salt("scan_results")
                                .max_height(280.0)
                                .show(ui, |ui| {
                                    if self.scan.live.is_empty() {
                                        ui.label(
                                            RichText::new(if self.scan.scanning {
                                                "Searching…"
                                            } else {
                                                "No ECUs found yet."
                                            })
                                            .small()
                                            .color(muted(dark)),
                                        );
                                    }
                                    for hit in &self.scan.live {
                                        ui.horizontal(|ui| {
                                            if let Some(href) = &hit.href {
                                                if ui
                                                    .small_button("Open")
                                                    .on_hover_text("Open this ECU")
                                                    .clicked()
                                                {
                                                    open_href = Some(href.clone());
                                                }
                                            }
                                            let hit_resp = ui.vertical(|ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&hit.name)
                                                            .strong()
                                                            .color(fg_color(dark)),
                                                    )
                                                    .truncate()
                                                    .halign(Align::Min),
                                                );
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        RichText::new(&hit.address)
                                                            .monospace()
                                                            .size(10.5)
                                                            .color(muted(dark)),
                                                    );
                                                    ui.label(
                                                        RichText::new(hit.protocol.to_string())
                                                            .size(10.0)
                                                            .color(muted(dark)),
                                                    );
                                                    if !hit.exact {
                                                        ui.label(
                                                            RichText::new("approx.")
                                                                .size(10.0)
                                                                .color(Color32::from_rgb(
                                                                    240, 180, 70,
                                                                )),
                                                        );
                                                    }
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "diag {} · {}",
                                                            hit.diagversion, hit.supplier
                                                        ))
                                                        .size(10.0)
                                                        .color(muted(dark)),
                                                    );
                                                });
                                                ui.label(
                                                    RichText::new(&hit.group)
                                                        .size(10.0)
                                                        .color(muted(dark)),
                                                );
                                            });
                                            hit_resp.response.on_hover_text(format!(
                                                "{} · group {}\nsoft {} · version {}\n{}",
                                                hit.address,
                                                hit.group,
                                                hit.soft,
                                                hit.version,
                                                if hit.exact {
                                                    "exact match"
                                                } else {
                                                    "approximate match"
                                                }
                                            ));
                                        });
                                        ui.separator();
                                    }
                                });
                        });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Tip: double-click an ECU in the library to open it. The scan window stays \
                         open while results stream in.",
                    )
                    .small()
                    .color(muted(dark)),
                );
            });

        self.show_scan = open;
        if !open && self.scan.scanning {
            self.stop_scan();
        }
        if start_scan {
            self.start_scan();
        }
        if stop_scan {
            self.stop_scan();
        }
        if let Some(href) = open_href {
            if let Some((idx, _)) = self
                .db
                .targets
                .iter()
                .enumerate()
                .find(|(_, t)| t.href == href)
            {
                self.open_target(idx);
                self.show_scan = false;
            }
        }
    }

    fn draw_confirm(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let mut open = true;
        let mut do_it = false;
        egui::Window::new(&confirm.title)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(&confirm.message);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirm = None;
                    }
                    if ui.button(RichText::new("Continue").strong()).clicked() {
                        do_it = true;
                    }
                });
            });
        if do_it {
            self.confirm = None;
            self.execute_sequence(confirm.payloads, confirm.after);
        } else if !open {
            self.confirm = None;
        }
    }

    fn draw_status(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let status = if self.connected {
                    format!(
                        "Connected · {}",
                        self.opened
                            .as_ref()
                            .and_then(|o| o.info.as_ref())
                            .map(|i| i.description.clone())
                            .unwrap_or_default()
                    )
                } else {
                    "Offline".to_string()
                };
                ui.label(RichText::new(status).small().weak());
                ui.separator();
                if let Some((msg, until)) = &self.toast {
                    if Instant::now() < *until {
                        ui.label(RichText::new(msg).color(accent()));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if self.polling {
                        ui.spinner();
                    }
                    if let Some(last) = self.logs.last() {
                        ui.label(RichText::new(last).small().weak());
                    }
                });
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;
        self.handle_worker_events();
        self.handle_agent_events();
        self.handle_agent_actions();
        if let Some(prompt) = self.agent.auto_prompt.clone() {
            if self.connected && self.opened.is_some() && !self.agent.running {
                self.agent.auto_prompt = None;
                self.agent.input = prompt;
                self.send_agent_message();
            }
        }
        let now = Instant::now();
        if self.auto_refresh
            && self.connected
            && !self.polling
            && now.duration_since(self.last_refresh).as_millis() >= self.refresh_ms as u128
        {
            if self.tab == Tab::Screens {
                self.poll_now();
            }
        }
        if self
            .toast
            .as_ref()
            .map(|(_, until)| Instant::now() >= *until)
            .unwrap_or(false)
        {
            self.toast = None;
        }
        ctx.request_repaint_after(Duration::from_millis(80));
        self.draw_top(root);
        self.draw_status(root);
        self.draw_left(root);
        self.draw_central(root);
        self.draw_settings(ctx);
        self.draw_scan_window(ctx);
        self.draw_confirm(ctx);
    }
}

fn build_target_search(db: &ddtrust_core::Database) -> Vec<String> {
    // Project codes are terse (x95, x07). Expand them to human vehicle names so that
    // typing "Clio" finds every ECU used by a Clio, without cluttering the ECU rows.
    let mut vehicle_labels: HashMap<String, String> = HashMap::new();
    for v in &db.vehicles {
        let code = v.code.to_ascii_lowercase();
        if code.is_empty() {
            continue;
        }
        let entry = vehicle_labels.entry(code).or_default();
        if !entry.is_empty() {
            entry.push(' ');
        }
        entry.push_str(&v.key.to_ascii_lowercase());
        if !v.code.is_empty() {
            entry.push(' ');
            entry.push_str(&v.code.to_ascii_lowercase());
        }
    }
    db.targets
        .iter()
        .map(|t| {
            let mut hay = format!(
                "{} {} {} {} {}",
                t.href, t.ecuname, t.group, t.protocol, t.address
            )
            .to_ascii_lowercase();
            for project in &t.projects {
                let code = project.to_ascii_lowercase();
                hay.push(' ');
                hay.push_str(&code);
                if let Some(labels) = vehicle_labels.get(&code) {
                    hay.push(' ');
                    hay.push_str(labels);
                }
            }
            hay
        })
        .collect()
}

fn adapter_card(
    ui: &mut Ui,
    selected: bool,
    title: &str,
    description: &str,
    enabled: bool,
    dark: bool,
) -> egui::Response {
    let fill = if selected {
        accent().gamma_multiply(0.14)
    } else if enabled {
        surface_alt(dark)
    } else {
        surface_alt(dark).gamma_multiply(0.5)
    };
    let frame = egui::Frame::new()
        .fill(fill)
        .corner_radius(10.0)
        .stroke(Stroke::new(
            1.0,
            if selected {
                accent().gamma_multiply(0.65)
            } else {
                border_color(dark)
            },
        ))
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.style_mut().interaction.selectable_labels = false;
            ui.set_width((ui.available_width() - 4.0).max(200.0));
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).strong().size(14.0).color(if enabled {
                        fg_color(dark)
                    } else {
                        muted(dark)
                    }));
                    ui.label(RichText::new(description).small().color(muted(dark)));
                });
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if !enabled {
                        badge(ui, "UNAVAILABLE", muted(dark), surface_hover(dark));
                    } else if selected {
                        badge(ui, "SELECTED", accent(), accent().gamma_multiply(0.16));
                    }
                });
            });
        });
    let rect = frame.response.rect;
    let id = ui.make_persistent_id(("adapter_card", title));
    let mut resp = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if enabled {
        resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    resp
}

fn strip_markdown(text: &str) -> String {
    text.replace("**", "")
        .replace("### ", "")
        .replace("## ", "")
        .replace("`", "")
}

fn card_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(27, 30, 38)
    } else {
        Color32::from_rgb(255, 255, 255)
    }
}

fn surface_alt(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(35, 39, 49)
    } else {
        Color32::from_rgb(246, 247, 251)
    }
}

fn surface_hover(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(44, 49, 61)
    } else {
        Color32::from_rgb(238, 240, 246)
    }
}

fn border_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(48, 53, 66)
    } else {
        Color32::from_rgb(222, 226, 235)
    }
}

fn fg_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(236, 238, 244)
    } else {
        Color32::from_rgb(28, 31, 38)
    }
}

fn muted(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(148, 155, 170)
    } else {
        Color32::from_rgb(105, 112, 126)
    }
}

fn badge(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(20.0)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.5).color(fg));
        });
}

fn chip(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let fill = if selected {
        accent().gamma_multiply(0.18)
    } else {
        surface_alt(dark)
    };
    let fg = if selected { accent() } else { muted(dark) };
    ui.add(
        egui::Button::new(RichText::new(text).size(11.5).color(fg))
            .fill(fill)
            .corner_radius(20.0)
            .min_size(vec2(0.0, 24.0)),
    )
}

fn section_header(ui: &mut Ui, title: &str, subtitle: &str, dark: bool) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .size(15.0)
                .strong()
                .color(fg_color(dark)),
        );
        ui.label(RichText::new(subtitle).size(11.0).color(muted(dark)));
    });
    ui.add_space(6.0);
}

fn value_card(ui: &mut Ui, rt: &ScreenRuntime, d: &ResolvedDisplay, width: f32, dark: bool) {
    let value = rt.display_value(&d.request, &d.item);
    let fresh = rt.is_fresh(&d.request, &d.item);
    let border = if fresh {
        accent().gamma_multiply(0.20)
    } else {
        border_color(dark)
    };
    let frame = egui::Frame::new()
        .fill(card_fill(dark))
        .corner_radius(12.0)
        .stroke(Stroke::new(1.0, border))
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width((width - 24.0).max(120.0));
            ui.set_min_height(92.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(&d.def.text)
                            .size(11.0)
                            .strong()
                            .color(muted(dark)),
                    )
                    .wrap(),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let age = rt
                        .updated
                        .get(&key(&d.request, &d.item))
                        .map(|t| t.elapsed().as_secs_f32())
                        .unwrap_or(f32::INFINITY);
                    let color = if value.is_none() {
                        Color32::from_rgb(230, 150, 80)
                    } else if age > 10.0 {
                        Color32::from_rgb(130, 136, 148)
                    } else if fresh {
                        Color32::from_rgb(140, 240, 170)
                    } else {
                        Color32::from_rgb(92, 221, 145)
                    };
                    status_dot(ui, color, 7.0);
                });
            });
            ui.add_space(4.0);
            let text = value.unwrap_or_else(|| "—".into());
            let large = if text.chars().count() > 14 {
                19.0
            } else {
                27.0
            };
            let value_color = if text == "—" {
                Color32::from_rgb(220, 150, 90)
            } else {
                fg_color(dark)
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&text).size(large).strong().color(value_color));
                if !d.unit.is_empty() {
                    ui.label(RichText::new(&d.unit).size(11.0).color(muted(dark)));
                }
            });
            ui.add_space(2.0);
            if let Some(history) = rt.history(&d.request, &d.item) {
                sparkline(ui, history, accent(), dark);
            } else {
                ui.add_space(24.0);
            }
        });
    let response = frame.response.on_hover_text(format!(
        "{}\nRequest: {}\nData item: {}\nUnit: {}",
        d.def.text,
        d.request,
        d.item,
        if d.unit.is_empty() {
            "—"
        } else {
            d.unit.as_str()
        }
    ));
    let _ = response;
}

fn status_dot(ui: &mut Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

fn sparkline(ui: &mut Ui, data: &VecDeque<f32>, color: Color32, dark: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let painter = ui.painter();
    let baseline = if dark {
        Color32::from_white_alpha(14)
    } else {
        Color32::from_black_alpha(18)
    };
    painter.line_segment(
        [
            pos2(rect.left(), rect.bottom() - 1.0),
            pos2(rect.right(), rect.bottom() - 1.0),
        ],
        Stroke::new(1.0, baseline),
    );
    if data.len() < 2 {
        return;
    }
    let min = data.iter().copied().fold(f32::INFINITY, f32::min);
    let max = data.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let range = (max - min).max(1e-6);
    let n = data.len();
    let points: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = rect.left() + rect.width() * (i as f32 / (n - 1) as f32);
            let y = rect.bottom() - 3.0 - ((v - min) / range) * (rect.height() - 6.0);
            pos2(x, y)
        })
        .collect();
    painter.add(egui::Shape::line(points.clone(), Stroke::new(1.4, color)));
    if let Some(last) = points.last() {
        painter.circle_filled(*last, 2.0, color);
    }
}

fn setup_style(ctx: &egui::Context, dark: bool) {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = if dark {
        Color32::from_rgb(18, 20, 26)
    } else {
        Color32::from_rgb(245, 246, 250)
    };
    visuals.window_fill = if dark {
        Color32::from_rgb(26, 29, 37)
    } else {
        Color32::from_rgb(255, 255, 255)
    };
    visuals.selection.bg_fill = accent().gamma_multiply(0.7);
    visuals.hyperlink_color = accent();
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(20.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.5));
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(8.0, 4.0);
    });
}

fn accent() -> Color32 {
    Color32::from_rgb(255, 184, 48)
}

fn screen_color(s: &str, fallback: Color32) -> Color32 {
    ddtrust_core::model::layout::parse_color(s)
        .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
        .unwrap_or(fallback)
}

fn font_for(f: &Font, scale: f32) -> FontId {
    let size = ((f.size.0 as f32).max(5.0) / 8.0 * 14.0 * scale).clamp(7.0, 48.0);
    if f.name.to_ascii_lowercase().contains("courier") {
        FontId::monospace(size)
    } else {
        FontId::proportional(size)
    }
}

fn layout_rect(origin: egui::Pos2, r: &ddtrust_core::model::layout::Rect, scale: f32) -> Rect {
    Rect::from_min_size(
        origin + vec2(r.left.0 as f32 * scale, r.top.0 as f32 * scale),
        vec2(r.width.0 as f32 * scale, r.height.0 as f32 * scale),
    )
}

fn fit_rect(outer: Rect, size: Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return outer;
    }
    let ratio = (outer.width() / size.x).min(outer.height() / size.y);
    let s = size * ratio;
    Rect::from_center_size(outer.center(), s)
}

fn draw_text_clipped(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32, align: Align2) {
    if rect.width() <= 2.0 || rect.height() <= 2.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    let pos = if align == Align2::LEFT_TOP {
        rect.left_top()
    } else if align == Align2::LEFT_CENTER {
        pos2(rect.left(), rect.center().y)
    } else if align == Align2::CENTER_CENTER {
        rect.center()
    } else if align == Align2::RIGHT_CENTER {
        pos2(rect.right(), rect.center().y)
    } else {
        rect.left_top()
    };
    painter.text(pos, align, text, font, color);
}

fn draw_welcome(ui: &mut Ui, db: &ddtrust_core::Database, store: &DataStore) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("DDTRust").size(46.0).strong().color(accent()));
        ui.label(RichText::new("A modern Rust rewrite of ddt4all").size(18.0).weak());
        ui.add_space(18.0);
        let (targets, vehicles, can) = db.stats();
        ui.label(format!(
            "{} ECU definitions · {} vehicles · {} CAN definitions",
            targets, vehicles, can
        ));
        ui.label(RichText::new(store.location()).small().weak());
        ui.add_space(24.0);
        egui::Frame::group(ui.style()).inner_margin(18.0).show(ui, |ui| {
            ui.set_max_width(680.0);
            ui.label(RichText::new("Quick start").heading());
            ui.label("1. Pick your vehicle and an ECU in the library on the left.");
            ui.label("2. Press Open ECU. With the simulator enabled you can explore every screen without a car.");
            ui.label("3. Connect a real ELM327 (serial or Wi-Fi/TCP) in Settings and press Connect.");
            ui.label("4. Use Fault codes to read/clear DTCs, or Terminal for raw KWP/UDS requests.");
            ui.add_space(8.0);
            ui.label(RichText::new("Read-only by default. Expert mode unlocks write/coding requests.").small().weak());
        });
    });
}

fn fallback_store_and_db() -> (DataStore, ddtrust_core::Database) {
    // An empty in-memory database keeps the UI usable and shows the startup error.
    let dir = std::env::temp_dir().join("ddtrust-empty-data");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("db.json"), "{}");
    let store = DataStore::open(&dir)
        .unwrap_or_else(|_| DataStore::open(std::env::temp_dir().as_path()).unwrap());
    let db = ddtrust_core::Database::load("{}", "{\"projects\":{}}").unwrap_or_default();
    (store, db)
}
