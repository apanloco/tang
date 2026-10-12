//! The plugin catalog: what is installed on this machine, as the selector
//! popups list it.
//!
//! A full scan instantiates every installed plugin and can take seconds, so it
//! runs on its own thread and delivers one batch per format: the built-ins
//! arrive at once, VST3, which loads each plugin, last. The owner of the
//! catalog calls `poll` to fold in what has arrived; the scan is over when the
//! thread has exited and `poll` has seen its channel close.

use crossbeam_channel::{Receiver, TryRecvError};

use crate::plugin::{self, PluginInfo};

#[derive(Default)]
pub struct Catalog {
    /// Everything found so far, sorted by name without regard to case.
    plugins: Vec<PluginInfo>,
    /// Where the scan thread delivers its batches; `None` when no scan is
    /// running.
    batches: Option<Receiver<Vec<PluginInfo>>>,
}

impl Catalog {
    /// Start scanning on a background thread. Results arrive through `poll`.
    pub fn scan(&mut self) {
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let _ = tx.send(plugin::builtin::enumerate_plugins());

            #[cfg(feature = "lv2")]
            let _ = tx.send(plugin::lv2::enumerate_plugins());

            let _ = tx.send(plugin::clap::enumerate_plugins());

            #[cfg(feature = "vst3")]
            let _ = tx.send(plugin::vst3::enumerate_plugins());
        });
        self.follow(rx);
    }

    /// Take batches from `batches` the way a scan delivers them; the scan is
    /// over when the sending side is dropped.
    pub fn follow(&mut self, batches: Receiver<Vec<PluginInfo>>) {
        self.batches = Some(batches);
    }

    /// Everything found so far, sorted by name without regard to case.
    pub fn plugins(&self) -> &[PluginInfo] {
        &self.plugins
    }

    /// True until the scan has finished (or if none was ever started: false).
    pub fn is_scanning(&self) -> bool {
        self.batches.is_some()
    }

    /// Add plugins to the catalog. True if there were any.
    pub fn add(&mut self, batch: Vec<PluginInfo>) -> bool {
        if batch.is_empty() {
            return false;
        }
        self.plugins.extend(batch);
        // Stable, so plugins with the same name stay in the order they arrived
        // in: built-in, LV2, CLAP, VST3.
        self.plugins.sort_by_key(|p| p.name.to_lowercase());
        true
    }

    /// Fold in what the scan has delivered since the last call. True if
    /// plugins were added.
    pub fn poll(&mut self) -> bool {
        let mut added = false;
        while let Some(batches) = &self.batches {
            match batches.try_recv() {
                Ok(batch) => added |= self.add(batch),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.batches = None,
            }
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, id: &str) -> PluginInfo {
        PluginInfo {
            name: name.to_string(),
            id: id.to_string(),
            is_instrument: false,
            param_count: 0,
            preset_count: 0,
            path: String::new(),
            scan_ms: 0,
        }
    }

    fn names(catalog: &Catalog) -> Vec<&str> {
        catalog.plugins().iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn plugins_are_sorted_by_name_without_regard_to_case() {
        let mut catalog = Catalog::default();
        assert!(catalog.add(vec![info("Zeta", "a"), info("alpha", "b")]));
        assert!(catalog.add(vec![info("Beta", "c")]));
        assert_eq!(names(&catalog), ["alpha", "Beta", "Zeta"]);
    }

    #[test]
    fn plugins_with_the_same_name_keep_the_order_they_arrived_in() {
        let mut catalog = Catalog::default();
        catalog.add(vec![info("Reverb", "builtin:reverb")]);
        catalog.add(vec![info("Other", "x")]);
        catalog.add(vec![info("reverb", "clap:reverb")]);
        catalog.add(vec![info("REVERB", "vst3:reverb")]);
        let ids: Vec<&str> = catalog.plugins().iter().filter(|p| p.name.eq_ignore_ascii_case("reverb")).map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["builtin:reverb", "clap:reverb", "vst3:reverb"]);
    }

    #[test]
    fn an_empty_batch_adds_nothing() {
        let mut catalog = Catalog::default();
        assert!(!catalog.add(vec![]));
        assert!(catalog.plugins().is_empty());
    }

    #[test]
    fn a_catalog_that_was_never_scanned_is_not_scanning() {
        let mut catalog = Catalog::default();
        assert!(!catalog.is_scanning());
        assert!(!catalog.poll());
    }

    #[test]
    fn poll_folds_in_the_batches_and_notices_the_end_of_the_scan() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut catalog = Catalog::default();
        catalog.follow(rx);
        assert!(catalog.is_scanning());
        assert!(!catalog.poll(), "nothing has arrived");
        assert!(catalog.is_scanning(), "the scan thread is still running");

        tx.send(vec![info("Zeta", "a")]).unwrap();
        tx.send(vec![info("alpha", "b")]).unwrap();
        tx.send(vec![]).unwrap(); // an empty batch last must not hide the others
        assert!(catalog.poll());
        assert_eq!(names(&catalog), ["alpha", "Zeta"]);
        assert!(!catalog.poll(), "what arrived has been folded in");

        tx.send(vec![]).unwrap();
        assert!(!catalog.poll(), "an empty batch is not news");

        tx.send(vec![info("Beta", "c")]).unwrap();
        drop(tx); // the scan thread exits, its last batch still in the channel
        assert!(catalog.poll());
        assert_eq!(names(&catalog), ["alpha", "Beta", "Zeta"]);
        assert!(!catalog.is_scanning());
        assert!(!catalog.poll());
    }
}
