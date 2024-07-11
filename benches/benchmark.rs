use std::sync::Arc;
use std::task::{Context, Wake, Waker};

use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::{criterion_group, criterion_main};
use futures::{Stream, StreamExt};

use stream_split::Outerleave;

#[derive(Clone, Copy)]
struct NoopWaker;
impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

fn outerleave(c: &mut Criterion) {
    let waker = Arc::new(NoopWaker);
    let waker = Waker::from(waker);
    let mut context = Context::from_waker(&waker);

    c.bench_with_input(BenchmarkId::new("poll_ready", ""), &(), |b, _| {
        b.iter_batched(
            || futures::stream::iter([0, 1, 2, 3, 4]).outerleave().0,
            |mut even| {
                let mut pinned = std::pin::pin!(even);
                pinned.as_mut().poll_next(&mut context)
            },
            criterion::BatchSize::SmallInput,
        );
    });
    c.bench_with_input(BenchmarkId::new("poll_pending", ""), &(), |b, _| {
        let (_, mut odd) = futures::stream::iter([0, 1, 2, 3, 4]).outerleave();
        let mut pinned = std::pin::pin!(odd);
        // Poll once to store waker
        let _ = pinned.as_mut().poll_next(&mut context);
        b.iter(|| pinned.as_mut().poll_next(&mut context));
    });
    c.bench_with_input(BenchmarkId::new("outerleave", ""), &(), |b, _| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter_batched(
                || futures::stream::iter([Ok(0), Ok(1), Ok(2), Ok(3), Ok(4)]).outerleave(),
                |(even, odd)| async move {
                    let (tx, _rx) = futures::channel::mpsc::unbounded();
                    let even_fut = even.forward(tx.clone());
                    let odd_fut = odd.forward(tx);
                    futures::join!(even_fut, odd_fut)
                },
                criterion::BatchSize::SmallInput,
            );
    });
    c.bench_with_input(BenchmarkId::new("spawning", ""), &(), |b, _| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter_batched(
                || futures::stream::iter([Ok::<_, ()>(0), Ok(1), Ok(2), Ok(3), Ok(4)]),
                |mut stream| async move {
                    let (even_tx, _rx) = futures::channel::mpsc::unbounded();
                    let (odd_tx, _rx) = futures::channel::mpsc::unbounded();
                    let jh = tokio::spawn(async move {
                        loop {
                            if let Some(next) = stream.next().await {
                                even_tx.unbounded_send(next).unwrap();
                            }
                            if let Some(next) = stream.next().await {
                                odd_tx.unbounded_send(next).unwrap();
                            } else {
                                break;
                            }
                        }
                    });
                    jh.await
                },
                criterion::BatchSize::SmallInput,
            )
    });
}

criterion_group!(benches, outerleave);
criterion_main!(benches);
