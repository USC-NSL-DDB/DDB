use arrayvec::ArrayVec;
use dashmap::DashMap;

pub type SignalSet = ArrayVec<Option<Signal>, 128>;
type Signals = SignalSet;

pub struct SignalListing {
    initialized: bool,
    signals: Signals,
}

impl SignalListing {
    pub fn new_with_signals(signals: Signals) -> Self {
        Self {
            initialized: true,
            signals,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn set_initialized(&mut self, initialized: bool) {
        self.initialized = initialized;
    }

    pub fn get_signals(&self) -> &Signals {
        &self.signals
    }

    pub fn get_signals_mut(&mut self) -> &mut Signals {
        &mut self.signals
    }
}

impl Default for SignalListing {
    fn default() -> Self {
        Self {
            initialized: false,
            signals: ArrayVec::new(),
        }
    }
}

pub struct Signal {
    pub name: String,
    pub stop: bool,
    pub print: bool,
    pub pass: bool,
    pub description: String,
}

pub struct SignalMgr {
    // Map of session id to supported signals
    // Usually, each session shares the same signal set, but we keep them separate
    // in case we need to consider mixed environments/architecture in the future.
    signal_map: DashMap<u64, SignalListing>,
}

impl SignalMgr {
    pub fn new() -> Self {
        Self {
            signal_map: DashMap::new(),
        }
    }

    #[inline]
    pub fn remove_session(&self, sid: u64) {
        self.signal_map.remove(&sid);
    }

    #[inline]
    pub fn replace_signals(&self, sid: u64, signals: Signals) {
        self.signal_map
            .insert(sid, SignalListing::new_with_signals(signals));
    }

    #[inline]
    pub fn with_listing_mut<U, F>(&self, sid: u64, f: F) -> U
    where
        F: FnOnce(&mut SignalListing) -> U,
    {
        let mut listing = self.signal_map.entry(sid).or_default();
        f(listing.value_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_listing_defaults_to_uninitialized_empty_state() {
        let listing = SignalListing::default();

        assert!(!listing.is_initialized());
        assert!(listing.get_signals().is_empty());
    }

    #[test]
    fn signal_manager_inserts_and_removes_session_signal_sets() {
        let mgr = SignalMgr::new();

        mgr.with_listing_mut(9, |listing| {
            assert!(!listing.is_initialized());
            assert!(listing.get_signals().is_empty());
        });

        let mut signals = ArrayVec::new();
        signals.push(Some(Signal {
            name: "SIGINT".to_string(),
            stop: true,
            print: false,
            pass: true,
            description: "Interrupt".to_string(),
        }));
        mgr.replace_signals(9, signals);

        mgr.with_listing_mut(9, |listing| {
            assert!(listing.is_initialized());
            assert_eq!(listing.get_signals().len(), 1);
            assert_eq!(
                listing.get_signals()[0]
                    .as_ref()
                    .map(|signal| signal.name.as_str()),
                Some("SIGINT")
            );

            listing.set_initialized(false);
            assert!(!listing.is_initialized());
        });

        mgr.remove_session(9);

        mgr.with_listing_mut(9, |listing| {
            assert!(!listing.is_initialized());
            assert!(listing.get_signals().is_empty());
        });
    }
}
