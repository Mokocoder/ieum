pub mod analysis;
mod check;
mod diagnostic;
mod korean;
pub mod limits;
mod modules;
mod repl;
mod runtime;
mod sources;
mod stdlib;
mod syntax;
mod testing;

use std::io::Write;
use std::path::Path;

pub use diagnostic::{
    CallFrame, Diagnostic, ExceptionCause, ExecutionError, Fix, Span, display_path,
};
pub use limits::Limits;
pub use modules::{FileError, FileLocation};
pub use repl::{ReplOptions, ReplOutcome, run_repl};
pub use sources::SourceFiles;
pub use stdlib::{FileSystem, Input};
pub use syntax::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, SyntaxTokenKind, SyntaxTokens, SyntaxTree,
    format_source, parse_syntax,
};
pub use testing::{TestCase, TestReport, find_tests, run_tests};

#[derive(Clone, Copy, Default)]
pub struct Options<'a> {
    pub limits: Limits,
    pub filesystem: Option<&'a FileSystem>,
    pub input: Option<&'a dyn Input>,
    pub arguments: &'a [String],
    pub seed: Option<u64>,
    pub network: bool,
    pub system: bool,
}

impl std::fmt::Debug for Options<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("limits", &self.limits)
            .field("filesystem", &self.filesystem)
            .field("input", &self.input.is_some())
            .field("arguments", &self.arguments)
            .field("seed", &self.seed)
            .field("network", &self.network)
            .field("system", &self.system)
            .finish()
    }
}

pub fn check(source: &[u8]) -> Vec<Diagnostic> {
    let parsed = match syntax::parse(source) {
        Ok(parsed) => parsed,
        Err(error) => return vec![error],
    };
    if parsed.tree.has_errors() {
        return parsed.tree.diagnostics().to_vec();
    }
    runtime::Program::prepare(parsed.statements)
        .err()
        .unwrap_or_default()
}

pub fn execute(
    source: &[u8],
    output: &mut impl Write,
    options: &Options,
) -> Result<(), ExecutionError> {
    let parsed = syntax::parse_valid(source)?;
    runtime::Program::prepare(parsed.statements)
        .map_err(first_error)?
        .run(output, options)
        .or_else(ExecutionError::unless_success)
}

pub fn disassemble(source: &[u8]) -> Result<String, ExecutionError> {
    let parsed = syntax::parse_valid(source)?;
    let program = runtime::Program::prepare(parsed.statements).map_err(first_error)?;
    let line = |span: Span| diagnostic::line_column(source, span.start).0;
    Ok(program.disassemble(&line)?)
}

pub fn check_file(path: impl AsRef<Path>) -> Vec<FileError> {
    modules::Graph::load(path.as_ref())
        .err()
        .unwrap_or_default()
}

pub fn check_sources(path: impl AsRef<Path>, sources: &SourceFiles) -> Vec<FileError> {
    modules::Graph::load_sources(path.as_ref(), sources)
        .err()
        .unwrap_or_default()
}

pub fn execute_file(
    path: impl AsRef<Path>,
    output: &mut impl Write,
    options: &Options,
) -> Result<(), FileError> {
    modules::Graph::load(path.as_ref())
        .map_err(first_error)?
        .run(output, options)
}

fn first_error<E>(errors: Vec<E>) -> E {
    errors
        .into_iter()
        .next()
        .expect("실패하면 오류가 하나 이상 있습니다")
}
