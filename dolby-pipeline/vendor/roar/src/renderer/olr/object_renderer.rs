// Copyright (c) 2026, Alliance for Open Media. All rights reserved
//
// This source code is subject to the terms of the BSD 3-Clause Clear License
// and the Alliance for Open Media Patent License 1.0. If the BSD 3-Clause Clear
// License was not distributed with this source code in the LICENSE file, you
// can obtain it at www.aomedia.org/license/software-license/bsd-3-c-c. If the
// Alliance for Open Media Patent License 1.0 was not distributed with this
// source code in the PATENTS file, you can obtain it at
// www.aomedia.org/license/patent.

#![forbid(unsafe_code)]

//! OLR renderer backend.
//!
//! This module implements the `ObjectRenderer`, which is the primary backend
//! responsible for taking object-based audio inputs, applying dynamic panning
//! gains (via VBAP or DBAP), and rendering them to a target loudspeaker layout.

use crate::common::definitions::{
    AudioElementConfig, OarError, ObjectPosition, OutputLayout, PlanarBufferMut, PlanarBufferRef, PolarCoordinate,
    SampleRate, Samples,
};
use crate::renderer::audio_renderer_api::AudioRenderer;
use crate::renderer::olr::block_processing_channel::BlockProcessingChannel;
use crate::renderer::olr::custom_gain_calculator::CustomGainCalculator;
use crate::renderer::olr::gain_calculator::GainCalculator;
use crate::renderer::olr::processing_block::GainOutput;
use std::collections::HashMap;
use std::sync::Arc;

impl GainOutput for PlanarBufferMut<'_, '_> {
    fn has_shape(&self, channels: usize, samples: usize) -> bool {
        self.num_channels() >= channels && self.num_samples() == samples
    }
    fn channel_slice(&mut self, channel: usize, _samples: usize) -> &mut [f32] {
        self.channel_mut(channel)
    }
}

#[derive(Debug)]
struct ObjectElement {
    channels: Vec<BlockProcessingChannel>,
}

/// Primary renderer for object-based audio.
///
/// `ObjectRenderer` processes object-based audio elements, applying panning
/// gains computed by a `GainCalculator` (which uses VBAP or DBAP depending on layout)
/// to generate multi-channel loudspeaker signals.
#[derive(Debug)]
pub struct ObjectRenderer {
    #[allow(dead_code)]
    output_layout: OutputLayout,
    gain_calculator: Arc<dyn GainCalculator>,
    sample_rate: SampleRate,
    elements: HashMap<u32, ObjectElement>,
    channel_out_buffer: Vec<f32>,
    max_block_samples: usize,
    sorted_element_ids: Vec<u32>,
    output_channels: usize,
    offset: u64,
}

impl ObjectRenderer {
    /// Creates a new `ObjectRenderer` for the specified target output layout.
    ///
    /// # Errors
    ///
    /// Returns `OarError::InvalidParameter` if the speaker layout is unsupported.
    pub fn new(
        output_layout: OutputLayout,
        sample_rate: SampleRate,
        max_block_size: Samples,
    ) -> Result<Self, OarError> {
        let gain_calculator = Arc::new(CustomGainCalculator::new(output_layout)?);
        Self::with_gain_calculator(output_layout, sample_rate, max_block_size, gain_calculator)
    }

    /// Reuses immutable geometry from the owning rendering context only.
    pub(crate) fn with_gain_calculator(
        output_layout: OutputLayout,
        sample_rate: SampleRate,
        max_block_size: Samples,
        gain_calculator: Arc<dyn GainCalculator>,
    ) -> Result<Self, OarError> {
        let output_channels = gain_calculator.gains_count();
        if output_channels != output_layout.channels().get() {
            return Err(OarError::InvalidParameter);
        }
        let max_block_samples = max_block_size.value() as usize;
        Ok(ObjectRenderer {
            output_layout,
            gain_calculator,
            sample_rate,
            elements: HashMap::new(),
            channel_out_buffer: Vec::new(),
            max_block_samples,
            sorted_element_ids: Vec::new(),
            output_channels,
            offset: 0,
        })
    }
}

impl AudioRenderer for ObjectRenderer {
    fn add_element(&mut self, id: u32, config: &AudioElementConfig) -> Result<(), OarError> {
        match config {
            AudioElementConfig::ObjectBased(cfg) => {
                // Reserve only for multi-object accumulation, before rendering.
                // One scratch block is reused across objects in the original order.
                let other_objects: usize = self.elements.iter()
                    .filter(|(existing, _)| **existing != id)
                    .map(|(_, element)| element.channels.len()).sum();
                let required_size = self.output_channels * self.max_block_samples;
                if other_objects + cfg.num_objects().get() > 1
                    && self.channel_out_buffer.len() < required_size {
                    self.channel_out_buffer.resize(required_size, 0.0);
                }
                let mut channels = Vec::new();
                for _ in 0..cfg.num_objects().get() {
                    channels.push(BlockProcessingChannel::new(Arc::clone(&self.gain_calculator)));
                }
                if !self.elements.contains_key(&id) {
                    self.sorted_element_ids.push(id);
                    self.sorted_element_ids.sort();
                }
                self.elements.insert(id, ObjectElement { channels });
                Ok(())
            }
            _ => Err(OarError::NotSupported),
        }
    }

    fn reset_olr_element(&mut self, id: u32) -> Result<(), OarError> {
        if self.elements.len() != 1 {
            return Err(OarError::NotSupported);
        }
        let element = self.elements.get_mut(&id).ok_or(OarError::InvalidParameter)?;
        for channel in &mut element.channels {
            channel.reset();
        }
        self.offset = 0;
        Ok(())
    }

    fn update_single_olr_position(
        &mut self, id: u32, position: PolarCoordinate, duration: Samples,
    ) -> Result<(), OarError> {
        let element = self.elements.get_mut(&id).ok_or(OarError::InvalidParameter)?;
        if element.channels.len() != 1 {
            return Err(OarError::NotSupported);
        }
        element.channels[0].add_metadata(self.sample_rate, position, duration)
    }

    fn update_element_positions(
        &mut self,
        id: u32,
        positions: &ObjectPosition,
        duration: Samples,
    ) -> Result<(), OarError> {
        if let Some(element) = self.elements.get_mut(&id) {
            match positions {
                ObjectPosition::Polar(polar_coords) => {
                    for (channel, &coord) in element.channels.iter_mut().zip(polar_coords.iter()) {
                        channel.add_metadata(self.sample_rate, coord, duration)?;
                    }
                    Ok(())
                }
                _ => Err(OarError::NotSupported), // OLR only supports constant polar positions
            }
        } else {
            Err(OarError::InvalidParameter)
        }
    }

    fn render(
        &mut self,
        inputs: PlanarBufferRef<'_, '_>,
        output: &mut PlanarBufferMut<'_, '_>,
    ) -> Result<(), OarError> {
        if inputs.num_channels() == 0 || output.num_channels() == 0 {
            return Err(OarError::InvalidParameter);
        }

        let samples = inputs.num_samples();
        if samples == 0 {
            return Err(OarError::InvalidParameter);
        }

        let required_size = self.output_channels * samples;
        if output.num_channels() != self.output_channels || output.num_samples() != samples {
            return Err(OarError::InvalidParameter);
        }

        if samples > self.max_block_samples {
            return Err(OarError::InvalidParameter);
        }

        output.fill(0.0);
        // Direct single-object accumulation keeps all processing-block additions
        // in their original order. Two or more objects retain isolated sums.
        let single_object = inputs.num_channels() == 1;
        if !single_object && self.channel_out_buffer.len() < required_size {
            return Err(OarError::InvalidParameter);
        }

        let mut global_chan_idx = 0;
        for &id in &self.sorted_element_ids {
            if let Some(element) = self.elements.get_mut(&id) {
                let num_objects = element.channels.len();
                for local_idx in 0..num_objects {
                    if global_chan_idx >= inputs.num_channels() {
                        return Err(OarError::InvalidParameter);
                    }
                    let input_channel = inputs.channel(global_chan_idx);
                    global_chan_idx += 1;

                    if single_object {
                        element.channels[local_idx].process_output(
                            self.sample_rate, self.offset, input_channel, output,
                        )?;
                    } else {
                        self.channel_out_buffer[..required_size].fill(0.0);
                        element.channels[local_idx].process(
                            self.sample_rate,
                            self.offset,
                            input_channel,
                            &mut self.channel_out_buffer[..required_size],
                        )?;
                        for out_idx in 0..self.output_channels {
                            let start = out_idx * samples;
                            for (dst, src) in output.channel_mut(out_idx).iter_mut()
                                .zip(self.channel_out_buffer[start..start + samples].iter()) {
                                *dst += src;
                            }
                        }
                    }
                }
            }
        }

        self.offset += samples as u64;
        Ok(())
    }
}

// ===== Tests =====

#[cfg(test)]
mod test {

    use super::*;
    use crate::common::definitions::{ObjectBasedConfig, PolarCoordinate};
    use googletest::prelude::*;

    const EPSILON: f32 = 1e-4;

    #[gtest]
    fn test_object_renderer_adds_valid_object_based_element() {
        let mut rdr = ObjectRenderer::new(
            OutputLayout::Stereo,
            SampleRate::new(48000).unwrap(),
            Samples::new(8).unwrap(),
        )
        .unwrap();
        let config = AudioElementConfig::ObjectBased(ObjectBasedConfig::new(1, None).unwrap());

        assert_ok!(rdr.add_element(10, &config));
    }

    #[gtest]
    fn test_object_renderer_rejects_channel_based_element() {
        let mut rdr = ObjectRenderer::new(
            OutputLayout::Stereo,
            SampleRate::new(48000).unwrap(),
            Samples::new(8).unwrap(),
        )
        .unwrap();
        let config_cb =
            AudioElementConfig::ChannelBased(crate::common::definitions::ChannelBasedConfig {
                layout: crate::common::definitions::InputLayout::Stereo,
                downmix_info: None,
                rendering_config: None,
            });

        expect_that!(rdr.add_element(11, &config_cb), eq(Err(OarError::NotSupported)));
    }

    #[gtest]
    fn test_object_renderer_renders_correct_gains_for_stereo() {
        let mut rdr = ObjectRenderer::new(
            OutputLayout::Stereo,
            SampleRate::new(48000).unwrap(),
            Samples::new(8).unwrap(),
        )
        .unwrap();
        let config = AudioElementConfig::ObjectBased(ObjectBasedConfig::new(1, None).unwrap());
        rdr.add_element(10, &config).unwrap();
        let positions =
            ObjectPosition::Polar(vec![PolarCoordinate::new_from_floats(0.0, 0.0, 1.0).unwrap()]);
        rdr.update_element_positions(10, &positions, Samples::new(8).unwrap()).unwrap();

        let input_data = [10.0f32; 8];
        let inputs = [&input_data[..]];
        let mut output = [0.0f32; 16];
        let (chunks, _) = output.as_chunks_mut::<8>();
        let (left, right) = chunks.split_at_mut(1);
        let mut out_slices = [&mut left[0][..], &mut right[0][..]];
        let mut out_planar =
            PlanarBufferMut::new(&mut out_slices, 2, Samples::new(8).unwrap()).unwrap();
        let validated_inputs = PlanarBufferRef::new(&inputs, 1, Samples::new(8).unwrap()).unwrap();

        assert_ok!(rdr.render(validated_inputs, &mut out_planar));

        for i in 0..8 {
            expect_that!(output[i], near(7.071068f32, EPSILON)); // L
            expect_that!(output[8 + i], near(7.071068f32, EPSILON)); // R
        }
    }

    #[gtest]
    fn test_object_renderer_advances_offset() {
        let mut rdr = ObjectRenderer::new(
            OutputLayout::Stereo,
            SampleRate::new(48000).unwrap(),
            Samples::new(8).unwrap(),
        )
        .unwrap();
        let config = AudioElementConfig::ObjectBased(ObjectBasedConfig::new(1, None).unwrap());
        rdr.add_element(10, &config).unwrap();
        let positions =
            ObjectPosition::Polar(vec![PolarCoordinate::new_from_floats(0.0, 0.0, 1.0).unwrap()]);
        rdr.update_element_positions(10, &positions, Samples::new(8).unwrap()).unwrap();

        let input_data = [10.0f32; 8];
        let inputs = [&input_data[..]];
        let mut output = [0.0f32; 16];
        let (chunks, _) = output.as_chunks_mut::<8>();
        let (left, right) = chunks.split_at_mut(1);
        let mut out_slices = [&mut left[0][..], &mut right[0][..]];
        let mut out_planar =
            PlanarBufferMut::new(&mut out_slices, 2, Samples::new(8).unwrap()).unwrap();
        let validated_inputs = PlanarBufferRef::new(&inputs, 1, Samples::new(8).unwrap()).unwrap();
        rdr.render(validated_inputs, &mut out_planar).unwrap();

        expect_eq!(rdr.offset, 8);
    }
}
