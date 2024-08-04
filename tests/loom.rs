#![cfg(loom)]

use std::task::{Context, Poll};

use futures::{Future, StreamExt};
use stream_split::Outerleave;

#[test]
fn handles_stale_wakers() {
    loom::model(|| {
        struct NoopWaker;
        impl std::task::Wake for NoopWaker {
            fn wake(self: std::sync::Arc<Self>) {}
        }
        let stream = futures::stream::iter([0, 1]);
        let (mut evens, mut odds) = stream.outerleave();

        let jh = loom::thread::spawn(move || {
            // Not using loom Arc so we can create a `Waker`
            let waker = std::sync::Arc::new(NoopWaker).into();
            let mut context = Context::from_waker(&waker);
            let next_even = std::pin::pin!(evens.next());
            let _ = next_even.poll(&mut context);
        });

        let mut spawned_odd = tokio_test::task::spawn(odds.next());
        let next_odd = spawned_odd.enter(|cx, task| task.poll(cx));

        jh.join().unwrap();

        assert!(spawned_odd.is_woken() || next_odd == Poll::Ready(Some(1)));
    });
}
