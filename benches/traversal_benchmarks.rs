use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

/// Create a test directory structure for benchmarking
fn create_test_tree(root: &Path, depth: usize, breadth: usize) -> std::io::Result<usize> {
    let mut count = 0;

    fn recursive_create(parent: &Path, depth: usize, breadth: usize, count: &mut usize) -> std::io::Result<()> {
        if depth == 0 {
            return Ok(());
        }

        for i in 0..breadth {
            let dir = parent.join(format!("dir_{:03}_{:03}", depth, i));
            fs::create_dir_all(&dir)?;
            *count += 1;
            recursive_create(&dir, depth - 1, breadth / 2, count)?;
        }

        Ok(())
    }

    recursive_create(root, depth, breadth, &mut count)?;
    Ok(count)
}

/// Benchmark directory tree traversal with different directory counts
fn bench_tree_traversal(c: &mut Criterion) {
    let temp_dir = std::env::temp_dir().join("ptree_bench");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let mut group = c.benchmark_group("tree_traversal");
    group.sample_size(10); // Reduce sample size since these are slow operations
    group.measurement_time(Duration::from_secs(30));

    // Benchmark different directory tree sizes
    for (depth, breadth) in &[(3, 4), (4, 3), (5, 2)] {
        let test_root = temp_dir.join(format!("test_d{}_b{}", depth, breadth));
        fs::create_dir_all(&test_root).unwrap();

        let dir_count = create_test_tree(&test_root, *depth, *breadth).unwrap();

        group.bench_with_input(BenchmarkId::from_parameter(format!("{} dirs", dir_count)), &dir_count, |b, _| {
            b.iter(|| {
                // Simple traversal counting directories
                let mut count = 0;
                fn walk(path: &Path, count: &mut usize) -> std::io::Result<()> {
                    for entry in fs::read_dir(path)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.is_dir() {
                            *count += 1;
                            walk(&path, count)?;
                        }
                    }
                    Ok(())
                }
                walk(&test_root, &mut count).ok();
                black_box(count)
            })
        });
    }

    group.finish();
    let _ = fs::remove_dir_all(&temp_dir);
}

/// Benchmark directory sorting with different sizes
fn bench_directory_sorting(c: &mut Criterion) {
    let mut group = c.benchmark_group("directory_sorting");

    // Generate random names of varying quantities
    for size in [10, 50, 100, 500, 1000].iter() {
        let names: Vec<String> = (0..*size).map(|i| format!("directory_name_{:04}", i)).collect();

        group.bench_with_input(BenchmarkId::from_parameter(format!("{} items", size)), size, |b, _| {
            b.iter(|| {
                let mut sorted = black_box(names.clone());
                sorted.sort();
                sorted
            })
        });
    }

    group.finish();
}

/// Benchmark parallel sorting threshold
fn bench_parallel_sorting(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_sorting");

    for size in [50, 100, 500, 1000, 5000].iter() {
        let mut names: Vec<String> = (0..*size).map(|i| format!("directory_name_{:04}", i)).collect();

        group.bench_with_input(BenchmarkId::from_parameter(format!("sequential_{}", size)), size, |b, _| {
            b.iter(|| {
                let mut sorted = black_box(names.clone());
                sorted.sort();
                sorted
            })
        });

        group.bench_with_input(BenchmarkId::from_parameter(format!("parallel_{}", size)), size, |b, _| {
            b.iter(|| {
                use rayon::slice::ParallelSliceMut;
                let mut sorted = black_box(names.clone());
                sorted.par_sort();
                sorted
            })
        });
    }

    group.finish();
}

/// Benchmark cache serialization/deserialization
fn bench_cache_operations(c: &mut Criterion) {
    use std::collections::HashMap;

    let mut group = c.benchmark_group("cache_operations");

    // Create test data of varying sizes
    for size in [100, 1000, 10000].iter() {
        let mut entries = HashMap::new();
        for i in 0..*size {
            entries.insert(PathBuf::from(format!("C:\\path\\to\\dir\\{}", i)), format!("dir_{}", i));
        }

        group.bench_with_input(BenchmarkId::from_parameter(format!("serialize_{}", size)), size, |b, _| {
            b.iter(|| {
                let _serialized = bincode::serialize(black_box(&entries)).unwrap();
            })
        });

        let serialized = bincode::serialize(&entries).unwrap();

        group.bench_with_input(BenchmarkId::from_parameter(format!("deserialize_{}", size)), size, |b, _| {
            b.iter(|| {
                let _deserialized: HashMap<PathBuf, String> = bincode::deserialize(black_box(&serialized)).unwrap();
            })
        });
    }

    group.finish();
}

/// Benchmark file reading from different depths
fn bench_file_enumeration(c: &mut Criterion) {
    let temp_dir = std::env::temp_dir().join("ptree_file_bench");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let mut group = c.benchmark_group("file_enumeration");
    group.sample_size(20);

    // Create directories with different numbers of files
    for file_count in [10, 50, 100, 500].iter() {
        let test_dir = temp_dir.join(format!("files_{}", file_count));
        fs::create_dir_all(&test_dir).unwrap();

        for i in 0..*file_count {
            fs::File::create(test_dir.join(format!("file_{:04}.txt", i))).unwrap();
        }

        group.bench_with_input(BenchmarkId::from_parameter(format!("{} files", file_count)), file_count, |b, _| {
            b.iter(|| {
                let mut count = 0;
                for _entry in fs::read_dir(black_box(&test_dir)).unwrap() {
                    count += 1;
                }
                count
            })
        });
    }

    group.finish();
    let _ = fs::remove_dir_all(&temp_dir);
}

criterion_group!(
    benches,
    bench_tree_traversal,
    bench_directory_sorting,
    bench_parallel_sorting,
    bench_cache_operations,
    bench_file_enumeration
);
criterion_main!(benches);
