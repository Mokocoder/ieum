use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::check;
use crate::diagnostic::{Diagnostic, ExecutionError, Span, display_path, line_column, snippet};
use crate::korean::particle::OBJECT;
use crate::limits::{Limit, MAX_MODULE_FILES, MAX_MODULE_SOURCE_BYTES};
use crate::runtime::{LinkedModule, run_modules};
use crate::sources::import_path;
use crate::syntax::{
    self,
    ast::{Exports, StatementKind},
};
use crate::{Options, SourceFiles};

#[derive(Debug)]
struct LoadedFile {
    path: PathBuf,
    bytes: Arc<[u8]>,
    offset: usize,
}

#[derive(Debug)]
struct LoadedFiles {
    files: Vec<LoadedFile>,
    entry_io: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLocation {
    pub path: PathBuf,
    pub span: Span,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug)]
pub struct FileError {
    pub error: Box<ExecutionError>,
    loaded: Arc<LoadedFiles>,
}

impl FileError {
    fn file(&self, span: Span) -> &LoadedFile {
        let index = self
            .loaded
            .files
            .partition_point(|file| file.offset <= span.start)
            .saturating_sub(1);
        &self.loaded.files[index]
    }

    fn source(&self, span: Span) -> &[u8] {
        &self.file(span).bytes
    }

    pub fn location(&self, span: Span) -> FileLocation {
        let file = self.file(span);
        let local = Span::new(
            span.start.saturating_sub(file.offset).min(file.bytes.len()),
            span.end.saturating_sub(file.offset).min(file.bytes.len()),
        );
        let (line, column) = line_column(&file.bytes, local.start);
        FileLocation {
            path: file.path.clone(),
            span: local,
            line,
            column,
        }
    }

    pub fn write_to(&self, output: &mut impl Write) -> io::Result<()> {
        self.error.write_locations(output, |span| {
            let location = self.location(span);
            let place = format!(
                "{}:{}:{}",
                display_path(&location.path),
                location.line,
                location.column
            );
            (place, snippet(self.source(span), location.span))
        })
    }

    pub fn exit_code(&self) -> u8 {
        match self.error.exit_code {
            Some(code) => code,
            None if self.loaded.entry_io => 2,
            None => 1,
        }
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let location = self.location(self.error.diagnostic.span);
        write!(
            f,
            "{}:{}:{}: {}",
            display_path(&location.path),
            location.line,
            location.column,
            self.error
        )
    }
}

impl std::error::Error for FileError {}

pub(crate) struct Graph {
    loaded: Arc<LoadedFiles>,
    modules: Vec<LinkedModule>,
    order: Vec<usize>,
}

struct Loader<'a> {
    loaded: Vec<LoadedFile>,
    modules: Vec<LinkedModule>,
    module_ids: HashMap<PathBuf, usize>,
    total_bytes: usize,
    next_offset: usize,
    entry_io: bool,
    sources: &'a SourceFiles,
}

impl Loader<'_> {
    fn read_failed(
        &mut self,
        error: io::Error,
        path: &Path,
        site: Option<Span>,
    ) -> Vec<Diagnostic> {
        self.entry_io = site.is_none();
        let path = display_path(path);
        let object = OBJECT.after(&path);
        vec![Diagnostic::new(
            if site.is_none() {
                "E-IO-001"
            } else {
                "E-MODULE-002"
            },
            site.unwrap_or_default(),
            format!("소스 '{path}'{object} 읽지 못했습니다: {error}"),
        )]
    }

    fn add(&mut self, path: PathBuf, site: Option<Span>) -> Result<usize, Vec<Diagnostic>> {
        let limit = |kind| vec![Diagnostic::limit(kind, site.unwrap_or_default())];
        if self.modules.len() >= MAX_MODULE_FILES {
            return Err(limit(Limit::ModuleFiles));
        }
        let bytes = match self.sources.read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                return Err(limit(Limit::SourceBytes));
            }
            Err(error) => return Err(self.read_failed(error, &path, site)),
        };
        if self.total_bytes + bytes.len() > MAX_MODULE_SOURCE_BYTES {
            return Err(limit(Limit::ModuleBytes));
        }
        let offset = self.next_offset;
        let span = Span::new(offset, offset + bytes.len());
        self.total_bytes += bytes.len();
        self.next_offset += bytes.len() + 1;
        self.loaded.push(LoadedFile {
            path: path.clone(),
            bytes,
            offset,
        });
        let file = self.loaded.last().expect("방금 넣은 파일이 있습니다");
        let body = syntax::parse_at(&file.bytes, offset)?;
        check::check_placement(&body).map_err(|error| vec![error])?;
        let id = self.modules.len();
        self.modules.push(LinkedModule { span, body });
        self.module_ids.insert(path, id);
        Ok(id)
    }

    fn visit(&mut self, entry: &Path) -> Result<Vec<usize>, Vec<Diagnostic>> {
        let path = self
            .sources
            .resolve(entry)
            .map_err(|error| self.read_failed(error, entry, None))?;
        self.add(path, None)?;
        let mut pending = vec![(0usize, 0usize)];
        let mut done = HashSet::new();
        let mut order = Vec::new();
        while let Some(&(id, next)) = pending.last() {
            let import = self.modules[id].body.get(next).and_then(|statement| {
                if let StatementKind::Import(import) = &statement.kind {
                    Some(import)
                } else {
                    None
                }
            });
            let Some(import) = import else {
                pending.pop();
                done.insert(id);
                order.push(id);
                continue;
            };
            let site = import.path_span;
            let path = Path::new(&import.path);
            if import.path.is_empty()
                || import.path.contains('\0')
                || import.path.ends_with(".이음")
                || path
                    .components()
                    .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
            {
                return Err(vec![Diagnostic::new(
                    "E-MODULE-001",
                    site,
                    "가져올 파일은 확장자 없이 상대 경로로 씁니다. 예: \"계산\", \"도구/정렬\"",
                )]);
            }
            let folder = self.loaded[id]
                .path
                .parent()
                .expect("불러온 파일의 경로에는 폴더가 있습니다");
            let candidate = import_path(folder, &import.path);
            let canonical = self
                .sources
                .resolve(&candidate)
                .map_err(|error| self.read_failed(error, &candidate, Some(site)))?;
            let target = if let Some(&target) = self.module_ids.get(&canonical) {
                if !done.contains(&target) {
                    let start = pending
                        .iter()
                        .position(|(node, _)| *node == target)
                        .expect("끝나지 않은 파일은 방문 중인 경로에 있습니다");
                    let mut cycle: Vec<_> = pending[start..]
                        .iter()
                        .map(|(node, _)| display_path(&self.loaded[*node].path))
                        .collect();
                    cycle.push(display_path(&self.loaded[target].path));
                    return Err(vec![Diagnostic::new(
                        "E-MODULE-003",
                        site,
                        format!(
                            "파일끼리 순환해서 가져옵니다(가져오는 순서: {}).",
                            cycle.join(", ")
                        ),
                    )]);
                }
                target
            } else {
                self.add(canonical, Some(site))?
            };
            let StatementKind::Import(import) = &mut self.modules[id].body[next].kind else {
                unreachable!("앞에서 찾은 가져오기 문장입니다")
            };
            import.target = Some(target);
            pending.last_mut().expect("방문 중인 파일이 있습니다").1 += 1;
            if !done.contains(&target) {
                pending.push((target, 0));
            }
        }
        let mut exports: Vec<Arc<Exports>> = vec![Arc::default(); self.modules.len()];
        for &id in &order {
            for statement in &mut self.modules[id].body {
                if let StatementKind::Import(import) = &mut statement.kind {
                    let target = import.target.expect("가져올 파일을 이미 연결했습니다");
                    import.exports = exports[target].clone();
                }
            }
            exports[id] = Arc::new(check::module_exports(&self.modules[id].body));
        }
        for &id in &order {
            check::prepare_module(&mut self.modules[id].body)
                .map_err(|check::Errors(errors)| errors)?;
        }
        Ok(order)
    }
}

impl Graph {
    pub(crate) fn load(entry: &Path) -> Result<Self, Vec<FileError>> {
        Self::load_sources(entry, &SourceFiles::default())
    }

    pub(crate) fn load_sources(
        entry: &Path,
        sources: &SourceFiles,
    ) -> Result<Self, Vec<FileError>> {
        let mut loader = Loader {
            loaded: Vec::new(),
            modules: Vec::new(),
            module_ids: HashMap::new(),
            total_bytes: 0,
            next_offset: 0,
            entry_io: false,
            sources,
        };
        let result = loader.visit(entry);
        if loader.loaded.is_empty() {
            loader.loaded.push(LoadedFile {
                path: entry.to_path_buf(),
                bytes: Arc::from([]),
                offset: 0,
            });
        }
        let loaded = Arc::new(LoadedFiles {
            files: loader.loaded,
            entry_io: loader.entry_io,
        });
        match result {
            Ok(order) => Ok(Self {
                loaded,
                modules: loader.modules,
                order,
            }),
            Err(errors) => Err(errors
                .into_iter()
                .map(|error| FileError {
                    error: Box::new(error.into()),
                    loaded: loaded.clone(),
                })
                .collect()),
        }
    }

    pub(crate) fn run(&self, output: &mut impl Write, options: &Options) -> Result<(), FileError> {
        run_modules(&self.modules, &self.order, output, options)
            .or_else(ExecutionError::unless_success)
            .map_err(|error| FileError {
                error: Box::new(error),
                loaded: self.loaded.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use crate::runtime::heap::Heap;
    use crate::runtime::run_modules_with_heap;

    #[test]
    fn imported_values_closures_and_cycles_stay_rooted_under_forced_collection() {
        let mut sources = SourceFiles::default();
        let folder = std::env::temp_dir().join("ieum-module-gc");
        for (name, source) in [
            (
                "기초.이음",
                "목록은 [7]이다.
만드는 법:
    현재는 0이다.
    증가하는 법:
        현재를 (현재 + 1)로 바꾼다.
        현재를 반환한다.
    증가하다를 반환한다.
",
            ),
            (
                "중간.이음",
                "\"기초\"에서 목록과 만들다를 가져온다.
카운터는 (만든 값)이다.
목록에 목록을 추가한다.
",
            ),
            (
                "시작.이음",
                "\"중간\"에서 카운터를 가져온다.
\"기초\"에서 목록을 가져온다.
임시는 [목록]이다.
임시에 임시를 추가한다.
((카운터)으로 호출한 값)을 출력한다.
((카운터)으로 호출한 값)을 출력한다.
목록을 출력한다.
",
            ),
        ] {
            sources.insert(&folder.join(name), source.as_bytes());
        }
        let graph = Graph::load_sources(&folder.join("시작.이음"), &sources).unwrap();
        let mut heap = Heap::new(Limits::default()).unwrap();
        heap.stress_gc = true;
        let mut output = Vec::new();
        let (modules, order) = (&graph.modules, &graph.order);
        let options = Options::default();
        let run = run_modules_with_heap(modules, order, &mut output, &options, heap);
        let mut heap = run.unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "1\n2\n[7, <순환>]\n");
        heap.collect(std::iter::empty(), Span::default()).unwrap();
        assert_eq!((heap.stats().live, heap.stats().bytes), (0, 0));
    }
}
