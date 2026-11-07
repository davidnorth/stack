//! Shared focus evidence for the stack benches.

use focus_stack::color::luma::encoded_luma_buffer;
use focus_stack::focus::confidence::{FrameCalibration, saturation_mask};
use focus_stack::focus::energy::{EnergyMaps, EnergyOptions};
use focus_stack::focus::noise::{self, DEFAULT_NOISE_K};
use focus_stack::focus::pyramid::Pyramid;

/// Per-frame focus evidence plus the reference guide luma.
pub struct Evidence {
    pub frames: Vec<Vec<f32>>,
    pub guide: Vec<f32>,
    pub energies: Vec<EnergyMaps>,
    pub aggregates: Vec<Vec<f32>>,
    pub calibrations: Vec<FrameCalibration>,
    pub saturation: Vec<Vec<bool>>,
}

impl Evidence {
    /// Build `frames` synthetic RGB frames and their focus evidence.
    pub fn build(width: u32, height: u32, frames: usize) -> Self {
        let count = width as usize * height as usize;
        let rgb: Vec<Vec<f32>> = (0..frames)
            .map(|frame| super::synthetic_rgb(width, height, 0x51a1_0100 + frame as u64))
            .collect();

        let mut energies = Vec::with_capacity(frames);
        let mut aggregates = Vec::with_capacity(frames);
        let mut calibrations = Vec::with_capacity(frames);
        let mut saturation = Vec::with_capacity(frames);

        for frame in &rgb {
            let mut luma = vec![0.0; count];
            encoded_luma_buffer(frame, &mut luma);

            let mut pyramid = Pyramid::new();
            pyramid.build(&luma, width, height);
            let noise = noise::estimate(&pyramid, DEFAULT_NOISE_K);
            let energy = EnergyMaps::compute(&pyramid, &noise, EnergyOptions::default());
            let aggregate = energy.aggregate();
            let sigmas: Vec<f32> = noise.sigmas().collect();

            calibrations.push(FrameCalibration::new(&aggregate, &sigmas));
            aggregates.push(aggregate);
            energies.push(energy);
            saturation.push(saturation_mask(frame, width, height, 3));
        }

        let mut guide = vec![0.0; count];
        encoded_luma_buffer(&rgb[0], &mut guide);

        Self {
            frames: rgb,
            guide,
            energies,
            aggregates,
            calibrations,
            saturation,
        }
    }
}
