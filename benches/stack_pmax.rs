//! PMax stack benchmarks from `docs/quality.md` §6.

mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use focus_stack::focus::confidence::{ConfidenceMap, ConfidenceOptions};
use focus_stack::stack::pmax::{self, WeightOptions};

fn bench_pmax(criterion: &mut Criterion) {
    for size in common::SIZES {
        let evidence =
            common::evidence::Evidence::build(size.width, size.height, size.stack_frames);
        let confidence = ConfidenceMap::compute(
            &evidence.aggregates,
            &evidence.guide,
            size.width,
            size.height,
            &evidence.calibrations,
            Some(&evidence.saturation),
            ConfidenceOptions::default(),
        );

        let views: Vec<&[f32]> = evidence.frames.iter().map(Vec::as_slice).collect();
        criterion.bench_function(&format!("stack/pmax_{}", size.name), |bencher| {
            bencher.iter(|| {
                black_box(pmax::stack(
                    black_box(&views),
                    size.width,
                    size.height,
                    black_box(&evidence.energies),
                    black_box(&confidence),
                    WeightOptions::default(),
                ))
            });
        });
    }
}

criterion_group! {
    name = benches;
    config = common::criterion_config();
    targets = bench_pmax
}
criterion_main!(benches);
