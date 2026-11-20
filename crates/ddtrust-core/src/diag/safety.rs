use crate::error::{Error, Result};

/// ddt4all's default safe command set. Everything else (writes, coding,
/// security access, actuator tests) requires expert mode.
pub const SAFE_SERVICES: [u8; 10] = [0x10, 0x12, 0x14, 0x17, 0x19, 0x1A, 0x21, 0x22, 0x23, 0x3E];

#[derive(Debug, Clone, Copy)]
pub struct Safety {
    pub expert: bool,
}

impl Default for Safety {
    fn default() -> Self {
        Self { expert: false }
    }
}

impl Safety {
    pub fn expert() -> Self {
        Self { expert: true }
    }

    pub fn check(&self, payload: &[u8]) -> Result<()> {
        if payload.is_empty() {
            return Err(Error::Protocol("empty request".into()));
        }
        if self.expert || SAFE_SERVICES.contains(&payload[0]) {
            Ok(())
        } else {
            Err(Error::Safety(format!(
                "service 0x{:02X} is blocked in read-only mode (enable expert mode)",
                payload[0]
            )))
        }
    }

    pub fn is_safe(payload: &[u8]) -> bool {
        payload
            .first()
            .map(|s| SAFE_SERVICES.contains(s))
            .unwrap_or(false)
    }
}
