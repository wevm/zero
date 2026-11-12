# Benchmarks

`benchmarks` (the `zero-benchmarks` crate, not published) has ports of the
JS client's benchmarks under the same names, and of the Swift client's own
(`swift/Sources/ZeroBenchmarks`, see `swift/BENCHMARKS.md`), so the three
clients compare on the same data:

```sh
cd rust
cargo run --release -p zero-benchmarks                 # everything, about two minutes
cargo run --release -p zero-benchmarks -- hydration "pokes > put 100" --time 3 --json out.json
```

A filter is a substring of `group > benchmark`. Each benchmark runs for
`--time` seconds (default 1) after a warm-up and reports the median time per
call with the 75th and 99th percentiles and the fastest, as
`packages/shared/src/bench.ts` does. Every benchmark checks that it computed
the right rows, and exits non-zero when one didn't.

The JS sides:

| Rust group                            | JS                                                                                |
| ------------------------------------- | --------------------------------------------------------------------------------- |
| `zero.bench.ts`                       | `pnpm --filter zero-client bench`                                                 |
| `hash.bench.ts`                       | `pnpm --filter shared bench --project shared/bench/node src/hash.bench.ts`        |
| `memory`, `hydration`, `mutations`    | `pnpm --filter zero-conformance bench` (`src/bench/client.bench.ts`)              |
| `pokes`, `first sync`, the round trip | none: the JS client applies pokes on the next frame, so a JS timing measures that |

Two of Swift's aren't ported: `btree-set.bench.ts` (the memory sources'
B+ tree is internal to the crate; the benchmarks time it through views and
mutations) and the optimistic update with a live query of structs (there is
no typed live query; an app reads a view's rows as structs, which
`hydration` times).

The benchmarks don't run in CI; numbers from shared runners vary too much to
gate on. CI builds them with the workspace (`cargo clippy --all-targets`,
`cargo test`), so they keep compiling.

## Results

Lower is better. The rows without a mark are medians of five runs of
`--time 2 memory hydration` taken in turns, rotating the order of the
three clients, on a quiet Amp orb (an 8-core Intel Xeon at 2.6 GHz with
15 GB), with Rust 1.99.1 (release build), Swift 6.2.4 (`swift build -c
release`), and JS on Node 22.23, the version the repository pins, under
vitest.

The rows marked † are medians of three runs from an earlier, slower orb,
where Rust and Swift ran 2.2–2.4 times slower than on the quiet one and
JS ran on Node 24.22, which runs these benchmarks about twice as fast as
Node 22. Compare the clients within a † row, not with the other rows.
Shared machines vary this much, so compare clients on the same machine, in
turns.

The rows marked ‡ are medians of three runs of `--time 2 mutations` taken
in turns on another orb (Swift's `mutations` with its view's size changed
to match, and `client.bench.ts` for JS), where materializing 100,000 rows
took 20.5 ms in Rust. Compare the clients within a ‡ row.

In short: Rust is 6–9× faster than JS and 1.5–1.8× faster than Swift on
views of 1,000 to 100,000 rows, and 1.7× faster than Swift reading them as
structs. It uses about half of JS's memory and two thirds of Swift's. Each
native client's cost per row is about the same at 10,000 and 100,000
rows; JS's is 1.6 times as much at 100,000.

| Benchmark                                  | JS (Node)                   | Rust                     | Swift (Linux)            |
| ------------------------------------------ | --------------------------- | ------------------------ | ------------------------ |
| zero.bench: all 1000 rows x 10 columns †   | 236 µs                      | 52.1 µs                  | 91.6 µs                  |
| zero.bench: pk = N †                       | 11.0 µs                     | 4.60 µs                  | 21.3 µs                  |
| zero.bench: lower 500 rows †               | 152 µs                      | 54.6 µs                  | 130 µs                   |
| materialize 1,000 rows                     | 279 µs                      | 42 µs                    | 74 µs                    |
| materialize 10,000 rows                    | 2.74 ms                     | 470 µs                   | 715 µs                   |
| materialize 100,000 rows                   | 43.1 ms                     | 4.64 ms                  | 7.04 ms                  |
| memory for 100,000 rows and a view of them | 158 MB                      | 82.7 MB                  | 126 MB                   |
| optimistic update with a 1000-row view †   | 274 µs                      | 15.9 µs                  | 39.5 µs                  |
| optimistic update, 1,000-row view ‡        | 465 µs                      | 14.5 µs                  | 39.3 µs                  |
| optimistic update, 10,000-row view ‡       | 767 µs                      | 16.4 µs                  | 43.6 µs                  |
| optimistic update, 100,000-row view ‡      | 1.25 ms                     | 18.8 µs                  | 47.5 µs                  |
| h32, 64 / 1024 / 8192 chars †              | 433 ns / 3.87 µs / 32.3 µs  | 10 ns / 151 ns / 1.21 µs | 27 ns / 172 ns / 1.23 µs |
| h64, 64 / 1024 / 8192 chars †              | 703 ns / 6.37 µs / 51.5 µs  | 22 ns / 303 ns / 2.42 µs | 42 ns / 323 ns / 2.45 µs |
| h128, 64 / 1024 / 8192 chars †             | 1.29 µs / 12.5 µs / 95.3 µs | 41 ns / 605 ns / 4.85 µs | 67 ns / 628 ns / 4.88 µs |

As `hash.bench.ts` notes, vitest keeps imported functions from being
inlined, so its JS hash numbers are pessimistic; bundled, they are a few
times faster. The memory figures are the change in resident memory
(`VmRSS`).

Rust and Swift only:

| Benchmark                                                   | Rust    | Swift (Linux) |
| ----------------------------------------------------------- | ------- | ------------- |
| materialize 1,000 rows as structs †                         | 121 µs  | 240 µs        |
| materialize 10,000 rows as structs                          | 1.15 ms | 1.98 ms       |
| materialize 100,000 rows as structs                         | 11.2 ms | 19.6 ms       |
| poke putting 1 row, 1000-row view †                         | 12.4 µs | 49.1 µs       |
| poke putting 100 rows, 1000-row view †                      | 485 µs  | 1.18 ms       |
| poke putting 1000 rows, 1000-row view †                     | 5.01 ms | 10.8 ms       |
| round trip (update, push, confirming poke), 1000-row view † | 39.7 µs | 118 µs        |
| round trip, 1,000-row view ‡                                | 42.5 µs | 118 µs        |
| round trip, 10,000-row view ‡                               | 47.1 µs | 129 µs        |
| round trip, 100,000-row view ‡                              | 76.7 µs | 134 µs        |
| first sync: zbugs poke of 2,058 rows, issue list view †     | 15.3 ms | 42.9 ms       |
| first sync: the same poke, no view †                        | 8.86 ms | 24.4 ms       |

`first sync` replays zbugs' first sync as the zbugs-swift app got it, with
the rows Swift's benchmark generates from the same seed: one poke of 21
parts with 516 issues, 894 comments, 603 issue labels, 30 labels, 14 users,
and a project, into a fresh client with the issue list's first page of
`issueListV2` (or no view, for the cost of the poke itself). Each sample is
timed from the poke's frames arriving to the view being flushed.

## What they found

- **Every write copied the replica.** The replica's map was an
  `Rc<BTreeMap>` shared by its snapshots, so the first write to a copy
  cloned every row and dropping the old copy freed them: an optimistic
  update, a poke, and a rebase each cost O(rows), and 80% of an
  optimistic update went to that copy and its drop. It is now a persistent
  B+tree like Replicache's, whose writes copy only the nodes on their
  path, with each entry behind an `Rc`. With 1000 rows, an optimistic
  update went from 1.17 ms to 25 µs, a poke of one row from 1.15 ms to
  21 µs, and a round trip from 2.28 ms to 52 µs.
- **Rows hashed their column names.** A JSON object kept a SipHash index
  of its keys once it had more than 8, so every column of a 10-column row
  was found by hashing, and each row kept its keys twice. Objects of up to
  16 keys now scan them: all 1000 rows went from 106 to 57 µs,
  materializing 100,000 rows from 64 to 31 ms, a poke of 1000 rows from 28
  to 15 ms, and the memory of 100,000 rows from 735 to 467 MB.
- **Rows were copied, not shared.** A poke's row was cloned from the
  parsed message into the patch, the replica, the diff, and the sources,
  and the replica and the sources each kept their own copy, where JS
  shares one object. A `json::Object` now keeps its entries behind an
  `Arc` that its copies share until one is changed, so every clone of a
  row is a reference count. The memory of 100,000 rows went from 467 to
  181 MB, a first sync from 25.7 to 19.0 ms (17.9 to 9.9 ms without a
  view), a poke of 1000 rows from 15.0 to 8.8 ms, and materializing
  100,000 rows from 32 to 23 ms.
- **Every row had its own column names.** A row kept a copy of each of
  its column names next to its values, so 100,000 rows of 10 columns kept
  a million strings, and a synced row allocated each of them when it was
  parsed. A `json::Object` now keeps its keys apart from its values, in a
  list that the rows of a table share, their layout, as Swift keys rows by
  column index and JS engines share hidden classes. The parser follows a
  recent object's keys while the next object has the same ones, so a
  synced row allocates neither its keys nor their list, and comparators
  and filters remember where their column was. Run in turns on the
  slower orb: the memory of 100,000 rows went from 181 to 83 MB,
  materializing them from 20.9 to 10.5 ms and reading them as structs
  from 48.3 to 23.8 ms, 10,000 rows from 678 to 565 µs (as structs 1.99
  to 1.42 ms), a poke of 1000 rows from 7.74 to 5.01 ms, and a first
  sync from 17.0 to 15.3 ms. Materializing 100,000 rows went from 3.1
  times the cost per row of 10,000 to 1.9 times there; on the quiet orb
  it costs the same per row (46 ns at 100,000 rows, 47 ns at 10,000).
  The slower orb's extra cost at 100,000 rows is cache misses: callgrind
  counts the same instructions per row at both sizes, and the rows of
  100,000 no longer fit in the caches.

- **Every mutation copied the sources.** A mutation runs on a fork of
  the views' sources, and each index was a sorted `Vec` behind an `Rc`,
  so the mutation's first write to a table copied the whole index: an
  optimistic update cost O(rows), 852 µs with a 100,000-row view, where
  Swift's took 48 µs. The indexes are now persistent B+ trees, as in
  Swift, whose writes copy only the nodes on their path. In turns on the
  ‡ orb, an optimistic update with a view of 1,000, 10,000, and 100,000
  rows went from 16.2, 72.9, and 852 µs to 14.5, 16.4, and 18.8 µs, and
  a round trip from 43.2, 75.6, and 721 µs to 42.5, 47.1, and 76.7 µs.
  Materializing and memory didn't move (20.4 and 20.5 ms for 100,000
  rows; 82.8 and 81.5 MB).

Where Rust is still behind:

- **The smallest changes.** Putting a row in its table's layout compares
  its column names with the layout's, so a poke of one row went from 11.4
  to 12.4 µs; `pk = N`, an optimistic update, and a round trip are 3–4%
  slower than before it.
