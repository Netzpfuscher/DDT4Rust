use crate::error::Result;

pub mod doip;
pub mod dtc;
pub mod link;
pub mod safety;
pub mod scanner;
pub mod session;
#[cfg(target_os = "linux")]
pub mod socketcan;

pub use doip::{discover_vehicles, DoipLink, DoipVehicle};
pub use dtc::{DtcRecord, DtcReport};
pub use link::{CanSpec, DiagnosticLink, DoipSpec, KwpSpec, LinkInfo};
pub use safety::Safety;
pub use scanner::{
    scan_can, scan_can_with_cancel, scan_doip, scan_doip_with_cancel, scan_kwp,
    scan_kwp_with_cancel, ScanEvent, ScanResult,
};
pub use session::{EcuSession, ReadResult, SessionOpen};
#[cfg(target_os = "linux")]
pub use socketcan::SocketCanLink;

#[derive(Debug, Clone, Default)]
pub struct CanInterfaceInfo {
    pub name: String,
    pub state: String,
}

pub fn socketcan_supported() -> bool {
    cfg!(target_os = "linux")
}

#[cfg(target_os = "linux")]
pub fn can_interfaces() -> Vec<CanInterfaceInfo> {
    socketcan::can_interfaces()
        .into_iter()
        .map(|name| {
            let state = socketcan::interface_state(&name);
            CanInterfaceInfo { name, state }
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub fn can_interfaces() -> Vec<CanInterfaceInfo> {
    Vec::new()
}

#[cfg(target_os = "linux")]
pub fn can_interface_state(name: &str) -> String {
    socketcan::interface_state(name)
}

#[cfg(not(target_os = "linux"))]
pub fn can_interface_state(_name: &str) -> String {
    "unavailable".into()
}

#[cfg(target_os = "linux")]
pub fn open_socketcan(
    interface: &str,
    timeout: std::time::Duration,
) -> Result<Box<dyn DiagnosticLink>> {
    Ok(Box::new(socketcan::SocketCanLink::new(interface, timeout)?))
}

#[cfg(not(target_os = "linux"))]
pub fn open_socketcan(
    _interface: &str,
    _timeout: std::time::Duration,
) -> Result<Box<dyn DiagnosticLink>> {
    Err(crate::error::Error::Unsupported(
        "SocketCAN/CandleLight is only supported on Linux".into(),
    ))
}
