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

//! Metadata interpreter for OLR.

use crate::common::definitions::{
    OarError, PolarCoordinate, SampleRate, Samples, MAX_OUTPUT_CHANNEL_COUNT,
};
use crate::renderer::olr::gain_calculator::GainCalculator;
use crate::renderer::olr::processing_block::{
    FixedGainsBlock, InterpGainsBlock, ProcessingBlockVariant,
};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
struct GainsBuffer {
    gains: [f32; MAX_OUTPUT_CHANNEL_COUNT],
    len: usize,
}

impl GainsBuffer {
    fn new(len: usize) -> Self {
        GainsBuffer { gains: [0.0f32; MAX_OUTPUT_CHANNEL_COUNT], len }
    }
    fn as_slice(&self) -> &[f32] {
        &self.gains[..self.len]
    }
    fn as_slice_mut(&mut self) -> &mut [f32] {
        &mut self.gains[..self.len]
    }
}

/// Interprets object-based metadata and calculates output channel gains for rendering.
#[derive(Debug)]
pub struct InterpretObjectMetadata {
    gain_calculator: Arc<dyn GainCalculator>,
    last_block_end: u64,
    last_block_gains_from: Option<GainsBuffer>,
    last_block_gains_to: Option<GainsBuffer>,
    gains_scratch: GainsBuffer,
    reuse_pool:Option<GainReusePool>,
}

impl InterpretObjectMetadata {
    /// Creates a new `InterpretObjectMetadata` instance.
    pub fn new(gain_calculator: Arc<dyn GainCalculator>) -> Self {
        let n = gain_calculator.gains_count();
        let reuse_pool=gain_calculator.reuse_safe().then(||GainReusePool::new(n));
        InterpretObjectMetadata {
            gain_calculator,
            last_block_end: u64::MAX,
            last_block_gains_from: None,
            last_block_gains_to: None,
            gains_scratch: GainsBuffer::new(n),
            reuse_pool,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new(Arc::clone(&self.gain_calculator));
    }

    /// Processes object metadata for a given object and time range, returning a processing block.
    pub fn process(
        &mut self,
        _sample_rate: SampleRate,
        coordinate: &PolarCoordinate,
        start: u64,
        duration: Samples,
    ) -> Result<Option<ProcessingBlockVariant>, OarError> {
        let start_sample = start;
        let dur_u32 = u32::from(duration);
        let end_sample = start + dur_u32 as u64;
        let interp = end_sample - start_sample;
        let mut target_sample = start_sample + interp;
        let n = self.gain_calculator.gains_count();

        if target_sample > end_sample {
            target_sample = end_sample;
        }

        if self.last_block_end != u64::MAX && start_sample == self.last_block_end {
            self.last_block_gains_from = self.last_block_gains_to;
        } else {
            target_sample = start_sample;
            self.last_block_gains_from = None;
        }

        let key=[coordinate.azimuth().value().to_bits(),coordinate.elevation().value().to_bits(),coordinate.distance().value().to_bits()];
        let cached=self.reuse_pool.as_mut().and_then(|pool|pool.lookup(key));
        if let Some(gains)=cached {self.gains_scratch=gains;} else {
        self.gain_calculator.calculate_gains(
            coordinate.azimuth(),
            coordinate.elevation(),
            coordinate.distance(),
            self.gains_scratch.as_slice_mut(),
        )?;
            if let Some(pool)=&mut self.reuse_pool {pool.insert(key,self.gains_scratch);}
        }

        self.last_block_end = end_sample;
        self.last_block_gains_to = Some(self.gains_scratch);

        if start_sample != target_sample {
            let from_slice = self.last_block_gains_from.as_ref().map(|b| b.as_slice());
            let to_slice = Some(self.gains_scratch.as_slice());
            Ok(Some(ProcessingBlockVariant::Interp(InterpGainsBlock::new(
                start_sample,
                target_sample,
                from_slice,
                to_slice,
                n,
            ))))
        } else if target_sample != end_sample {
            Ok(Some(ProcessingBlockVariant::Fixed(FixedGainsBlock::new(
                target_sample,
                end_sample,
                self.gains_scratch.as_slice(),
            ))))
        } else {
            Ok(None)
        }
    }
}

// ===== Tests =====

#[cfg(test)]
mod test {

    use super::*;
    use crate::common::definitions::PolarCoordinate;
    use crate::renderer::olr::gain_calculator::VogPanner;
    use googletest::prelude::*;

    #[gtest]
    fn test_interpret_object_metadata_first_block_returns_fixed_gains() {
        let mut interpreter = InterpretObjectMetadata::new(Arc::new(VogPanner));
        let coord = PolarCoordinate::new_from_floats(0.0, 0.0, 1.0).unwrap();

        let result = interpreter.process(
            SampleRate::new(48000).unwrap(),
            &coord,
            0,
            Samples::new(8).unwrap(),
        );

        let block_opt = result.unwrap();
        let block = block_opt.unwrap();
        expect_true!(matches!(block, ProcessingBlockVariant::Fixed(_)));
        expect_eq!(block.first_sample(), 0);
        expect_eq!(block.last_sample(), 8);
    }

    #[gtest]
    fn test_interpret_object_metadata_aligned_subsequent_block_returns_interp_gains() {
        let mut interpreter = InterpretObjectMetadata::new(Arc::new(VogPanner));
        let coord = PolarCoordinate::new_from_floats(0.0, 0.0, 1.0).unwrap();
        let samples_8 = Samples::new(8).unwrap();
        interpreter.process(SampleRate::new(48000).unwrap(), &coord, 0, samples_8).unwrap();

        let result = interpreter.process(SampleRate::new(48000).unwrap(), &coord, 8, samples_8);

        let block_opt = result.unwrap();
        let block = block_opt.unwrap();
        expect_true!(matches!(block, ProcessingBlockVariant::Interp(_)));
        expect_eq!(block.first_sample(), 8);
        expect_eq!(block.last_sample(), 16);
    }
}

const REUSE_CAPACITY:usize=4;
const REUSE_WINDOW:u8=64;
#[derive(Debug,Clone,Copy)]
struct GainReuseEntry {key:Option<[u32;3]>,gains:GainsBuffer,score:u8}
#[derive(Debug)]
struct GainReusePool {entries:[GainReuseEntry;REUSE_CAPACITY],requests:u8,hits:u8}
impl GainReusePool {
 fn new(n:usize)->Self {Self{entries:[GainReuseEntry{key:None,gains:GainsBuffer::new(n),score:0};REUSE_CAPACITY],requests:0,hits:0}}
 fn lookup(&mut self,key:[u32;3])->Option<GainsBuffer>{
  let mut found=None;
  for e in &mut self.entries {if e.key==Some(key){e.score=e.score.saturating_add(2);self.hits+=1;found=Some(e.gains);break;}}
  self.requests+=1;
  if self.requests==REUSE_WINDOW {
   for e in &mut self.entries {e.score>>=1;}
   // At most one hit-rate-driven eviction, with four bounded comparisons.
   if self.hits<REUSE_WINDOW/8 {
    if let Some(index)=(0..REUSE_CAPACITY).filter(|i|self.entries[*i].key.is_some()).min_by_key(|i|self.entries[*i].score){
     self.entries[index].key=None;self.entries[index].score=0;
    }
   }
   self.requests=0;self.hits=0;
  }
  found
 }
 fn insert(&mut self,key:[u32;3],gains:GainsBuffer){
  let index=self.entries.iter().position(|e|e.key.is_none()).unwrap_or_else(||(0..REUSE_CAPACITY).min_by_key(|i|self.entries[*i].score).unwrap());
  self.entries[index]=GainReuseEntry{key:Some(key),gains,score:1};
 }
}
