use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::{ready, Stream};
use pin_project_lite::pin_project;

pub trait Outersperse {
    fn outersperse(self) -> (Even<Self>, Odd<Self>)
    where
        Self: Sized;
}
pin_project! {
    pub struct Even<S> {
        #[pin]
        stream: Arc<Mutex<S>>,
        #[pin]
        odd_next: Arc<AtomicBool>,
    }
}
pin_project! {
    pub struct Odd<S> {
        #[pin]
        stream: Arc<Mutex<S>>,
        #[pin]
        odd_next: Arc<AtomicBool>,
    }
}

impl<S: Stream> Stream for Even<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let odd_next = this.odd_next.load(Ordering::SeqCst);
        if odd_next {
            return Poll::Pending;
        }

        let inner_stream = &mut *this.stream.lock().expect("Ignoring poisoning for now");

        // SAFETY: This probably isn't actually safe, but I'm hoping the fact that we own this
        // stream and we maybe promise not to move it is fine. We'll test with MIRI later and still
        // probably be wrong...
        let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
        let next_item = ready!(inner_stream.as_mut().poll_next(cx));

        this.odd_next.store(true, Ordering::SeqCst);
        Poll::Ready(next_item)
    }
}
impl<S: Stream> Stream for Odd<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let odd_next = this.odd_next.load(Ordering::SeqCst);
        if !odd_next {
            return Poll::Pending;
        }

        let inner_stream = &mut *this.stream.lock().expect("Ignoring poisoning for now");

        // SAFETY: This probably isn't actually safe, but I'm hoping the fact that we own this
        // stream and we maybe promise not to move it is fine. We'll test with MIRI later and still
        // probably be wrong...
        let mut inner_stream = unsafe { Pin::new_unchecked(inner_stream) };
        let next_item = ready!(inner_stream.as_mut().poll_next(cx));

        this.odd_next.store(false, Ordering::SeqCst);
        Poll::Ready(next_item)
    }
}

impl<S: Stream> Outersperse for S {
    fn outersperse(self) -> (Even<S>, Odd<S>) {
        let stream = Arc::new(Mutex::new(self));
        let odd_next = Arc::new(AtomicBool::new(false));
        (
            Even {
                stream: Arc::clone(&stream),
                odd_next: Arc::clone(&odd_next),
            },
            Odd { stream, odd_next },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::{FutureExt, StreamExt};

    #[tokio::test]
    async fn outersperses() {
        let stream = futures::stream::iter([0, 1, 2, 3, 4, 5, 6]);
        let (mut evens, mut odds) = stream.outersperse();

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
}
