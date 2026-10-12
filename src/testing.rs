use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{FileError, Options, execute_file};

pub struct TestCase {
    pub path: PathBuf,
    pub output: Vec<u8>,
    pub error: Option<FileError>,
}

pub struct TestReport {
    pub cases: Vec<TestCase>,
}

impl TestReport {
    pub fn failed(&self) -> usize {
        self.cases
            .iter()
            .filter(|case| case.error.is_some())
            .count()
    }
}

pub fn find_tests(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !name.starts_with('.') && name != "target" {
                    folders.push(entry.path());
                }
            } else if name.starts_with("test_") && name.ends_with(".이음") {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

pub fn run_tests(files: &[PathBuf], options: &Options) -> TestReport {
    let cases = files
        .iter()
        .map(|path| {
            let mut output = Vec::new();
            let error = execute_file(path, &mut output, options).err();
            TestCase {
                path: path.clone(),
                output,
                error,
            }
        })
        .collect();
    TestReport { cases }
}
