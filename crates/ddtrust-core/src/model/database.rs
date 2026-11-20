use std::collections::HashSet;

use indexmap::IndexMap;
use serde::Deserialize;

use super::ecu::Protocol;
use crate::error::Result;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DbIdentSpec {
    #[serde(default)]
    pub diagnostic_version: String,
    #[serde(default)]
    pub supplier_code: String,
    #[serde(default)]
    pub soft_version: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TargetIdent {
    pub diagnostic_version: String,
    pub supplier_code: String,
    pub soft_version: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DbTargetSpec {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub ecuname: String,
    #[serde(default)]
    pub autoidents: Vec<DbIdentSpec>,
}

#[derive(Debug, Clone)]
pub struct Target {
    pub href: String,
    pub ecuname: String,
    pub group: String,
    pub protocol: Protocol,
    pub address: String,
    pub projects: Vec<String>,
    pub autoidents: Vec<TargetIdent>,
}

impl Target {
    pub fn display_name(&self) -> String {
        if self.group.is_empty() {
            self.ecuname.clone()
        } else {
            format!("[{}] {}", self.group, self.ecuname)
        }
    }

    pub fn supports(&self, protocol: Protocol) -> bool {
        match (&self.protocol, &protocol) {
            (Protocol::Can, Protocol::Can) => true,
            (Protocol::Kwp2000, Protocol::Kwp2000) | (Protocol::Iso8, Protocol::Kwp2000) => true,
            (Protocol::Iso8, Protocol::Iso8) => true,
            (Protocol::Doip, Protocol::Doip) => true,
            (Protocol::Doip, Protocol::Can) => true,
            _ => false,
        }
    }

    pub fn exact_match(&self, diag: &str, supplier: &str, soft: &str, version: &str) -> bool {
        if self.autoidents.is_empty() {
            return false;
        }
        self.autoidents.iter().any(|a| {
            ident_exact(
                &a.diagnostic_version,
                &a.supplier_code,
                &a.soft_version,
                &a.version,
                diag,
                supplier,
                soft,
                version,
            )
        })
    }

    pub fn approximate_match(&self, _diag: &str, supplier: &str, soft: &str) -> bool {
        if self.autoidents.is_empty() {
            return false;
        }
        self.autoidents.iter().any(|a| {
            !a.diagnostic_version.trim().is_empty()
                && a.supplier_code.trim() == supplier.trim()
                && a.soft_version.trim() == soft.trim()
        })
    }

    pub fn version_candidates(&self) -> Vec<i64> {
        self.autoidents
            .iter()
            .filter_map(|a| i64::from_str_radix(a.version.trim(), 16).ok())
            .collect()
    }
}

fn ident_exact(
    t_diag: &str,
    t_supplier: &str,
    t_soft: &str,
    t_version: &str,
    diag: &str,
    supplier: &str,
    soft: &str,
    version: &str,
) -> bool {
    if t_diag.trim().is_empty() {
        return false;
    }
    let td = i64::from_str_radix(t_diag.trim(), 16);
    let d = i64::from_str_radix(diag.trim(), 16);
    if td.is_err() || d.is_err() || td.unwrap() != d.unwrap() {
        return false;
    }
    supplier.trim().starts_with(t_supplier.trim())
        && soft.trim().starts_with(t_soft.trim())
        && version.trim().starts_with(t_version.trim())
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddressEntry(pub String, pub String);

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VehicleSpec {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub addressing: IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub dnat: IndexMap<String, String>,
    #[serde(default)]
    pub snat: IndexMap<String, String>,
    #[serde(default)]
    pub dnat_ext: IndexMap<String, String>,
    #[serde(default)]
    pub snat_ext: IndexMap<String, String>,
    #[serde(default, rename = "DoIP")]
    pub doip: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectsFile {
    #[serde(default)]
    pub projects: IndexMap<String, VehicleSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct AddressInfo {
    pub short: String,
    pub long: String,
}

#[derive(Debug, Clone)]
pub struct Vehicle {
    pub key: String,
    pub code: String,
    pub addressing: IndexMap<String, AddressInfo>,
    pub dnat: IndexMap<String, String>,
    pub snat: IndexMap<String, String>,
    pub dnat_ext: IndexMap<String, String>,
    pub snat_ext: IndexMap<String, String>,
    pub has_doip: bool,
}

impl Vehicle {
    pub fn title(&self) -> String {
        self.key.clone()
    }

    pub fn address_info(&self, addr: &str) -> AddressInfo {
        self.addressing.get(addr).cloned().unwrap_or(AddressInfo {
            short: addr.to_string(),
            long: addr.to_string(),
        })
    }

    pub fn can_ids(&self, addr: &str) -> Option<(String, String)> {
        let a = addr.trim().to_ascii_uppercase();
        if let (Some(tx), Some(rx)) = (self.dnat_ext.get(&a), self.snat_ext.get(&a)) {
            return Some((tx.clone(), rx.clone()));
        }
        if let (Some(tx), Some(rx)) = (self.dnat.get(&a), self.snat.get(&a)) {
            return Some((tx.clone(), rx.clone()));
        }
        // addr may be a TX identifier: reverse lookup
        for (logical, tx) in &self.dnat {
            if tx.eq_ignore_ascii_case(&a) {
                return Some((a.clone(), self.snat.get(logical)?.clone()));
            }
        }
        for (logical, tx) in &self.dnat_ext {
            if tx.eq_ignore_ascii_case(&a) {
                return Some((a.clone(), self.snat_ext.get(logical)?.clone()));
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct TargetMatch {
    pub href: String,
    pub name: String,
    pub group: String,
    pub protocol: Protocol,
    pub address: String,
    pub exact: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Database {
    pub targets: Vec<Target>,
    pub vehicles: Vec<Vehicle>,
    pub db_path_hint: String,
}

impl Database {
    pub fn load(db_json: &str, projects_json: &str) -> Result<Self> {
        let specs: IndexMap<String, DbTargetSpec> = serde_json::from_str(db_json)?;
        let mut targets = Vec::with_capacity(specs.len());
        for (href, s) in specs {
            let autoidents = s
                .autoidents
                .iter()
                .map(|a| TargetIdent {
                    diagnostic_version: a.diagnostic_version.clone(),
                    supplier_code: a.supplier_code.clone(),
                    soft_version: a.soft_version.clone(),
                    version: a.version.clone(),
                })
                .collect();
            targets.push(Target {
                href,
                ecuname: s.ecuname,
                group: s.group,
                protocol: Protocol::parse(&s.protocol),
                address: s.address.trim().to_ascii_uppercase(),
                projects: s.projects,
                autoidents,
            });
        }
        let pf: ProjectsFile = serde_json::from_str(projects_json)?;
        let vehicles = pf
            .projects
            .into_iter()
            .map(|(key, v)| {
                let addressing = v
                    .addressing
                    .into_iter()
                    .map(|(k, entry)| {
                        let short = entry.first().cloned().unwrap_or_else(|| k.clone());
                        let long = entry.get(1).cloned().unwrap_or_else(|| short.clone());
                        (k.trim().to_ascii_uppercase(), AddressInfo { short, long })
                    })
                    .collect();
                Vehicle {
                    key,
                    code: v.code,
                    addressing,
                    dnat: v.dnat,
                    snat: v.snat,
                    dnat_ext: v.dnat_ext,
                    snat_ext: v.snat_ext,
                    has_doip: v.doip.is_some(),
                }
            })
            .collect();
        Ok(Self {
            targets,
            vehicles,
            db_path_hint: String::new(),
        })
    }

    pub fn vehicle(&self, key_or_code: &str) -> Option<&Vehicle> {
        self.vehicles.iter().find(|v| {
            v.key.eq_ignore_ascii_case(key_or_code) || v.code.eq_ignore_ascii_case(key_or_code)
        })
    }

    pub fn all_vehicle(&self) -> Option<&Vehicle> {
        self.vehicles
            .iter()
            .find(|v| v.code.eq_ignore_ascii_case("ALL"))
    }

    pub fn vehicle_targets<'a>(&'a self, vehicle: Option<&Vehicle>) -> Vec<&'a Target> {
        match vehicle {
            None => self.targets.iter().collect(),
            Some(v) if v.code.eq_ignore_ascii_case("ALL") => self.targets.iter().collect(),
            Some(v) => {
                let code = v.code.to_ascii_uppercase();
                let filtered: Vec<&Target> = self
                    .targets
                    .iter()
                    .filter(|t| t.projects.iter().any(|p| p.eq_ignore_ascii_case(&code)))
                    .collect();
                if filtered.is_empty() {
                    self.targets.iter().collect()
                } else {
                    filtered
                }
            }
        }
    }

    pub fn scan_addresses(&self, vehicle: Option<&Vehicle>, protocol: Protocol) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for t in self.vehicle_targets(vehicle) {
            if !t.supports(protocol.clone()) {
                continue;
            }
            let a = t.address.clone();
            if a.is_empty() || a == "00" || a == "FF" {
                continue;
            }
            if seen.insert(a.clone()) {
                out.push(a);
            }
        }
        out.sort();
        out
    }

    pub fn target(&self, href: &str) -> Option<&Target> {
        self.targets.iter().find(|t| {
            t.href.eq_ignore_ascii_case(href)
                || t.href.eq_ignore_ascii_case(&format!("{href}.json"))
        })
    }

    pub fn targets_by_name(&self, name: &str) -> Vec<&Target> {
        self.targets
            .iter()
            .filter(|t| t.ecuname.eq_ignore_ascii_case(name))
            .collect()
    }

    pub fn address_info(&self, vehicle: Option<&Vehicle>, addr: &str) -> AddressInfo {
        if let Some(v) = vehicle {
            if let Some(info) = v.addressing.get(addr) {
                return info.clone();
            }
        }
        self.all_vehicle()
            .and_then(|v| v.addressing.get(addr).cloned())
            .unwrap_or(AddressInfo {
                short: addr.to_string(),
                long: addr.to_string(),
            })
    }

    pub fn can_ids(&self, vehicle: Option<&Vehicle>, addr: &str) -> Option<(String, String)> {
        vehicle
            .and_then(|v| v.can_ids(addr))
            .or_else(|| self.all_vehicle().and_then(|v| v.can_ids(addr)))
    }

    /// Find exact matches for an ECU identification. Returns target matches with address set.
    pub fn identify_exact(
        &self,
        diag: &str,
        supplier: &str,
        soft: &str,
        version: &str,
        addr: &str,
        protocol: Protocol,
    ) -> Vec<TargetMatch> {
        self.targets
            .iter()
            .filter(|t| {
                t.supports(protocol.clone()) && t.exact_match(diag, supplier, soft, version)
            })
            .map(|t| TargetMatch {
                href: t.href.clone(),
                name: t.display_name(),
                group: t.group.clone(),
                protocol: t.protocol.clone(),
                address: addr.to_string(),
                exact: true,
            })
            .collect()
    }

    /// Find the closest approximate ECU by version distance (ddt4all's heuristic).
    pub fn identify_approximate(
        &self,
        diag: &str,
        supplier: &str,
        soft: &str,
        version: &str,
        addr: &str,
        protocol: Protocol,
    ) -> Option<TargetMatch> {
        let actual_version = i64::from_str_radix(version.trim(), 16).ok()?;
        let mut best: Option<(&Target, i64)> = None;
        for t in &self.targets {
            if !t.supports(protocol.clone()) || !t.approximate_match(diag, supplier, soft) {
                continue;
            }
            for v in t.version_candidates() {
                let delta = (v - actual_version).abs();
                if best.as_ref().map(|(_, d)| delta < *d).unwrap_or(true) {
                    best = Some((t, delta));
                }
            }
        }
        best.map(|(t, _)| TargetMatch {
            href: t.href.clone(),
            name: t.display_name(),
            group: t.group.clone(),
            protocol: t.protocol.clone(),
            address: addr.to_string(),
            exact: false,
        })
    }

    pub fn find_by_href_or_name(&self, needle: &str) -> Option<&Target> {
        self.target(needle).or_else(|| {
            let n = needle.to_ascii_lowercase();
            self.targets.iter().find(|t| {
                t.href.to_ascii_lowercase().contains(&n)
                    || t.ecuname.to_ascii_lowercase().contains(&n)
                    || t.display_name().to_ascii_lowercase().contains(&n)
            })
        })
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.targets.len(),
            self.vehicles.len(),
            self.targets.iter().filter(|t| t.protocol.is_can()).count(),
        )
    }
}

impl Vehicle {
    pub fn matches_project(&self, project: &str) -> bool {
        self.code.eq_ignore_ascii_case(project)
    }
}
