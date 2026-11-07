//! DMap stack benchmarks from `docs/quality.md` §6.

mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use focus_stack::stack::dmap::{self, CostVolume, DmapOptions};

fn bench_dmap(criterion: &mut Criterion) {
    for size in common::SIZES {
        let evidence =
            common::evidence::Evidence::build(size.width, size.height, size.stack_frames);
        let volume = CostVolume::new(&evidence.aggregates, size.width, size.height);
        let views: Vec<&[f32]> = evidence.frames.iter().map(Vec::as_slice).collect();

        criterion.bench_function(&format!("stack/dmap_{}", size.name), |bencher| {
            bencher.iter(|| {
                black_box(dmap::stack_dmap(
                    black_box(&views),
                    size.width,
                    size.height,
                    black_box(&volume),
                    black_box(&evidence.guide),
                    DmapOptions::default(),
                ))
            });
        });
    }
}

criterion_group! {
    name = benches;
    config = common::criterion_config();
    targets = bench_dmap
}
criterion_main!(benches);
