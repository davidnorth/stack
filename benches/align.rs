//! Alignment stage benchmarks from `docs/quality.md` §6.
//!
//! Alignment resolution is capped by `PreprocessParams::DEFAULT_LONG_EDGES`
//! (1024 and 2048 px), so only preprocessing scales with the source size.

mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use focus_stack::Similarity;
use focus_stack::align::ecc::{EccParams, refine_similarity};
use focus_stack::align::phase::phase_correlate;
use focus_stack::align::preprocess::{PreprocessParams, preprocess_rgb};

fn bench_align(criterion: &mut Criterion) {
    let params = PreprocessParams::default();

    for size in common::SIZES {
        let rgb = common::synthetic_rgb(size.width, size.height, 0x51a1_0001);

        criterion.bench_function(&format!("align/preprocess_{}", size.name), |bencher| {
            bencher.iter(|| {
                black_box(preprocess_rgb(
                    black_box(&rgb),
                    size.width,
                    size.height,
                    &params,
                ))
            });
        });

        // The two capped levels are the real inputs to correlation and ECC.
        let levels = preprocess_rgb(&rgb, size.width, size.height, &params);
        let coarse = levels.first().expect("alignment level");
        let fine = levels.last().expect("alignment level");

        let source_windowed = common::shifted(&coarse.windowed, coarse.width, coarse.height, 7, -5);
        criterion.bench_function(&format!("align/phase_{}", size.name), |bencher| {
            bencher.iter(|| {
                black_box(phase_correlate(
                    black_box(&source_windowed),
                    black_box(&coarse.windowed),
                    coarse.width,
                    coarse.height,
                ))
                .expect("phase correlation")
            });
        });

        let source_high_pass = common::shifted(&fine.high_pass, fine.width, fine.height, 7, -5);
        let identity = Similarity {
            a: 1.0,
            b: 0.0,
            tx: 0.0,
            ty: 0.0,
        };
        let ecc = EccParams::default();
        criterion.bench_function(&format!("align/ecc_{}", size.name), |bencher| {
            bencher.iter(|| {
                black_box(refine_similarity(
                    black_box(&source_high_pass),
                    black_box(&fine.high_pass),
                    fine.width,
                    fine.height,
                    black_box(&identity),
                    Some(black_box(&fine.mask)),
                    black_box(&ecc),
                ))
                .expect("ECC refinement")
            });
        });
    }
}

criterion_group! {
    name = benches;
    config = common::criterion_config();
    targets = bench_align
}
criterion_main!(benches);
