use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::limits::MAX_SOURCE_BYTES;
use crate::stdlib::read_regular;

#[derive(Clone, Default)]
pub struct SourceFiles {
    opened: HashMap<PathBuf, Arc<[u8]>>,
}

impl SourceFiles {
    pub fn insert(&mut self, path: &Path, source: &[u8]) {
        self.opened
            .insert(Self::canonical_path(path), Arc::from(source));
    }

    pub fn remove(&mut self, path: &Path) {
        self.opened.remove(&Self::canonical_path(path));
    }

    pub fn canonical_path(path: &Path) -> PathBuf {
        if let Ok(path) = std::fs::canonicalize(path) {
            return path;
        }
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let mut normalized = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                other => normalized.push(other),
            }
        }
        if let (Some(parent), Some(name)) = (normalized.parent(), normalized.file_name()) {
            if let Ok(parent) = std::fs::canonicalize(parent) {
                return parent.join(name);
            }
        }
        normalized
    }

    pub fn read(&self, path: &Path) -> io::Result<Arc<[u8]>> {
        if let Some(source) = self.opened.get(&Self::canonical_path(path)) {
            if source.len() > MAX_SOURCE_BYTES {
                return Err(io::ErrorKind::FileTooLarge.into());
            }
            return Ok(source.clone());
        }
        read_regular(path, MAX_SOURCE_BYTES).map(Arc::from)
    }

    pub(crate) fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        let canonical = Self::canonical_path(path);
        if self.opened.contains_key(&canonical) {
            Ok(canonical)
        } else {
            std::fs::canonicalize(path)
        }
    }
}

pub(crate) fn import_path(folder: &Path, name: &str) -> PathBuf {
    folder.join(format!("{name}.이음"))
}
