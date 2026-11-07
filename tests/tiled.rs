//! Tiled PMax blending: seam-free tiles and the 60 MP memory budget.

use focus_stack::focus::confidence::ConfidenceMap;
use focus_stack::focus::energy::{EnergyMaps, EnergyOptions};
use focus_stack::focus::noise::NoiseModel;
use focus_stack::focus::pyramid::Pyramid;
use focus_stack::stack::pmax::{BlendOptions, LevelWeights, WeightOptions, blend};
use focus_stack::stack::tiled::{TiledOptions, blend as tiled_blend, plan, working_set_bytes};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAMES: usize = 3;

fn synthetic_frames() -> Vec<Vec<f32>> {
    (0..FRAMES)
        .map(|frame| {
            let mut rgb = vec![0.0f32; (WIDTH * HEIGHT * 3) as usize];
            for y in 0..HEIGHT as usize {
                for x in 0..WIDTH as usize {
                    let index = y * WIDTH as usize + x;
                    let fx = x as f32;
                    let fy = y as f32;
                    let detail = 0.35
                        + 0.20 * (fx * 0.31 + frame as f32 * 0.7).sin()
                        + 0.15 * (fy * 0.23).cos()
                        + 0.05 * ((fx + 3.0 * fy) * 0.9).sin();
                    let left_focus = if x < (WIDTH as usize) / 2 {
                        frame == 0
                    } else {
                        frame == 2
                    };
                    let sharpness = if left_focus { 1.0 } else { 0.35 };
                    rgb[index * 3] = (detail * sharpness + 0.02).clamp(0.0, 1.0);
                    rgb[index * 3 + 1] = (detail * 0.8 * sharpness + 0.05).clamp(0.0, 1.0);
                    rgb[index * 3 + 2] = (detail * 0.6 * sharpness + 0.08).clamp(0.0, 1.0);
                }
            }
            rgb
        })
        .collect()
}

fn weights(frames: &[Vec<f32>]) -> LevelWeights {
    let mut energies = Vec::with_capacity(frames.len());
    for rgb in frames {
        let luma: Vec<f32> = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .map(|pixel| focus_stack::color::luma::linear_luma(pixel[0], pixel[1], pixel[2]))
            .collect();
        let mut pyramid = Pyramid::new();
        pyramid.build(&luma, WIDTH, HEIGHT);
        energies.push(EnergyMaps::compute(
            &pyramid,
            &NoiseModel::default(),
            EnergyOptions::default(),
        ));
    }

    let count = (WIDTH * HEIGHT) as usize;
    let confidence = ConfidenceMap {
        width: WIDTH,
        height: HEIGHT,
        best_frame: vec![0; count],
        best_energy: vec![0.0; count],
        margin: vec![1.0; count],
        confidence: vec![0.8; count],
    };
    LevelWeights::compute(&energies, &confidence, WeightOptions::default())
}

fn max_difference(left: &[f32], right: &[f32]) -> f32 {
    assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .map(|(&left, &right)| (left - right).abs())
        .fold(0.0f32, f32::max)
}

#[test]
fn plan_covers_every_pixel_exactly_once() {
    let (width, height) = (97, 61);
    let tiles = plan(width, height, TiledOptions::new(32, 20, 8));
    assert!(!tiles.is_empty());

    let mut coverage = vec![0u8; (width * height) as usize];
    for tile in tiles {
        assert!(tile.width > 0 && tile.height > 0);
        assert!(tile.x + tile.width <= width);
        assert!(tile.y + tile.height <= height);
        for y in tile.y..tile.y + tile.height {
            for x in tile.x..tile.x + tile.width {
                coverage[(y * width + x) as usize] += 1;
            }
        }
    }
    assert!(coverage.iter().all(|&count| count == 1));
}

#[test]
fn tiled_blend_matches_untiled_within_one_lsb() {
    let frames = synthetic_frames();
    let weights = weights(&frames);
    let views: Vec<&[f32]> = frames.iter().map(Vec::as_slice).collect();

    let untiled = blend(&views, WIDTH, HEIGHT, &weights, BlendOptions::default());
    let tiled = tiled_blend(
        &views,
        WIDTH,
        HEIGHT,
        &weights,
        BlendOptions::default(),
        TiledOptions::new(19, 13, 9),
    );

    let difference = max_difference(&untiled, &tiled);
    assert!(
        difference <= 1.0 / 65535.0,
        "maximum difference {difference} exceeds one 16-bit LSB"
    );
}

#[test]
fn tiny_tiles_are_seam_free() {
    let frames = synthetic_frames();
    let weights = weights(&frames);
    let views: Vec<&[f32]> = frames.iter().map(Vec::as_slice).collect();

    let untiled = blend(&views, WIDTH, HEIGHT, &weights, BlendOptions::default());
    let tiled = tiled_blend(
        &views,
        WIDTH,
        HEIGHT,
        &weights,
        BlendOptions::default(),
        TiledOptions::new(7, 5, 2),
    );

    let difference = max_difference(&untiled, &tiled);
    assert!(
        difference <= 1.0 / 65535.0,
        "maximum difference {difference} exceeds one 16-bit LSB"
    );
}

#[test]
fn sixty_megapixels_fit_the_eight_gigabyte_budget() {
    const BUDGET: usize = 8_000_000_000;
    let (width, height) = (9504, 6336);
    let options = TiledOptions::for_memory_budget(BUDGET, width, height, 4, 64)
        .expect("tiled path should fit four 60 MP frames");
    let estimate = working_set_bytes(width, height, 4, options);

    assert!(
        estimate <= BUDGET,
        "estimated {estimate} bytes exceeds the {BUDGET}-byte budget"
    );
    assert!(!plan(width, height, options).is_empty());
}
