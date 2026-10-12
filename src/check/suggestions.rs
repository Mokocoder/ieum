use crate::korean::hangul::jamo;

const MAX_CANDIDATES: usize = 750;
const MAX_UNITS: usize = 40;
const EDIT_COST: usize = 2;
const CASE_COST: usize = 1;

pub(crate) fn closest<'a>(word: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut known: Vec<&str> = known.into_iter().collect();
    if known.len() > MAX_CANDIDATES {
        return None;
    }
    known.sort_unstable();
    known.dedup();
    let word_units = jamo(word);
    let mut best: Option<(&str, usize)> = None;
    for candidate in known {
        if candidate == word {
            continue;
        }
        let units = jamo(candidate);
        let mut limit = (word_units.len() + units.len() + 3) * EDIT_COST / 6;
        if let Some((_, distance)) = best {
            limit = limit.min(distance - 1);
        }
        if let Some(distance) = distance_within(&word_units, &units, limit) {
            best = Some((candidate, distance));
        }
    }
    best.map(|(word, _)| word)
}

fn replace_cost(a: u32, b: u32) -> usize {
    let same_letter = || {
        char::from_u32(a)
            .zip(char::from_u32(b))
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(&b))
    };
    if a == b {
        0
    } else if same_letter() {
        CASE_COST
    } else {
        EDIT_COST
    }
}

fn distance_within(a: &[u32], b: &[u32], limit: usize) -> Option<usize> {
    let common_prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[common_prefix..], &b[common_prefix..]);
    let common_suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[..a.len() - common_suffix], &b[..b.len() - common_suffix]);
    if a.len().abs_diff(b.len()) * EDIT_COST > limit {
        return None;
    }
    if a.is_empty() || b.is_empty() {
        return Some((a.len() + b.len()) * EDIT_COST);
    }
    if a.len() > MAX_UNITS || b.len() > MAX_UNITS {
        return None;
    }
    let mut row: Vec<usize> = (0..=b.len()).map(|j| j * EDIT_COST).collect();
    for (i, &x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = (i + 1) * EDIT_COST;
        for (j, &y) in b.iter().enumerate() {
            let replaced = diagonal + replace_cost(x, y);
            let edited = row[j].min(row[j + 1]) + EDIT_COST;
            diagonal = row[j + 1];
            row[j + 1] = replaced.min(edited);
        }
        if row.iter().min().is_some_and(|&least| least > limit) {
            return None;
        }
    }
    Some(row[b.len()]).filter(|&distance| distance <= limit)
}
