//! Source inputs of the game and local dependencies linked into the schema probe.
use serde_json::Value;
use std::io;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const INDEX: &str = ".jackdaw/schema-probe/source-inputs.json";

pub(super) fn record(root: &Path, metadata: &Value) -> io::Result<()> {
    let mut inputs = BTreeSet::new();
    for pkg in metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["source"].is_null())
    {
        let manifest = Path::new(
            pkg["manifest_path"]
                .as_str()
                .ok_or_else(|| io::Error::other("missing package manifest"))?,
        );
        let base = manifest
            .parent()
            .ok_or_else(|| io::Error::other("invalid manifest path"))?;
        inputs.insert(manifest.to_owned());
        inputs.insert(base.join("src"));
        for target in pkg["targets"].as_array().into_iter().flatten() {
            let Some(path) = target["src_path"].as_str().map(Path::new) else {
                continue;
            };
            if !path.starts_with(base.join("src")) && path.extension().is_some_and(|e| e == "rs") {
                inputs.insert(if path.parent() == Some(base) {
                    path.to_owned()
                } else {
                    path.parent().unwrap_or(base).to_owned()
                });
            }
        }
    }
    let path = root.join(INDEX);
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer(tmp.as_file_mut(), &inputs)?;
    tmp.persist(path)?;
    Ok(())
}

#[derive(Default)]
pub struct SourceScan {
    cached: BTreeMap<PathBuf, (SystemTime, u64, u64)>,
    last_full_read: Option<Instant>,
}

impl SourceScan {
    /// Enumerate membership on every pass; reuse individual content hashes for
    /// unchanged files. The first pass in a new editor always reads the bytes.
    pub fn fingerprint(&mut self, root: &Path) -> io::Result<String> {
        // An external tool can preserve both size and mtime when replacing a
        // source file. Bound how long that can hide a content change.
        if self
            .last_full_read
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(120))
        {
            self.cached.clear();
            self.last_full_read = Some(Instant::now());
        }
        let index = match fs::read(root.join(INDEX)) {
            Ok(index) => index,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok("unindexed".into()),
            Err(e) => return Err(e),
        };
        let mut pending: Vec<PathBuf> = serde_json::from_slice(&index)?;
        let mut visited = BTreeSet::new();
        let mut files = BTreeSet::new();
        while let Some(path) = pending.pop() {
            let resolved = match fs::canonicalize(&path) {
                Ok(path) => path,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if !visited.insert(resolved.clone()) {
                continue;
            }
            if resolved.is_dir() {
                for entry in fs::read_dir(&resolved)? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir()
                        || entry.path().extension().is_some_and(|e| e == "rs")
                    {
                        pending.push(entry.path());
                    }
                }
            } else {
                files.insert(resolved);
            }
        }
        self.cached.retain(|path, _| files.contains(path));
        let mut combined = DefaultHasher::new();
        for path in files {
            let meta = fs::metadata(&path)?;
            let mtime = meta.modified()?;
            let length = meta.len();
            let digest = match self.cached.get(&path) {
                Some(&(t, n, h)) if (t, n) == (mtime, length) => h,
                _ => {
                    let mut hash = DefaultHasher::new();
                    fs::read(&path)?.hash(&mut hash);
                    let digest = hash.finish();
                    self.cached.insert(path.clone(), (mtime, length, digest));
                    digest
                }
            };
            path.hash(&mut combined);
            digest.hash(&mut combined);
        }
        Ok(format!("{:016x}", combined.finish()))
    }
}
