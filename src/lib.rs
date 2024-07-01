// 1. Documentation
// 2. Code Organization
//    - Consolidate structs with const generics? (pin_project doesn't handle this)
// 3. Better tests
//    - Loom
//    - Quickcheck/proptest
//    - tokio::test to skip time
// 4. Performance optimizations
//    - Improve use of pin_project

use std::cell::UnsafeCell;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use futures::{ready, Stream};
use pin_project_lite::pin_project;

pub trait Outerleave {
    fn outerleave(self) -> (Even<Self>, Odd<Self>)
    where
        Self: Sized;
}

// SAFETY: The only issue here is `UnsafeCell` which is not `Sync`. But since we the `odd_next`
// bool controls mutual exclusion of the value, we'll never derefence the pointer on two threads at
// the same time. This is similar to, for example,
// https://marabos.nl/atomics/building-spinlock.html#an-unsafe-spin-lock.
unsafe impl<S: Send> Sync for SharedState<S> {}
struct SharedState<S> {
    stream: UnsafeCell<S>,
    odd_next: AtomicBool,
    wakers: Mutex<SharedWakers>,
}
struct SharedWakers {
    odd_waker: Option<Waker>,
    even_waker: Option<Waker>,
}
pin_project! {
    pub struct Even<S> {
        #[pin]
        shared_state: Arc<SharedState<S>>,
    }
}
pin_project! {
    pub struct Odd<S> {
        #[pin]
        shared_state: Arc<SharedState<S>>,
    }
}

impl<S: Stream> Stream for Even<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let odd_next = this.shared_state.odd_next.load(Ordering::Acquire);
        if odd_next {
            match &mut this.shared_state.wakers.lock().unwrap().even_waker {
                Some(waker) => waker.clone_from(cx.waker()),
                waker @ None => *waker = Some(cx.waker().clone()),
            }
            return Poll::Pending;
        }

        let next_item = {
            // SAFETY: `odd_next` ensures we aren't concurrently doing this on the other stream.
            let inner_stream = unsafe { &mut *this.shared_state.stream.get() };

            // SAFETY: We don't have any semantic moves on this value.
            let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
            ready!(inner_stream.as_mut().poll_next(cx))
        };

        this.shared_state.odd_next.store(true, Ordering::Release);
        if let Some(waker) = this.shared_state.wakers.lock().unwrap().odd_waker.take() {
            waker.wake();
        }
        Poll::Ready(next_item)
    }
}
impl<S: Stream> Stream for Odd<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let odd_next = this.shared_state.odd_next.load(Ordering::Acquire);
        if !odd_next {
            match &mut this.shared_state.wakers.lock().unwrap().odd_waker {
                Some(waker) => waker.clone_from(cx.waker()),
                waker @ None => *waker = Some(cx.waker().clone()),
            }
            return Poll::Pending;
        }

        let next_item = {
            // SAFETY: `odd_next` ensures we aren't concurrently doing this on the other stream.
            let inner_stream = unsafe { &mut *this.shared_state.stream.get() };

            // SAFETY: We don't have any semantic moves on this value.
            let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
            ready!(inner_stream.as_mut().poll_next(cx))
        };

        this.shared_state.odd_next.store(false, Ordering::Release);
        if let Some(waker) = this.shared_state.wakers.lock().unwrap().even_waker.take() {
            waker.wake();
        }
        Poll::Ready(next_item)
    }
}

impl<S: Stream> Outerleave for S {
    fn outerleave(self) -> (Even<S>, Odd<S>) {
        let shared_state = Arc::new(SharedState {
            stream: UnsafeCell::new(self),
            odd_next: AtomicBool::new(false),
            wakers: Mutex::new(SharedWakers {
                odd_waker: None,
                even_waker: None,
            }),
        });
        (
            Even {
                shared_state: Arc::clone(&shared_state),
            },
            Odd { shared_state },
        )
    }
}

#[cfg(test)]
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
