//! The arena: where the state of nodes lives, so that it outlives every plan.
//!
//! A [`Key`] is a slot and a generation. A slot that is freed has its generation raised, so a key of a value that is gone finds nothing, and never finds the value that
//! came to its slot later. A slot whose generation would wrap is retired and not used again (4 billion uses of one slot is not a thing that happens, but the rule costs
//! one comparison and makes "never finds another value" true without a proviso).

use std::ops::{Index, IndexMut};

/// The address of a value in an [`Arena`]: where it is, and which one that was.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    index: u32,
    generation: u32,
}

impl Key {
    /// The slot, which is also the order that [`Arena::iter`] visits in.
    pub fn index(self) -> usize {
        self.index as usize
    }

    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}v{}", self.index, self.generation)
    }
}

enum Slot<T> {
    Full { generation: u32, value: T },
    /// Free, with the generation that the next value in it will have, and the next free slot (`NONE` at the end of the list).
    Empty { generation: u32, next: u32 },
    Retired,
}

/// The end of the list of free slots.
const NONE: u32 = u32::MAX;

pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    /// The slot that was freed last: the first one to be used again.
    free: u32,
    len: usize,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Arena<T> {
    pub fn new() -> Self {
        Arena { slots: Vec::new(), free: NONE, len: 0 }
    }

    /// An arena with room for `slots` values: until it holds more than that at once, `insert` does not ask the allocator for anything.
    pub fn with_capacity(slots: usize) -> Self {
        Arena { slots: Vec::with_capacity(slots), free: NONE, len: 0 }
    }

    /// How many values the arena can hold at once before `insert` has to grow it.
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Put a value in: in the slot that was freed last if there is one, and else in a new slot at the end (which is the only time the arena grows).
    pub fn insert(&mut self, value: T) -> Key {
        self.len += 1;
        if self.free != NONE {
            let index = self.free;
            let (generation, next) = match &self.slots[index as usize] {
                Slot::Empty { generation, next } => (*generation, *next),
                _ => unreachable!("the list of free slots holds empty slots"),
            };
            self.free = next;
            self.slots[index as usize] = Slot::Full { generation, value };
            Key { index, generation }
        } else {
            let index = u32::try_from(self.slots.len()).ok().filter(|&index| index != NONE).expect("an arena has fewer than 2^32 - 1 slots");
            self.slots.push(Slot::Full { generation: 0, value });
            Key { index, generation: 0 }
        }
    }

    /// Take a value out, with its state: `None` for a key of a value that is not there.
    pub fn remove(&mut self, key: Key) -> Option<T> {
        let slot = self.slots.get_mut(key.index())?;
        if !matches!(slot, Slot::Full { generation, .. } if *generation == key.generation) {
            return None;
        }
        let Slot::Full { generation, value } = std::mem::replace(slot, Slot::Retired) else { unreachable!("the slot was full") };
        // A slot at the last generation stays retired.
        if let Some(next_generation) = generation.checked_add(1) {
            *slot = Slot::Empty { generation: next_generation, next: self.free };
            self.free = key.index;
        }
        self.len -= 1;
        Some(value)
    }

    pub fn contains(&self, key: Key) -> bool {
        self.get(key).is_some()
    }

    pub fn get(&self, key: Key) -> Option<&T> {
        match self.slots.get(key.index())? {
            Slot::Full { generation, value } if *generation == key.generation => Some(value),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: Key) -> Option<&mut T> {
        match self.slots.get_mut(key.index())? {
            Slot::Full { generation, value } if *generation == key.generation => Some(value),
            _ => None,
        }
    }

    /// Two values at once, for the code that moves something from one to the other: `None` if either key finds nothing, and for two keys of one slot.
    pub fn get2_mut(&mut self, a: Key, b: Key) -> Option<(&mut T, &mut T)> {
        if a.index == b.index {
            return None;
        }
        let (low, high) = if a.index < b.index { (a, b) } else { (b, a) };
        if high.index() >= self.slots.len() {
            return None;
        }
        let (left, right) = self.slots.split_at_mut(high.index());
        let low_value = match left.get_mut(low.index())? {
            Slot::Full { generation, value } if *generation == low.generation => value,
            _ => return None,
        };
        let high_value = match right.first_mut()? {
            Slot::Full { generation, value } if *generation == high.generation => value,
            _ => return None,
        };
        Some(if a.index < b.index { (low_value, high_value) } else { (high_value, low_value) })
    }

    /// The values that are in, in the order of their slots.
    pub fn iter(&self) -> impl Iterator<Item = (Key, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| match slot {
            Slot::Full { generation, value } => Some((Key { index: index as u32, generation: *generation }, value)),
            _ => None,
        })
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Key, &mut T)> {
        self.slots.iter_mut().enumerate().filter_map(|(index, slot)| match slot {
            Slot::Full { generation, value } => Some((Key { index: index as u32, generation: *generation }, value)),
            _ => None,
        })
    }
}

impl<T> Index<Key> for Arena<T> {
    type Output = T;

    #[track_caller]
    fn index(&self, key: Key) -> &T {
        self.get(key).expect("a key of a value that is not in the arena")
    }
}

impl<T> IndexMut<Key> for Arena<T> {
    #[track_caller]
    fn index_mut(&mut self, key: Key) -> &mut T {
        self.get_mut(key).expect("a key of a value that is not in the arena")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::testutil::Rng;

    #[test]
    fn a_value_that_is_put_in_is_found_by_its_key() {
        let mut arena = Arena::new();
        let a = arena.insert("a");
        let b = arena.insert("b");
        assert_eq!((arena.get(a), arena.get(b)), (Some(&"a"), Some(&"b")));
        assert_eq!((arena[a], arena[b]), ("a", "b"));
        assert!(arena.contains(a) && arena.contains(b));
        *arena.get_mut(a).unwrap() = "c";
        arena[b] = "d";
        assert_eq!((arena[a], arena[b]), ("c", "d"));
    }

    #[test]
    fn the_length_is_the_number_of_values_that_are_in() {
        let mut arena = Arena::new();
        assert!(arena.is_empty());
        let keys: Vec<_> = (0..5).map(|n| arena.insert(n)).collect();
        assert_eq!((arena.len(), arena.is_empty()), (5, false));
        arena.remove(keys[1]);
        arena.remove(keys[3]);
        assert_eq!(arena.len(), 3);
        arena.remove(keys[1]);
        assert_eq!(arena.len(), 3, "taking out what is out does nothing");
        arena.insert(9);
        assert_eq!(arena.len(), 4);
    }

    #[test]
    fn a_value_that_is_taken_out_is_handed_back_with_its_state() {
        let mut arena = Arena::new();
        let key = arena.insert(vec![1, 2, 3]);
        arena[key].push(4);
        assert_eq!(arena.remove(key), Some(vec![1, 2, 3, 4]));
        assert_eq!(arena.len(), 0);
    }

    #[test]
    fn a_key_of_a_value_that_is_gone_finds_nothing_whichever_way_it_is_asked() {
        let mut arena = Arena::new();
        let gone = arena.insert(1);
        let kept = arena.insert(2);
        arena.remove(gone);
        assert_eq!(arena.get(gone), None);
        assert_eq!(arena.get_mut(gone), None);
        assert!(!arena.contains(gone));
        assert_eq!(arena.remove(gone), None);
        assert!(arena.get2_mut(gone, kept).is_none());
        assert!(arena.get2_mut(kept, gone).is_none());
        assert_eq!(arena[kept], 2, "and the others are as they were");
    }

    #[test]
    fn a_key_that_was_never_given_out_finds_nothing() {
        let mut arena = Arena::new();
        let key = arena.insert(1);
        let mut other = Arena::new();
        for n in 0..4 {
            other.insert(n);
        }
        let far = other.insert(9);
        assert_eq!(arena.get(far), None, "a slot that is past the end");
        assert_eq!(arena.remove(far), None);
        assert_eq!(arena.get(key), Some(&1));
    }

    #[test]
    fn a_slot_that_is_used_again_has_another_key_and_the_old_key_does_not_find_the_new_value() {
        let mut arena = Arena::new();
        let old = arena.insert("old");
        arena.remove(old);
        let new = arena.insert("new");
        assert_eq!(new.index(), old.index(), "the slot is used again");
        assert_ne!(new.generation(), old.generation());
        assert_ne!(new, old);
        assert_eq!(arena.get(old), None);
        assert_eq!(arena.get_mut(old), None);
        assert_eq!(arena.remove(old), None);
        assert_eq!(arena.get(new), Some(&"new"), "and removing by the old key took nothing from the new value");
    }

    #[test]
    fn two_keys_are_borrowed_together_only_if_both_are_of_the_values_that_are_in() {
        let mut arena = Arena::new();
        let old_low = arena.insert(1);
        let old_high = arena.insert(2);
        arena.remove(old_low);
        arena.remove(old_high);
        let new_high = arena.insert(20);
        let new_low = arena.insert(10);
        assert_eq!((new_high.index(), new_low.index()), (1, 0), "both slots are used again");
        for (a, b) in [(old_low, new_high), (new_high, old_low), (new_low, old_high), (old_high, new_low), (old_low, old_high)] {
            assert!(arena.get2_mut(a, b).is_none(), "{a:?} {b:?}");
        }
        let (low, high) = arena.get2_mut(new_low, new_high).unwrap();
        assert_eq!((*low, *high), (10, 20));
        let (high, low) = arena.get2_mut(new_high, new_low).unwrap();
        assert_eq!((*high, *low), (20, 10));
    }

    #[test]
    fn a_slot_is_used_again_before_the_arena_grows() {
        let mut arena = Arena::new();
        let keys: Vec<_> = (0..4).map(|n| arena.insert(n)).collect();
        arena.remove(keys[2]);
        let again = arena.insert(10);
        assert_eq!(again.index(), 2);
        assert_eq!(arena.iter().count(), 4);
        let beyond = arena.insert(11);
        assert_eq!(beyond.index(), 4, "no slot is free, so a new one");
    }

    #[test]
    fn the_slot_that_was_freed_last_is_the_first_to_be_used_again() {
        let mut arena = Arena::new();
        let keys: Vec<_> = (0..4).map(|n| arena.insert(n)).collect();
        arena.remove(keys[0]);
        arena.remove(keys[3]);
        arena.remove(keys[1]);
        let order: Vec<_> = (0..3).map(|n| arena.insert(n).index()).collect();
        assert_eq!(order, [1, 3, 0]);
    }

    #[test]
    fn a_slot_at_the_last_generation_is_retired_and_never_used_again() {
        let mut arena = Arena::new();
        let first = arena.insert("first");
        let guard = arena.insert("guard");
        arena.remove(first);
        // The next value in the slot is the last one it can have.
        let last = match &mut arena.slots[first.index()] {
            Slot::Empty { generation, .. } => {
                *generation = u32::MAX;
                Key { index: first.index, generation: u32::MAX }
            }
            _ => panic!("the slot was freed"),
        };
        let at_last = arena.insert("last");
        assert_eq!(at_last, last);
        assert_eq!(arena.remove(at_last), Some("last"));
        assert_eq!(arena.get(at_last), None);
        let next = arena.insert("next");
        assert_ne!(next.index(), first.index(), "the slot is not used a wrapped number of times");
        assert_eq!(arena.get(at_last), None);
        assert_eq!(arena.get(first), None);
        assert_eq!(arena.get(guard), Some(&"guard"));
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn values_are_visited_in_the_order_of_their_slots_and_empty_slots_are_skipped() {
        let mut arena = Arena::new();
        let keys: Vec<_> = (0..6).map(|n| arena.insert(n * 10)).collect();
        arena.remove(keys[1]);
        arena.remove(keys[4]);
        let visited: Vec<_> = arena.iter().map(|(key, value)| (key.index(), *value)).collect();
        assert_eq!(visited, [(0, 0), (2, 20), (3, 30), (5, 50)]);
        assert!(arena.iter().all(|(key, value)| arena[key] == *value), "each key is the value's");
        let new = arena.insert(99);
        assert_eq!(arena.iter().map(|(key, _)| key.index()).collect::<Vec<_>>(), [0, 2, 3, 4, 5], "the slot that was used again is visited at its place ({new:?})");
        assert!(arena.iter().all(|(key, value)| arena[key] == *value), "each key is the value's, also in a slot that is used again");
        assert!(arena.iter().any(|(key, _)| key == new), "and the key of the value that came is among them");
    }

    #[test]
    fn values_are_changed_through_the_mutable_visit_and_the_keys_it_gives_find_them() {
        let mut arena = Arena::new();
        let keys: Vec<_> = (0..4).map(|n| arena.insert(n)).collect();
        arena.remove(keys[2]);
        for (_, value) in arena.iter_mut() {
            *value += 100;
        }
        let seen: Vec<_> = arena.iter_mut().map(|(key, value)| (key, *value)).collect();
        assert_eq!(seen.len(), 3);
        for (key, value) in seen {
            assert_eq!(arena[key], value);
        }
        assert_eq!((arena[keys[0]], arena[keys[1]], arena[keys[3]]), (100, 101, 103));
    }

    #[test]
    fn two_values_are_borrowed_together_by_two_different_keys_in_either_order() {
        let mut arena = Arena::new();
        let a = arena.insert(String::from("a"));
        let b = arena.insert(String::from("b"));
        let c = arena.insert(String::from("c"));
        {
            let (x, y) = arena.get2_mut(a, c).unwrap();
            std::mem::swap(x, y);
        }
        assert_eq!((arena[a].as_str(), arena[c].as_str()), ("c", "a"));
        {
            let (x, y) = arena.get2_mut(c, b).unwrap();
            assert_eq!((x.as_str(), y.as_str()), ("a", "b"), "the first is the first key's whatever the order of the slots");
        }
        assert!(arena.get2_mut(a, a).is_none(), "one value is not two");
    }

    #[test]
    #[should_panic(expected = "a key of a value that is not in the arena")]
    fn indexing_by_a_key_that_finds_nothing_panics() {
        let mut arena = Arena::new();
        let key = arena.insert(1);
        arena.remove(key);
        let _ = arena[key];
    }

    #[test]
    fn an_arena_made_with_room_for_n_has_it_and_an_arena_that_is_not_has_none_until_it_grows() {
        let arena = Arena::<u8>::with_capacity(10);
        assert!(arena.capacity() >= 10);
        assert_eq!(arena.len(), 0);
        assert_eq!(Arena::<u8>::new().capacity(), 0);
    }

    #[test]
    fn keys_are_equal_when_slot_and_generation_are() {
        let mut arena = Arena::new();
        let a = arena.insert(1);
        let copy = a;
        assert_eq!(a, copy);
        assert_eq!(format!("{a:?}"), "0v0");
        arena.remove(a);
        let again = arena.insert(2);
        assert_eq!(format!("{again:?}"), "0v1");
    }

    /// A long seeded run of puts and takes, held to a map: what is in is found by its key, what was taken out by none, and the number in is the length.
    #[test]
    fn a_seeded_run_of_inserts_and_removals_agrees_with_a_map() {
        for seed in 1..=20u64 {
            let mut rng = Rng::new(seed);
            let mut arena = Arena::new();
            let mut model: HashMap<Key, u64> = HashMap::new();
            let mut gone: Vec<Key> = Vec::new();
            for step in 0..600u64 {
                match rng.below(5) {
                    0..=2 => {
                        let key = arena.insert(step);
                        assert!(model.insert(key, step).is_none(), "seed {seed} step {step}: a key was given twice");
                        assert!(!gone.contains(&key), "seed {seed} step {step}: the key of a value that is gone was given again");
                    }
                    3 if !model.is_empty() => {
                        let at = rng.below(model.len());
                        let key = *model.keys().nth(at).unwrap();
                        assert_eq!(arena.remove(key), model.remove(&key), "seed {seed} step {step}");
                        gone.push(key);
                    }
                    _ if !gone.is_empty() => {
                        let key = gone[rng.below(gone.len())];
                        assert_eq!(arena.remove(key), None, "seed {seed} step {step}");
                        assert_eq!(arena.get(key), None);
                    }
                    _ => {}
                }
                assert_eq!(arena.len(), model.len(), "seed {seed} step {step}");
            }
            for (key, value) in &model {
                assert_eq!(arena.get(*key), Some(value), "seed {seed}");
            }
            for key in &gone {
                assert_eq!(arena.get(*key), None, "seed {seed}");
            }
            assert_eq!(arena.iter().count(), model.len());
        }
    }
}
