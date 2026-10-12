use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct Random(u64);

impl Random {
    pub(crate) fn new(seed: Option<u64>) -> Self {
        Self(seed.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos() as u64)
        }))
    }

    pub(crate) fn split(&mut self) -> u64 {
        self.next()
    }

    pub(crate) fn for_item(seed: u64, index: usize) -> Self {
        let mut mixed = Self(seed ^ (index as u64).wrapping_mul(0xd1b5_4a32_d192_ed03));
        Self(mixed.next())
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub(crate) fn below(&mut self, count: usize) -> usize {
        let count = count as u64;
        let limit = u64::MAX - u64::MAX % count;
        loop {
            let value = self.next();
            if value < limit {
                return (value % count) as usize;
            }
        }
    }
}
