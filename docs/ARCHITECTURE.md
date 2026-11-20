# DDTRust architecture

## Workspace

```
crates/
  ddtrust-core/   library: database, codec, ELM327, ISO-TP, diagnostics, simulator
  ddtrust-cli/    `ddtrust` binary (clap)
  ddtrust-gui/    `ddtrust-gui` binary (eframe/egui)
assets/projects.json   vehicle list + CAN addressing maps (from ddt4all, GPL)
tools/gen_vectors.py   golden-vector generator using the original Python implementation
```

`ddtrust-core` has no GUI or CLI dependency. `ddtrust-cli` and `ddtrust-gui` both drive the same
`EcuSession`/`DiagnosticLink` API.

## Core modules

| Module | Responsibility |
| --- | --- |
| `model::data` | `EcuData` definitions and the bit-exact encode/decode codec |
| `model::request` | `DataItem`, `EcuRequest`, request building and response decoding |
| `model::ecu` | `EcuFile` (protocol, IDs, data, requests, devices, identification) |
| `model::layout` | ddt4all `.layout` JSON: screens, categories, displays, inputs, buttons, labels |
| `model::database` | `db.json` + `projects.json`: targets, vehicles, addressing, identification |
| `store` | data sources: extracted directory or `ecu.zip`; graphics lookup; cached ZIP index |
| `elm::transport` | byte transports: serial, TCP, in-memory |
| `elm::isotp` | ISO-TP framing/reassembly (single, first, consecutive, flow control) |
| `elm::driver` | ELM327 command layer: init, CAN/KWP configuration, request/response, retries |
| `diag::link` | `DiagnosticLink` trait implemented by the real ELM and the simulator |
| `diag::session` | `EcuSession`: ECU configuration, 10C0 session, safe request/response |
| `diag::scanner` | CAN/KWP scanning and exact/approximate identification |
| `diag::dtc` | DTC read (including `MoreDTC`), record decoding, clear |
| `sim` | `VirtualEcu` + `EmulatedElm` (an ASCII ELM327 with ISO-TP) |

## Data flow

```
user action
   │
   ▼
EcuRequest::build_stream(inputs)          model::request + model::data
   │  bytes
   ▼
Safety::check                             diag::safety
   │
   ▼
DiagnosticLink::request                   diag::link
   │
   ├── Elm327::request ── ISO-TP framing ─ SerialTransport / TcpTransport
   └── Elm327 over EmulatedElm ── VirtualEcu (offline)
   │  response bytes
   ▼
EcuRequest::decode + EcuData::display_value
   │  values
   ▼
CLI print / CSV       GUI screen widgets
```

## Value codec

`model::data.rs` deliberately reproduces ddt4all's algorithm, including its less obvious rules:

- `firstbyte` is 1-based; `bitoffset` counts from the MSB of that byte.
- Big endian extracts a `bitscount`-wide bit slice starting at `bitoffset`.
- Little endian extracts `((raw >> bitoffset) & mask)` from the whole field, emits the value bytes
  little-endian, then interprets them as a big-endian integer for display. This mirrors ddt4all's
  three-step write logic too.
- Unscaled values display as the raw hex (unless signed/enum handling applies); scaled values use
  `(value * step + offset) / divideby` and the optional fixed-decimal `format`.
- Enum labels (`lists`/`items`) map values in both directions.
- Huge definitions (up to 32,736 bits) are handled with `num-bigint`.
- ASCII fields are padded/truncated to `bytescount`.

The tests in `tests/value_codec.rs` compare against vectors produced by the actual ddt4all Python
classes, so changes to the codec are checked against the reference implementation.

## ELM327 and ISO-TP

DDTRust uses the portable ELM327 feature set rather than adapter-specific extensions:

- `ATZ`, `ATI`, echo/linefeed/spaces/headers off, `ATAL`, `ATCAF0`, `ATCFC1`.
- CAN: `ATSP6/7` (500 kbit/s 11/29-bit), `ATSP8/9` (250 kbit/s), `ATSH`, `ATCRA`, `ATCP` for 29-bit,
  `ATFCSH/ATFCSD/ATFCSM`.
- KWP: `ATIB10`, `ATSH 81 <addr> F1`, `ATWM`, `ATSW`, `ATSP4/5`, `ATSI`/`ATFI`.

### CandleLight / SocketCAN

On Linux, CandleLight firmware appears as a SocketCAN interface (`can0`). `SocketCanLink` implements
`DiagnosticLink` directly over a `CanSocket`: it parses 11/29-bit IDs, sends ISO-TP single/first/
consecutive frames, waits for ECU flow control while transmitting long requests, sends flow control
for long responses, and validates consecutive-frame sequence numbers. It is CAN-only: `open_kwp`
returns a clear error because SocketCAN has no K-line. The interface bitrate is configured outside
the app (`ip link … type can bitrate …`); the settings UI shows the command and interface state.
### DoIP / ISO 13400

`DoipLink` implements the ISO 13400-2 TCP client directly. It sends a routing-activation request for
the configured tester logical address, validates the activation response, then frames diagnostics as
DoIP diagnostic messages (`0x8001`) with tester and target logical addresses. It handles diagnostic
ACK/NACK (`0x8002/0x8003`), responds to alive checks (`0x0007/0x0008`), validates source/target
addresses and ignores unrelated announcements. Response-pending UDS replies (`7F xx 78`) are
transparently awaited. A separate UDP helper can send vehicle-identification requests.

For an ECU, the DoIP target address is derived from its ddt4all `funcaddr` (hex logical address).
CAN-defined ECUs can be opened through a DoIP gateway this way. The DoIP scanner iterates logical
addresses from `projects.json`'s `DoIP` map plus DoIP-defined targets, uses the same UDS DID
identification as CAN, and matches results against both CAN and DoIP target definitions. DoIP is
transport-independent of the OS CAN stack, so it is available on Linux, Windows and macOS.

- Requests are framed by DDTRust (CAF0): ISO-TP single/first/consecutive frames; the adapter sends
  flow control. Responses are reassembled with sequence checking, broadcast filtering by expected
  positive SID, and `7F xx 78` response-pending retries.
- A KWP/ISO path parses the adapter's plain hex response.

This path works across common ELM327 clones, VLinker, ELS27 and OBDLink in ELM-compatible mode. The
original ddt4all STPX/CFC0 fast paths are intentionally not replicated; they are adapter-specific
optimisations.

## Screens

`.layout` files contain absolute positions in a design canvas (ddt4all divides by 8). DDTRust
resolves display/input names against the ECU request data items (case-insensitive fallback), builds
one poll per unique display request, and decodes results on a worker thread into
`(request, data item)` values.

Two renderers share the same `ScreenRuntime` and live values:

1. **Dashboard (default)** — ignores absolute coordinates and lays out a responsive card grid
   grouped by request. Displays become cards with a name, large value, unit, freshness dot and a
   scrollback sparkline; inputs become a configuration form; buttons become a wrapped actions row.
   A header provides category/mode info, a parameter filter and live/refresh status. Picture labels
   appear in a collapsible graphics gallery. This makes 1990s fixed-pixel screens usable on any
   window size.
2. **Layout** — renders the original absolute positions, colours, fonts and background graphics for
   car-specific diagrams and pixel-faithful screens. It auto-fits the width when opened or when the
   view is switched to Layout; Fit width, 1:1 and a zoom slider remain available. Width fit uses the
   available width minus a small margin and is clamped to a usable zoom range; tall layouts scroll
   vertically.

Both renderers support animated GIFs (decoded into egui textures with per-frame delays), hover
tooltips, value freshness (updated < 450 ms), stale indicators and numeric sparklines. Buttons build
their configured request sequence from current inputs and require confirmation if the layout attaches
messages. Pre-send requests run when a screen opens and after reconnecting.

The GUI keeps value history (48 samples per data item) inside `ScreenRuntime`; this feeds the
sparklines and is independent of the database model.

## AI Agent

The optional `Agent` tab is an OpenAI-compatible tool-calling assistant (DeepSeek by default). A request runs on a
`ddtrust-agent` thread so the UI never blocks. The thread receives:

- the conversation history plus a fresh context snapshot (vehicle, ECU metadata, screen, latest
  values, DTCs, database stats);
- the diagnostic worker's command sender and an event sender;
- an action channel for UI navigation.

The loop sends a chat-completions request with function tools, executes any tool calls, appends
`tool` results and repeats (maximum six rounds). Read tools use local database/layout access or the
synchronous worker commands (`PollSync`, `ReadDtcSync`). Navigation tools (`open_ecu`,
`open_screen`) enqueue `AgentAction`s that the GUI applies. The agent is deliberately read-only:
`read_request` enforces the same safe-service allowlist as normal UI reads, and no tool can clear
DTCs, write coding, run routines or request security access. Tool activity is shown inline in the
chat. API configuration is in-memory by default; environment variables provide startup defaults.

## GUI threading

The egui UI never blocks on the adapter. A `ddtrust-diag` worker owns the `EcuSession` and receives
commands over a channel (`Connect`, `Poll`, `Sequence`, `ReadDtc`, `ClearDtc`, `Raw*`, `Scan`).
Events flow back (`Connected`, `Poll`, `Dtc`, `Scan`, …). Large ECU JSON files are parsed on the UI
thread when opened (typically < 100 ms) and then shared as `Arc`.

## Safety model

Read-only mode allows services `10 12 14 17 19 1A 21 22 23 3E` (ddt4all's set). All other services,
including `2E`/`3B` coding, `31` routines, `27` security access and actuator writes, require expert
mode. DTC clearing uses the ECU's own `ClearDTC` request when present, otherwise `14 FF 00`, and is
confirmed interactively in the GUI.

## Simulator

`EmulatedElm` implements the `Transport` trait, so the real `Elm327` driver is exercised end-to-end.
It understands ELM AT commands, ISO-TP framing and multiple virtual ECUs. `VirtualEcu` answers
identification DIDs, sessions, DTC reads and any request found in the ECU file, synthesising values
for its data items. This makes offline demos, screenshots and integration tests deterministic-ish and
hardware-free. `ddtrust sim-serve` exposes the same emulated adapter over TCP.
