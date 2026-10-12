#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_steps: u64,
    pub max_call_depth: usize,
    pub max_heap_objects: usize,
    pub max_heap_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 1_000_000,
            max_call_depth: 1024,
            max_heap_objects: 1_000_000,
            max_heap_bytes: 256 * 1024 * 1024,
        }
    }
}

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_TOKENS: usize = 262_144;
pub const MAX_BRACKET_DEPTH: usize = 32;
pub const MAX_BLOCK_DEPTH: usize = 32;
pub const MAX_PARSER_RECURSION: usize = 48;
pub const MAX_EXPRESSION_DEPTH: usize = 64;
pub const MAX_DIAGNOSTICS: usize = 64;

pub const MAX_INT_BITS: u64 = 1_048_576;
pub const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ITEMS: usize = 16_777_216;
pub const MAX_KEY_DEPTH: usize = 64;
pub const MAX_DATA_DEPTH: usize = 64;

pub const MAX_TRACE_FRAMES: usize = 64;
pub const MAX_ERROR_CAUSES: usize = 16;

pub const MAX_MODULE_FILES: usize = 128;
pub const MAX_MODULE_SOURCE_BYTES: usize = 8 * 1024 * 1024;

pub const MAX_REPL_INPUTS: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Limit {
    Steps(u64),
    CallDepth(usize),
    HeapObjects(usize),
    HeapBytes(usize),
    SourceBytes,
    Tokens,
    BracketDepth,
    BlockDepth,
    ParserRecursion,
    ExpressionDepth,
    ModuleFiles,
    ModuleBytes,
    IntBits,
    Text,
    Items,
    KeyDepth,
    DataDepth,
    Counter,
}

impl Limit {
    pub(crate) fn message(self) -> String {
        let count = |n: u64| {
            let digits = n.to_string();
            let mut out = String::new();
            for (i, digit) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i) % 3 == 0 {
                    out.push(',');
                }
                out.push(digit);
            }
            out
        };
        let mib = |bytes: usize| format!("{} MiB", count((bytes / (1024 * 1024)) as u64));
        match self {
            Self::Steps(n) => format!("실행한 명령어가 한도({}개)를 넘었습니다.", count(n)),
            Self::CallDepth(n) => format!(
                "함수 호출 깊이가 한도({})를 넘었습니다. 끝나지 않는 재귀가 아닌지 확인하세요.",
                count(n as u64)
            ),
            Self::HeapObjects(n) => {
                format!("살아 있는 객체가 한도({}개)를 넘었습니다.", count(n as u64))
            }
            Self::HeapBytes(n) => format!("사용하는 메모리가 한도({})를 넘었습니다.", mib(n)),

            Self::SourceBytes => {
                format!("소스 파일이 한도({})를 넘었습니다.", mib(MAX_SOURCE_BYTES))
            }
            Self::Tokens => format!(
                "소스 파일의 토큰이 한도({}개)를 넘었습니다.",
                count(MAX_TOKENS as u64)
            ),
            Self::BracketDepth => format!(
                "괄호가 한도({}단계)보다 깊게 겹쳤습니다.",
                MAX_BRACKET_DEPTH
            ),
            Self::BlockDepth => {
                format!("블록이 한도({}단계)보다 깊게 겹쳤습니다.", MAX_BLOCK_DEPTH)
            }
            Self::ParserRecursion => format!(
                "식이 한도({}단계)보다 깊게 겹쳤습니다.",
                MAX_PARSER_RECURSION
            ),
            Self::ExpressionDepth => format!(
                "식이 한도({}단계)보다 깊게 겹쳤습니다.",
                MAX_EXPRESSION_DEPTH
            ),
            Self::ModuleFiles => {
                format!("가져온 파일이 한도({}개)를 넘었습니다.", MAX_MODULE_FILES)
            }
            Self::ModuleBytes => format!(
                "가져온 파일 크기의 합이 한도({})를 넘었습니다.",
                mib(MAX_MODULE_SOURCE_BYTES)
            ),
            Self::IntBits => format!("정수가 한도({}비트)를 넘었습니다.", count(MAX_INT_BITS)),
            Self::Text => format!("문자열이 한도({})를 넘었습니다.", mib(MAX_TEXT_BYTES)),
            Self::Items => format!("원소가 한도({}개)를 넘었습니다.", count(MAX_ITEMS as u64)),
            Self::KeyDepth => format!(
                "키로 쓴 튜플이 한도({}단계)보다 깊게 겹쳤습니다.",
                MAX_KEY_DEPTH
            ),
            Self::DataDepth => format!("자료가 한도({}단계)보다 깊게 겹쳤습니다.", MAX_DATA_DEPTH),
            Self::Counter => "실행기의 내부 계수가 한계에 닿았습니다.".to_owned(),
        }
    }
}

pub(crate) const GC_MIN_OBJECTS: usize = 4096;
pub(crate) const GC_MIN_BYTES: usize = 1024 * 1024;
