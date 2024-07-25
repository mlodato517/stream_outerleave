//! See <https://docs.rs/loom/latest/loom/index.html#writing-tests>

#[cfg(loom)]
pub(crate) mod atomic {
    pub(crate) use loom::sync::atomic::{AtomicBool, Ordering};
}
#[cfg(loom)]
pub(crate) use loom::sync::Arc;

#[cfg(not(loom))]
pub(crate) mod atomic {
    pub(crate) use std::sync::atomic::{AtomicBool, Ordering};
}

#[cfg(not(loom))]
pub(crate) use std::sync::Arc;
