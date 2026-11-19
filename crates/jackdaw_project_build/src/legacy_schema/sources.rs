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

/// Cargo can spell a path inside the project as e.g. vendor/a/../../plugins/b.
/// Reject paths that leave the project even temporarily. Canonical containment
/// is checked separately before traversing directories.
fn project_path(root: &Path, path: &Path) -> bool {
    use std::path::Component;
    let Ok(relative) = dunce::simplified(path).strip_prefix(dunce::simplified(root)) else {
        return false;
    };
    let mut depth = 0usize;
    for part in relative.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return false,
        }
    }
    true
}

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
        self.scan(root, false)
    }

    /// Opening a project may check timestamps, but must not read Rust source
    /// contents or follow a project-provided inventory outside the project.
    pub fn metadata_stamp(&mut self, root: &Path) -> io::Result<String> {
        self.scan(root, true)
    }

    fn scan(&mut self, root: &Path, metadata_only: bool) -> io::Result<String> {
        let canonical_root = fs::canonicalize(root)?;
        // An external tool can preserve both size and mtime when replacing a
        // source file. Bound how long that can hide a content change.
        if self
            .last_full_read
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(120))
        {
            self.cached.clear();
            self.last_full_read = Some(Instant::now());
        }
        let index_path = match fs::canonicalize(root.join(INDEX)) {
            Ok(path) => path,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok("unindexed".into()),
            Err(e) => return Err(e),
        };
        if !index_path.is_file() || (metadata_only && !index_path.starts_with(&canonical_root)) {
            return Err(io::Error::other("invalid source inventory"));
        }
        let index = fs::read(index_path)?;
        let mut pending: Vec<PathBuf> = serde_json::from_slice(&index)?;
        let mut visited = BTreeSet::new();
        let mut files = BTreeSet::new();
        while let Some(path) = pending.pop() {
            if metadata_only && !project_path(root, &path) && !project_path(&canonical_root, &path)
            {
                return Err(io::Error::other("source outside project"));
            }
            let resolved = match fs::canonicalize(&path) {
                Ok(path) => path,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if metadata_only && !resolved.starts_with(&canonical_root) {
                return Err(io::Error::other("source outside project"));
            }
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
            } else if resolved.is_file() {
                files.insert(resolved);
            }
        }
        self.cached.retain(|path, _| files.contains(path));
        let mut combined = DefaultHasher::new();
        for path in files {
            let meta = fs::metadata(&path)?;
            let mtime = meta.modified()?;
            let length = meta.len();
            if metadata_only {
                path.hash(&mut combined);
                mtime.hash(&mut combined);
                length.hash(&mut combined);
                continue;
            }
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
