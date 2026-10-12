mod collections;
mod data;
mod format;
mod fs;
mod input;
mod net;
mod pattern;
mod process;
mod random;
mod text;
mod time;

use std::io::Write;
use std::rc::Rc;
use std::sync::OnceLock;

use num_bigint::BigInt;
use num_traits::FromPrimitive;

use crate::diagnostic::{Diagnostic, Result, Span, internal};
use crate::limits::{Limit, MAX_TEXT_BYTES};
use crate::runtime::heap::{Handle, Heap};
use crate::runtime::objects::CollectionKind;
use crate::runtime::render::write_line;
use crate::runtime::value::{Constant, Int, Number, Value, operand_error};
use crate::syntax::ast::{Constraint, ParameterSpec, Role, SignatureParameter, plain_signature};
use Constraint::{Any, Bool, Sequence, Set, Text};
use Role::{Amount, Direction, Object, Partner, Source, Subject, Target};

pub use fs::FileSystem;
pub use input::Input;

pub(crate) use collections::sorted;
pub(crate) use format::{format, is_spec};
pub(crate) use fs::{path, path_text, read_regular};
pub(crate) use process::environment;
pub(crate) use random::Random;

const fn arg(role: Role, constraint: Constraint) -> ParameterSpec {
    ParameterSpec::new(role, constraint, false)
}

const fn input(role: Role, constraint: Constraint) -> ParameterSpec {
    ParameterSpec::new(role, constraint, true)
}

const fn optional(role: Role, constraint: Constraint) -> ParameterSpec {
    let mut spec = ParameterSpec::new(role, constraint, false);
    spec.optional = true;
    spec
}

const fn optional_input(role: Role, constraint: Constraint) -> ParameterSpec {
    let mut spec = ParameterSpec::new(role, constraint, true);
    spec.optional = true;
    spec
}

pub(crate) type Job = Box<dyn FnOnce() -> Result<JobValue> + Send>;

pub(crate) enum JobValue {
    Text(String),
    Nothing,
}

impl JobValue {
    pub(crate) fn into_value(
        self,
        heap: &mut Heap,
        roots: impl Iterator<Item = Handle>,
        span: Span,
    ) -> Result<Value> {
        match self {
            Self::Text(text) => heap.new_text(text, roots, span),
            Self::Nothing => Ok(Value::None),
        }
    }
}

impl Native {
    pub(crate) fn job(
        self,
        args: &[Value],
        reach: Reach,
        filesystem: Option<&FileSystem>,
        heap: &Heap,
        span: Span,
    ) -> Result<Option<Job>> {
        let text = |index: usize| match args.get(index).and_then(|value| heap.text(value)) {
            Some(text) => Ok(text.to_string()),
            None => Err(internal(span)),
        };
        if matches!(self, Self::Download | Self::Send) && !reach.network {
            return Err(Diagnostic::new(
                "E-NET-003",
                span,
                "이 실행에서는 네트워크를 쓸 수 없습니다.",
            ));
        }
        if self == Self::Run && !reach.system {
            return Err(Diagnostic::new(
                "E-PROC-003",
                span,
                "이 실행에서는 다른 프로그램을 실행할 수 없습니다.",
            ));
        }
        Ok(Some(match self {
            Self::Download => {
                let url = text(0)?;
                Box::new(move || net::get(&url, span).map(JobValue::Text))
            }
            Self::Send => {
                let url = text(1)?;
                let (body, json) = match heap.text(&args[0]) {
                    Some(body) => (body.to_string(), false),
                    None => (data::write_json(&args[0], heap, span)?, true),
                };
                Box::new(move || net::post(&url, body, json, span).map(JobValue::Text))
            }
            Self::Wait => {
                let seconds = time::seconds(heap, &args[0], span)?;
                Box::new(move || {
                    std::thread::sleep(seconds);
                    Ok(JobValue::Nothing)
                })
            }
            Self::Run => {
                let command = heap
                    .items(&args[0], span)?
                    .into_iter()
                    .map(|part| match heap.text(&part) {
                        Some(text) => Ok(text.to_string()),
                        None => Err(Diagnostic::new(
                            "E-TYPE-001",
                            span,
                            "실행할 명령은 문자열의 목록이나 튜플이어야 합니다.",
                        )),
                    })
                    .collect::<Result<Vec<_>>>()?;
                let folder = filesystem.map(|filesystem| filesystem.base().to_owned());
                Box::new(move || {
                    process::run(&command, folder.as_deref(), span).map(JobValue::Text)
                })
            }
            _ => return Ok(None),
        }))
    }
}

pub(crate) struct Host<'a> {
    pub filesystem: Option<&'a FileSystem>,
    pub input: Option<&'a dyn Input>,
    pub output: &'a mut dyn Write,
    pub random: &'a mut Random,
    pub reach: Reach,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Reach {
    pub network: bool,
    pub system: bool,
}

pub(crate) const ARGUMENTS: &str = "실행인자";
pub(crate) const ENVIRONMENT: &str = "환경변수";
const PI: &str = "원주율";
const E: &str = "자연상수";
pub(crate) const CONSTANTS: [&str; 4] = [ARGUMENTS, ENVIRONMENT, PI, E];
pub(crate) const CONSTANT_DESCRIPTIONS: [(&str, &str); 4] = [
    (ARGUMENTS, "명령줄에서 파일 뒤에 준 인자의 목록입니다."),
    (ENVIRONMENT, "환경변수의 사전입니다."),
    (PI, "원의 둘레와 지름의 비입니다."),
    (E, "자연로그의 밑입니다."),
];

pub(crate) fn numbers() -> [(&'static str, Constant); 2] {
    [
        (PI, Constant::Float(std::f64::consts::PI)),
        (E, Constant::Float(std::f64::consts::E)),
    ]
}

macro_rules! natives {
    ($($variant:ident = $name:literal [$($parameter:expr),* $(,)?],)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub(crate) enum Native {
            $($variant,)*
        }

        impl Native {
            pub(crate) const ALL: [Self; [$(stringify!($variant)),*].len()] = [$(Self::$variant),*];

            pub(crate) fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            pub(crate) fn signature(self) -> &'static [ParameterSpec] {
                match self {
                    $(Self::$variant => const { &[$($parameter),*] },)*
                }
            }
        }
    };
}

natives! {
    Print = "출력하다" [input(Object, Any)],
    Ask = "묻다" [optional_input(Object, Text)],
    Round = "반올림하다" [input(Object, Any)],
    Ceil = "올림하다" [input(Object, Any)],
    Floor = "내림하다" [input(Object, Any)],
    Pick = "뽑다" [input(Source, Any)],
    Shuffle = "섞다" [input(Object, Any)],
    Zip = "짝짓다" [input(Object, Any), arg(Partner, Any)],
    Enumerate = "번호붙이다" [input(Target, Any)],
    Upper = "대문자변환하다" [input(Object, Text)],
    Lower = "소문자변환하다" [input(Object, Text)],
    Count = "세다" [input(Source, Any), arg(Object, Any)],
    Now = "현재시각구하다" [],
    ParseTime = "시각해석하다" [input(Object, Text), arg(Direction, Text)],
    Wait = "기다리다" [input(Amount, Any)],
    Matches = "일치하다" [input(Subject, Text), arg(Partner, Text)],
    Extract = "추출하다" [input(Source, Text), arg(Object, Text)],
    RegexReplace = "정규식치환하다" [input(Source, Text), arg(Object, Text), arg(Direction, Text)],
    Download = "내려받다" [input(Source, Text)],
    Send = "보내다" [input(Object, Any), arg(Direction, Text)],
    Add = "추가하다" [arg(Target, Any), arg(Object, Any)],
    Assert = "확인하다" [input(Object, Bool)],
    ToInt = "정수변환하다" [input(Object, Any)],
    ToFloat = "실수변환하다" [input(Object, Any)],
    ToText = "문자열변환하다" [input(Object, Any), optional(Direction, Text)],
    ToList = "목록변환하다" [input(Object, Any)],
    ToTuple = "튜플변환하다" [input(Object, Any)],
    ToSet = "집합변환하다" [input(Object, Any)],
    ToJson = "JSON변환하다" [input(Object, Any)],
    ToCsv = "CSV변환하다" [input(Object, Sequence)],
    ParseJson = "JSON해석하다" [input(Object, Text)],
    ParseCsv = "CSV해석하다" [input(Object, Text)],
    Split = "분리하다" [input(Object, Text), optional(Direction, Text)],
    SplitLines = "줄분리하다" [input(Object, Text)],
    Join = "연결하다" [input(Object, Sequence), arg(Direction, Text)],
    Trim = "다듬다" [input(Object, Text)],
    Replace = "치환하다" [input(Source, Text), arg(Object, Text), arg(Direction, Text)],
    StartsWith = "시작하다" [input(Subject, Text), arg(Direction, Text)],
    EndsWith = "끝나다" [input(Subject, Text), arg(Direction, Text)],
    Find = "찾다" [input(Source, Any), arg(Object, Any)],
    Contains = "있다" [input(Target, Any), arg(Subject, Any)],
    Missing = "없다" [input(Target, Any), arg(Subject, Any)],
    Empty = "비어있다" [input(Subject, Any)],
    Sort = "정렬하다" [input(Object, Any), optional(Direction, Any)],
    SortDown = "내림차순정렬하다" [input(Object, Any), optional(Direction, Any)],
    Reverse = "뒤집다" [input(Object, Any)],
    Remove = "지우다" [input(Source, Any), arg(Object, Any)],
    Copy = "복사하다" [input(Object, Any)],
    Union = "합집합구하다" [input(Object, Set), arg(Partner, Set)],
    Intersection = "교집합구하다" [input(Object, Set), arg(Partner, Set)],
    Difference = "차집합구하다" [input(Object, Set), arg(Partner, Set)],
    IsSubset = "포함되다" [input(Subject, Set), arg(Target, Set)],
    Read = "읽다" [input(Object, Text)],
    Write = "쓰다" [input(Object, Text), arg(Target, Text)],
    Append = "덧붙이다" [input(Object, Text), arg(Target, Text)],
    JoinPath = "결합하다" [input(Object, Text), arg(Partner, Text)],
    Exists = "존재하다" [input(Subject, Text)],
    MakeDir = "폴더만들다" [input(Target, Text)],
    ListDir = "폴더읽다" [input(Object, Text)],
    FindFiles = "파일찾다" [input(Object, Text)],
    RemoveFile = "파일지우다" [input(Object, Text)],
    Move = "옮기다" [input(Object, Text), arg(Direction, Text)],
    Run = "실행하다" [input(Object, Sequence)],
}

impl Native {
    pub(crate) fn plain_signature(self) -> &'static [SignatureParameter] {
        static SIGNATURES: OnceLock<Vec<Vec<SignatureParameter>>> = OnceLock::new();
        let all = SIGNATURES.get_or_init(|| {
            Native::ALL
                .iter()
                .map(|native| plain_signature(native.signature()))
                .collect()
        });
        &all[self as usize]
    }

    // 여기서 부르는 큰 함수는 #[inline(never)]로 두어야 그 지역 변수가 모든 내장 호출의 프레임에 쌓이지 않습니다.
    pub(crate) fn invoke(
        self,
        args: &[Value],
        host: Host<'_>,
        heap: &mut Heap,
        roots: impl Iterator<Item = Handle> + Clone,
        span: Span,
    ) -> Result<Value> {
        let roots = roots.chain(args.iter().filter_map(Value::handle));
        let text = |heap: &Heap, index: usize| {
            args.get(index)
                .and_then(|value| heap.text(value))
                .cloned()
                .ok_or_else(|| internal(span))
        };
        match self {
            Self::Print => {
                write_line(host.output, heap, &args[0], span)?;
                Ok(Value::None)
            }
            Self::Ask => {
                let answer = ask(heap.text(&args[0]).cloned(), host, span)?;
                match answer {
                    Some(line) => heap.new_text(line, roots, span),
                    None => Ok(Value::None),
                }
            }
            Self::Round | Self::Ceil | Self::Floor => {
                let rounded = round(self, &args[0], heap, span)?;
                heap.new_int(rounded, roots, span)
            }
            Self::Pick => {
                let items = heap.snapshot(&args[0], roots, span)?;
                if items.is_empty() {
                    return Err(Diagnostic::new(
                        "E-VALUE-001",
                        span,
                        "빈 모음에서는 뽑을 수 없습니다.",
                    ));
                }
                Ok(items[host.random.below(items.len())])
            }
            Self::Zip => collections::zip(&args[0], &args[1], heap, roots, span),
            Self::Upper => {
                let upper = text(heap, 0)?.to_uppercase();
                heap.new_text(upper, roots, span)
            }
            Self::Lower => {
                let lower = text(heap, 0)?.to_lowercase();
                heap.new_text(lower, roots, span)
            }
            Self::Count => collections::count(&args[0], &args[1], heap, span),
            Self::Now => time::now(heap, roots, span),
            Self::ParseTime => {
                let (input, spec) = (text(heap, 0)?, text(heap, 1)?);
                time::parse(&input, &spec, heap, roots, span)
            }
            Self::Matches => Ok(Value::Bool(pattern::matches(
                &text(heap, 0)?,
                &text(heap, 1)?,
                span,
            )?)),
            Self::Extract => {
                let (input, regex) = (text(heap, 0)?, text(heap, 1)?);
                pattern::extract(&input, &regex, heap, roots, span)
            }
            Self::RegexReplace => {
                let replaced =
                    pattern::replace(&text(heap, 0)?, &text(heap, 1)?, &text(heap, 2)?, span)?;
                heap.new_text(replaced, roots, span)
            }
            Self::Download | Self::Send | Self::Wait | Self::Run => {
                let job = self.job(args, host.reach, host.filesystem, heap, span)?;
                job.ok_or_else(|| internal(span))?()?.into_value(heap, roots, span)
            }
            Self::Enumerate => collections::enumerate(&args[0], heap, roots, span),
            Self::Shuffle => {
                let mut items = heap.snapshot(&args[0], roots.clone(), span)?;
                for last in (1..items.len()).rev() {
                    items.swap(last, host.random.below(last + 1));
                }
                let held: Vec<Handle> = items.iter().filter_map(Value::handle).collect();
                heap.new_list(items, roots.chain(held), span)
            }
            Self::Add => collections::add(&args[0], &args[1], heap, roots, span),
            Self::Assert => match args[0] {
                Value::Bool(true) => Ok(Value::None),
                _ => Err(Diagnostic::new(
                    "E-TEST-001",
                    span,
                    "확인한 조건이 거짓입니다.",
                )),
            },
            Self::ToInt => {
                let n = text::to_int(&args[0], heap, span)?;
                heap.new_int(n, roots, span)
            }
            Self::ToFloat => Ok(Value::Float(text::to_float(&args[0], heap, span)?)),
            Self::ToText => {
                let shown = match heap.text(&args[1]).cloned() {
                    Some(spec) => format(heap, &args[0], &spec, span)?,
                    None => heap.render(&args[0], span)?,
                };
                heap.new_text(shown, roots, span)
            }
            Self::ToList | Self::ToTuple | Self::ToSet => {
                let kind = match self {
                    Self::ToTuple => CollectionKind::Tuple,
                    Self::ToSet => CollectionKind::Set,
                    _ => CollectionKind::List,
                };
                let items = heap.snapshot(&args[0], roots.clone(), span)?;
                let held: Vec<Handle> = items.iter().filter_map(Value::handle).collect();
                heap.new_collection(kind, items, roots.chain(held), span)
            }
            Self::ToJson => {
                let json = data::write_json(&args[0], heap, span)?;
                heap.new_text(json, roots, span)
            }
            Self::ToCsv => {
                let csv = data::write_csv(&args[0], heap, span)?;
                heap.new_text(csv, roots, span)
            }
            Self::ParseJson => data::read_json(&text(heap, 0)?, heap, roots, span),
            Self::ParseCsv => data::read_csv(&text(heap, 0)?, heap, roots, span),
            Self::Split if args[1].is_none() => text::words(&text(heap, 0)?, heap, roots, span),
            Self::Split => {
                let (input, separator) = (text(heap, 0)?, text(heap, 1)?);
                text::split(&input, &separator, heap, roots, span)
            }
            Self::SplitLines => text::lines(&text(heap, 0)?, heap, roots, span),
            Self::Join => {
                let joined = text::join(&args[0], &text(heap, 1)?, heap, span)?;
                heap.new_text(joined, roots, span)
            }
            Self::Trim => {
                let input = text(heap, 0)?;
                let trimmed = text::trim(&input);
                if trimmed.len() == input.len() {
                    return Ok(args[0]);
                }
                heap.new_text(trimmed, roots, span)
            }
            Self::Replace => {
                let replaced =
                    text::replace(&text(heap, 0)?, &text(heap, 1)?, &text(heap, 2)?, span)?;
                heap.new_text(replaced, roots, span)
            }
            Self::StartsWith => Ok(Value::Bool(text(heap, 0)?.starts_with(&*text(heap, 1)?))),
            Self::EndsWith => Ok(Value::Bool(text(heap, 0)?.ends_with(&*text(heap, 1)?))),
            Self::Find => collections::find(&args[0], &args[1], heap, span),
            Self::Contains => {
                collections::contains(&args[0], &args[1], heap, span).map(Value::Bool)
            }
            Self::Missing => collections::contains(&args[0], &args[1], heap, span)
                .map(|found| Value::Bool(!found)),
            Self::Empty => match &args[0] {
                Value::Text(_) => Ok(Value::Bool(text(heap, 0)?.is_empty())),
                value @ (Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::Dict(_)) => {
                    Ok(Value::Bool(heap.items(value, span)?.is_empty()))
                }
                other => Err(Diagnostic::new(
                    "E-TYPE-001",
                    span,
                    format!("{}에는 비었는지 물을 수 없습니다.", other.kind_name()),
                )),
            },
            Self::Sort | Self::SortDown => collections::sort(
                &args[0],
                &args[1],
                self == Self::SortDown,
                heap,
                roots,
                span,
            ),
            Self::Reverse => {
                let mut items = heap.snapshot(&args[0], roots.clone(), span)?;
                items.reverse();
                let held: Vec<Handle> = items.iter().filter_map(Value::handle).collect();
                heap.new_list(items, roots.chain(held), span)
            }
            Self::Remove => collections::remove(&args[0], &args[1], heap, span),
            Self::Copy => collections::copy(&args[0], heap, roots, span),
            Self::Union | Self::Intersection | Self::Difference | Self::IsSubset => {
                collections::set_operation(self, &args[0], &args[1], heap, roots, span)
            }
            Self::JoinPath => {
                let joined = fs::join(&text(heap, 0)?, &text(heap, 1)?, span)?;
                heap.new_text(joined, roots, span)
            }
            Self::Read
            | Self::Write
            | Self::Append
            | Self::Exists
            | Self::MakeDir
            | Self::ListDir
            | Self::FindFiles
            | Self::RemoveFile
            | Self::Move => {
                let filesystem = host.filesystem.ok_or_else(|| {
                    Diagnostic::new(
                        "E-FILE-003",
                        span,
                        "이 실행에서는 파일을 읽거나 쓸 수 없습니다.",
                    )
                })?;
                match fs::invoke(self, args, heap, filesystem, span)? {
                    fs::Outcome::Value(value) => Ok(value),
                    fs::Outcome::Text(text) => heap.new_text(text, roots, span),
                    fs::Outcome::Paths(paths) => {
                        let texts = paths
                            .iter()
                            .map(|path| path_text(path, span).map(Rc::from))
                            .collect::<Result<Vec<_>>>()?;
                        heap.text_list(texts, roots, span)
                    }
                }
            }
        }
    }
}

fn round(how: Native, value: &Value, heap: &Heap, span: Span) -> Result<Int> {
    let number = match heap.number(value) {
        Some(Number::Int(n)) => return Ok(n),
        Some(Number::Float(number)) => number,
        None => {
            return Err(operand_error(value, how.name(), value, span));
        }
    };
    let rounded = match how {
        Native::Ceil => number.ceil(),
        Native::Floor => number.floor(),
        _ => number.round(),
    };
    BigInt::from_f64(rounded).map(Int::from).ok_or_else(|| {
        Diagnostic::new(
            "E-VALUE-001",
            span,
            "무한대나 NaN은 정수로 바꿀 수 없습니다.",
        )
    })
}

fn ask(question: Option<Rc<str>>, host: Host<'_>, span: Span) -> Result<Option<String>> {
    let Host { input, output, .. } = host;
    let failed = |error: std::io::Error| {
        Diagnostic::new(
            "E-INPUT-001",
            span,
            format!("입력을 읽지 못했습니다: {error}"),
        )
    };
    if let Some(question) = question {
        write!(output, "{question}")
            .and_then(|()| output.flush())
            .map_err(|error| {
                Diagnostic::new("E-IO-002", span, format!("출력하지 못했습니다: {error}"))
            })?;
    }
    let input = input.ok_or_else(|| {
        Diagnostic::new(
            "E-INPUT-001",
            span,
            "이 실행에서는 입력을 받을 수 없습니다.",
        )
    })?;
    match input.read_line().map_err(failed)? {
        Some(line) if line.len() > MAX_TEXT_BYTES => Err(Diagnostic::limit(Limit::Text, span)),
        line => Ok(line),
    }
}

pub(crate) fn prelude() -> impl Iterator<Item = (&'static str, Value)> + Clone {
    Native::ALL
        .into_iter()
        .map(|native| (native.name(), Value::Native(native)))
}
