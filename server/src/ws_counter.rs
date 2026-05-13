use std::sync::atomic::{AtomicUsize, Ordering};

static COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn get() -> usize {
    COUNT.load(Ordering::Relaxed)
}

/// RAII guard: 创建时 +1，drop 时 -1。
/// 用 Arc 包裹后可安全 Clone，引用计数归零才真正 -1。
pub struct ConnectionGuard;

impl ConnectionGuard {
    pub fn new() -> Self {
        COUNT.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}
