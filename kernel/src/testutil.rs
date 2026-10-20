//! What the tests of the crate share: a seeded generator, so that a random graph is the same on every platform and every run.

/// xorshift64*: small, the same everywhere, and good enough to draw graphs from.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A seed of 0 would stay at 0 for ever.
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n` (`n` above 0).
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0);
        (self.next_u64() >> 11) as usize % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_draws_the_same_numbers_and_another_seed_others() {
        let draw = |seed| {
            let mut rng = Rng::new(seed);
            (0..8).map(|_| rng.next_u64()).collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
        assert_ne!(draw(0), draw(1), "a seed of 0 is a seed like the others");
    }

    #[test]
    fn a_number_below_n_is_below_n_and_all_of_them_come() {
        let mut rng = Rng::new(3);
        let mut seen = [false; 5];
        for _ in 0..200 {
            let n = rng.below(5);
            assert!(n < 5);
            seen[n] = true;
        }
        assert!(seen.iter().all(|&s| s), "{seen:?}");
    }
}
