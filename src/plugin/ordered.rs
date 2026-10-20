//! `Ordered<T>`: a vector whose items have keys, for an engine that is addressed by position and is run by a plan that names nodes (P1 of `docs/patch-graph.md` §11.6).
//!
//! The items are where they have always been: in a vector, in the order of their places, so that every slice method and every `Vec` method that the engine's code uses is the
//! same (`iter`, `get_mut`, indexing, `len`, `push`, `insert`, `remove`, `swap`). What is new is that each item has a [`Key`] that stays with it while the items around it come
//! and go and change places, and that a key of an item that is gone finds nothing (a slot that is used again has another generation: the keys are an arena's, `kernel::Arena`).
//! The plan of the kernel names keys; the commands of the engine still name places. P2 puts the items in the arena itself and the commands name keys, and this goes.

use std::ops::{Deref, DerefMut};

use kernel::{Arena, Key};

/// No place.
const NONE: u32 = u32::MAX;

pub struct Ordered<T> {
    items: Vec<T>,
    /// The key of each item, in the same order.
    keys: Vec<Key>,
    /// Where the keys come from: an arena that holds nothing, so that a key is a slot and a generation.
    allocator: Arena<()>,
    /// By the slot of a key: the place in `items` of the item that the key is of. What it holds for a key that is gone, or for a slot that has not been used yet (`NONE`), is not read:
    /// `position_of` asks the allocator first.
    place: Vec<u32>,
}

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Ordered<T> {
    pub fn new() -> Self {
        Ordered { items: Vec::new(), keys: Vec::new(), allocator: Arena::new(), place: Vec::new() }
    }

    /// Put an item at the end, and say its key.
    pub fn push(&mut self, value: T) -> Key {
        self.insert(self.items.len(), value)
    }

    /// Put an item at a place (the ones from there on move one place on, as in a `Vec`), and say its key. Panics when `at` is past the end, as `Vec::insert` does.
    pub fn insert(&mut self, at: usize, value: T) -> Key {
        assert!(at <= self.items.len(), "insertion index (is {at}) should be <= len (is {})", self.items.len());
        let key = self.allocator.insert(());
        self.items.insert(at, value);
        self.keys.insert(at, key);
        if self.place.len() <= key.index() {
            self.place.resize(key.index() + 1, NONE);
        }
        self.renumber(at);
        key
    }

    /// Take the item at a place out (the ones after it move one place back). Its key is gone with it. Panics when there is none, as `Vec::remove` does.
    pub fn remove(&mut self, at: usize) -> T {
        let key = self.keys.remove(at);
        let value = self.items.remove(at);
        self.allocator.remove(key);
        self.renumber(at);
        value
    }

    /// Two items change places, and their keys with them.
    pub fn swap(&mut self, a: usize, b: usize) {
        self.items.swap(a, b);
        self.keys.swap(a, b);
        self.place[self.keys[a].index()] = a as u32;
        self.place[self.keys[b].index()] = b as u32;
    }

    pub fn key_at(&self, at: usize) -> Option<Key> {
        self.keys.get(at).copied()
    }

    /// The keys, in the order of the places.
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// The place of the item that a key is of: `None` for a key of an item that is gone.
    pub fn position_of(&self, key: Key) -> Option<usize> {
        if !self.allocator.contains(key) {
            return None;
        }
        let at = *self.place.get(key.index())? as usize;
        debug_assert_eq!(self.keys.get(at), Some(&key), "the keys of an Ordered follow its items");
        Some(at)
    }

    /// The places from `from` on have moved: say where each key's item is now.
    fn renumber(&mut self, from: usize) {
        for at in from..self.keys.len() {
            self.place[self.keys[at].index()] = at as u32;
        }
    }

    /// The item that a key is of, and the place it is at.
    pub fn get_by_key_mut(&mut self, key: Key) -> Option<(usize, &mut T)> {
        let at = self.position_of(key)?;
        Some((at, &mut self.items[at]))
    }
}

impl<T> Deref for Ordered<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.items
    }
}

/// The slice methods that change what is where (`reverse`, `sort`, `rotate_left`...) are not to be used on an `Ordered`: the keys would not follow (`swap` is its own). A
/// `debug_assert` in `position_of` is what finds it.
impl<T> DerefMut for Ordered<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.items
    }
}

impl<'a, T> IntoIterator for &'a Ordered<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Ordered<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic generator for the random run (this crate has no `rand`).
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn filled(n: u32) -> (Ordered<u32>, Vec<Key>) {
        let mut ordered = Ordered::new();
        let keys = (0..n).map(|v| ordered.push(v * 10)).collect();
        (ordered, keys)
    }

    #[test]
    fn items_are_in_the_order_they_were_pushed_and_are_read_as_a_slice() {
        let (ordered, keys) = filled(4);
        assert_eq!(ordered.len(), 4);
        assert_eq!(&ordered[..], &[0, 10, 20, 30]);
        assert_eq!(ordered[2], 20);
        assert_eq!(ordered.iter().sum::<u32>(), 60);
        assert_eq!(ordered.get(3), Some(&30));
        assert_eq!(ordered.get(4), None);
        assert_eq!(ordered.keys(), keys.as_slice());
        assert_eq!(ordered.key_at(1), Some(keys[1]));
        assert_eq!(ordered.key_at(4), None);
        let as_slice = |s: &[u32]| s.len();
        assert_eq!(as_slice(&ordered), 4, "a reference to it is a reference to a slice");
        let mut seen = Vec::new();
        for item in &ordered {
            seen.push(*item);
        }
        assert_eq!(seen, [0, 10, 20, 30]);
    }

    #[test]
    fn items_are_changed_in_place_by_index_by_get_mut_and_by_iteration() {
        let (mut ordered, keys) = filled(3);
        ordered[0] += 1;
        *ordered.get_mut(1).unwrap() += 2;
        for item in ordered.iter_mut() {
            *item += 100;
        }
        for item in &mut ordered {
            *item += 1000;
        }
        assert_eq!(&ordered[..], &[1101, 1112, 1120]);
        assert_eq!(ordered.keys(), keys.as_slice(), "no key changed");
    }

    #[test]
    fn a_key_finds_the_place_of_its_item_and_the_item() {
        let (mut ordered, keys) = filled(4);
        for (at, key) in keys.iter().enumerate() {
            assert_eq!(ordered.position_of(*key), Some(at));
        }
        let (at, item) = ordered.get_by_key_mut(keys[2]).unwrap();
        assert_eq!((at, *item), (2, 20));
        *item = 21;
        assert_eq!(ordered[2], 21);
    }

    #[test]
    fn an_item_put_in_the_middle_pushes_the_ones_after_it_on_and_every_key_follows_its_item() {
        let (mut ordered, keys) = filled(4);
        let new = ordered.insert(1, 99);
        assert_eq!(&ordered[..], &[0, 99, 10, 20, 30]);
        let places: Vec<_> = keys.iter().map(|key| ordered.position_of(*key)).collect();
        assert_eq!(places, [Some(0), Some(2), Some(3), Some(4)]);
        assert_eq!(ordered.position_of(new), Some(1));
        assert_eq!(ordered.key_at(1), Some(new));
        let at_end = ordered.insert(5, 7);
        assert_eq!((ordered.position_of(at_end), ordered.len()), (Some(5), 6), "a place at the end is allowed");
        let at_start = ordered.insert(0, 8);
        assert_eq!(ordered.position_of(at_start), Some(0));
        assert_eq!(ordered.position_of(keys[0]), Some(1));
    }

    #[test]
    #[should_panic]
    fn a_place_past_the_end_panics_as_a_vector_does() {
        let (mut ordered, _) = filled(2);
        ordered.insert(3, 1);
    }

    #[test]
    fn an_item_that_is_taken_out_is_handed_back_and_its_key_finds_nothing_and_the_others_keep_theirs() {
        let (mut ordered, keys) = filled(5);
        assert_eq!(ordered.remove(1), 10);
        assert_eq!(&ordered[..], &[0, 20, 30, 40]);
        assert_eq!(ordered.position_of(keys[1]), None);
        assert!(ordered.get_by_key_mut(keys[1]).is_none());
        let places: Vec<_> = [0, 2, 3, 4].iter().map(|&i| ordered.position_of(keys[i])).collect();
        assert_eq!(places, [Some(0), Some(1), Some(2), Some(3)]);
        assert_eq!(ordered.remove(3), 40);
        assert_eq!(ordered.position_of(keys[4]), None);
        assert_eq!(ordered.keys().len(), 3);
    }

    #[test]
    #[should_panic]
    fn taking_out_a_place_that_is_not_there_panics_as_a_vector_does() {
        let (mut ordered, _) = filled(2);
        ordered.remove(2);
    }

    #[test]
    fn two_items_that_swap_places_take_their_keys_with_them() {
        let (mut ordered, keys) = filled(4);
        ordered.swap(0, 3);
        assert_eq!(&ordered[..], &[30, 10, 20, 0]);
        assert_eq!((ordered.position_of(keys[0]), ordered.position_of(keys[3])), (Some(3), Some(0)));
        assert_eq!((ordered.key_at(0), ordered.key_at(3)), (Some(keys[3]), Some(keys[0])));
        ordered.swap(1, 1);
        assert_eq!(&ordered[..], &[30, 10, 20, 0], "an item swapped with itself stays");
        assert_eq!(ordered.position_of(keys[1]), Some(1));
    }

    #[test]
    fn the_key_of_a_slot_that_is_used_again_is_another_and_the_old_one_finds_nothing() {
        let (mut ordered, keys) = filled(3);
        ordered.remove(1);
        let new = ordered.push(77);
        assert_eq!(new.index(), keys[1].index(), "the slot is used again");
        assert_ne!(new, keys[1]);
        assert_eq!(ordered.position_of(keys[1]), None, "the old key does not find the item that came");
        assert_eq!(ordered.position_of(new), Some(2));
        assert_eq!(ordered[2], 77);
    }

    #[test]
    fn a_key_of_another_ordered_or_one_that_was_never_given_finds_nothing() {
        let (ordered, _) = filled(2);
        let (other, far) = filled(9);
        assert_eq!(ordered.position_of(far[8]), None);
        let _ = other;
    }

    /// A long seeded run of every change there is, held to a vector of (number, key): every key is where the model says, every removed key finds nothing.
    #[test]
    fn a_seeded_run_of_changes_keeps_every_key_with_its_item() {
        for seed in 1..=30u64 {
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut ordered: Ordered<u64> = Ordered::new();
            let mut model: Vec<(u64, Key)> = Vec::new();
            let mut gone: Vec<Key> = Vec::new();
            for step in 0..400u64 {
                let roll = xorshift(&mut state) % 10;
                match roll {
                    0..=3 => {
                        let at = (xorshift(&mut state) as usize) % (model.len() + 1);
                        let key = ordered.insert(at, step);
                        model.insert(at, (step, key));
                    }
                    4 => {
                        let key = ordered.push(step);
                        model.push((step, key));
                    }
                    5..=6 if !model.is_empty() => {
                        let at = (xorshift(&mut state) as usize) % model.len();
                        let (value, key) = model.remove(at);
                        assert_eq!(ordered.remove(at), value, "seed {seed} step {step}");
                        gone.push(key);
                    }
                    7..=8 if model.len() > 1 => {
                        let a = (xorshift(&mut state) as usize) % model.len();
                        let b = (xorshift(&mut state) as usize) % model.len();
                        ordered.swap(a, b);
                        model.swap(a, b);
                    }
                    _ => {}
                }
                assert_eq!(ordered.len(), model.len());
                for (at, (value, key)) in model.iter().enumerate() {
                    assert_eq!(ordered[at], *value, "seed {seed} step {step}");
                    assert_eq!(ordered.key_at(at), Some(*key));
                    assert_eq!(ordered.position_of(*key), Some(at), "seed {seed} step {step}");
                }
                for key in &gone {
                    assert_eq!(ordered.position_of(*key), None, "seed {seed} step {step}");
                }
            }
        }
    }
}
