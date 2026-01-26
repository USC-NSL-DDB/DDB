use dashmap::{DashMap, mapref::one::RefMut};
use arrayvec::ArrayVec;

type Signals = ArrayVec<Option<Signal>, 128>;

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
    pub fn insert_signals(&self, sid: u64, signals: Signals) {
        self.signal_map.insert(sid, SignalListing::new_with_signals(signals));
    }
    
    #[inline]
    pub fn lock_signal_listing(&self, sid: u64) -> RefMut<'_, u64, SignalListing> {
        self.signal_map.entry(sid).or_default()
    }
    
}