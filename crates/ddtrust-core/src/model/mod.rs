pub mod data;
pub mod database;
pub mod ecu;
pub mod layout;
pub mod request;

use serde::{Deserialize, Serialize};

pub use data::EcuData;
pub use database::{AddressInfo, Database, Target, TargetIdent, TargetMatch, Vehicle};
pub use ecu::{EcuDevice, EcuFile, Protocol};
pub use layout::{ButtonDef, DisplayDef, EcuLayout, InputDef, LabelDef, Screen};
pub use request::{DataItem, EcuRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Endian {
    #[default]
    Big,
    Little,
}

impl Endian {
    pub fn parse(s: &str) -> Endian {
        if s.trim().eq_ignore_ascii_case("little") {
            Endian::Little
        } else {
            Endian::Big
        }
    }
}
