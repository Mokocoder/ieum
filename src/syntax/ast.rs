use crate::diagnostic::{Diagnostic, Result, Span};
use crate::korean::particle::{OBJECT, SUBJECT, TOWARD, WITH};
use crate::limits::{Limit, MAX_EXPRESSION_DEPTH};
use crate::runtime::value::{Constant, Value};

#[derive(Clone, Debug)]
pub(crate) struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub name_span: Span,
    pub depth: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum ExprKind {
    Literal(Constant),
    Number(String),
    Name(String),
    Unary(UnaryOp, Span, Box<Expr>),
    Binary(BinOp, Span, Box<Expr>, Box<Expr>),
    Compare(Vec<Expr>, Vec<(CmpOp, Span)>),
    Group(Box<Expr>),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Set(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Index(Box<Expr>, Box<Expr>, Span),
    Slice(Box<Expr>, [Option<Box<Expr>>; 3], Span),
    Attribute(Box<Expr>, String),
    Chain(Chain),
    Record(Box<RecordLiteral>),
    Collect(Box<Collect>),
    Range(Box<Expr>, Box<Expr>, bool),
    Template(Vec<Part>),
    Choice(Box<[Expr; 3]>),
    Either(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    And,
    Or,
}

impl BinOp {
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Some(match symbol {
            "+" => Self::Add,
            "-" => Self::Sub,
            "*" => Self::Mul,
            "/" => Self::Div,
            "//" => Self::FloorDiv,
            "%" => Self::Mod,
            "**" => Self::Pow,
            _ => return None,
        })
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::FloorDiv => "//",
            Self::Mod => "%",
            Self::Pow => "**",
            Self::And => "그리고",
            Self::Or => "또는",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
    Pos,
    Not,
}

impl UnaryOp {
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Some(match symbol {
            "+" => Self::Pos,
            "-" => Self::Neg,
            _ => return None,
        })
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Neg => "-",
            Self::Pos => "+",
            Self::Not => "아님",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Some(match symbol {
            "==" => Self::Eq,
            "!=" => Self::Ne,
            "<" => Self::Lt,
            "<=" => Self::Le,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            _ => return None,
        })
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
        }
    }

    pub fn holds(self, order: std::cmp::Ordering) -> bool {
        match self {
            Self::Eq => order.is_eq(),
            Self::Ne => order.is_ne(),
            Self::Lt => order.is_lt(),
            Self::Le => order.is_le(),
            Self::Gt => order.is_gt(),
            Self::Ge => order.is_ge(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RecordLiteral {
    pub type_name: String,
    pub type_span: Span,
    pub fields: Vec<(String, Span, Expr)>,
}

impl Expr {
    pub fn leaf(kind: ExprKind, span: Span) -> Self {
        Self {
            kind,
            span,
            name_span: span,
            depth: 1,
        }
    }

    pub fn bounded(kind: ExprKind, span: Span, depth: usize) -> Result<Self> {
        if depth > MAX_EXPRESSION_DEPTH {
            return Err(Diagnostic::limit(Limit::ExpressionDepth, span));
        }
        Ok(Self {
            kind,
            span,
            name_span: span,
            depth,
        })
    }

    pub fn unary(op: UnaryOp, op_span: Span, value: Self, span: Span) -> Result<Self> {
        let depth = depth([&value]);
        Self::bounded(ExprKind::Unary(op, op_span, Box::new(value)), span, depth)
    }

    pub fn binary(op: BinOp, op_span: Span, left: Self, right: Self) -> Result<Self> {
        let depth = depth([&left, &right]);
        let span = left.span.through(right.span);
        Self::bounded(
            ExprKind::Binary(op, op_span, Box::new(left), Box::new(right)),
            span,
            depth,
        )
    }

    pub fn ungrouped(&self) -> &Self {
        match &self.kind {
            ExprKind::Group(inner) => inner.ungrouped(),
            _ => self,
        }
    }

    pub fn as_name(&self) -> Option<(&str, Span)> {
        let expr = self.ungrouped();
        match &expr.kind {
            ExprKind::Name(name) => Some((name, expr.name_span)),
            _ => None,
        }
    }
}

pub(crate) fn depth<'a>(children: impl IntoIterator<Item = &'a Expr>) -> usize {
    children.into_iter().map(|e| e.depth).max().unwrap_or(0) + 1
}

#[derive(Clone, Debug)]
pub(crate) struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

pub(crate) struct Declaration<'a> {
    pub name: &'a str,
    pub span: Span,
    pub fixed: Option<&'static str>,
    pub lemmas: Vec<String>,
    pub class: Option<WordClass>,
}

impl Statement {
    pub fn flattened(statements: &[Statement]) -> Vec<&Statement> {
        let mut flat = Vec::new();
        for statement in statements {
            match &statement.kind {
                StatementKind::Concurrent(_, body) => flat.extend(Statement::flattened(body)),
                _ => flat.push(statement),
            }
        }
        flat
    }

    pub fn declarations(&self) -> Vec<Declaration<'_>> {
        match &self.kind {
            StatementKind::Concurrent(_, body) => {
                body.iter().flat_map(Statement::declarations).collect()
            }
            _ => self.declaration().into_iter().collect(),
        }
    }

    pub fn declaration(&self) -> Option<Declaration<'_>> {
        let (name, span, fixed) = match &self.kind {
            StatementKind::Bind {
                name,
                name_span,
                mutable,
                ..
            } => (name, *name_span, (!mutable).then_some("상수")),
            StatementKind::Function(f) => (&f.name, f.name_span, None),
            StatementKind::Record(record) => (&record.name, record.name_span, Some("자료형")),
            _ => return None,
        };
        let (lemmas, class) = match &self.kind {
            StatementKind::Function(f) => (f.lemmas.clone(), f.class),
            _ => (verb_lemmas(name), None),
        };
        Some(Declaration {
            name,
            span,
            fixed,
            lemmas,
            class,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) enum StatementKind {
    Import(Import),
    Bind {
        name: String,
        name_span: Span,
        mutable: bool,
        value: Expr,
    },
    Assign {
        target: Expr,
        target_span: Span,
        value: Expr,
        adjust: Option<BinOp>,
    },
    Chain(Chain),
    Function(Function),
    Record(RecordType),
    Try(TryBlock),
    Throw(Expr),
    Rethrow,
    If {
        test: Expr,
        test_span: Span,
        body: Vec<Statement>,
        otherwise: Vec<Statement>,
    },
    While {
        test: Expr,
        test_span: Span,
        body: Vec<Statement>,
    },
    For {
        name: String,
        name_span: Span,
        second: Option<(String, Span)>,
        collection: Expr,
        last: Option<Box<(Expr, bool)>>,
        filter: Option<(Expr, Span)>,
        body: Vec<Statement>,
        concurrency: Concurrency,
    },
    Concurrent(Concurrency, Vec<Statement>),
    Return(Expr),
    Break,
    Continue,
    Exit(Option<Expr>),
}

#[derive(Clone, Debug)]
pub(crate) struct RecordType {
    pub name: String,
    pub name_span: Span,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub name: String,
    pub span: Span,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Export {
    pub signature: Option<Vec<SignatureParameter>>,
    pub fields: Option<RecordFields>,
    pub lemmas: Vec<String>,
    pub class: Option<WordClass>,
}

pub(crate) fn verb_lemmas(name: &str) -> Vec<String> {
    if name.ends_with('다') {
        vec![name.to_owned()]
    } else {
        Vec::new()
    }
}

pub(crate) type RecordFields = Vec<(String, bool)>;

pub(crate) type Exports = std::collections::HashMap<String, Export>;

#[derive(Clone, Debug)]
pub(crate) struct Import {
    pub path: String,
    pub path_span: Span,
    pub names: Vec<(String, Span)>,
    pub target: Option<usize>,
    pub exports: std::sync::Arc<Exports>,
}

#[derive(Clone, Debug)]
pub(crate) struct TryBlock {
    pub body: Vec<Statement>,
    pub handler: Option<Handler>,
    pub cleanup: Vec<Statement>,
    pub cleanup_span: Span,
}

#[derive(Clone, Debug)]
pub(crate) struct Handler {
    pub name: String,
    pub name_span: Span,
    pub span: Span,
    pub body: Vec<Statement>,
}

#[derive(Clone, Debug)]
pub(crate) struct Function {
    pub name: String,
    pub lemmas: Vec<String>,
    pub shown: String,
    pub class: Option<WordClass>,
    pub name_span: Span,
    pub fixed: Option<(String, Span)>,
    pub parameters: Vec<Parameter>,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Constraint {
    Any,
    Bool,
    Text,
    Set,
    Sequence,
}

impl Constraint {
    pub fn accepts_constant(self, value: &Constant) -> bool {
        match self {
            Self::Any => true,
            Self::Bool => matches!(value, Constant::Bool(_)),
            Self::Text => matches!(value, Constant::Text(_)),
            Self::Set | Self::Sequence => false,
        }
    }

    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Any => true,
            Self::Bool => matches!(value, Value::Bool(_)),
            Self::Text => matches!(value, Value::Text(_)),
            Self::Set => matches!(value, Value::Set(_)),
            Self::Sequence => matches!(value, Value::List(_) | Value::Tuple(_)),
        }
    }
}

pub(crate) fn phrase(noun: &str, role: Role, lemma: &str) -> String {
    format!("{noun}{} {lemma}", role.after(noun))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Role {
    Subject,
    Object,
    Recipient,
    Target,
    Source,
    Direction,
    Partner,
    Amount,
}

impl Role {
    pub const PARTICLES: &[&str] = &[
        "에게", "에서", "으로", "만큼", "이", "가", "을", "를", "에", "로", "와", "과",
    ];

    pub fn from_particle(particle: &str) -> Option<Self> {
        Some(match particle {
            "이" | "가" => Self::Subject,
            "을" | "를" => Self::Object,
            "에게" => Self::Recipient,
            "에" => Self::Target,
            "에서" => Self::Source,
            "으로" | "로" => Self::Direction,
            "와" | "과" => Self::Partner,
            "만큼" => Self::Amount,
            _ => return None,
        })
    }

    pub fn after(self, word: &str) -> String {
        match self {
            Self::Subject => SUBJECT.after(word),
            Self::Object => OBJECT.after(word),
            Self::Direction => TOWARD.after(word),
            Self::Partner => WITH.after(word),
            _ => self.particle().to_owned(),
        }
    }

    pub fn particle(self) -> &'static str {
        match self {
            Self::Subject => "이/가",
            Self::Object => "을/를",
            Self::Recipient => "에게",
            Self::Target => "에",
            Self::Source => "에서",
            Self::Direction => "으로/로",
            Self::Partner => "와/과",
            Self::Amount => "만큼",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParameterSpec {
    pub role: Role,
    pub constraint: Constraint,
    pub input: bool,
    pub optional: bool,
}

impl ParameterSpec {
    pub const fn new(role: Role, constraint: Constraint, input: bool) -> Self {
        Self {
            role,
            constraint,
            input,
            optional: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SignatureParameter {
    pub spec: ParameterSpec,
    pub name: Option<String>,
}

pub(crate) fn plain_signature(parameters: &[ParameterSpec]) -> Vec<SignatureParameter> {
    parameters
        .iter()
        .map(|spec| SignatureParameter {
            spec: *spec,
            name: None,
        })
        .collect()
}

#[derive(Clone, Debug)]
pub(crate) struct Parameter {
    pub name: String,
    pub name_span: Span,
    pub role_span: Span,
    pub spec: ParameterSpec,
    pub default: Option<Expr>,
}

impl Parameter {
    pub fn signature(&self) -> SignatureParameter {
        SignatureParameter {
            spec: ParameterSpec {
                optional: self.default.is_some(),
                ..self.spec
            },
            name: Some(self.name.clone()),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Chain {
    pub calls: Vec<Call>,
    pub edges: Vec<Edge>,
    pub implied: Vec<Argument>,
    pub each: Option<Box<Expr>>,
    pub concurrency: Concurrency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Concurrency {
    Sequential,
    Together,
    Parallel,
}

impl Concurrency {
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "함께" => Some(Self::Together),
            "병렬로" => Some(Self::Parallel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Sequence,
    Pipe,
}

#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub callee: Callee,
    pub span: Span,
    pub arguments: Vec<Argument>,
}

#[derive(Clone, Debug)]
pub(crate) enum Callee {
    Verb {
        word: String,
        lemmas: Vec<String>,
        name: String,
        class: Option<WordClass>,
    },
    Invoke(Box<Expr>),
}

#[derive(Clone, Debug)]
pub(crate) struct Argument {
    pub role: Role,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Flow,
    Word,
    Field,
    Condition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WordClass {
    Verb,
    Adjective,
}

impl WordClass {
    pub fn name(self) -> &'static str {
        match self {
            Self::Verb => "동사",
            Self::Adjective => "형용사",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Part {
    Text(String),
    Value(Expr, Option<String>),
}

#[derive(Clone, Debug)]
pub(crate) struct Collect {
    pub name: String,
    pub name_span: Span,
    pub collection: Expr,
    pub filter: Option<Expr>,
    pub value: Option<Expr>,
    pub concurrency: Concurrency,
}
