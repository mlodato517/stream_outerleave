//! This crate offers traits for splitting [`Stream`]s into two streams that return alternating
//! elements. See the docs on [`Outerleave`] for more details.

// TODOs
// - Code Organization
//    - Consolidate structs with const generics?
//    - Move code out of lib.rs.
//    - Separate modules for waiting vs non-waiting outerleave halves.
// - Better tests
//    - Quickcheck/proptest
//    - tokio::test to skip time

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::task::AtomicWaker;
use futures::{ready, Stream};

use crate::cell::UnsafeCell;
use crate::sync::atomic::{AtomicBool, Ordering};
use crate::sync::Arc;

mod cell;
mod sync;

/// Trait for splitting a [`Stream`] into two halves that return alternating elements from the
/// original.
///
/// # Examples
///
/// ```
/// # futures::executor::block_on(async {
/// use futures::StreamExt;
/// use stream_split::Outerleave;
///
/// let stream = futures::stream::iter([0, 1, 2, 3, 4, 5]);
/// let (even, odd) = stream.outerleave();
///
/// let (evens, odds) = futures::join!(even.collect::<Vec<_>>(), odd.collect::<Vec<_>>());
/// assert_eq!(evens, [0, 2, 4]);
/// assert_eq!(odds, [1, 3, 5]);
/// # });
/// ```
///
/// The streams split from the original stream will wait for each other before producing new
/// elements:
///
/// ```
/// # futures::executor::block_on(async {
/// use futures::{StreamExt, FutureExt};
/// use stream_split::Outerleave;
///
/// let stream = futures::stream::iter([0, 1, 2, 3, 4, 5]);
/// let (mut even, mut odd) = stream.outerleave();
///
/// assert_eq!(even.next().now_or_never(), Some(Some(0)));
/// assert_eq!(even.next().now_or_never(), None);
/// assert_eq!(odd.next().now_or_never(), Some(Some(1)));
/// assert_eq!(even.next().now_or_never(), Some(Some(2)));
/// # });
/// ```
pub trait Outerleave {
    fn outerleave(self) -> (Even<Self>, Odd<Self>)
    where
        Self: Sized;
}

// SAFETY: The only issue here is `UnsafeCell` which is not `Sync`. But since the `odd_next` bool
// controls mutual exclusion of the value, we'll never derefence the pointer on two threads at the
// same time. This is similar to, for example,
// https://marabos.nl/atomics/building-spinlock.html#an-unsafe-spin-lock.
unsafe impl<S: Send> Sync for SharedState<S> {}
struct SharedState<S> {
    stream: UnsafeCell<S>,
    odd_next: AtomicBool,
    odd_waker: AtomicWaker,
    even_waker: AtomicWaker,
}

/// One half a split stream. This half will return the 0th, 2nd, 4th, etc. elements from the
/// original stream.
pub struct Even<S> {
    shared_state: Arc<SharedState<S>>,
}

/// One half a split stream. This half will return the 1st, 3rd, 5th, etc. elements from the
/// original stream.
pub struct Odd<S> {
    shared_state: Arc<SharedState<S>>,
}

impl<S: Stream> Stream for Even<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let odd_next = self.shared_state.odd_next.load(Ordering::Acquire);
        if odd_next {
            self.shared_state.even_waker.register(cx.waker());

            // Check again -- it's possible that the other half updated the bool and woke the Waker
            // before we stored it. If that's the case, we won't be woken, but we can proceed
            // immediately. See <https://docs.rs/futures/latest/futures/task/struct.AtomicWaker.html#examples>.
            let odd_next = self.shared_state.odd_next.load(Ordering::Acquire);
            if odd_next {
                return Poll::Pending;
            }
        }

        let next_item = {
            // SAFETY: `odd_next` ensures we aren't concurrently doing this on the other stream.
            let inner_result = self.shared_state.stream.with_mut(|stream_ptr| {
                let inner_stream = unsafe { &mut *stream_ptr };

                // SAFETY: We don't have any semantic moves on this value.
                let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
                inner_stream.as_mut().poll_next(cx)
            });
            ready!(inner_result)
        };

        self.shared_state.odd_next.store(true, Ordering::Release);
        self.shared_state.odd_waker.wake();
        Poll::Ready(next_item)
    }
}

impl<S: Stream> Stream for Odd<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let odd_next = self.shared_state.odd_next.load(Ordering::Acquire);
        if !odd_next {
            self.shared_state.odd_waker.register(cx.waker());

            // Check again -- it's possible that the other half updated the bool and woke the Waker
            // before we stored it. If that's the case, we won't be woken, but we can proceed
            // immediately. See <https://docs.rs/futures/latest/futures/task/struct.AtomicWaker.html#examples>.
            let odd_next = self.shared_state.odd_next.load(Ordering::Acquire);
            if !odd_next {
                return Poll::Pending;
            }
        }

        let next_item = {
            // SAFETY: `odd_next` ensures we aren't concurrently doing this on the other stream.
            let inner_result = self.shared_state.stream.with_mut(|stream_ptr| {
                let inner_stream = unsafe { &mut *stream_ptr };

                // SAFETY: We don't have any semantic moves on this value.
                let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
                inner_stream.as_mut().poll_next(cx)
            });
            ready!(inner_result)
        };

        self.shared_state.odd_next.store(false, Ordering::Release);
        self.shared_state.even_waker.wake();
        Poll::Ready(next_item)
    }
}

impl<S: Stream> Outerleave for S {
    fn outerleave(self) -> (Even<S>, Odd<S>) {
        let shared_state = Arc::new(SharedState {
            stream: UnsafeCell::new(self),
            odd_next: AtomicBool::new(false),
            odd_waker: AtomicWaker::new(),
            even_waker: AtomicWaker::new(),
        });
        (
            Even {
                shared_state: Arc::clone(&shared_state),
            },
            Odd { shared_state },
        )
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    use std::time::Duration;

    use async_stream::stream;
    use futures::{Future, FutureExt, StreamExt};
    use itertools::Itertools;

    // See https://github.com/rust-lang/miri/issues/602#issuecomment-884019764
    fn miri_test(f: impl Future<Output = ()>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(f)
    }

    #[test]
    fn outerleaves() {
        miri_test(async move {
            let stream = futures::stream::iter([0, 1, 2, 3, 4, 5, 6]);
            let (mut evens, mut odds) = stream.outerleave();

            assert_eq!(evens.next().await, Some(0));
            assert_eq!(odds.next().await, Some(1));
            assert_eq!(evens.next().await, Some(2));

            assert_eq!(evens.next().now_or_never(), None);

            let jh = tokio::spawn(async move {
                assert_eq!(evens.next().await, Some(4));
            });
            assert_eq!(odds.next().await, Some(3));
            jh.await.unwrap();
        });
    }

    #[test]
    fn handles_stream_not_ready() {
        miri_test(async move {
            let stream = futures::stream::iter([0, 1]);
            let (mut evens, mut odds) = stream.outerleave();

            let jh = tokio::spawn(async move { odds.next().await });

            tokio::time::sleep(Duration::from_millis(10)).await;
            assert_eq!(evens.next().await, Some(0));

            assert_eq!(
                tokio::time::timeout(Duration::from_millis(10), jh)
                    .await
                    .unwrap()
                    .unwrap(),
                Some(1)
            );
        })
    }

    #[test]
    fn handles_different_yielding_patterns() {
        miri_test(async move {
            let sleeps: Vec<_> = (0..4).map(|n| Duration::from_millis(n * 10)).collect();
            let sleeps = sleeps.iter().copied().permutations(sleeps.len());
            for sleeps in sleeps {
                let stream = stream! {
                    for (i, sleep) in sleeps.into_iter().enumerate() {
                        tokio::time::sleep(sleep).await;
                        yield Ok(i);
                    }
                };

                let (evens, odds) = stream.outerleave();
                let (tx, rx) = futures::channel::mpsc::unbounded();
                let even_fut = evens.forward(tx.clone());
                let odd_fut = odds.forward(tx);
                let (even_result, odd_result) = futures::join!(even_fut, odd_fut);
                even_result.unwrap();
                odd_result.unwrap();

                let received: Vec<_> = rx.collect().await;
                assert_eq!(received, [0, 1, 2, 3]);
            }
        });
    }
}
