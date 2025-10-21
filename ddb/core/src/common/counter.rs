use lazy_static::lazy_static;
use std::sync::atomic::{AtomicU64, Ordering};

struct SimpleCounter(AtomicU64);

impl SimpleCounter {
    fn new() -> Self {
        SimpleCounter(AtomicU64::new(1))
    }

    fn next(&self) -> u64 {
        // Fetch and add 1, returning the previous value
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

lazy_static! {
    static ref SESSION_COUNTER: SimpleCounter = SimpleCounter::new();
    static ref G_INFERIOR_ID_COUNTER: SimpleCounter = SimpleCounter::new();
    static ref G_THREAD_ID_COUNTER: SimpleCounter = SimpleCounter::new();
    static ref TOKEN_COUNTER: SimpleCounter = SimpleCounter::new();
    static ref GROUP_COUNTER: SimpleCounter = SimpleCounter::new();
    
    static ref RPC_REQ_COUNTER: SimpleCounter = SimpleCounter::new();
}

pub fn next_session_id() -> u64 {
    SESSION_COUNTER.next()
}

pub fn next_g_inferior_id() -> u64 {
    G_INFERIOR_ID_COUNTER.next()
}

pub fn next_g_thread_id() -> u64 {
    G_THREAD_ID_COUNTER.next()
}

pub fn next_token() -> u64 {
    TOKEN_COUNTER.next()
}

pub fn next_group_id() -> u64 {
    GROUP_COUNTER.next()
}

pub fn next_rpc_req_id() -> u64 {
    RPC_REQ_COUNTER.next()
}
