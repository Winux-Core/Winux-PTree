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
├── ptree-scheduler  (scheduled refresh facade)
│   ├── ptree-scheduler-windows (Windows Task Scheduler impl)
│   └── ptree-scheduler-unix    (cron-based impl)
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
sudo bash scripts/linux/install-linux.sh
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
sudo bash scripts/linux/update-driver.sh
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

### Windows Install (scheduled refresh optional)

```powershell
powershell -ExecutionPolicy Bypass -File scripts/windows/install-windows.ps1
# Add -RegisterScheduledTask to set up a 30-minute refresh
```

Update after pulling/changing code:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/windows/update-driver.ps1
```

Performance tuning:
- Set `PTREE_THREADS="1"` in `/etc/default/ptree-driver` if your scan roots are small and lock contention outweighs parallelism.
- Adjust `PTREE_ARGS` (default: `--quiet --cache-ttl 0`) for refresh behavior.

## Usage

```bash
# Basic usage - show current directory tree
ptree

# Scan a specific path (supports ~ expansion)
ptree ~/Desktop/path --max-depth 2 --stats

# Force a full rescan of the default root
# Windows: selected drive root
# Unix/Linux: /
ptree --force

# JSON output with depth limit
ptree ~/Desktop/path --format json --max-depth 2

# Warm-cache timing check
# Run twice with the same cache dir; second run should show
# "Execution Mode: CACHED (< 1 hour)" and "Lazy Load Time"
ptree ~/Desktop/path --cache-dir /tmp/ptree-demo-cache --max-depth 2 --stats

# Show hidden files
ptree --hidden

# Rebuild cache with skip filters and print skip statistics
ptree ~/Desktop/path --force --skip .git,node_modules --skip-stats

# Update cache without printing the tree
ptree ~/Desktop/path --quiet --stats

# Setup automatic cache refresh (every 30 minutes)
ptree --scheduler

# Custom cache location
ptree ~/Desktop/path --cache-dir /tmp/ptree-demo-cache
```

Notes:
- `PATH` is positional: use `ptree /some/path`, not `ptree --path /some/path`.
- `--skip` affects traversal and cache refresh. If you change skip rules on an existing cache, use `--force` or a fresh `--cache-dir`.

### Command-Line Options

```
Usage: ptree [OPTIONS] [PATH]

Arguments:
    [PATH]                           Optional path to scan (overrides drive); supports ~ expansion

Options:
    -d, --drive <DRIVE>              Drive letter (e.g. C, D) [default: C]
    -a, --admin                      Enable admin mode to scan system directories
    -f, --force                      Force full rescan (ignore cache)
        --cache-ttl <CACHE_TTL>      Cache time-to-live in seconds (default: 3600)
        --cache-dir <CACHE_DIR>      Override cache directory location
        --no-cache                   Disable cache entirely (scan fresh every time)
    -q, --quiet                      Suppress tree output (useful when just updating cache)
        --format <FORMAT>            Output format: tree or json [default: tree]
        --color <COLOR>              Color output: auto, always, never [default: auto]
        --size                       Include directory sizes in output
        --file-count                 Include file count per directory
    -m, --max-depth <MAX_DEPTH>      Maximum depth to display
    -s, --skip <SKIP>                Directories to skip (comma-separated)
        --hidden                     Show hidden files
    -j, --threads <THREADS>          Maximum worker threads (default: up to 4, or CPU cores with --force)
        --stats                      Display summary statistics (total dirs, files, timing, cache location)
        --skip-stats                 Show skip statistics (directories skipped during traversal)
        --scheduler                  Setup automatic cache refresh every 30 minutes (Windows Task Scheduler / cron)
        --scheduler-uninstall        Remove scheduled cache updates
        --scheduler-status           Show scheduler status
    -h, --help                       Print help
```

## Cache Behavior

The cache operates on a time-to-live model:

- **First run**: Full disk scan stored in cache
- **Subsequent runs**: Cache returned when age < TTL (default 1 hour) and the live root summary still matches the persisted cache summary
- **Cache location**: `%APPDATA%\ptree\cache\ptree.dat` (Windows),
  `$XDG_CACHE_HOME/ptree/ptree.dat` or `~/.cache/ptree/ptree.dat` (Linux/Unix)
- **Cache format**: Rkyv binary with lazy-loading index for O(1) cold start
- **Cached output path**: Cache hits load the index immediately, then expand only the visible tree from the root. `--stats` reports this work as `Lazy Load Time`.
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
- Optional always-on systemd watcher via `bash scripts/linux/install-linux.sh`
- No incremental update support
- Auto-update failures on wake can trigger a one-time egui permission prompt
- Scans outside your home directory require root (`sudo ptree` when scanning /, /opt, etc.)

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
