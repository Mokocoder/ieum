// 합성어는 마지막 동사를 따르고, 동음이의어가 있는 어간(걷다, 묻다, 굽다)은 두 활용 모두 표준입니다.

use super::hangul::{last, last_syllable};

const D: &[&str] = &["긷", "깨닫", "눋", "듣", "붇", "싣", "일컫", "겯"];
const D_AND_REGULAR: &[&str] = &["걷", "묻"];

const S: &[&str] = &["잇", "짓", "낫", "붓", "젓", "긋", "잣"];

const B: &[&str] = &[
    "돕", "눕", "줍", "깁", "여쭙", "롭", "럽", "답", "겁", "갑", "깝", "엽", "쉽", "덥", "춥",
    "맵", "곱", "밉", "렵", "껍", "볍", "섭", "랍", "맙", "립", "습", "겹",
];
const B_AND_REGULAR: &[&str] = &["굽"];
const REGULAR_B: &[&str] = &["수줍"];

const REGULAR_REU: &[&str] = &["따르", "치르", "들르", "다다르", "우러르"];
const REO: &[&str] = &["푸르", "노르"];
const REO_AND_REU: &[&str] = &["이르", "누르"];

const REGULAR_H: &[&str] = &["좋", "놓", "낳", "넣", "닿", "쌓", "찧", "땋", "빻", "않"];

fn listed(stem: &str, list: &[&str]) -> bool {
    list.iter().any(|entry| stem.ends_with(entry))
}

pub(super) fn is_d(stem: &str) -> bool {
    listed(stem, D) || listed(stem, D_AND_REGULAR)
}

pub(super) fn is_s(stem: &str) -> bool {
    listed(stem, S)
}

pub(super) fn is_b(stem: &str) -> bool {
    (listed(stem, B) || listed(stem, B_AND_REGULAR)) && !listed(stem, REGULAR_B)
}

pub(super) fn is_reu(stem: &str) -> bool {
    stem.ends_with('르')
        && stem.chars().count() > 1
        && !listed(stem, REGULAR_REU)
        && !listed(stem, REO)
}

pub(super) fn is_reo(stem: &str) -> bool {
    listed(stem, REO) || listed(stem, REO_AND_REU)
}

pub(super) fn is_h(stem: &str) -> bool {
    last_syllable(stem).is_some_and(|s| s.last == last::H) && !listed(stem, REGULAR_H)
}

pub(super) fn is_regular(stem: &str) -> bool {
    let only_d = listed(stem, D);
    let only_b = is_b(stem) && !listed(stem, B_AND_REGULAR);
    !(only_d
        || only_b
        || is_s(stem)
        || is_reu(stem)
        || is_reo(stem)
        || is_h(stem)
        || stem.ends_with('푸'))
}
