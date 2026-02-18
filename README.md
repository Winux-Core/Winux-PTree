# PTree - High-Performance Disk Tree Visualization

A fast, cache-first disk tree traversal tool for Windows and Unix systems with multi-threaded parallelism and persistent caching.

## Features

- **Cache-first design**: Near-instant subsequent runs using persistent cache
- **Parallel traversal**: Multi-threaded DFS with configurable thread count
- **Scheduled refreshes**: Automatic cache updates via Windows Task Scheduler or cron
- **Flexible output**: Tree view or JSON output with configurable depth limiting
- **Memory-bounded**: Strict O(n) memory usage guarantees (200 bytes per directory)
- **Cross-platform**: Windows and Unix/Linux support

## Architecture

PTree uses a modular crate-based architecture:

```
ptree (binary)
├── ptree-core       (CLI, types, error handling)
├── ptree-cache      (disk cache, serialization)
├── ptree-traversal  (parallel DFS traversal)
├── ptree-scheduler  (scheduled refresh)
└── ptree-incremental (planned incremental backend)
```

### Key Components

- **ptree-core**: Command-line argument parsing and core types
- **ptree-cache**: In-memory cache with rkyv-based persistence
- **ptree-traversal**: Multi-threaded iterative DFS with batching and lock-free optimization
- **ptree-scheduler**: Task scheduling for automatic cache refresh (30-minute intervals)
- **ptree-incremental**: Placeholder crate for future incremental updates

## Building

### Requirements

- Rust 1.70+
- Windows 10+ or Linux/macOS

### Build

```bash
cargo build --release
```

The release binary will be at `target/release/ptree`.

### Linux Install (systemd watcher + persistent command)

```bash
bash scripts/install-linux.sh
```

This installer will:
- Build and install `ptree` to `/usr/local/bin/ptree`
- Install `/usr/local/bin/Ptree` as a command alias symlink
- Install and enable `ptree-driver.service` with `Nice=-15`
- Start a continuous filesystem watch loop (via `inotifywait`)
- Install and enable `ptree-auto-update.timer` (pull/build/reinstall automatically)
- Install a wake hook that triggers update checks after resume
- On wake update failure, show a one-time egui prompt asking permission to update

Useful commands:

```bash
sudo systemctl status ptree-driver.service
sudo journalctl -u ptree-driver.service -f
sudo systemctl restart ptree-driver.service
```

Update after pulling/changing code:

```bash
bash scripts/update-driver.sh
```

Auto-update configuration:

```bash
/etc/default/ptree-auto-update
```

XDG notes (user-level paths):
- Cache defaults to `$XDG_CACHE_HOME/ptree/ptree.dat` (or `~/.cache/ptree/ptree.dat`).
- Wake prompt one-time marker is stored at `$XDG_STATE_HOME/ptree/update-prompt-shown`
  (or `~/.local/state/ptree/update-prompt-shown`).

Service configuration file:

```bash
/etc/default/ptree-driver
```

Performance tuning:
- Set `PTREE_THREADS="1"` in `/etc/default/ptree-driver` if your scan roots are small and lock contention outweighs parallelism.
- Adjust `PTREE_ARGS` (default: `--quiet --cache-ttl 0`) for refresh behavior.

## Usage

```bash
# Basic usage - show current directory tree
ptree

# Show full drive C:
ptree --force

# JSON output with depth limit
ptree --format json --max-depth 3

# Show hidden files
ptree --hidden

# Display statistics
ptree --stats

# Setup automatic cache refresh (every 30 minutes)
ptree --scheduler

# Custom cache location
ptree --cache-dir "C:\Custom\Path"
```

### Command-Line Options

```
USAGE:
    ptree [OPTIONS]

OPTIONS:
    -d, --drive <DRIVE>              Drive letter (default: C)
    -f, --force                      Force full rescan (ignore cache)
    -a, --admin                      Admin mode (scan system directories)
    --cache-ttl <SECONDS>            Cache time-to-live (default: 3600)
    --cache-dir <DIR>                Custom cache directory
    --no-cache                       Disable cache entirely
    -q, --quiet                      Suppress output
    --format <FORMAT>                Output format: tree or json (default: tree)
    --color <MODE>                   Color output: auto, always, never (default: auto)
    -m, --max-depth <DEPTH>          Maximum display depth
    -j, --threads <COUNT>            Thread count (default: CPU cores * 2)
    --stats                          Show timing statistics
    --skip-stats                     Show skipped directory statistics
    --scheduler                      Install scheduled cache refresh
    --scheduler-uninstall            Remove scheduled refresh
    --scheduler-status               Check scheduler status
```

## Cache Behavior

The cache operates on a time-to-live model:

- **First run**: Full disk scan stored in cache
- **Subsequent runs**: Cache returned if age < TTL (default 1 hour)
- **Cache location**: `%APPDATA%\ptree\cache\ptree.dat` (Windows),
  `$XDG_CACHE_HOME/ptree/ptree.dat` or `~/.cache/ptree/ptree.dat` (Linux/Unix)
- **Cache format**: Rkyv binary with lazy-loading index for O(1) cold start
- **Force rescan**: Use `--force` flag to bypass cache

## Performance

### Benchmarks

Performance varies by system configuration. Expected results on typical systems:

| Operation | Cold Start | Warm Cache | Notes |
|-----------|-----------|-----------|-------|
| First scan (1M dirs) | TBD | - | Full traversal |
| Cached read | - | TBD | ~1ms cold-start |
| Formatting output | TBD | - | Parallel sort |
| Scheduler overhead | - | TBD | 30-min refresh |

*Benchmarks to be filled in after performance testing.*

## Development

### Project Structure

```
PerfTree/
├── src/                 # Binary entrypoint
├── crates/
│   ├── ptree-core/
│   ├── ptree-cache/
│   ├── ptree-traversal/
│   ├── ptree-scheduler/
│   ├── ptree-incremental/
│   ├── ptree-NTFS/      # (placeholder)
│   ├── ptree-USN/       # (placeholder)
│   └── ptree-MFT/       # (placeholder)
├── benches/             # Benchmarks
└── docs/                # Documentation
```

### Running Tests

```bash
cargo test
```

### Running Benchmarks

```bash
cargo bench --bench traversal_benchmarks
```

## Features (Compile-time)

```bash
# Default (with scheduler)
cargo build --release

# Minimal (cache + traversal only)
cargo build --release --no-default-features

# Custom feature selection
cargo build --release --features scheduler
```

## Platform-Specific Notes

### Windows
- Incremental USN Journal updates are not yet implemented
- Windows Task Scheduler integration for scheduled refresh
- System directory skipping (without `--admin` flag)

### Unix/Linux
- Basic traversal and caching
- Cron scheduler support via `ptree --scheduler`
- Optional always-on systemd watcher via `bash scripts/install-linux.sh`
- No incremental update support
- Auto-update failures on wake can trigger a one-time egui permission prompt

## Future Work

- [ ] Fill in performance benchmarks
- [ ] Implement incremental NTFS/USN/MFT crates
- [ ] Web UI for visualization
- [ ] Database export support

## License

Licensed under either:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT license (`LICENSE-MIT`)

## Contributing

See `CONTRIBUTING.md`.
