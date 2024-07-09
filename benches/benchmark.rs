use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::{criterion_group, criterion_main};
use futures::StreamExt;

use stream_split::Outerleave;

fn outerleave(c: &mut Criterion) {
    c.bench_with_input(BenchmarkId::new("poll_ready", ""), &(), |b, _| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter_batched(
                || futures::stream::iter([0, 1, 2, 3, 4]).outerleave().0,
                |mut even| async move { futures::poll!(even.next()) },
                criterion::BatchSize::SmallInput,
            );
    });
    c.bench_with_input(BenchmarkId::new("poll_pending", ""), &(), |b, _| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter_batched(
                || futures::stream::iter([0, 1, 2, 3, 4]).outerleave().1,
                |mut odd| async move { futures::poll!(odd.next()) },
                criterion::BatchSize::SmallInput,
            );
    });
    c.bench_with_input(BenchmarkId::new("collect", ""), &(), |b, _| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter_batched(
                || futures::stream::iter([Ok(0), Ok(1), Ok(2), Ok(3), Ok(4)]).outerleave(),
                |(even, odd)| async move {
                    let (tx, rx) = futures::channel::mpsc::unbounded();
                    let even_fut = even.forward(tx.clone());
                    let odd_fut = odd.forward(tx);
                    let _ = futures::join!(even_fut, odd_fut);
                    rx.collect::<Vec<_>>().await
                },
                criterion::BatchSize::SmallInput,
            );
    });
    c.bench_with_input(BenchmarkId::new("spawning", ""), &(), |b, _| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter_batched(
                || futures::stream::iter([Ok::<_, ()>(0), Ok(1), Ok(2), Ok(3), Ok(4)]).outerleave(),
                |(mut even, mut odd)| async move {
                    let (tx, rx) = futures::channel::mpsc::unbounded();
                    let jh = tokio::spawn(async move {
                        loop {
                            if let Some(next) = even.next().await {
                                tx.unbounded_send(next).unwrap();
                            }
                            if let Some(next) = odd.next().await {
                                tx.unbounded_send(next).unwrap();
                            } else {
                                break;
                            }
                        }
                    });
                    jh.await.unwrap();
                    rx.collect::<Vec<_>>().await
                },
                criterion::BatchSize::SmallInput,
            )
    });
}

criterion_group!(benches, outerleave);
criterion_main!(benches);
