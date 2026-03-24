# Winux-PTree Architectural Fixes

Combined and deduplicated from `LLM_Conv_C1.md` and `LLM_Conv_C2.md`.

Priority legend:
- `P1` is highest priority.
- `P15` is lowest priority in this batch.
- Ranking is based on end-to-end impact first, then correctness/risk reduction, then cleanup.

## Goals

- Stop doing whole-tree work for hot-cache and small-change cases.
- Remove avoidable heap churn, lock contention, and syscall overhead.
- Make cache persistence, stats, and CLI behavior match the real implementation.

## P3 - Implement real incremental refresh instead of root rescans

Why:
- The Windows driver now applies incremental refreshes, but other caller paths still do not source localized live change feeds.
- Journal path reconstruction is better than `root + filename`, but it is still only as complete as the parent-reference path cache the driver has observed.

Primary targets:
- `src/main.rs:61`
- `Driver/src/service.rs:133`
- `Driver/src/usn_journal.rs:353`

Actions:
- Feed watcher or journal change events into `traverse_disk_incremental` from every real caller path that can provide them, not just the Windows service.
- Strengthen USN path reconstruction so parent-reference misses do not collapse back to root-joined paths.
- Add a Windows integration test or harness that exercises real journal-driven incremental updates against a persisted cache.

Validation:
- End-to-end: modify one small subtree and confirm the real caller path invokes incremental refresh, not just the unit-tested traversal entry point.
- On Windows, verify localized journal changes update only the changed subtree plus affected ancestors and persist the cache correctly.

## P4 - Align the cache correctness story with the implementation

Why:
- Warm-cache acceptance now compares the persisted root summary against a live recursive summary, but the docs and terminology still need to describe the exact guarantee precisely.
- The remaining work here is mostly contract clarity and keeping the check performant as the tree grows.

Primary targets:
- `crates/ptree-traversal/src/traversal.rs:130`
- `crates/ptree-traversal/src/traversal.rs:488`
- `crates/ptree-cache/src/cache.rs:28`

Actions:
- Document that warm-cache reuse now depends on both TTL and a live root-summary match.
- Keep the live-summary check scoped enough that hot-cache reads stay materially faster than a full traversal.
- Add targeted tests for rename/create/delete and size-only mutations against the warm-cache acceptance path.

Validation:
- Add targeted correctness tests for rename/create/delete propagation and stale-cache detection.
- Ensure docs and runtime stats use the same freshness terminology.

## P5 - Eliminate cache cloning in traversal

Why:
- Traversal still deep-clones `DiskCache` before parallel work.
- That doubles heap pressure and front-loads warm refresh cost.

Primary targets:
- `crates/ptree-traversal/src/traversal.rs:191`
- `crates/ptree-traversal/src/traversal.rs:248`

Actions:
- Pass `DiskCache` into traversal by ownership rather than `cache.clone()` under `Arc<RwLock<_>>`.
- Return the same cache instance from traversal and keep the synchronization boundary while dropping the deep clone.

Validation:
- Re-run perf and confirm clone/realloc hotspots disappear.
- Measure peak RSS before/after on a warm refresh.

## P6 - Make cache writes change-aware and skip no-op saves

Why:
- Warm refreshes still rewrite full cache state even when nothing changed.
- This adds avoidable write amplification and save latency.

Primary targets:
- `crates/ptree-traversal/src/traversal.rs:277`
- `crates/ptree-cache/src/cache.rs:279`
- `crates/ptree-cache/src/cache.rs:298`
- `crates/ptree-cache/src/cache.rs:325`

Actions:
- Track dirty subtrees or changed entries.
- Skip save entirely when nothing changed.
- Write only touched depth shards where possible instead of rebuilding every `ptree-d*.dat` file.

Validation:
- Confirm unchanged refreshes do not rewrite `ptree.idx` or every depth file.
- Compare save latency for no-op and small-change runs before/after.

## P9 - Replace lock-convoy traversal coordination

Why:
- Global mutexes around the queue, in-progress set, and skip stats cap scaling.
- Threads spend too much time coordinating instead of walking directories.

Primary targets:
- `crates/ptree-traversal/src/traversal.rs:31`
- `crates/ptree-traversal/src/traversal.rs:37`
- `crates/ptree-traversal/src/traversal.rs:47`
- `crates/ptree-traversal/src/traversal.rs:330`
- `crates/ptree-traversal/src/traversal.rs:366`
- `crates/ptree-traversal/src/traversal.rs:437`
- `crates/ptree-traversal/src/traversal.rs:501`

Actions:
- Move to work-stealing deques or sharded per-thread queues.
- Replace global skip-stat locking with per-thread counters plus a final reduction.
- Remove the global `in_progress` hot path if queue ownership already guarantees single processing.

Validation:
- Benchmark traversal at 1, 2, 4, and N threads.
- Confirm multi-thread speedup remains meaningful beyond 2 to 4 threads.

## P10 - Restore a low-syscall Unix directory enumeration path

Why:
- `std::fs::read_dir` plus `entry.file_type()` adds syscall and allocation overhead on Unix/Linux.
- High-fanout directories pay the most.

Primary targets:
- `crates/ptree-traversal/src/traversal.rs:396`
- `crates/ptree-traversal/src/traversal.rs:417`

Actions:
- Reintroduce a Unix fast path using `nix::dir::Dir` or equivalent.
- Keep the standard-library fallback for non-Unix targets.
- Avoid unnecessary metadata fetches on the cold path.

Validation:
- Compare syscall counts and wall-clock time before/after on a large tree.
- Verify parity for symlinks, hidden detection, and skip filtering.

## P11 - Make cache-hit output truly lazy

Why:
- Cache hits still deserialize the whole tree before rendering.
- That defeats the mmap-oriented cache design, especially for shallow views.

Primary targets:
- `src/main.rs:70`
- `crates/ptree-cache/src/cache.rs:409`
- `crates/ptree-cache/src/cache_rkyv.rs:162`

Actions:
- Replace `load_all_entries_lazy` with demand-driven expansion from the root.
- Apply `max_depth` during lazy expansion so shallow reads stay shallow.
- Keep output work proportional to what is actually rendered.

Validation:
- Compare `--max-depth 1` and `--max-depth 2` on a large cache before/after.
- Confirm memory usage stops scaling with total directory count for shallow renders.

## P14 - Shrink cache entry footprint

Why:
- Each cached directory currently stores path information twice.
- This increases serialization size, mmap working set, and memory pressure.

Primary targets:
- `crates/ptree-cache/src/cache.rs:19`
- `crates/ptree-cache/src/cache.rs:116`
- `crates/ptree-cache/src/cache_rkyv.rs:19`

Actions:
- Remove `DirEntry.path` and treat the map key as the canonical path.
- If needed, add component or string interning for repeated child-name storage.

Validation:
- Compare serialized cache size and resident memory before/after on the same snapshot.

## P15 - Add a standing validation and telemetry harness

Why:
- Architectural fixes need durable proof, not one-off spot checks.
- This priority keeps the performance plan from regressing later.

Primary targets:
- `benches/traversal_benchmarks.rs`
- traversal/cache integration tests
- CLI stats tests

Actions:
- Re-run perf after each major fix and archive the before/after evidence.
- Add interrupted-save recovery coverage, cache-hit stats coverage, and one targeted test per CLI flag.
- Track cold scan, warm refresh, shallow render, and no-op refresh as the core acceptance scenarios.

Validation:
- Each priority above should map to at least one automated check or repeatable benchmark.

## Execution order

1. `P1`
2. `P2`
3. `P3`
4. `P4`
5. `P5`
6. `P6`
7. `P7`
8. `P8`
9. `P9`
10. `P10`
11. `P11`
12. `P12`
13. `P13`
14. `P14`
15. `P15`

## Success criteria

- Automated refreshes do not force full rescans by default.
- Post-TTL refreshes scale with changed directories, not total directories.
- No-op refreshes avoid full cache rewrites.
- Cache-hit rendering scales with requested output depth, not total cache size.
- Warm-run stats are accurate.
- Multi-thread traversal shows meaningful speedup beyond 2 to 4 threads.
- Cache persistence survives interruptions without silent corruption.
- CLI flags and documented guarantees match implemented behavior.
