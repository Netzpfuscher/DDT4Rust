use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zip::ZipArchive;

use crate::error::{Error, Result};
use crate::model::{Database, EcuFile, EcuLayout};

/// Provides access to the ddt4all data set, either an extracted directory or ecu.zip.
pub struct DataStore {
    backend: Backend,
    zip: Mutex<Option<ZipArchive<BufReader<File>>>>,
    zip_names: Mutex<Option<HashMap<String, String>>>,
    dir_names: Mutex<Option<HashMap<String, String>>>,
    projects: String,
}

enum Backend {
    Dir(PathBuf),
    Zip(PathBuf),
}

impl DataStore {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .map_err(|_| Error::NotFound(path.to_path_buf()))?;
        if path.is_dir() {
            Ok(Self {
                backend: Backend::Dir(path),
                zip: Mutex::new(None),
                zip_names: Mutex::new(None),
                dir_names: Mutex::new(None),
                projects: projects_json().to_string(),
            })
        } else {
            let file = File::open(&path)?;
            let archive = ZipArchive::new(BufReader::new(file))?;
            Ok(Self {
                backend: Backend::Zip(path),
                zip: Mutex::new(Some(archive)),
                zip_names: Mutex::new(None),
                dir_names: Mutex::new(None),
                projects: projects_json().to_string(),
            })
        }
    }

    /// Find the standard data set: explicit path, ./data/ecu, ./ecu.zip, or the user data dir.
    pub fn discover(explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            return Self::open(p);
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut candidates = vec![cwd.join("data/ecu"), cwd.join("ecu.zip"), cwd.join("json")];
        if let Some(data) = dirs::data_dir() {
            candidates.push(data.join("ddtrust").join("data/ecu"));
            candidates.push(data.join("ddtrust").join("ecu.zip"));
        }
        for c in candidates {
            if c.exists() && Self::open(&c).is_ok() {
                return Self::open(&c);
            }
        }
        Err(Error::Missing(
            "no ECU database found (pass --data <dir|ecu.zip>; ddt4all's ecu.zip is supported)"
                .into(),
        ))
    }

    pub fn projects_json(&self) -> &str {
        &self.projects
    }

    pub fn set_projects_json(&mut self, s: String) {
        self.projects = s;
    }

    pub fn location(&self) -> String {
        match &self.backend {
            Backend::Dir(p) => p.display().to_string(),
            Backend::Zip(p) => p.display().to_string(),
        }
    }

    fn normalize(rel: &str) -> String {
        let mut s = rel.trim().replace('\\', "/");
        while let Some(rest) = s.strip_prefix("./") {
            s = rest.to_string();
        }
        s.trim_start_matches('/').to_string()
    }

    fn dir_lookup(&self, dir: &Path, rel: &str) -> Option<PathBuf> {
        let direct = dir.join(rel);
        if direct.is_file() {
            return Some(direct);
        }
        let mut guard = self.dir_names.lock().ok()?;
        if guard.is_none() {
            let mut map = HashMap::new();
            collect_files(dir, dir, &mut map);
            *guard = Some(map);
        }
        let map = guard.as_ref()?;
        map.get(&rel.to_ascii_lowercase()).map(PathBuf::from)
    }

    fn zip_lookup(&self, rel: &str) -> Option<Vec<u8>> {
        let mut guard = self.zip.lock().ok()?;
        let archive = guard.as_mut()?;
        let mut names_guard = self.zip_names.lock().ok()?;
        if names_guard.is_none() {
            let mut map = HashMap::new();
            for name in archive.file_names() {
                if let Ok(name) = name {
                    map.insert(name.to_ascii_lowercase(), name.to_string());
                }
            }
            *names_guard = Some(map);
        }
        let map = names_guard.as_ref()?;
        let name = map.get(&rel.to_ascii_lowercase())?.clone();
        let mut file = archive.by_name(&name).ok()?;
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    pub fn read(&self, rel: &str) -> Result<Vec<u8>> {
        let rel = Self::normalize(rel);
        match &self.backend {
            Backend::Dir(dir) => {
                if let Some(p) = self.dir_lookup(dir, &rel) {
                    return Ok(std::fs::read(p)?);
                }
                // ddt4all sometimes stores files in a json/ subdirectory.
                if let Some(p) = self.dir_lookup(dir, &format!("json/{rel}")) {
                    return Ok(std::fs::read(p)?);
                }
            }
            Backend::Zip(_) => {
                if let Some(b) = self.zip_lookup(&rel) {
                    return Ok(b);
                }
            }
        }
        Err(Error::Missing(rel))
    }

    pub fn read_optional(&self, rel: &str) -> Option<Vec<u8>> {
        self.read(rel).ok()
    }

    pub fn load_database(&self) -> Result<Database> {
        let db = self.read("db.json")?;
        let db = String::from_utf8_lossy(&db).to_string();
        Database::load(&db, &self.projects)
    }

    pub fn load_ecu(&self, href: &str) -> Result<EcuFile> {
        let data = self.read(href)?;
        let text = String::from_utf8_lossy(&data);
        EcuFile::from_json(href, &text)
    }

    pub fn load_layout(&self, href: &str) -> Result<EcuLayout> {
        let mut name = href.to_string();
        if !name.ends_with(".layout") {
            name.push_str(".layout");
        }
        let data = self.read(&name).or_else(|_| {
            let base = Path::new(href)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| href.to_string());
            self.read(&format!("{base}.layout"))
        })?;
        let text = String::from_utf8_lossy(&data);
        EcuLayout::from_json(&text)
    }

    /// Read a graphic by logical name (without extension is also accepted).
    pub fn graphic(&self, name: &str) -> Option<Vec<u8>> {
        let mut n = name.trim().replace('\\', "/");
        n = n.trim_start_matches('/').to_string();
        if let Some(rest) = n.strip_prefix("graphics/") {
            n = rest.to_string();
        }
        for ext in [".gif", ".GIF", ".gif", ".png", ".PNG", ".jpg", ".jpeg"] {
            if let Ok(b) = self.read(&format!("graphics/{n}{ext}")) {
                return Some(b);
            }
            if let Ok(b) = self.read(&format!("graphics/{n}")) {
                return Some(b);
            }
        }
        self.read(&format!("graphics/{n}")).ok()
    }

    pub fn has_file(&self, rel: &str) -> bool {
        self.read_optional(rel).is_some()
    }
}

fn collect_files(root: &Path, dir: &Path, out: &mut HashMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            let key = rel
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            out.insert(key, path.to_string_lossy().to_string());
        }
    }
}

/// projects.json is part of ddt4all (GPL-3.0-or-later) and is embedded so that
/// vehicle/addressing metadata is available even with an extracted ECU directory.
pub fn projects_json() -> &'static str {
    include_str!("../../../assets/projects.json")
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ddtrust")
}
