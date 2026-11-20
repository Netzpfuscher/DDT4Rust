//! DDTRust core: ddt4all-compatible Renault/Dacia ECU database, value codec,
//! ELM327/ISO-TP transport, scanning and diagnostics.
//!
//! The ECU definitions and `projects.json` are data from the ddt4all project
//! (GPL-3.0-or-later). This crate is an independent Rust implementation.

pub mod diag;
pub mod elm;
pub mod error;
pub mod hexutil;
pub mod model;
pub mod sim;
pub mod store;

pub use error::{Error, Result};
pub use model::{
    ButtonDef, DataItem, Database, DisplayDef, EcuData, EcuDevice, EcuFile, EcuLayout, Endian,
    InputDef, LabelDef, Protocol, Screen, Target, TargetIdent, Vehicle,
};
pub use store::DataStore;
