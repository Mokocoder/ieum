// 으로/로는 ㄹ 받침을 모음처럼 다루고, 받침은 읽는 소리로 정합니다(1 일, 10 십, CSV 브이, URL 엘).

use super::hangul::{last, last_syllable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sound {
    Vowel,
    Rieul,
    Consonant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pair {
    consonant: &'static str,
    vowel: &'static str,
}

pub(crate) const SUBJECT: Pair = Pair::new("이", "가");
pub(crate) const OBJECT: Pair = Pair::new("을", "를");
pub(crate) const TOPIC: Pair = Pair::new("은", "는");
pub(crate) const WITH: Pair = Pair::new("과", "와");
pub(crate) const TOWARD: Pair = Pair::new("으로", "로");
const COPULA: [Pair; 3] = [
    Pair::new("이다", "다"),
    Pair::new("이면", "면"),
    Pair::new("이고", "고"),
];

impl Pair {
    const fn new(consonant: &'static str, vowel: &'static str) -> Self {
        Self { consonant, vowel }
    }

    fn fitting(self, sound: Sound) -> &'static str {
        match sound {
            Sound::Rieul if self == TOWARD => self.vowel,
            Sound::Rieul | Sound::Consonant => self.consonant,
            Sound::Vowel => self.vowel,
        }
    }

    pub fn after(self, word: &str) -> String {
        match sound(word) {
            Some(sound) => self.fitting(sound).to_owned(),
            None => format!("{}({})", self.consonant, self.vowel),
        }
    }
}

pub(crate) fn correction(word: &str, particle: &str) -> Option<&'static str> {
    let sound = sound(word)?;
    if let Some(copula) = COPULA.iter().find(|pair| pair.vowel == particle) {
        return (sound != Sound::Vowel).then_some(copula.consonant);
    }
    let pair = [SUBJECT, OBJECT, TOPIC, WITH, TOWARD]
        .into_iter()
        .find(|pair| pair.consonant == particle || pair.vowel == particle)?;
    let fitting = pair.fitting(sound);
    (fitting != particle).then_some(fitting)
}

fn sound(word: &str) -> Option<Sound> {
    let last_char = word.chars().next_back()?;
    if let Some(syllable) = last_syllable(word) {
        return Some(match syllable.last {
            last::NONE => Sound::Vowel,
            last::L => Sound::Rieul,
            _ => Sound::Consonant,
        });
    }
    let unsigned = word.trim_start_matches(['+', '-']);
    if unsigned.starts_with(|c: char| c.is_ascii_digit()) {
        return number_sound(unsigned);
    }
    let spelled = word.chars().count() == 1
        || word
            .chars()
            .all(|c| c.is_ascii_uppercase() && !"AEIOU".contains(c));
    if last_char.is_ascii_alphabetic() && spelled {
        return Some(match last_char.to_ascii_uppercase() {
            'L' | 'R' => Sound::Rieul,
            'M' | 'N' => Sound::Consonant,
            _ => Sound::Vowel,
        });
    }
    None
}

fn number_sound(word: &str) -> Option<Sound> {
    let digits: String = word.chars().filter(|c| *c != '_').collect();
    if digits.starts_with("0x") || digits.starts_with("0o") || digits.starts_with("0b") {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let digit = |digit: char| match digit {
        '1' | '7' | '8' => Sound::Rieul,     // 일, 칠, 팔
        '0' | '3' | '6' => Sound::Consonant, // 영, 삼, 육
        _ => Sound::Vowel,                   // 이, 사, 오, 구
    };
    let zeros = digits.len() - digits.trim_end_matches('0').len();
    if digits.contains('.') || zeros == 0 || zeros == digits.len() {
        return digits.chars().next_back().map(digit);
    }
    Some(if zeros % 4 != 0 || zeros / 4 != 3 {
        Sound::Consonant
    } else {
        Sound::Vowel
    })
}
