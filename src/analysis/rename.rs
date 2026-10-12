use std::path::Path;

use super::{Analysis, Symbol, SymbolKind, analyze};
use crate::korean::particle;
use crate::korean::verb::{self, Ending};
use crate::syntax::lexer;
use crate::{Fix, SourceFiles, Span};

pub fn can_rename(symbol: &Symbol) -> bool {
    symbol.imported_from.is_none()
        && !symbol
            .lemmas
            .iter()
            .any(|name| name.chars().any(char::is_whitespace))
}

pub fn rename(
    path: Option<&Path>,
    source: &str,
    analysis: &Analysis,
    sources: &SourceFiles,
    definition: Span,
    new_name: &str,
) -> Result<Vec<Fix>, String> {
    let symbol = analysis
        .symbol(definition)
        .ok_or("정의를 찾지 못했습니다.")?;
    if !can_rename(symbol) {
        return Err(
            "가져온 이름이나 고정 낱말이 들어간 함수는 여기서 바꿀 수 없습니다.".to_owned(),
        );
    }
    let new_name = lexer::normalize(new_name.trim().trim_matches('`'));
    if !lexer::valid_name(&new_name) {
        return Err("새 이름은 공백이나 기호 없이 이름 규칙에 맞게 쓰세요.".to_owned());
    }
    let function = symbol.kind == SymbolKind::Function;
    if function
        && (!new_name.ends_with('다')
            || matches!(new_name.as_str(), "호출하다" | "반환하다" | "바꾸다"))
    {
        return Err("새 함수 이름은 '키우다'처럼 정의할 수 있는 기본형으로 쓰세요.".to_owned());
    }
    let mut spans = vec![symbol.span];
    spans.extend(
        analysis
            .references
            .iter()
            .filter(|reference| reference.definition == Some(definition))
            .map(|reference| reference.span),
    );
    spans.sort_by_key(|span| span.start);
    spans.dedup();
    let mut edits = Vec::new();
    for span in spans {
        let raw = source
            .get(span.start..span.end)
            .ok_or("이름의 위치가 올바르지 않습니다.")?;
        let end = span.start + raw.find(char::is_whitespace).unwrap_or(raw.len());
        let word = &source[span.start..end];
        let quoted = word.starts_with('`') && word.ends_with('`');
        let written = word.trim_matches('`');
        let replacement = if function {
            reconjugate(written, &symbol.lemmas, &new_name, &source[end..])
                .ok_or_else(|| format!("'{written}'의 활용을 새 이름으로 옮길 수 없습니다."))?
        } else {
            new_name.clone()
        };
        let replacement = if quoted || lexer::literal(&replacement).is_some() {
            format!("`{replacement}`")
        } else {
            replacement
        };
        let tail = &source[end..];
        let length = tail
            .char_indices()
            .take_while(|(_, c)| unicode_ident::is_xid_continue(*c))
            .last()
            .map_or(0, |(i, c)| i + c.len_utf8());
        let suffix = &tail[..length];
        let corrected = particle::correction(&new_name, suffix);
        let (end, replacement) = match corrected {
            Some(particle) => (end + length, format!("{replacement}{particle}")),
            None => (end, replacement),
        };
        edits.push(Fix {
            span: Span::new(span.start, end),
            text: replacement.into(),
        });
    }
    if edits
        .windows(2)
        .any(|pair| pair[0].span.end > pair[1].span.start)
    {
        return Err("이름의 사용 위치가 겹쳐 안전하게 바꿀 수 없습니다.".to_owned());
    }
    let mut updated = source.to_owned();
    for edit in edits.iter().rev() {
        updated.replace_range(edit.span.start..edit.span.end, &edit.text);
    }
    let parsed = crate::syntax::parse(updated.as_bytes()).map_err(|error| error.to_string())?;
    if parsed.tree.has_errors() {
        return Err("이 이름으로 바꾸면 문법이 맞지 않습니다.".to_owned());
    }
    let moved = |offset: usize| {
        let shift: isize = edits
            .iter()
            .take_while(|edit| edit.span.end <= offset)
            .map(|edit| edit.text.len() as isize - (edit.span.end - edit.span.start) as isize)
            .sum();
        offset.saturating_add_signed(shift)
    };
    let updated = analyze(path, updated.as_bytes(), sources);
    let selected = updated
        .symbols
        .iter()
        .find(|symbol| symbol.span.start == moved(definition.start))
        .ok_or("새 이름의 정의를 확인하지 못했습니다.")?;
    if updated.symbols.iter().any(|other| {
        other.span != selected.span
            && other.scope == selected.scope
            && (other.name == selected.name
                || other
                    .lemmas
                    .iter()
                    .any(|name| selected.lemmas.contains(name)))
    }) {
        return Err("같은 범위에 이미 있는 이름과 겹칩니다.".to_owned());
    }
    for reference in &analysis.references {
        let other = updated
            .references
            .iter()
            .find(|other| other.span.start == moved(reference.span.start));
        let same = other.is_some_and(|other| match (reference.definition, other.definition) {
            (Some(old), Some(new)) => moved(old.start) == new.start,
            (None, None) => reference.name == other.name,
            _ => false,
        });
        if !same {
            return Err(
                "이 이름으로 바꾸면 일부 사용 위치가 다른 이름을 가리키거나 읽히지 않습니다."
                    .to_owned(),
            );
        }
    }
    Ok(edits)
}

fn reconjugate(written: &str, old: &[String], new: &str, tail: &str) -> Option<String> {
    if old.iter().any(|lemma| lemma == written) {
        return Some(new.to_owned());
    }
    let endings: &[Ending] = if tail.trim_start().starts_with("법") {
        &[Ending::Present]
    } else if tail.trim_start().starts_with("값") {
        &[Ending::Past]
    } else {
        &[
            Ending::Finite,
            Ending::Sequence,
            Ending::Or,
            Ending::Pipe,
            Ending::Present,
            Ending::Past,
            Ending::Conditional,
            Ending::WhetherIs,
            Ending::WhetherDoes,
            Ending::Negative,
        ]
    };
    let mut forms = Vec::new();
    for &ending in endings {
        if verb::analyze(written, ending)
            .iter()
            .any(|candidate| old.contains(&candidate.lemma()))
        {
            let form = verb::conjugate_any(new.strip_suffix('다')?, ending)?;
            if !forms.contains(&form) {
                forms.push(form);
            }
        }
    }
    (forms.len() == 1).then(|| forms.remove(0))
}
