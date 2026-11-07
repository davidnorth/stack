//! Hybrid detail-transfer benchmarks from `docs/quality.md` §6.

mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use focus_stack::stack::hybrid::{self, DetailOptions};

fn bench_hybrid(criterion: &mut Criterion) {
    for size in common::SIZES {
        let dmap = common::synthetic_rgb(size.width, size.height, 0x51a1_0200);
        let pmax = common::synthetic_rgb(size.width, size.height, 0x51a1_0201);

        criterion.bench_function(&format!("stack/hybrid_merge_{}", size.name), |bencher| {
            bencher.iter(|| {
                black_box(hybrid::stack_hybrid(
                    black_box(&dmap),
                    black_box(&pmax),
                    size.width,
                    size.height,
                    DetailOptions::default(),
                ))
            });
        });
    }
}

criterion_group! {
    name = benches;
    config = common::criterion_config();
    targets = bench_hybrid
}
criterion_main!(benches);
