use super::ast::{Role, WordClass};
use super::lexer::valid_name;
use crate::korean::hangul::{last, last_syllable};
use crate::korean::verb::{Analysis, Class, Ending, analyze};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    Finite,
    Sequence,
    Or,
    Pipe,
    Definition,
    Nominal,
    Conditional,
    Descriptive,
    WhetherIs,
    WhetherDoes,
    Negative,
}

impl Form {
    fn ending(self) -> Ending {
        match self {
            Self::Finite => Ending::Finite,
            Self::Sequence => Ending::Sequence,
            Self::Pipe => Ending::Pipe,
            Self::Definition => Ending::Present,
            Self::Nominal => Ending::Past,
            Self::Conditional => Ending::Conditional,
            Self::Descriptive => Ending::Past,
            Self::WhetherIs => Ending::WhetherIs,
            Self::WhetherDoes => Ending::WhetherDoes,
            Self::Or => Ending::Or,
            Self::Negative => Ending::Negative,
        }
    }

    pub(crate) fn class(self) -> Option<WordClass> {
        match self {
            Self::Conditional | Self::Sequence | Self::Or | Self::Negative => None,
            Self::Descriptive | Self::WhetherIs => Some(WordClass::Adjective),
            _ => Some(WordClass::Verb),
        }
    }
}

pub(crate) fn lemmas(word: &str, form: Form) -> Vec<String> {
    let mut found: Vec<Analysis> = analyze(word, form.ending())
        .into_iter()
        .filter(|analysis| valid_name(&analysis.stem))
        .collect();
    found.sort_by_key(|analysis| {
        let rieul = last_syllable(&analysis.stem).is_some_and(|s| s.last == last::L);
        // 가장 긴 어미가 먼저입니다. 읽어서는 읽어 + 서보다 읽 + 어서가 앞섭니다.
        let length = analysis.stem.chars().count();
        (analysis.class != Class::Regular, length, rieul)
    });
    let mut lemmas: Vec<String> = Vec::new();
    for analysis in found {
        let lemma = analysis.lemma();
        if !lemmas.contains(&lemma) {
            lemmas.push(lemma);
        }
    }
    lemmas
}

pub(crate) fn is_form(word: &str, form: Form) -> bool {
    !lemmas(word, form).is_empty()
}

// -는지를 형용사로 읽으려면 ㄹ이 탈락해야 하므로 동사가 먼저입니다(나누어떨어지늘다가 아니라 나누어떨어지다).
pub(crate) fn whether_lemmas(word: &str) -> (Vec<String>, Option<WordClass>) {
    let mut all = lemmas(word, Form::WhetherDoes);
    let is = lemmas(word, Form::WhetherIs);
    let class = match (is.is_empty(), all.is_empty()) {
        (false, false) => None,
        (true, _) => Some(WordClass::Verb),
        (_, true) => Some(WordClass::Adjective),
    };
    for lemma in is {
        if !all.contains(&lemma) {
            all.push(lemma);
        }
    }
    (all, class)
}

pub(crate) fn ends_with_particle(word: &str) -> bool {
    Role::PARTICLES
        .iter()
        .any(|particle| word.strip_suffix(particle).is_some_and(valid_name))
}

pub(crate) fn is_connection(word: &str) -> bool {
    !ends_with_particle(word) && (is_form(word, Form::Sequence) || is_form(word, Form::Pipe))
}
