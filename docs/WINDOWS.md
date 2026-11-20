# DDTRust for Windows

DDTRust is a modern Renault / Dacia / Nissan diagnostics application built in Rust. This package
contains the native 64-bit Windows binaries.

## Contents

- `ddtrust-gui.exe` — desktop application (no console window)
- `ddtrust.exe` — command-line interface
- `LICENSE`, `NOTICE`

## Requirements

- Windows 10 or Windows 11, 64-bit
- An ELM327-compatible adapter (USB serial or Wi-Fi/TCP) for vehicle diagnostics
- ddt4all's `ecu.zip` ECU database

CandleLight / SocketCAN is **not available on Windows** in this build; it is Linux-only. Use an
ELM327-compatible adapter. KWP2000/K-line ECUs also require an ELM327.

## ECU data

DDTRust does not bundle the ECU database. Download `ecu.zip` from the ddt4all project and do one of:

1. Put `ecu.zip` next to `ddtrust-gui.exe`, or
2. Create a `data\ecu` folder next to the executable and extract the database there, or
3. Pass an explicit path:

```bat
ddtrust-gui.exe --data "C:\path\to\ecu.zip"
```

or set `DDTRUST_DATA`.

## Run the GUI

Double-click `ddtrust-gui.exe` (or use `run-gui.bat`).

The GUI has no console window. If you start it from a terminal and want logs, use `ddtrust.exe`
for CLI work instead.

## Run the CLI

```bat
ddtrust.exe --data "C:\path\to\ecu.zip" info
ddtrust.exe --data "C:\path\to\ecu.zip" --simulate dtc UCH_84_J84_05_00
ddtrust.exe --data "C:\path\to\ecu.zip" --port COM3 --baud 38400 ecu UCH_84_J84_05_00
```

For a USB ELM327, install the appropriate driver (CH340, FTDI, CP210x, etc.) and use the COM port
shown in Device Manager. For a Wi-Fi ELM327, use:

```bat
ddtrust.exe --tcp 192.168.0.10:35000 ...
```

## Simulator

You can explore the full application without hardware:

```bat
ddtrust-gui.exe --simulate --data "C:\path\to\ecu.zip" --ecu UCH_84_J84_05_00
```

or choose **Simulator** in Settings.

## AI Agent (DeepSeek by default)

Set your API key before launching, or enter it in **AI Agent → Settings**:

```bat
set DEEPSEEK_API_KEY=sk-...
```

The default endpoint is `https://api.deepseek.com/chat/completions` with model `deepseek-chat`.
Any OpenAI-compatible endpoint can be configured. The agent is read-only.

## Notes

- The binaries are unsigned; Windows SmartScreen may warn on first launch. Verify the SHA-256
  checksum published with the download.
- Clipboard copy/paste (Ctrl+V) is enabled in all text fields.
- The interface is light/dark theme aware.
- Vehicle writes/coding/actuators require Expert mode, but CandleLight and DoIP are unavailable here.

## Licensing

DDTRust is GPL-3.0-or-later. See `LICENSE` and `NOTICE`. ECU data is provided by the ddt4all
project and is not included in this archive.
