use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use ddtrust_core::diag::dtc::dtc_summary;
use ddtrust_core::diag::link::DiagnosticLink;
use ddtrust_core::diag::{scan_can, scan_kwp, EcuSession, Safety};
use ddtrust_core::elm::{Elm327, ElmConfig, Transport};
use ddtrust_core::model::{Database, EcuFile, Protocol, Target, Vehicle};
use ddtrust_core::sim::{EmulatedElm, VirtualEcu};
use ddtrust_core::DataStore;
use indexmap::IndexMap;
use serde_json::{json, Value};

#[derive(Parser, Debug)]
#[command(
    name = "ddtrust",
    version,
    about = "Modern Renault/Dacia diagnostics (ddt4all-compatible) in Rust",
    long_about = None
)]
struct Cli {
    /// ECU data directory or ddt4all ecu.zip
    #[arg(long, global = true, env = "DDTRUST_DATA")]
    data: Option<PathBuf>,
    /// Override projects.json (vehicle/addressing metadata)
    #[arg(long, global = true)]
    projects: Option<PathBuf>,
    /// Serial adapter port (e.g. /dev/ttyUSB0, COM3)
    #[arg(long, global = true)]
    port: Option<String>,
    /// ELM327 over TCP (e.g. 192.168.0.10:35000)
    #[arg(long, global = true)]
    tcp: Option<String>,
    /// Linux SocketCAN interface for a CandleLight adapter (e.g. can0)
    #[arg(long, global = true)]
    can_interface: Option<String>,
    /// CAN bitrate used for SocketCAN scans
    #[arg(long, global = true, default_value_t = 500_000)]
    can_bitrate: u32,
    /// Adapter UART baud rate
    #[arg(long, global = true, default_value_t = 38400)]
    baud: u32,
    /// Use the built-in ECU simulator instead of hardware
    #[arg(long, global = true)]
    simulate: bool,
    /// Allow write/actuator/security services
    #[arg(long, global = true)]
    expert: bool,
    /// Vehicle project code or name (e.g. X95, X84)
    #[arg(long, global = true)]
    vehicle: Option<String>,
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    /// Verbose logging
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show database and configuration summary
    Info,
    /// List vehicles/projects
    Vehicles {
        #[arg(long)]
        search: Option<String>,
    },
    /// List ECU targets (optionally filtered by vehicle)
    Ecus {
        #[arg(long)]
        search: Option<String>,
        #[arg(long, value_enum)]
        protocol: Option<ProtocolArg>,
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// Show details of one ECU definition
    Ecu {
        ecu: String,
        #[arg(long)]
        screens: bool,
        #[arg(long)]
        requests: bool,
        #[arg(long = "show-data")]
        show_data: bool,
    },
    /// List the screens/categories of an ECU
    Screens { ecu: String },
    /// Scan the CAN or KWP bus for ECUs
    Scan {
        #[arg(long, value_enum, default_value_t = ProtocolArg::Can)]
        protocol: ProtocolArg,
        /// Restrict to specific addresses (repeatable)
        #[arg(long = "address")]
        addresses: Vec<String>,
    },
    /// Read one request or all displays of a screen
    Read {
        ecu: String,
        #[arg(long)]
        request: Option<String>,
        #[arg(long)]
        screen: Option<String>,
        /// Input values as NAME=VALUE (repeatable)
        #[arg(long = "input")]
        inputs: Vec<String>,
        #[arg(long, default_value_t = 1)]
        count: usize,
        #[arg(long, default_value_t = 0)]
        interval: u64,
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// Read or clear diagnostic trouble codes
    Dtc {
        ecu: String,
        #[arg(long)]
        clear: bool,
    },
    /// Send raw diagnostic bytes or an adapter command
    Raw {
        ecu: String,
        #[arg(long)]
        hex: Option<String>,
        #[arg(long)]
        at: Option<String>,
    },
    /// Poll a request continuously (like a live data screen)
    Monitor {
        ecu: String,
        #[arg(long)]
        request: String,
        #[arg(long, default_value_t = 1000)]
        interval: u64,
        #[arg(long, default_value_t = 0)]
        count: usize,
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// Serve a simulated ELM327 over TCP (for testing other tools)
    SimServe {
        ecu: String,
        #[arg(long, default_value = "127.0.0.1:35000")]
        listen: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum ProtocolArg {
    Can,
    Kwp,
}

struct Ctx {
    store: DataStore,
    db: Database,
    vehicle: Option<Vehicle>,
    expert: bool,
    simulate: bool,
    port: Option<String>,
    tcp: Option<String>,
    can_interface: Option<String>,
    can_bitrate: u32,
    baud: u32,
    json: bool,
}

impl Ctx {
    fn safety(&self) -> Safety {
        Safety {
            expert: self.expert,
        }
    }

    fn target(&self, name: &str) -> Result<Target> {
        self.db
            .find_by_href_or_name(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("ECU not found: {name}"))
    }

    fn load_ecu(&self, target: &Target) -> Result<(Arc<EcuFile>, Arc<ddtrust_core::EcuLayout>)> {
        let ecu = self
            .store
            .load_ecu(&target.href)
            .with_context(|| format!("loading {}", target.href))?;
        let layout = self.store.load_layout(&target.href).unwrap_or_default();
        Ok((Arc::new(ecu), Arc::new(layout)))
    }

    fn link(&self) -> Result<Box<dyn DiagnosticLink>> {
        if let Some(interface) = &self.can_interface {
            return Ok(ddtrust_core::diag::open_socketcan(
                interface,
                std::time::Duration::from_secs(5),
            )?);
        }
        let cfg = ElmConfig::default();
        if let Some(addr) = &self.tcp {
            let mut elm = Elm327::tcp(addr, cfg)?;
            elm.initialize()?;
            return Ok(Box::new(elm));
        }
        if let Some(port) = &self.port {
            let mut elm = Elm327::serial(port, self.baud, cfg)?;
            elm.initialize()?;
            return Ok(Box::new(elm));
        }
        bail!("no adapter selected: pass --port, --tcp or --can-interface, or use --simulate");
    }

    fn ecu_session(&self, target: &Target) -> Result<EcuSession> {
        let (ecu, _) = self.load_ecu(target)?;
        let link: Box<dyn DiagnosticLink> = if self.simulate {
            Box::new(self.sim_link_for_ecu(target, ecu.clone())?)
        } else {
            self.link()?
        };
        EcuSession::open(link, ecu, self.safety()).map_err(Into::into)
    }

    fn sim_link_for_ecu(&self, target: &Target, ecu: Arc<EcuFile>) -> Result<Elm327> {
        let tx = if ecu.send_id.is_empty() {
            self.db
                .can_ids(self.vehicle.as_ref(), &target.address)
                .map(|(tx, _)| tx)
                .unwrap_or_default()
        } else {
            ecu.send_id.clone()
        };
        let virtual_ecu = VirtualEcu::from_file(target, ecu, tx);
        let mut elm = Elm327::new(
            Box::new(EmulatedElm::with_ecus(vec![virtual_ecu])),
            ElmConfig::default(),
        );
        elm.initialize()?;
        Ok(elm)
    }

    fn sim_scan_link(&self, protocol: ProtocolArg) -> Result<Elm327> {
        let proto = match protocol {
            ProtocolArg::Can => Protocol::Can,
            ProtocolArg::Kwp => Protocol::Kwp2000,
        };
        let mut ecus = Vec::new();
        for t in self.db.vehicle_targets(self.vehicle.as_ref()) {
            if !t.supports(proto.clone()) {
                continue;
            }
            let tx = self
                .db
                .can_ids(self.vehicle.as_ref(), &t.address)
                .map(|(tx, _)| tx)
                .unwrap_or_else(|| t.address.clone());
            ecus.push(VirtualEcu::from_target(t, tx));
        }
        let mut elm = Elm327::new(Box::new(EmulatedElm::with_ecus(ecus)), ElmConfig::default());
        elm.initialize()?;
        Ok(elm)
    }
}

fn main() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(level)
        .without_time()
        .init();

    let mut store = DataStore::discover(cli.data.as_deref())?;
    if let Some(path) = &cli.projects {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        store.set_projects_json(text);
    }
    let db = store.load_database()?;
    let vehicle = match &cli.vehicle {
        Some(v) => Some(
            db.vehicle(v)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown vehicle {v}; see `ddtrust vehicles`"))?,
        ),
        None => None,
    };
    let ctx = Ctx {
        store,
        db,
        vehicle,
        expert: cli.expert,
        simulate: cli.simulate,
        port: cli.port,
        tcp: cli.tcp,
        can_interface: cli.can_interface,
        can_bitrate: cli.can_bitrate,
        baud: cli.baud,
        json: cli.json,
    };
    match cli.command {
        Command::Info => cmd_info(&ctx),
        Command::Vehicles { search } => cmd_vehicles(&ctx, search.as_deref()),
        Command::Ecus {
            search,
            protocol,
            limit,
        } => cmd_ecus(&ctx, search.as_deref(), protocol, limit),
        Command::Ecu {
            ecu,
            screens,
            requests,
            show_data,
        } => cmd_ecu(&ctx, &ecu, screens, requests, show_data),
        Command::Screens { ecu } => cmd_screens(&ctx, &ecu),
        Command::Scan {
            protocol,
            addresses,
        } => cmd_scan(&ctx, protocol, addresses),
        Command::Read {
            ecu,
            request,
            screen,
            inputs,
            count,
            interval,
            csv,
        } => cmd_read(&ctx, &ecu, request, screen, &inputs, count, interval, csv),
        Command::Dtc { ecu, clear } => cmd_dtc(&ctx, &ecu, clear),
        Command::Raw { ecu, hex, at } => cmd_raw(&ctx, &ecu, hex, at),
        Command::Monitor {
            ecu,
            request,
            interval,
            count,
            csv,
        } => cmd_monitor(&ctx, &ecu, &request, interval, count, csv),
        Command::SimServe { ecu, listen } => cmd_sim_serve(&ctx, &ecu, &listen),
    }
}

fn cmd_info(ctx: &Ctx) -> Result<()> {
    let (targets, vehicles, can) = ctx.db.stats();
    let kwp = ctx
        .db
        .targets
        .iter()
        .filter(|t| t.protocol.is_kwp())
        .count();
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "data": ctx.store.location(),
                "targets": targets,
                "vehicles": vehicles,
                "can": can,
                "kwp": kwp,
                "simulate": ctx.simulate,
            }))?
        );
        return Ok(());
    }
    println!("DDTRust {}", env!("CARGO_PKG_VERSION"));
    println!("Data:     {}", ctx.store.location());
    println!("ECUs:     {targets} definitions ({can} CAN, {kwp} KWP/ISO)");
    println!("Vehicles: {vehicles} projects");
    println!(
        "Mode:     {}",
        if ctx.simulate {
            "simulator"
        } else {
            "hardware"
        }
    );
    Ok(())
}

fn cmd_vehicles(ctx: &Ctx, search: Option<&str>) -> Result<()> {
    let needle = search.map(|s| s.to_ascii_lowercase());
    let mut rows = Vec::new();
    for v in &ctx.db.vehicles {
        if let Some(n) = &needle {
            if !v.key.to_ascii_lowercase().contains(n) && !v.code.to_ascii_lowercase().contains(n) {
                continue;
            }
        }
        rows.push(json!({"code": v.code, "name": v.key, "addresses": v.addressing.len()}));
    }
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for r in rows {
            println!(
                "{:<10} {:<60} {}",
                r["code"].as_str().unwrap_or(""),
                r["name"].as_str().unwrap_or(""),
                r["addresses"]
            );
        }
    }
    Ok(())
}

fn cmd_ecus(
    ctx: &Ctx,
    search: Option<&str>,
    protocol: Option<ProtocolArg>,
    limit: usize,
) -> Result<()> {
    let needle = search.map(|s| s.to_ascii_lowercase());
    let proto = protocol.map(|p| match p {
        ProtocolArg::Can => Protocol::Can,
        ProtocolArg::Kwp => Protocol::Kwp2000,
    });
    let mut rows = Vec::new();
    for t in ctx.db.vehicle_targets(ctx.vehicle.as_ref()) {
        if let Some(p) = &proto {
            if !t.supports(p.clone()) {
                continue;
            }
        }
        if let Some(n) = &needle {
            let hay =
                format!("{} {} {} {}", t.href, t.ecuname, t.group, t.address).to_ascii_lowercase();
            if !hay.contains(n) {
                continue;
            }
        }
        rows.push(json!({
            "href": t.href,
            "ecuname": t.ecuname,
            "group": t.group,
            "protocol": t.protocol.as_str(),
            "address": t.address,
            "idents": t.autoidents.len(),
            "projects": t.projects,
        }));
        if rows.len() >= limit {
            break;
        }
    }
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for r in rows {
            println!(
                "{:<55} {:<8} {:<6} {:<5} {}",
                r["href"].as_str().unwrap_or(""),
                r["group"].as_str().unwrap_or(""),
                r["protocol"].as_str().unwrap_or(""),
                r["address"].as_str().unwrap_or(""),
                r["ecuname"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

fn cmd_ecu(ctx: &Ctx, name: &str, screens: bool, requests: bool, data: bool) -> Result<()> {
    let target = ctx.target(name)?;
    let (ecu, layout) = ctx.load_ecu(&target)?;
    if ctx.json {
        let mut obj = json!({
            "href": ecu.href,
            "ecuname": ecu.ecuname,
            "group": target.group,
            "protocol": ecu.protocol.as_str(),
            "address": target.address,
            "send_id": ecu.send_id,
            "recv_id": ecu.recv_id,
            "baudrate": ecu.baudrate,
            "endian": ecu.endian_str,
            "autoidents": ecu.autoidents.iter().map(|a| json!({
                "diagversion": a.diagversion, "supplier": a.supplier, "soft": a.soft, "version": a.version
            })).collect::<Vec<_>>(),
            "requests": ecu.requests.len(),
            "data": ecu.data.len(),
            "screens": layout.screens.len(),
            "categories": layout.categories.keys().collect::<Vec<_>>(),
        });
        if screens {
            obj["screen_list"] = json!(layout.ordered_screens());
        }
        if requests {
            obj["request_list"] = json!(ecu.requests.keys().collect::<Vec<_>>());
        }
        if data {
            obj["data_list"] = json!(ecu.data.keys().collect::<Vec<_>>());
        }
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }
    println!("{}", ecu.ecuname);
    println!("  file:      {}", ecu.href);
    println!("  group:     {}", target.group);
    println!(
        "  protocol:  {} @ {} (tx {}, rx {})",
        ecu.protocol, target.address, ecu.send_id, ecu.recv_id
    );
    println!("  endian:    {}", ecu.endian_str);
    println!("  requests:  {}", ecu.requests.len());
    println!("  data:      {}", ecu.data.len());
    println!(
        "  screens:   {} ({} categories)",
        layout.screens.len(),
        layout.categories.len()
    );
    for a in &ecu.autoidents {
        println!(
            "  ident:     diag={} supplier={} soft={} version={}",
            a.diagversion, a.supplier, a.soft, a.version
        );
    }
    if screens {
        for (cat, screen) in layout.ordered_screens() {
            println!("  screen:    [{}] {}", cat.unwrap_or_default(), screen);
        }
    }
    if requests {
        for name in ecu.requests.keys() {
            println!("  request:   {name}");
        }
    }
    if data {
        for name in ecu.data.keys() {
            println!("  data:      {name}");
        }
    }
    Ok(())
}

fn cmd_screens(ctx: &Ctx, name: &str) -> Result<()> {
    let target = ctx.target(name)?;
    let (_, layout) = ctx.load_ecu(&target)?;
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&layout.ordered_screens())?
        );
        return Ok(());
    }
    for (cat, screen) in layout.ordered_screens() {
        println!("[{}] {}", cat.unwrap_or_default(), screen);
    }
    Ok(())
}

fn cmd_scan(ctx: &Ctx, protocol: ProtocolArg, addresses: Vec<String>) -> Result<()> {
    let mut link: Box<dyn DiagnosticLink> = if ctx.simulate {
        Box::new(ctx.sim_scan_link(protocol)?)
    } else {
        ctx.link()?
    };
    let (protocol, addresses) = if !addresses.is_empty() {
        (protocol, addresses)
    } else {
        let p = match protocol {
            ProtocolArg::Can => Protocol::Can,
            ProtocolArg::Kwp => Protocol::Kwp2000,
        };
        (protocol, ctx.db.scan_addresses(ctx.vehicle.as_ref(), p))
    };
    let mut last = String::new();
    let mut on_event = |ev: ddtrust_core::diag::ScanEvent| match ev {
        ddtrust_core::diag::ScanEvent::Attempt { address, group } => {
            if !ctx.json {
                eprint!("\rscanning {address} {group:<45}");
            }
        }
        ddtrust_core::diag::ScanEvent::Found {
            address,
            name,
            exact,
            ..
        } => {
            if !ctx.json {
                last = format!(
                    "found {name} @ {address} ({})",
                    if exact { "exact" } else { "approximate" }
                );
                eprintln!("\r{last:<100}");
            }
        }
        _ => {}
    };
    let results = match protocol {
        ProtocolArg::Can => scan_can(
            link.as_mut(),
            &ctx.db,
            ctx.vehicle.as_ref(),
            &addresses,
            if ctx.can_interface.is_some() {
                ctx.can_bitrate
            } else {
                500_000
            },
            &mut on_event,
        ),
        ProtocolArg::Kwp => scan_kwp(
            link.as_mut(),
            &ctx.db,
            ctx.vehicle.as_ref(),
            &addresses,
            &mut on_event,
        ),
    };
    if ctx.json {
        let rows: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "address": r.address,
                    "protocol": r.protocol.as_str(),
                    "diagversion": r.diagversion,
                    "supplier": r.supplier,
                    "soft": r.soft,
                    "version": r.version,
                    "legacy": r.legacy,
                    "matched": r.matched.as_ref().map(|m| json!({
                        "href": m.href, "name": m.name, "exact": m.exact
                    })),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        if results.is_empty() {
            println!("No ECUs found ({} addresses scanned)", addresses.len());
        }
        for r in &results {
            println!("{}", r.label());
        }
    }
    Ok(())
}

fn parse_inputs(inputs: &[String]) -> Result<Vec<(String, String)>> {
    inputs
        .iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| anyhow::anyhow!("invalid input {s:?}, expected NAME=VALUE"))
        })
        .collect()
}

fn cmd_read(
    ctx: &Ctx,
    name: &str,
    request: Option<String>,
    screen: Option<String>,
    inputs: &[String],
    count: usize,
    interval: u64,
    csv: Option<PathBuf>,
) -> Result<()> {
    let target = ctx.target(name)?;
    let (ecu, layout) = ctx.load_ecu(&target)?;
    let inputs = parse_inputs(inputs)?;
    let mut session = ctx.ecu_session(&target)?;
    let mut csv_writer = match &csv {
        Some(path) => {
            let f = std::fs::File::create(path)?;
            Some(csv::Writer::from_writer(f))
        }
        None => None,
    };
    if let Some(req) = request {
        for iteration in 0..count.max(1) {
            let result = session.read_named(&req, &inputs)?;
            output_read(ctx, &result, &mut csv_writer)?;
            if iteration + 1 < count.max(1) {
                std::thread::sleep(Duration::from_millis(interval));
            }
        }
        return Ok(());
    }
    let Some(screen_name) = screen else {
        bail!("pass --request NAME or --screen NAME (see `ddtrust screens {name}`)");
    };
    let screen = layout
        .screen(&screen_name)
        .ok_or_else(|| anyhow::anyhow!("unknown screen {screen_name}"))?;
    let mut requests: IndexMap<String, Vec<(String, String)>> = IndexMap::new();
    for d in &screen.displays {
        if ecu.request(&d.request).is_some() {
            requests.entry(d.request.clone()).or_default();
        }
    }
    for (iteration, (req_name, _)) in requests.iter().enumerate() {
        let _ = iteration;
        let result = session.read_named(req_name, &inputs)?;
        output_read(ctx, &result, &mut csv_writer)?;
    }
    let _ = count;
    Ok(())
}

fn output_read(
    ctx: &Ctx,
    result: &ddtrust_core::diag::ReadResult,
    csv_writer: &mut Option<csv::Writer<std::fs::File>>,
) -> Result<()> {
    if ctx.json {
        let values: IndexMap<String, Value> = result
            .values
            .iter()
            .map(|v| {
                (
                    v.name.clone(),
                    json!({"value": v.display, "raw": v.raw_hex, "unit": v.unit}),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({
                "request": result.request,
                "raw": ddtrust_core::hexutil::to_hex(&result.raw),
                "values": values,
            }))?
        );
    } else {
        println!(
            "{} [{}]",
            result.request,
            ddtrust_core::hexutil::to_hex(&result.raw)
        );
        for v in &result.values {
            println!(
                "  {:<45} {:>15} {}",
                v.name,
                v.display.clone().unwrap_or_else(|| "N/A".into()),
                v.unit
            );
        }
    }
    if let Some(w) = csv_writer {
        if w.write_record(
            result
                .values
                .iter()
                .map(|v| v.display.clone().unwrap_or_default()),
        )
        .is_ok()
        {
            w.flush().ok();
        }
    }
    Ok(())
}

fn cmd_dtc(ctx: &Ctx, name: &str, clear: bool) -> Result<()> {
    let target = ctx.target(name)?;
    let mut session = ctx.ecu_session(&target)?;
    if clear {
        let rsp = session.clear_dtc()?;
        if ctx.json {
            println!(
                "{}",
                json!({"cleared": true, "response": ddtrust_core::hexutil::to_hex(&rsp)})
            );
        } else {
            println!(
                "DTC cleared: {}",
                ddtrust_core::hexutil::to_hex_spaced(&rsp)
            );
        }
        return Ok(());
    }
    let report = session.read_dtc()?;
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "request": report.request,
                "count": report.count,
                "raw": ddtrust_core::hexutil::to_hex(&report.raw),
                "records": report.records.iter().map(|r| json!({
                    "index": r.index,
                    "code": r.code_text(),
                    "name": r.name,
                    "raw": ddtrust_core::hexutil::to_hex(&r.raw),
                    "fields": r.fields.iter().map(|f| json!({"name": f.name, "value": f.value, "raw": f.raw_hex, "unit": f.unit})).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("{}", dtc_summary(&report));
    }
    Ok(())
}

fn cmd_raw(ctx: &Ctx, name: &str, hex: Option<String>, at: Option<String>) -> Result<()> {
    if hex.is_none() && at.is_none() {
        bail!("pass --hex HEX or --at COMMAND");
    }
    let target = ctx.target(name)?;
    let mut session = ctx.ecu_session(&target)?;
    if let Some(cmd) = at {
        let r = session.raw_command(&cmd)?;
        println!("{r}");
        return Ok(());
    }
    let payload = ddtrust_core::hexutil::parse_hex(&hex.unwrap())?;
    let rsp = session.send_payload(&payload, ctx.expert)?;
    if ctx.json {
        println!(
            "{}",
            json!({"request": ddtrust_core::hexutil::to_hex(&payload), "response": ddtrust_core::hexutil::to_hex(&rsp)})
        );
    } else {
        println!("{}", ddtrust_core::hexutil::to_hex_spaced(&rsp));
    }
    Ok(())
}

fn cmd_monitor(
    ctx: &Ctx,
    name: &str,
    request: &str,
    interval: u64,
    count: usize,
    csv: Option<PathBuf>,
) -> Result<()> {
    let target = ctx.target(name)?;
    let mut session = ctx.ecu_session(&target)?;
    let mut writer = match csv {
        Some(path) => Some(csv::Writer::from_writer(std::fs::File::create(path)?)),
        None => None,
    };
    let mut value_names: Option<Vec<String>> = None;
    let mut i = 0usize;
    loop {
        let started = std::time::Instant::now();
        let result = session.read_named(request, &[])?;
        if value_names.is_none() {
            value_names = Some(result.values.iter().map(|v| v.name.clone()).collect());
            if let Some(w) = &mut writer {
                let mut header = vec!["time_ms".to_string()];
                header.extend(value_names.clone().unwrap_or_default());
                w.write_record(header)?;
                w.flush()?;
            }
        }
        let values: Vec<String> = result
            .values
            .iter()
            .map(|v| v.display.clone().unwrap_or_default())
            .collect();
        if ctx.json {
            println!(
                "{}",
                json!({"t_ms": i, "raw": ddtrust_core::hexutil::to_hex(&result.raw), "values": result.values.iter().map(|v| (&v.name, &v.display)).collect::<IndexMap<_, _>>()})
            );
        } else {
            println!(
                "{:>8.3}s  {}",
                started.elapsed().as_secs_f64() + i as f64 * interval as f64 / 1000.0,
                result
                    .values
                    .iter()
                    .map(|v| format!(
                        "{}={}",
                        v.name,
                        v.display.clone().unwrap_or_else(|| "N/A".into())
                    ))
                    .collect::<Vec<_>>()
                    .join("  ")
            );
        }
        if let Some(w) = &mut writer {
            let mut row = vec![format!("{i}")];
            row.extend(values);
            w.write_record(row)?;
            w.flush()?;
        }
        i += 1;
        if count > 0 && i >= count {
            break;
        }
        std::thread::sleep(Duration::from_millis(interval));
    }
    Ok(())
}

fn cmd_sim_serve(ctx: &Ctx, name: &str, listen: &str) -> Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let target = ctx.target(name)?;
    let (ecu, _) = ctx.load_ecu(&target)?;
    let tx_id = if ecu.send_id.is_empty() {
        ctx.db
            .can_ids(ctx.vehicle.as_ref(), &target.address)
            .map(|(tx, _)| tx)
            .unwrap_or_else(|| target.address.clone())
    } else {
        ecu.send_id.clone()
    };
    let virtual_ecu = VirtualEcu::from_file(&target, ecu.clone(), tx_id);
    let mut emulated = EmulatedElm::with_ecus(vec![virtual_ecu]);
    let listener = TcpListener::bind(listen).with_context(|| format!("binding {listen}"))?;
    println!(
        "Simulated ELM327 for {} listening on {} (serial/TCP ELM327 clients can connect)",
        ecu.ecuname, listen
    );
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        stream.set_read_timeout(Some(Duration::from_millis(50)))?;
        stream.set_nodelay(true).ok();
        let mut buf = [0u8; 512];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    emulated.write_bytes(&buf[..n])?;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
            let mut out = Vec::new();
            while let Some(b) = emulated.read_byte(Duration::from_millis(0))? {
                out.push(b);
            }
            if !out.is_empty() {
                if stream.write_all(&out).is_err() {
                    break;
                }
                stream.flush().ok();
            }
        }
    }
    Ok(())
}
