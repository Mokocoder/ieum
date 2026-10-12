const BASE: u32 = 0xac00;
const COUNT: u32 = 11_172;

pub(crate) mod vowel {
    pub const A: u32 = 0; // ㅏ
    pub const AE: u32 = 1; // ㅐ
    pub const YA: u32 = 2; // ㅑ
    pub const EO: u32 = 4; // ㅓ
    pub const E: u32 = 5; // ㅔ
    pub const YEO: u32 = 6; // ㅕ
    pub const O: u32 = 8; // ㅗ
    pub const WA: u32 = 9; // ㅘ
    pub const WAE: u32 = 10; // ㅙ
    pub const OE: u32 = 11; // ㅚ
    pub const U: u32 = 13; // ㅜ
    pub const WEO: u32 = 14; // ㅝ
    pub const EU: u32 = 18; // ㅡ
    pub const I: u32 = 20; // ㅣ
}

pub(crate) mod last {
    pub const NONE: u32 = 0;
    pub const N: u32 = 4; // ㄴ
    pub const D: u32 = 7; // ㄷ
    pub const L: u32 = 8; // ㄹ
    pub const B: u32 = 17; // ㅂ
    pub const S: u32 = 19; // ㅅ
    pub const H: u32 = 27; // ㅎ
}

pub(crate) mod first {
    pub const L: u32 = 5; // ㄹ
    pub const NG: u32 = 11; // ㅇ (소리 없음)
    pub const H: u32 = 18; // ㅎ
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Syllable {
    pub first: u32,
    pub vowel: u32,
    pub last: u32,
}

impl Syllable {
    pub fn of(c: char) -> Option<Self> {
        let index = (c as u32).checked_sub(BASE).filter(|&i| i < COUNT)?;
        Some(Self {
            first: index / 588,
            vowel: index % 588 / 28,
            last: index % 28,
        })
    }

    pub fn char(self) -> char {
        char::from_u32(BASE + (self.first * 21 + self.vowel) * 28 + self.last)
            .expect("자모 번호는 범위 안에 있습니다")
    }

    pub fn with_last(self, last: u32) -> Self {
        Self { last, ..self }
    }

    pub fn with_vowel(self, vowel: u32) -> Self {
        Self { vowel, ..self }
    }

    // ㅏ, ㅑ, ㅗ는 양성 어미 아/았을 받고, 나머지는 어를 받습니다.
    pub fn is_bright(self) -> bool {
        matches!(self.vowel, vowel::A | vowel::O | vowel::YA)
    }
}

pub(crate) fn last_syllable(word: &str) -> Option<Syllable> {
    word.chars().next_back().and_then(Syllable::of)
}

pub(crate) fn split_last(text: &str) -> Option<(&str, Syllable)> {
    let last = text.chars().next_back()?;
    let head = &text[..text.len() - last.len_utf8()];
    Some((head, Syllable::of(last)?))
}

pub(crate) fn join(head: &str, syllable: Syllable) -> String {
    let mut text = String::with_capacity(head.len() + 3);
    text.push_str(head);
    text.push(syllable.char());
    text
}

pub(crate) fn jamo(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for c in text.chars() {
        match Syllable::of(c) {
            Some(s) => {
                out.extend([0x1_0000 + s.first, 0x2_0000 + s.vowel]);
                if s.last != last::NONE {
                    out.push(0x3_0000 + s.last);
                }
            }
            None => out.push(c as u32),
        }
    }
    out
}
