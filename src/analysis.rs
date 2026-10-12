mod rename;

pub use rename::{can_rename, rename};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::SourceFiles;
use crate::check::{self, record};
use crate::diagnostic::Span;
use crate::korean::verb::{Ending, conjugate_any};
use crate::limits::{MAX_MODULE_FILES, MAX_MODULE_SOURCE_BYTES};
use crate::sources::import_path;
use crate::stdlib::{self, Native};
use crate::syntax;
use crate::syntax::ast::{Exports, Role, StatementKind};

pub use crate::syntax::ast::Category;
pub use crate::syntax::cursor::{CompletionContext, completion_context};

pub fn particle_after(word: &str, particle: &str) -> String {
    crate::korean::particle::correction(word, particle)
        .unwrap_or(particle)
        .to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Variable,
    Constant,
    Parameter,
    Function,
    Record,
    Imported,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub span: Span,
    pub kind: SymbolKind,
    pub scope: Option<Span>,
    pub lemmas: Vec<String>,
    pub adjective: bool,
    pub particles: Vec<&'static str>,
    pub imported_from: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Reference {
    pub name: String,
    pub span: Span,
    pub definition: Option<Span>,
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
    pub fixed_nouns: Vec<Span>,
    pub categories: Vec<(Span, Category)>,
    pub dependencies: Vec<PathBuf>,
}

impl Analysis {
    pub fn symbol(&self, definition: Span) -> Option<&Symbol> {
        self.symbols.iter().find(|symbol| symbol.span == definition)
    }
}

pub fn analyze(path: Option<&Path>, source: &[u8], sources: &SourceFiles) -> Analysis {
    let Ok(parsed) = syntax::parse_editing(source) else {
        return Analysis::default();
    };
    let mut statements = parsed.statements;
    let mut categories = parsed.categories;
    let mut modules = HashMap::new();
    let mut exports = HashMap::new();
    let mut remaining = MAX_MODULE_SOURCE_BYTES.saturating_sub(source.len());
    if let Some(folder) = path.and_then(Path::parent) {
        for statement in &mut statements {
            let StatementKind::Import(import) = &mut statement.kind else {
                continue;
            };
            let file = SourceFiles::canonical_path(&import_path(folder, &import.path));
            import.exports = imported_exports(&file, sources, &mut exports, &mut remaining);
            for (_, span) in &import.names {
                modules.insert(span.start, file.clone());
            }
        }
    }
    let (_, record) = record::recording(|| check::prepare_module(&mut statements));
    let written = |name: &str| name != syntax::EACH;
    let symbols = record
        .definitions
        .into_iter()
        .filter(|definition| written(&definition.name))
        .map(|definition| Symbol {
            imported_from: modules.get(&definition.span.start).cloned(),
            name: definition.name,
            span: definition.span,
            kind: match definition.kind {
                record::Kind::Variable => SymbolKind::Variable,
                record::Kind::Constant => SymbolKind::Constant,
                record::Kind::Parameter => SymbolKind::Parameter,
                record::Kind::Function => SymbolKind::Function,
                record::Kind::Record => SymbolKind::Record,
                record::Kind::Imported => SymbolKind::Imported,
            },
            scope: definition.scope,
            lemmas: definition.lemmas,
            adjective: definition.adjective,
            particles: definition.particles,
        })
        .collect();
    let references = record
        .usages
        .into_iter()
        .filter(|usage| written(&usage.name))
        .map(|usage| Reference {
            name: usage.name,
            span: usage.span,
            definition: usage.definition,
        })
        .collect();
    let worded = |span: &Span| {
        source
            .get(span.start..span.end)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| text.chars().next())
            .is_some_and(char::is_alphabetic)
    };
    categories.extend(
        record
            .categories
            .into_iter()
            .filter(|(span, _)| worded(span)),
    );
    let mut dependencies: Vec<_> = exports.into_keys().collect();
    dependencies.sort();
    Analysis {
        symbols,
        references,
        fixed_nouns: record.fixed,
        categories,
        dependencies,
    }
}

fn imported_exports(
    path: &Path,
    sources: &SourceFiles,
    cached: &mut HashMap<PathBuf, Arc<Exports>>,
    remaining: &mut usize,
) -> Arc<Exports> {
    if let Some(exports) = cached.get(path) {
        return exports.clone();
    }
    let empty = Arc::default();
    if cached.len() >= MAX_MODULE_FILES {
        return empty;
    }
    cached.insert(path.to_path_buf(), empty.clone());
    let Some(mut parsed) = sources.read(path).ok().and_then(|source| {
        *remaining = remaining.checked_sub(source.len())?;
        syntax::parse_editing(&source).ok()
    }) else {
        return empty;
    };
    for statement in &mut parsed.statements {
        if let StatementKind::Import(import) = &mut statement.kind {
            if let Some(folder) = path.parent() {
                let target = SourceFiles::canonical_path(&import_path(folder, &import.path));
                import.exports = imported_exports(&target, sources, cached, remaining);
            }
        }
    }
    let exports = Arc::new(check::module_exports(&parsed.statements));
    cached.insert(path.to_path_buf(), exports.clone());
    exports
}

pub fn declaration_span(source: &[u8], name: &str) -> Option<Span> {
    let parsed = syntax::parse_editing(source).ok()?;
    syntax::ast::Statement::flattened(&parsed.statements)
        .into_iter()
        .filter_map(|statement| statement.declaration())
        .find(|declared| declared.name == name || declared.lemmas.iter().any(|l| l == name))
        .map(|declared| declared.span)
}

#[derive(Clone, Debug)]
pub struct Builtin {
    pub name: &'static str,
    pub arguments: Option<String>,
    pub particles: Vec<&'static str>,
    pub description: String,
}

pub fn builtins() -> Vec<Builtin> {
    let documented = documented();
    let mut natives = Native::ALL.to_vec();
    natives.sort_by_key(|native| documented.get(native.name()).map_or(usize::MAX, |d| d.0));
    let verbs = natives.into_iter().map(|native| {
        let name = native.name();
        let (_, arguments, description) = documented.get(name).cloned().unwrap_or_default();
        Builtin {
            name,
            arguments: Some(arguments),
            particles: native
                .signature()
                .iter()
                .map(|parameter| parameter.role.particle())
                .collect(),
            description,
        }
    });
    let constants = stdlib::CONSTANT_DESCRIPTIONS
        .into_iter()
        .map(|(name, description)| Builtin {
            name,
            arguments: None,
            particles: Vec::new(),
            description: description.to_owned(),
        });
    verbs.chain(constants).collect()
}

fn documented() -> HashMap<&'static str, (usize, String, String)> {
    let mut found = HashMap::new();
    for line in include_str!("../docs/stdlib.md").lines() {
        let cells: Vec<&str> = line.trim_matches('|').split(" | ").map(str::trim).collect();
        let [names, arguments, result] = cells[..] else {
            continue;
        };
        if !line.starts_with("| ") || names.starts_with('`') || names == "동사" {
            continue;
        }
        for name in names.split(", ") {
            let order = found.len();
            found.insert(
                name,
                (order, arguments.replace("**", ""), result.to_owned()),
            );
        }
    }
    found
}

pub fn trailing_particle(word: &str) -> Option<&'static str> {
    Role::PARTICLES
        .iter()
        .find(|particle| word.len() > particle.len() && word.ends_with(*particle))
        .and_then(|particle| Role::from_particle(particle))
        .map(Role::particle)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormUse {
    Statement,
    Value,
    Pipe,
    Condition,
    Modifier,
}

impl FormUse {
    pub fn label(self) -> &'static str {
        match self {
            Self::Statement => "문장",
            Self::Value => "값",
            Self::Pipe => "전달",
            Self::Condition => "조건",
            Self::Modifier => "꾸밈",
        }
    }
}

pub fn verb_forms(lemma: &str, adjective: bool) -> Vec<(String, FormUse)> {
    let Some(stem) = lemma.strip_suffix('다') else {
        return Vec::new();
    };
    let endings: &[(Ending, &str, FormUse)] = if adjective {
        &[
            (Ending::Conditional, "", FormUse::Condition),
            (Ending::WhetherIs, "", FormUse::Value),
            (Ending::Past, "", FormUse::Modifier),
        ]
    } else {
        &[
            (Ending::Finite, "", FormUse::Statement),
            (Ending::Past, " 값", FormUse::Value),
            (Ending::Pipe, "", FormUse::Pipe),
            (Ending::Conditional, "", FormUse::Condition),
        ]
    };
    endings
        .iter()
        .filter_map(|&(ending, tail, usage)| {
            conjugate_any(stem, ending).map(|form| (format!("{form}{tail}"), usage))
        })
        .collect()
}
