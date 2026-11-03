//! What the tests of the crate share: a seeded generator, so that a random graph is the same on every platform and every run.

use crate::patch::{NodeSpec, Patch, Port, PortSpec};

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

    /// True with the chance `p` (0 to 1).
    pub fn chance(&mut self, p: f64) -> bool {
        ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64) < p
    }

    /// The numbers `0..n` in an order that is drawn.
    pub fn shuffled(&mut self, n: usize) -> Vec<usize> {
        let mut all: Vec<usize> = (0..n).collect();
        for at in (1..n).rev() {
            all.swap(at, self.below(at + 1));
        }
        all
    }
}

/// A patch that is drawn from a seed: 2 to 14 nodes with up to two ports each way, of one or two channels, half of the inputs `single` (drawn from a generator of their own, so
/// that the shape of a patch is what it was before there were any), ranks from 0 to 3 (so that many are equal), labels that are not in the order that the nodes are put in, and up
/// to twice as many cables as nodes between ports of equal width, some of them twice. It has no cycle: a cable goes from a node to one that is later in an order that is drawn
/// first and kept hidden.
pub fn random_patch(seed: u64) -> Patch<u32> {
    let mut rng = Rng::new(seed);
    let n = 2 + rng.below(13);
    let hidden = rng.shuffled(n); // the place of each node in the order that no cable goes against
    let labels = rng.shuffled(n); // the id of each node
    let width = |rng: &mut Rng| PortSpec::audio(1 + rng.below(2) as u16);
    let mut kinds = Rng::new(seed ^ 0x51_9E_1E);
    let mut specs: Vec<NodeSpec> = Vec::new();
    for _ in 0..n {
        let inputs = (0..rng.below(3))
            .map(|_| {
                let port = width(&mut rng);
                if kinds.chance(0.5) { PortSpec::single(port.channels) } else { port }
            })
            .collect::<Vec<_>>();
        let outputs = (0..rng.below(3)).map(|_| width(&mut rng)).collect::<Vec<_>>();
        specs.push(NodeSpec::new(inputs, outputs, rng.below(4) as u32));
    }
    // The nodes are put in in an order of their own.
    let mut patch = Patch::new();
    let insertion = rng.shuffled(n);
    for &node in &insertion {
        patch.add_node(labels[node] as u32 * 3 + 11, specs[node].clone());
    }
    for _ in 0..2 * n {
        let (a, b) = (rng.below(n), rng.below(n));
        if hidden[a] >= hidden[b] || specs[a].outputs.is_empty() || specs[b].inputs.is_empty() {
            continue;
        }
        let (o, i) = (rng.below(specs[a].outputs.len()), rng.below(specs[b].inputs.len()));
        if specs[a].outputs[o].channels != specs[b].inputs[i].channels {
            continue;
        }
        let (from, to) = (Port::new(labels[a] as u32 * 3 + 11, o as u16), Port::new(labels[b] as u32 * 3 + 11, i as u16));
        patch.connect(from, to);
        if rng.chance(0.15) {
            patch.connect(from, to);
        }
    }
    patch
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
    fn a_shuffle_has_every_number_once_and_is_not_always_the_same_order() {
        let mut rng = Rng::new(5);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..30 {
            let order = rng.shuffled(6);
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, [0, 1, 2, 3, 4, 5]);
            seen.insert(order);
        }
        assert!(seen.len() > 10);
    }

    #[test]
    fn a_chance_of_nothing_never_comes_and_a_chance_of_all_always() {
        let mut rng = Rng::new(9);
        assert!((0..100).all(|_| !rng.chance(0.0)));
        assert!((0..100).all(|_| rng.chance(1.0)));
        let heads = (0..1000).filter(|_| rng.chance(0.5)).count();
        assert!((400..600).contains(&heads), "{heads}");
    }

    #[test]
    fn a_random_patch_is_a_patch_that_compiles_and_has_nodes_and_cables_of_every_size() {
        let mut cables = std::collections::BTreeSet::new();
        for seed in 0..200 {
            let patch = random_patch(seed);
            assert!((2..=14).contains(&patch.nodes().len()));
            assert!(crate::compile(&patch).is_ok(), "seed {seed}");
            assert_eq!(patch, random_patch(seed), "the same seed, the same patch");
            cables.insert(patch.cables().len());
        }
        assert!(cables.len() > 8, "{cables:?}");
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
