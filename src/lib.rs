// 2. Documentation
// 3. Code Organization
//    - Consolidate structs with const generics? (pin_project doesn't handle this)
// 4. Better tests including Loom, Miri (can this work with tokio?), and quickcheck or proptest
// 5. Performance optimizations
//    - Get rid of Mutex (AtomicBool governs mutual exclusion)
//    - Improved Ordering variant

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
struct SharedState<S> {
    stream: Mutex<S>,
    odd_next: AtomicBool,
    odd_waker: Mutex<Option<Waker>>,
    even_waker: Mutex<Option<Waker>>,
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

        let odd_next = this.shared_state.odd_next.load(Ordering::SeqCst);
        if odd_next {
            let mut waker = this.shared_state.even_waker.lock().unwrap();
            *waker = Some(cx.waker().clone());
            return Poll::Pending;
        }

        let inner_stream = &mut *this.shared_state.stream.lock().unwrap();

        // SAFETY: This probably isn't actually safe, but I'm hoping the fact that we own this
        // stream and we maybe promise not to move it is fine. We'll test with MIRI later and still
        // probably be wrong...
        let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
        let next_item = ready!(inner_stream.as_mut().poll_next(cx));

        this.shared_state.odd_next.store(true, Ordering::SeqCst);
        if let Some(waker) = this.shared_state.odd_waker.lock().unwrap().take() {
            waker.wake();
        }
        Poll::Ready(next_item)
    }
}
impl<S: Stream> Stream for Odd<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let odd_next = this.shared_state.odd_next.load(Ordering::SeqCst);
        if !odd_next {
            let mut waker = this.shared_state.odd_waker.lock().unwrap();
            *waker = Some(cx.waker().clone());
            return Poll::Pending;
        }

        let inner_stream = &mut *this.shared_state.stream.lock().unwrap();

        // SAFETY: This probably isn't actually safe, but I'm hoping the fact that we own this
        // stream and we maybe promise not to move it is fine. We'll test with MIRI later and still
        // probably be wrong...
        let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
        let next_item = ready!(inner_stream.as_mut().poll_next(cx));

        this.shared_state.odd_next.store(false, Ordering::SeqCst);
        if let Some(waker) = this.shared_state.even_waker.lock().unwrap().take() {
            waker.wake();
        }
        Poll::Ready(next_item)
    }
}

impl<S: Stream> Outerleave for S {
    fn outerleave(self) -> (Even<S>, Odd<S>) {
        let shared_state = Arc::new(SharedState {
            stream: Mutex::new(self),
            odd_next: AtomicBool::new(false),
            odd_waker: Mutex::new(None),
            even_waker: Mutex::new(None),
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

    use async_stream::stream;
    use futures::{FutureExt, StreamExt};
    use itertools::Itertools;

    #[tokio::test]
    async fn outerleaves() {
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
    }

    #[tokio::test]
    async fn handles_different_yielding_patterns() {
        let sleeps: Vec<_> = (0..6)
            .map(|n| std::time::Duration::from_millis(n * 10))
            .collect();
        let sleeps = sleeps.iter().copied().permutations(sleeps.len()).take(300);
        for sleeps in sleeps {
            let stream = stream! {
                for (i, sleep) in (0..6).zip(sleeps) {
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
            assert_eq!(received, [0, 1, 2, 3, 4, 5]);
        }
    }
}
