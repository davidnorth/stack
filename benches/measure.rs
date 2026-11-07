//! Focus measurement stage benchmarks from `docs/quality.md` §6.
//!
//! The pipeline stage is pyramid build -> noise estimate -> energy maps ->
//! cross-level aggregate, so each component is reported separately.

mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use focus_stack::focus::energy::{EnergyMaps, EnergyOptions};
use focus_stack::focus::noise::{self, DEFAULT_NOISE_K};
use focus_stack::focus::pyramid::Pyramid;

fn bench_measure(criterion: &mut Criterion) {
    for size in common::SIZES {
        let luma = common::synthetic_luma(size.width, size.height, 0x51a1_0002);

        criterion.bench_function(&format!("measure/pyramid_{}", size.name), |bencher| {
            bencher.iter(|| {
                let mut pyramid = Pyramid::new();
                pyramid.build(black_box(&luma), size.width, size.height);
                black_box(pyramid)
            });
        });

        let mut pyramid = Pyramid::new();
        pyramid.build(&luma, size.width, size.height);
        let noise = noise::estimate(&pyramid, DEFAULT_NOISE_K);

        criterion.bench_function(&format!("measure/noise_{}", size.name), |bencher| {
            bencher.iter(|| black_box(noise::estimate(black_box(&pyramid), DEFAULT_NOISE_K)));
        });

        let energy = EnergyMaps::compute(&pyramid, &noise, EnergyOptions::default());
        criterion.bench_function(&format!("measure/energy_{}", size.name), |bencher| {
            bencher.iter(|| {
                black_box(EnergyMaps::compute(
                    black_box(&pyramid),
                    black_box(&noise),
                    EnergyOptions::default(),
                ))
            });
        });

        criterion.bench_function(&format!("measure/aggregate_{}", size.name), |bencher| {
            bencher.iter(|| black_box(energy.aggregate()));
        });
    }
}

criterion_group! {
    name = benches;
    config = common::criterion_config();
    targets = bench_measure
}
criterion_main!(benches);
