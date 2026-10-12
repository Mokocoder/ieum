use super::hangul::{Syllable, first, join, last, split_last, vowel};
use super::irregular;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ending {
    // 평서형 -ㄴ다/-는다: `읽는다`, `간다`
    Finite,
    // 연결어미 -고: `읽고`, `작고`
    Sequence,
    // 연결어미 -거나: `읽거나`, `작거나`
    Or,
    // 연결어미 -아서/-어서: `읽어서`, `봐서`
    Pipe,
    // 현재 관형사형 -는: `읽는 법`, `시작하는 이름`
    Present,
    // 관형사형 -(으)ㄴ: 동사의 과거(`읽은 값`), 형용사의 현재(`작은 수`)
    Past,
    // 연결어미 -(으)면: `읽으면`, `가면`
    Conditional,
    // 형용사의 명사절 어미 -(으)ㄴ지: `작은지`, `부족한지`
    WhetherIs,
    // 동사의 명사절 어미 -는지: `읽는지`, `나누어떨어지는지`
    WhetherDoes,
    // -지 않다의 -지: `읽지 않으면`, `작지 않은`
    Negative,
}

#[cfg(test)]
impl Ending {
    pub const ALL: [Self; 6] = [
        Self::Finite,
        Self::Sequence,
        Self::Pipe,
        Self::Present,
        Self::Past,
        Self::Conditional,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Class {
    Regular,
    // ㄷ 불규칙(듣다는 들어서, 들으면)
    D,
    // ㅂ 불규칙(돕다는 도와서, 굽다는 구워서)
    B,
    // ㅅ 불규칙(잇다는 이어서, 짓다는 지으면)
    S,
    // 르 불규칙(고르다는 골라서, 부르다는 불러서)
    Reu,
    // 우 불규칙(푸다는 퍼서)
    U,
    // 러 불규칙(이르다는 이르러서)
    Reo,
    // ㅎ 불규칙, 형용사만(파랗다는 파란, 파라면)
    H,
}

impl Class {
    const ALL: [Self; 8] = [
        Self::Regular,
        Self::D,
        Self::B,
        Self::S,
        Self::Reu,
        Self::U,
        Self::Reo,
        Self::H,
    ];

    fn fits(self, stem: &str, end: Syllable) -> bool {
        match self {
            Self::Regular => irregular::is_regular(stem),
            Self::D => end.last == last::D && irregular::is_d(stem),
            Self::B => end.last == last::B && irregular::is_b(stem),
            Self::S => end.last == last::S && irregular::is_s(stem),
            Self::Reu => irregular::is_reu(stem),
            Self::U => end.char() == '푸',
            Self::H => irregular::is_h(stem),
            Self::Reo => irregular::is_reo(stem),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Analysis {
    pub stem: String,
    pub class: Class,
}

impl Analysis {
    pub fn lemma(&self) -> String {
        format!("{}다", self.stem)
    }
}

// 있다, 없다와 그 합성어(들어있다)는 존재사로 활용합니다: 있다(있는다 아님), 있는(있은 아님).
pub(crate) fn existential(stem: &str) -> bool {
    stem.ends_with('있') || stem.ends_with('없')
}

// 규칙과 불규칙 활용이 다 맞는 어간은 불규칙 활용입니다.
pub(crate) fn conjugate_any(stem: &str, ending: Ending) -> Option<String> {
    Class::ALL
        .into_iter()
        .rev()
        .find_map(|class| conjugate(stem, class, ending))
}

pub(crate) fn conjugate(stem: &str, class: Class, ending: Ending) -> Option<String> {
    let (head, end) = split_last(stem)?;
    if !class.fits(stem, end) {
        return None;
    }
    if class == Class::H
        && !matches!(
            ending,
            Ending::Past
                | Ending::WhetherIs
                | Ending::Conditional
                | Ending::Sequence
                | Ending::Or
                | Ending::Negative
        )
    {
        return None;
    }
    Some(match ending {
        Ending::WhetherIs => format!("{}지", conjugate(stem, class, Ending::Past)?),
        Ending::WhetherDoes => format!("{}지", conjugate(stem, class, Ending::Present)?),
        Ending::Or => format!("{stem}거나"),
        Ending::Negative => format!("{stem}지"),
        Ending::Finite if existential(stem) => format!("{stem}다"),
        Ending::Finite => match end.last {
            last::NONE | last::L => format!("{}다", join(head, end.with_last(last::N))),
            _ => format!("{stem}는다"),
        },
        Ending::Sequence => format!("{stem}고"),
        Ending::Present if end.last == last::L => {
            format!("{}는", join(head, end.with_last(last::NONE)))
        }
        Ending::Present => format!("{stem}는"),
        Ending::Past if existential(stem) => format!("{stem}는"),
        Ending::Past => with_vowel_ending(head, end, class, |base| match base {
            Base::Open(head, end) => join(head, end.with_last(last::N)),
            Base::Closed(stem) => format!("{stem}은"),
            Base::Rounded(stem) => format!("{stem}운"),
        }),
        Ending::Conditional => with_vowel_ending(head, end, class, |base| match base {
            Base::Open(head, end) => format!("{}면", join(head, end)),
            Base::Closed(stem) => format!("{stem}으면"),
            Base::Rounded(stem) => format!("{stem}우면"),
        }),
        Ending::Pipe => format!("{}서", infinitive(head, end, class)?),
    })
}

fn spellings(stem: &str, class: Class, ending: Ending) -> Vec<String> {
    let Some(usual) = conjugate(stem, class, ending) else {
        return Vec::new();
    };
    let mut all = vec![usual];
    if let (Ending::Pipe, Class::Regular, Some((head, end))) = (ending, class, split_last(stem)) {
        let full = if end.first == first::H && end.vowel == vowel::A && end.last == last::NONE {
            Some(format!("{stem}여서"))
        } else if end.last == last::NONE
            && matches!(
                end.vowel,
                vowel::O | vowel::U | vowel::I | vowel::OE | vowel::AE | vowel::E
            )
        {
            let suffix = if end.is_bright() { "아서" } else { "어서" };
            Some(format!("{}{suffix}", join(head, end)))
        } else {
            None
        };
        all.extend(full.filter(|full| *full != all[0]));
    }
    all
}

enum Base<'a> {
    Open(&'a str, Syllable),
    Closed(String),
    Rounded(String),
}

fn with_vowel_ending(
    head: &str,
    end: Syllable,
    class: Class,
    attach: impl FnOnce(Base) -> String,
) -> String {
    match class {
        Class::D => attach(Base::Closed(join(head, end.with_last(last::L)))),
        Class::S => attach(Base::Closed(join(head, end.with_last(last::NONE)))),
        Class::B => attach(Base::Rounded(join(head, end.with_last(last::NONE)))),
        Class::H => attach(Base::Open(head, end.with_last(last::NONE))),
        Class::Regular | Class::Reu | Class::U | Class::Reo => match end.last {
            last::NONE | last::L => attach(Base::Open(head, end)),
            _ => attach(Base::Closed(join(head, end))),
        },
    }
}

fn infinitive(head: &str, end: Syllable, class: Class) -> Option<String> {
    let suffix = |s: Syllable| if s.is_bright() { "아" } else { "어" };
    Some(match class {
        Class::D => format!("{}{}", join(head, end.with_last(last::L)), suffix(end)),
        Class::S => format!("{}{}", join(head, end.with_last(last::NONE)), suffix(end)),
        Class::B => {
            let rounded = if end.vowel == vowel::O { "와" } else { "워" };
            format!("{}{rounded}", join(head, end.with_last(last::NONE)))
        }
        Class::Reu => {
            let (before, previous) = split_last(head)?;
            let ending = if previous.is_bright() { "라" } else { "러" };
            format!("{}{ending}", join(before, previous.with_last(last::L)))
        }
        Class::U => join(head, end.with_vowel(vowel::EO)),
        Class::H => return None,
        Class::Reo => format!("{}러", join(head, end)),
        Class::Regular if end.last != last::NONE => {
            format!("{}{}", join(head, end), suffix(end))
        }
        Class::Regular if end.first == first::H && end.vowel == vowel::A => {
            join(head, end.with_vowel(vowel::AE))
        }
        Class::Regular => {
            let contracted = match end.vowel {
                vowel::A | vowel::AE | vowel::EO | vowel::E | vowel::YEO => end,
                vowel::O => end.with_vowel(vowel::WA),
                vowel::U => end.with_vowel(vowel::WEO),
                vowel::I => end.with_vowel(vowel::YEO),
                vowel::OE => end.with_vowel(vowel::WAE),
                vowel::EU => {
                    let bright = split_last(head).is_some_and(|(_, s)| s.is_bright());
                    end.with_vowel(if bright { vowel::A } else { vowel::EO })
                }
                _ => return Some(format!("{}어", join(head, end))),
            };
            join(head, contracted)
        }
    })
}

// -하다, -되다는 파생 접미사라 할, 될, 해, 돼, 하여, 핳으로 끝나는 어간이 없고 느로 끝나는 어간도 없습니다.
pub(crate) fn analyze(word: &str, ending: Ending) -> Vec<Analysis> {
    let mut found: Vec<Analysis> = Vec::new();
    for stem in stem_guesses(word, ending) {
        if stem.ends_with(['할', '될', '해', '돼', '핳', '느']) || stem.ends_with("하여") {
            continue;
        }
        for class in Class::ALL {
            if spellings(&stem, class, ending)
                .iter()
                .any(|form| form == word)
                && !found.iter().any(|a| a.stem == stem && a.class == class)
            {
                found.push(Analysis {
                    stem: stem.clone(),
                    class,
                });
            }
        }
    }
    found
}

fn stem_guesses(word: &str, ending: Ending) -> Vec<String> {
    let mut guesses = Vec::new();
    let marker = match ending {
        Ending::Finite => "다",
        Ending::Sequence => "고",
        Ending::Pipe => "서",
        Ending::Present => "는",
        Ending::WhetherDoes => "는지",
        Ending::WhetherIs => "지",
        Ending::Or => "거나",
        Ending::Negative => "지",
        Ending::Past | Ending::Conditional => "",
    };
    let Some(body) = word.strip_suffix(marker) else {
        return guesses;
    };
    match ending {
        Ending::Finite => {
            if let Some(stem) = body.strip_suffix('는') {
                guesses.push(stem.to_owned());
            }
            guesses.push(body.to_owned());
            vary_last(body, &[last::NONE, last::L], &mut guesses);
        }
        Ending::Sequence | Ending::Or | Ending::Negative => guesses.push(body.to_owned()),
        Ending::Present | Ending::WhetherDoes => {
            guesses.push(body.to_owned());
            vary_last(body, &[last::L], &mut guesses);
        }
        Ending::Past | Ending::Conditional | Ending::WhetherIs => {
            for suffix in ["으면", "우면", "면", "은", "운", "는"] {
                if let Some(base) = body.strip_suffix(suffix) {
                    guesses.push(base.to_owned());
                    let finals = [last::D, last::B, last::S, last::L, last::H];
                    vary_last(base, &finals, &mut guesses);
                }
            }
            if let Some((head, end)) = split_last(body).filter(|(_, e)| e.last == last::N) {
                for stem_final in [last::NONE, last::L, last::H] {
                    guesses.push(join(head, end.with_last(stem_final)));
                }
            }
        }
        Ending::Pipe => {
            guesses.push(body.to_owned());
            if let Some((head, end)) = split_last(body) {
                if end.first == first::NG && end.last == last::NONE {
                    guesses.push(head.to_owned());
                    vary_last(head, &[last::D, last::B, last::S, last::L], &mut guesses);
                }
                for original in [vowel::O, vowel::U, vowel::I, vowel::OE, vowel::A, vowel::EU] {
                    guesses.push(join(head, end.with_vowel(original)));
                }
                if end.char() == '러' {
                    guesses.push(head.to_owned());
                }
                if end.first == first::L {
                    if let Some((before, previous)) = split_last(head) {
                        let previous = previous.with_last(last::NONE);
                        guesses.push(format!("{}르", join(before, previous)));
                    }
                }
            }
        }
    }
    guesses
}

fn vary_last(text: &str, finals: &[u32], guesses: &mut Vec<String>) {
    if let Some((head, end)) = split_last(text) {
        for &final_consonant in finals {
            guesses.push(join(head, end.with_last(final_consonant)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rustfmt::skip]
    const VERBS: &[(&str, Class, [&str; 6])] = &[
        ("읽다", Class::Regular, ["읽는다", "읽고", "읽어서", "읽는", "읽은", "읽으면"]),
        ("받다", Class::Regular, ["받는다", "받고", "받아서", "받는", "받은", "받으면"]),
        ("가다", Class::Regular, ["간다", "가고", "가서", "가는", "간", "가면"]),
        ("보다", Class::Regular, ["본다", "보고", "봐서", "보는", "본", "보면"]),
        ("주다", Class::Regular, ["준다", "주고", "줘서", "주는", "준", "주면"]),
        ("보내다", Class::Regular, ["보낸다", "보내고", "보내서", "보내는", "보낸", "보내면"]),
        ("붙이다", Class::Regular, ["붙인다", "붙이고", "붙여서", "붙이는", "붙인", "붙이면"]),
        ("되다", Class::Regular, ["된다", "되고", "돼서", "되는", "된", "되면"]),
        ("출력하다", Class::Regular, ["출력한다", "출력하고", "출력해서", "출력하는", "출력한", "출력하면"]),
        ("쓰다", Class::Regular, ["쓴다", "쓰고", "써서", "쓰는", "쓴", "쓰면"]),
        ("모으다", Class::Regular, ["모은다", "모으고", "모아서", "모으는", "모은", "모으면"]),
        ("따르다", Class::Regular, ["따른다", "따르고", "따라서", "따르는", "따른", "따르면"]),
        ("만들다", Class::Regular, ["만든다", "만들고", "만들어서", "만드는", "만든", "만들면"]),
        ("열다", Class::Regular, ["연다", "열고", "열어서", "여는", "연", "열면"]),
        ("있다", Class::Regular, ["있다", "있고", "있어서", "있는", "있는", "있으면"]),
        ("닫다", Class::Regular, ["닫는다", "닫고", "닫아서", "닫는", "닫은", "닫으면"]),
        ("듣다", Class::D, ["듣는다", "듣고", "들어서", "듣는", "들은", "들으면"]),
        ("걷다", Class::D, ["걷는다", "걷고", "걸어서", "걷는", "걸은", "걸으면"]),
        ("입다", Class::Regular, ["입는다", "입고", "입어서", "입는", "입은", "입으면"]),
        ("돕다", Class::B, ["돕는다", "돕고", "도와서", "돕는", "도운", "도우면"]),
        ("굽다", Class::B, ["굽는다", "굽고", "구워서", "굽는", "구운", "구우면"]),
        ("씻다", Class::Regular, ["씻는다", "씻고", "씻어서", "씻는", "씻은", "씻으면"]),
        ("잇다", Class::S, ["잇는다", "잇고", "이어서", "잇는", "이은", "이으면"]),
        ("짓다", Class::S, ["짓는다", "짓고", "지어서", "짓는", "지은", "지으면"]),
        ("고르다", Class::Reu, ["고른다", "고르고", "골라서", "고르는", "고른", "고르면"]),
        ("부르다", Class::Reu, ["부른다", "부르고", "불러서", "부르는", "부른", "부르면"]),
        ("푸다", Class::U, ["푼다", "푸고", "퍼서", "푸는", "푼", "푸면"]),
        ("이르다", Class::Reo, ["이른다", "이르고", "이르러서", "이르는", "이른", "이르면"]),
    ];

    #[rustfmt::skip]
    const ADJECTIVES: &[(&str, Class, [&str; 3])] = &[
        ("작다", Class::Regular, ["작은", "작은지", "작으면"]),
        ("부족하다", Class::Regular, ["부족한", "부족한지", "부족하면"]),
        ("길다", Class::Regular, ["긴", "긴지", "길면"]),
        ("쉽다", Class::B, ["쉬운", "쉬운지", "쉬우면"]),
        ("파랗다", Class::H, ["파란", "파란지", "파라면"]),
        ("비어있다", Class::Regular, ["비어있는", "비어있는지", "비어있으면"]),
    ];

    fn round_trip(base: &str, class: Class, ending: Ending, form: &str) {
        let stem = base.strip_suffix('다').unwrap();
        assert_eq!(
            conjugate(stem, class, ending).as_deref(),
            Some(form),
            "{base} {ending:?}"
        );
        let found = analyze(form, ending);
        assert!(
            found.iter().any(|a| a.stem == stem && a.class == class),
            "{form} {found:?}"
        );
    }

    #[test]
    fn every_form_conjugates_and_analyzes_back() {
        for (base, class, forms) in VERBS {
            for (ending, form) in Ending::ALL.iter().zip(forms) {
                round_trip(base, *class, *ending, form);
            }
        }
        for (base, class, forms) in ADJECTIVES {
            let endings = [Ending::Past, Ending::WhetherIs, Ending::Conditional];
            for (ending, form) in endings.iter().zip(forms) {
                round_trip(base, *class, *ending, form);
            }
        }
        round_trip("만들다", Class::Regular, Ending::Or, "만들거나");
        round_trip("만들다", Class::Regular, Ending::WhetherDoes, "만드는지");
        assert_eq!(conjugate("파랗", Class::H, Ending::Finite), None);
    }

    #[test]
    fn analysis_keeps_every_standard_reading_and_no_other() {
        let stems = |word, ending| -> Vec<String> {
            analyze(word, ending).into_iter().map(|a| a.stem).collect()
        };
        for (form, stem) in [
            ("되어서", "되"),
            ("보아서", "보"),
            ("하여서", "하"),
            ("걷어서", "걷"),
        ] {
            assert!(
                stems(form, Ending::Pipe).contains(&stem.to_owned()),
                "{form}"
            );
        }
        assert_eq!(stems("사는", Ending::Present), ["사", "살"]);
        let found = stems("들어서", Ending::Pipe);
        assert!(found.contains(&"듣".into()) && found.contains(&"들".into()));
        for (form, stem) in [("달아서", "닫"), ("고라서", "고르"), ("잇어서", "잇")] {
            assert!(
                !stems(form, Ending::Pipe).contains(&stem.to_owned()),
                "{form}"
            );
        }
        assert!(stems("창고", Ending::Finite).is_empty());
    }
}
