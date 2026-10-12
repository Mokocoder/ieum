use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use super::Native;
use crate::diagnostic::{Diagnostic, Result, Span, display_path, internal};
use crate::limits::{Limit, MAX_TEXT_BYTES};
use crate::runtime::heap::Heap;
use crate::runtime::value::Value;
use Outcome::Paths;

#[derive(Clone, Debug)]
pub struct FileSystem {
    base: PathBuf,
}

impl FileSystem {
    pub fn new(base: impl AsRef<Path>) -> io::Result<Self> {
        let base = fs::canonicalize(base)?;
        if !fs::metadata(&base)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "기준 경로가 폴더가 아닙니다.",
            ));
        }
        Ok(Self { base })
    }

    pub fn current_dir() -> io::Result<Self> {
        Self::new(std::env::current_dir()?)
    }

    pub(crate) fn base(&self) -> &Path {
        &self.base
    }

    fn resolve(&self, text: &str, span: Span) -> Result<PathBuf> {
        let path = path(text, false, span)?;
        Ok(if path.is_absolute() {
            path.to_owned()
        } else {
            self.base.join(path)
        })
    }
}

pub(super) fn join(left: &str, right: &str, span: Span) -> Result<String> {
    path_text(
        &path(left, true, span)?.join(path(right, true, span)?),
        span,
    )
}

pub(super) enum Outcome {
    Value(Value),
    Text(String),
    Paths(Vec<PathBuf>),
}

#[inline(never)]
pub(super) fn invoke(
    native: Native,
    args: &[Value],
    heap: &Heap,
    filesystem: &FileSystem,
    span: Span,
) -> Result<Outcome> {
    let text = |index: usize| {
        args.get(index)
            .and_then(|value| heap.text(value))
            .ok_or_else(|| internal(span))
    };
    let target = text(usize::from(matches!(
        native,
        Native::Write | Native::Append
    )))?;
    if native == Native::FindFiles {
        return find(target, filesystem, span).map(Paths);
    }
    let path = filesystem.resolve(target, span)?;
    let io_error = |error: io::Error| file_error(native.name(), &path, error, span);
    match native {
        Native::Read => {
            let bytes = read_regular(&path, MAX_TEXT_BYTES).map_err(|error| {
                if error.kind() == io::ErrorKind::FileTooLarge {
                    Diagnostic::limit(Limit::Text, span)
                } else {
                    io_error(error)
                }
            })?;
            let mut content = String::from_utf8(bytes).map_err(|error| {
                Diagnostic::new(
                    "E-FILE-002",
                    span,
                    format!(
                        "파일 '{}'의 {}번째 바이트부터 UTF-8로 읽을 수 없습니다. CP949(EUC-KR) 같은 다른 인코딩으로 저장된 파일이면 UTF-8로 다시 저장하세요.",
                        display_path(&path),
                        error.utf8_error().valid_up_to() + 1
                    ),
                )
            })?;
            if content.starts_with('\u{feff}') {
                content.drain(..'\u{feff}'.len_utf8());
            }
            Ok(Outcome::Text(content))
        }
        Native::Write | Native::Append => {
            let content = text(0)?;
            match regular(fs::metadata(&path)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(error)),
            }
            let mut options = OpenOptions::new();
            if native == Native::Append {
                options.append(true).create(true);
            } else {
                options.write(true).create(true).truncate(true);
            }
            let mut file = options.open(&path).map_err(io_error)?;
            file.write_all(content.as_bytes()).map_err(io_error)?;
            Ok(Outcome::Value(Value::None))
        }
        Native::Exists => path
            .try_exists()
            .map(|exists| Outcome::Value(Value::Bool(exists)))
            .map_err(io_error),
        Native::MakeDir => {
            fs::create_dir_all(&path).map_err(io_error)?;
            Ok(Outcome::Value(Value::None))
        }
        Native::ListDir => {
            let mut names = Vec::new();
            for entry in fs::read_dir(&path).map_err(io_error)? {
                names.push(entry.map_err(io_error)?.file_name());
            }
            names.sort();
            let folder = Path::new(&**target);
            let folder = if folder == Path::new(".") {
                Path::new("")
            } else {
                folder
            };
            Ok(Paths(names.iter().map(|name| folder.join(name)).collect()))
        }
        Native::RemoveFile => {
            regular(fs::symlink_metadata(&path)).map_err(io_error)?;
            fs::remove_file(&path).map_err(io_error)?;
            Ok(Outcome::Value(Value::None))
        }
        Native::Move => {
            let destination = filesystem.resolve(text(1)?, span)?;
            match fs::rename(&path, &destination) {
                Err(error) if error.kind() == io::ErrorKind::CrossesDevices && path.is_file() => {
                    fs::copy(&path, &destination).map_err(io_error)?;
                    fs::remove_file(&path).map_err(io_error)?;
                }
                result => result.map_err(io_error)?,
            }
            Ok(Outcome::Value(Value::None))
        }
        _ => Err(internal(span)),
    }
}

fn find(pattern: &str, filesystem: &FileSystem, span: Span) -> Result<Vec<PathBuf>> {
    let folders = pattern.ends_with('/') || (cfg!(windows) && pattern.ends_with('\\'));
    let parts: Vec<_> = path(pattern, false, span)?.components().collect();
    let fixed = parts
        .iter()
        .take_while(|part| !part.as_os_str().to_string_lossy().contains(['*', '?', '[']))
        .count();
    let shown: PathBuf = parts[..fixed].iter().collect();
    let start = if shown.as_os_str().is_empty() {
        filesystem.base.clone()
    } else {
        filesystem.resolve(path_str(&shown, span)?, span)?
    };
    let mut rest = parts[fixed..]
        .iter()
        .map(|part| path_str(Path::new(part), span))
        .collect::<Result<Vec<_>>>()?;
    if rest.last() == Some(&"**") {
        rest.push("*");
    }
    let mut found = Vec::new();
    walk(&start, &shown, &rest, folders, &mut found);
    found.sort();
    found.dedup();
    Ok(found)
}

fn path_str(path: &Path, span: Span) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Diagnostic::new(
            "E-PATH-002",
            span,
            "이 운영체제 경로를 Unicode 문자열로 표현할 수 없습니다.",
        )
    })
}

fn walk(actual: &Path, shown: &Path, rest: &[&str], folders: bool, found: &mut Vec<PathBuf>) {
    let Some((part, rest_after)) = rest.split_first() else {
        let kind = fs::metadata(actual);
        if kind.is_ok_and(|kind| {
            if folders {
                kind.is_dir()
            } else {
                kind.is_file()
            }
        }) {
            found.push(shown.to_owned());
        }
        return;
    };
    if *part == "**" {
        walk(actual, shown, rest_after, folders, found);
    }
    let Ok(entries) = fs::read_dir(actual) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            continue;
        };
        if *part == "**" {
            if !text.starts_with('.') && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk(
                    &actual.join(&name),
                    &shown.join(&name),
                    rest,
                    folders,
                    found,
                );
            }
        } else if matches(part, text) {
            walk(
                &actual.join(&name),
                &shown.join(&name),
                rest_after,
                folders,
                found,
            );
        }
    }
}

enum Piece {
    Star,
    Any,
    Set(bool, Vec<(char, char)>),
    Char(char),
}

impl Piece {
    fn fits(&self, c: char) -> bool {
        match self {
            Self::Star | Self::Any => true,
            Self::Set(negated, ranges) => {
                ranges.iter().any(|(a, b)| (*a..=*b).contains(&c)) != *negated
            }
            Self::Char(expected) => *expected == c,
        }
    }
}

fn pieces(pattern: &[char]) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pattern.len() {
        let piece = match pattern[i] {
            '*' => Piece::Star,
            '?' => Piece::Any,
            '[' => {
                let negated = pattern.get(i + 1) == Some(&'!');
                let first = i + 1 + usize::from(negated);
                let close = (first + 1..pattern.len()).find(|&j| pattern[j] == ']');
                match close {
                    Some(close) if first < pattern.len() => {
                        let set = &pattern[first..close];
                        let mut ranges = Vec::new();
                        let mut j = 0;
                        while j < set.len() {
                            if j + 2 < set.len() && set[j + 1] == '-' {
                                ranges.push((set[j], set[j + 2]));
                                j += 3;
                            } else {
                                ranges.push((set[j], set[j]));
                                j += 1;
                            }
                        }
                        i = close;
                        Piece::Set(negated, ranges)
                    }
                    _ => Piece::Char('['),
                }
            }
            c => Piece::Char(c),
        };
        out.push(piece);
        i += 1;
    }
    out
}

fn matches(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let normal = |text: &str| -> Vec<char> {
        let text: String = text.nfc().collect();
        if cfg!(windows) {
            text.to_lowercase().chars().collect()
        } else {
            text.chars().collect()
        }
    };
    let pieces = pieces(&normal(pattern));
    let name = normal(name);
    let (mut p, mut n, mut star) = (0, 0, None);
    while n < name.len() {
        match pieces.get(p) {
            Some(Piece::Star) => {
                star = Some((p, n));
                p += 1;
            }
            Some(piece) if piece.fits(name[n]) => (p, n) = (p + 1, n + 1),
            _ => match star {
                Some((at, covered)) => {
                    (p, n, star) = (at + 1, covered + 1, Some((at, covered + 1)))
                }
                None => return false,
            },
        }
    }
    pieces[p..].iter().all(|piece| matches!(piece, Piece::Star))
}

pub(crate) fn read_regular(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    regular(fs::metadata(path))?;
    let file = File::open(path)?;
    regular(file.metadata())?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    Ok(bytes)
}

pub(crate) fn path(text: &str, empty: bool, span: Span) -> Result<&Path> {
    let path = Path::new(text);
    if (!empty && text.is_empty()) || text.contains('\0') {
        return Err(Diagnostic::new(
            "E-PATH-001",
            span,
            "경로가 비어 있거나 NUL 문자가 들어 있습니다.",
        ));
    }
    #[cfg(windows)]
    if !path.is_absolute()
        && (path.has_root()
            || matches!(
                path.components().next(),
                Some(std::path::Component::Prefix(_))
            ))
    {
        return Err(Diagnostic::new(
            "E-PATH-001",
            span,
            "드라이브 상대 경로나 드라이브 없는 루트 경로 대신 일반 상대 경로나 완전한 절대 경로를 쓰세요.",
        ));
    }
    Ok(path)
}

pub(crate) fn path_text(path: &Path, span: Span) -> Result<String> {
    let text = path_str(path, span)?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(Diagnostic::limit(Limit::Text, span));
    }
    Ok(text.to_owned())
}

fn regular(metadata: io::Result<Metadata>) -> io::Result<()> {
    if metadata?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "일반 파일이 아닙니다.",
        ))
    }
}

pub(super) fn closed(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-FILE-003",
        span,
        "이 실행에서는 파일을 읽거나 쓸 수 없습니다.",
    )
}

pub(super) fn module_file(filesystem: &FileSystem, text: &str, span: Span) -> Result<PathBuf> {
    let file = filesystem.resolve(text, span)?;
    match regular(fs::metadata(&file)) {
        Ok(()) => Ok(file),
        Err(error) => Err(file_error(Native::ImportPython.name(), &file, error, span)),
    }
}

fn file_error(action: &str, path: &Path, error: io::Error, span: Span) -> Diagnostic {
    Diagnostic::new(
        "E-FILE-001",
        span,
        format!(
            "{action}: 경로 '{}'에서 입출력 오류가 났습니다: {error}",
            display_path(path)
        ),
    )
}
