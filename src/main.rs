use std::ffi::OsString;
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

mod lsp;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "이음: 한국어로 읽히는 프로그래밍 언어

사용법:
  ieum <파일.이음> [인자...]  실행. 인자는 프로그램의 `실행인자`가 됩니다
  ieum run <파일.이음> [인자...]
  ieum check <파일.이음>      실행하지 않고 검사
  ieum fmt [--check] <파일>   정리한 코드를 출력 (--check: 정리가 필요하면 1)
  ieum dis <파일>             컴파일한 바이트코드를 출력
  ieum test [폴더]            폴더 안의 test_*.이음을 각각 실행
  ieum repl                   대화형 실행
  ieum lsp                    편집기용 언어 서버 (표준 입출력으로 LSP)

파일 대신 -를 쓰면 표준 입력을 읽습니다.";

enum Command {
    Run(OsString, Vec<String>),
    Check(OsString),
    Format { path: OsString, check: bool },
    Disassemble(OsString),
    Test(OsString),
    Repl,
    Lsp,
    Help,
    Version,
}

fn parse(args: &[OsString]) -> Option<Command> {
    let args: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let file = |path: &str| path == "-" || !path.starts_with('-');
    Some(match args.as_slice() {
        [] | ["--help" | "-h" | "help"] => Command::Help,
        ["--version"] => Command::Version,
        ["repl"] => Command::Repl,
        ["lsp"] => Command::Lsp,
        ["test"] => Command::Test(".".into()),
        ["test", folder] if file(folder) => Command::Test(folder.into()),
        ["run", path, arguments @ ..] if file(path) => Command::Run(
            path.into(),
            arguments.iter().map(|a| a.to_string()).collect(),
        ),
        ["check", path] if file(path) => Command::Check(path.into()),
        ["dis", path] if file(path) => Command::Disassemble(path.into()),
        ["fmt", "--check", path] if file(path) => Command::Format {
            path: path.into(),
            check: true,
        },
        ["fmt", path] if file(path) => Command::Format {
            path: path.into(),
            check: false,
        },
        [path, arguments @ ..]
            if file(path)
                && !matches!(
                    *path,
                    "run" | "check" | "fmt" | "dis" | "test" | "repl" | "lsp"
                ) =>
        {
            Command::Run(
                path.into(),
                arguments.iter().map(|a| a.to_string()).collect(),
            )
        }
        _ => return None,
    })
}

// 구문 분석기가 중첩 한도까지 재귀하므로 플랫폼의 주 스레드 스택 크기에 기대지 않습니다.
const STACK_BYTES: usize = 64 * 1024 * 1024;

fn main() -> ExitCode {
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(run)
        .and_then(|thread| thread.join().map_err(|_| std::io::Error::other("panic")))
        .unwrap_or(ExitCode::from(2))
}

fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some(command) = parse(&args) else {
        let _ = writeln!(io::stderr(), "{USAGE}");
        return ExitCode::from(2);
    };
    match command {
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("ieum {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Repl => repl(),
        Command::Lsp => lsp::serve(),
        Command::Test(folder) => test(Path::new(&folder)),
        Command::Run(path, arguments) if path != "-" => run_file(&path, &arguments),
        Command::Check(path) if path != "-" => report(ieum::check_file(&path)),
        Command::Run(_, arguments) => with_stdin(|source, label| {
            let filesystem = filesystem()?;
            let options = ieum::Options {
                arguments: &arguments,
                ..options(&filesystem)
            };
            let result = ieum::execute(source, &mut io::stdout().lock(), &options);
            Ok(report_source(
                result.err().into_iter().collect(),
                label,
                source,
            ))
        }),
        Command::Check(_) => with_stdin(|source, label| {
            let errors = ieum::check(source).into_iter().map(Into::into).collect();
            Ok(report_source(errors, label, source))
        }),
        Command::Format { path, check } => format(&path, check),
        Command::Disassemble(path) => disassemble(&path),
    }
}

fn disassemble(path: &OsString) -> ExitCode {
    let label = path.to_string_lossy();
    let source = match source(path) {
        Ok(source) => source,
        Err(code) => return code,
    };
    match ieum::disassemble(&source) {
        Ok(text) => {
            if io::stdout().lock().write_all(text.as_bytes()).is_err() {
                return ExitCode::from(2);
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_source(vec![error], &label, &source),
    }
}

fn run_file(path: &OsString, arguments: &[String]) -> ExitCode {
    let filesystem = match filesystem() {
        Ok(filesystem) => filesystem,
        Err(code) => return code,
    };
    let stdin = io::stdin();
    let options = ieum::Options {
        input: Some(&stdin),
        arguments,
        ..options(&filesystem)
    };
    let result = ieum::execute_file(path, &mut io::stdout().lock(), &options);
    report(result.err().into_iter().collect())
}

fn report(errors: Vec<ieum::FileError>) -> ExitCode {
    let code = errors.first().map_or(0, ieum::FileError::exit_code);
    for error in errors {
        let _ = error.write_to(&mut io::stderr().lock());
    }
    ExitCode::from(code)
}

fn report_source(errors: Vec<ieum::ExecutionError>, label: &str, source: &[u8]) -> ExitCode {
    let Some(first) = errors.first() else {
        return ExitCode::SUCCESS;
    };
    let code = first.exit_code.unwrap_or(1);
    for error in errors {
        let _ = error.write_to(label, source, &mut io::stderr().lock());
    }
    ExitCode::from(code)
}

fn format(path: &OsString, check: bool) -> ExitCode {
    let label = path.to_string_lossy();
    let source = match source(path) {
        Ok(source) => source,
        Err(code) => return code,
    };
    match ieum::format_source(&source) {
        Ok(formatted) if check => {
            if formatted.as_bytes() == source {
                ExitCode::SUCCESS
            } else {
                eprintln!("{label}: 코드 정리가 필요합니다.");
                ExitCode::from(1)
            }
        }
        Ok(formatted) => {
            if io::stdout().lock().write_all(formatted.as_bytes()).is_err() {
                return ExitCode::from(2);
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_source(vec![error.into()], &label, &source),
    }
}

fn test(folder: &Path) -> ExitCode {
    let filesystem = match filesystem() {
        Ok(filesystem) => filesystem,
        Err(code) => return code,
    };
    let files = match ieum::find_tests(folder) {
        Ok(files) if files.is_empty() => {
            eprintln!("{}: test_*.이음 파일이 없습니다.", folder.display());
            return ExitCode::from(1);
        }
        Ok(files) => files,
        Err(error) => {
            eprintln!(
                "{}: E-IO-001: 폴더를 읽지 못했습니다: {error}",
                folder.display()
            );
            return ExitCode::from(2);
        }
    };
    let report = ieum::run_tests(&files, &options(&filesystem));
    let mut stdout = io::stdout().lock();
    for case in &report.cases {
        let name = case
            .path
            .strip_prefix(folder)
            .unwrap_or(&case.path)
            .display();
        let Some(error) = &case.error else {
            let _ = writeln!(stdout, "[통과] {name}");
            continue;
        };
        let _ = writeln!(stdout, "[실패] {name}");
        let _ = stdout.write_all(&case.output);
        let _ = error.write_to(&mut stdout);
    }
    let failed = report.failed();
    let _ = writeln!(
        stdout,
        "테스트 {}개: {}개 통과, {failed}개 실패",
        report.cases.len(),
        report.cases.len() - failed
    );
    ExitCode::from(u8::from(failed > 0))
}

fn repl() -> ExitCode {
    let filesystem = match filesystem() {
        Ok(filesystem) => filesystem,
        Err(code) => return code,
    };
    let options = ieum::ReplOptions {
        prompts: io::stdin().is_terminal(),
        filesystem: Some(&filesystem),
        network: true,
        system: true,
        limits: limits(),
    };
    match ieum::run_repl(
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        options,
    ) {
        Ok(result) => ExitCode::from(u8::from(result.had_errors)),
        Err(error) => {
            eprintln!("대화형 실행: E-IO-001: 입출력 오류가 났습니다: {error}");
            ExitCode::from(2)
        }
    }
}

fn with_stdin(run: impl FnOnce(&[u8], &str) -> Result<ExitCode, ExitCode>) -> ExitCode {
    match source(&OsString::from("-")) {
        Ok(source) => run(&source, "-").unwrap_or_else(|code| code),
        Err(code) => code,
    }
}

fn options(filesystem: &ieum::FileSystem) -> ieum::Options<'_> {
    ieum::Options {
        filesystem: Some(filesystem),
        network: true,
        system: true,
        limits: limits(),
        ..Default::default()
    }
}

fn limits() -> ieum::Limits {
    ieum::Limits {
        max_steps: u64::MAX,
        max_heap_objects: usize::MAX,
        max_heap_bytes: usize::MAX,
        ..Default::default()
    }
}

fn filesystem() -> Result<ieum::FileSystem, ExitCode> {
    ieum::FileSystem::current_dir().map_err(|error| {
        eprintln!("E-IO-001: 현재 폴더를 알 수 없습니다: {error}");
        ExitCode::from(2)
    })
}

fn source(path: &OsString) -> Result<Vec<u8>, ExitCode> {
    read_source(path).map_err(|error| {
        eprintln!(
            "{}: E-IO-001: 소스를 읽지 못했습니다: {error}",
            path.to_string_lossy()
        );
        ExitCode::from(2)
    })
}

fn read_source(path: &OsString) -> io::Result<Vec<u8>> {
    let reader: Box<dyn Read> = if path == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(path)?)
    };
    let mut bytes = Vec::new();
    reader
        .take(ieum::limits::MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}
