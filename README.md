# Stream Outerleaver

You may be asking yourself, "What on earth is 'outerleaving'?" You'd be right
to. I'm sure there are other names for this, but this is a reference to
`itertool`'s [`interleave`][interleave]. At some point I should spend the five
seconds it would take to find the equivalent of this in `futures` or
`itertools`, but for now, :shrug:

This crate offers traits for splitting [`Stream`s][stream] into two streams
that return alternating elements:

```rust
use futures::StreamExt;
use stream_split::Outerleave;

let stream = futures::stream::iter([0, 1, 2, 3, 4, 5]);
let (even, odd) = stream.outerleave();

let (evens, odds) = futures::join!(
    even.collect::<Vec<_>>(),
    odd.collect::<Vec<_>>(),
);
assert_eq!(evens, [0, 2, 4]);
assert_eq!(odds, [1, 3, 5]);
```

## Testing

Just for myself, here's what I usually run to test because CI for this thing
just isn't worth it:

```shell
cargo test -q && \
    cargo clippy --all-targets -q -- -D warnings && \
    cargo rustdoc -- -D warnings && \
    cargo +nightly miri test -- -q && \
    RUSTFLAGS="--cfg loom" cargo test -q --test loom
```

[interleave]: https://docs.rs/itertools/0.13.0/itertools/trait.Itertools.html#method.interleave
[stream]: https://docs.rs/futures/0.3.30/futures/stream/trait.Stream.html
