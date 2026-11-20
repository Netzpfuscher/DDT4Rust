use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("file not found: {0}")]
    NotFound(PathBuf),
    #[error("resource not found: {0}")]
    Missing(String),
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("hex error: {0}")]
    Hex(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("no response from ECU/adapter")]
    NoResponse,
    #[error("adapter error: {0}")]
    Adapter(String),
    #[error("ECU negative response: 0x{service:02X} / 0x{code:02X} ({meaning})")]
    NegativeResponse {
        service: u8,
        code: u8,
        meaning: &'static str,
    },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("safety: {0}")]
    Safety(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// UDS/KWP negative response codes used by Renault ECUs (from ddt4all).
pub fn negative_response_meaning(code: u8) -> &'static str {
    match code {
        0x10 => "General Reject",
        0x11 => "Service Not Supported",
        0x12 => "SubFunction Not Supported",
        0x13 => "Incorrect Message Length Or Invalid Format",
        0x21 => "Busy Repeat Request",
        0x22 => "Conditions Not Correct Or Request Sequence Error",
        0x23 => "Routine Not Complete",
        0x24 => "Request Sequence Error",
        0x31 => "Request Out Of Range",
        0x33 => "Security Access Denied",
        0x35 => "Invalid Key",
        0x36 => "Exceed Number Of Attempts",
        0x37 => "Required Time Delay Not Expired",
        0x40 => "Download not accepted",
        0x41 => "Improper download type",
        0x42 => "Cannot download to specified address",
        0x43 => "Cannot download number of bytes requested",
        0x50 => "Upload not accepted",
        0x51 => "Improper upload type",
        0x52 => "Cannot upload from specified address",
        0x53 => "Cannot upload number of bytes requested",
        0x70 => "Upload Download Not Accepted",
        0x71 => "Transfer Data Suspended",
        0x72 => "General Programming Failure",
        0x73 => "Wrong Block Sequence Counter",
        0x74 => "Illegal Address In Block Transfer",
        0x75 => "Illegal Byte Count In Block Transfer",
        0x76 => "Illegal Block Transfer Type",
        0x77 => "Block Transfer Data Checksum Error",
        0x78 => "Request Correctly Received - Response Pending",
        0x79 => "Incorrect Byte Count During Block Transfer",
        0x7E => "SubFunction Not Supported In Active Session",
        0x7F => "Service Not Supported In Active Session",
        0x80 => "Service Not Supported In Active Diagnostic Mode",
        0x81 => "Rpm Too High",
        0x82 => "Rpm Too Low",
        0x83 => "Engine Is Running",
        0x84 => "Engine Is Not Running",
        0x85 => "Engine RunTime Too Low",
        0x86 => "Temperature Too High",
        0x87 => "Temperature Too Low",
        0x88 => "Vehicle Speed Too High",
        0x89 => "Vehicle Speed Too Low",
        0x8A => "Throttle/Pedal Too High",
        0x8B => "Throttle/Pedal Too Low",
        0x8C => "Transmission Range In Neutral",
        0x8D => "Transmission Range In Gear",
        0x8F => "Brake Switch(es) Not Closed",
        0x90 => "Shifter Lever Not In Park",
        0x91 => "Torque Converter Clutch Locked",
        0x92 => "Voltage Too High",
        0x93 => "Voltage Too Low",
        _ => "Unregistered error",
    }
}

impl Error {
    pub fn negative(service: u8, code: u8) -> Self {
        Error::NegativeResponse {
            service,
            code,
            meaning: negative_response_meaning(code),
        }
    }
}
