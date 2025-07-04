// SPDX-FileCopyrightText: Copyright (c) 2024-2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod chat_completions;
pub mod completions;
pub mod embeddings;
pub mod models;
pub mod nvext;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    fmt::Display,
    ops::{Add, Div, Mul, Sub},
};

use super::{
    common::{self, SamplingOptionsProvider, StopConditionsProvider},
    ContentProvider,
};

/// Minimum allowed value for OpenAI's `temperature` sampling option
pub const MIN_TEMPERATURE: f32 = 0.0;

/// Maximum allowed value for OpenAI's `temperature` sampling option
pub const MAX_TEMPERATURE: f32 = 2.0;

/// Allowed range of values for OpenAI's `temperature`` sampling option
pub const TEMPERATURE_RANGE: (f32, f32) = (MIN_TEMPERATURE, MAX_TEMPERATURE);

/// Minimum allowed value for OpenAI's `top_p` sampling option
pub const MIN_TOP_P: f32 = 0.0;

/// Maximum allowed value for OpenAI's `top_p` sampling option
pub const MAX_TOP_P: f32 = 1.0;

/// Allowed range of values for OpenAI's `top_p` sampling option
pub const TOP_P_RANGE: (f32, f32) = (MIN_TOP_P, MAX_TOP_P);

/// Minimum allowed value for OpenAI's `frequency_penalty` sampling option
pub const MIN_FREQUENCY_PENALTY: f32 = -2.0;

/// Maximum allowed value for OpenAI's `frequency_penalty` sampling option
pub const MAX_FREQUENCY_PENALTY: f32 = 2.0;

/// Allowed range of values for OpenAI's `frequency_penalty` sampling option
pub const FREQUENCY_PENALTY_RANGE: (f32, f32) = (MIN_FREQUENCY_PENALTY, MAX_FREQUENCY_PENALTY);

/// Minimum allowed value for OpenAI's `presence_penalty` sampling option
pub const MIN_PRESENCE_PENALTY: f32 = -2.0;

/// Maximum allowed value for OpenAI's `presence_penalty` sampling option
pub const MAX_PRESENCE_PENALTY: f32 = 2.0;

/// Allowed range of values for OpenAI's `presence_penalty` sampling option
pub const PRESENCE_PENALTY_RANGE: (f32, f32) = (MIN_PRESENCE_PENALTY, MAX_PRESENCE_PENALTY);

/// Represents a streaming response from the OpenAI API
/// The object is generalized on R, which is the type of the response.
/// For SSE streaming responses, the expected `data: ` field is always a JSON
/// object corresponding to `R`; however, the comments in the SSE stream `: `
/// may correspond to other types of information, such as performance metrics,
/// as represented by other arms of this enum.
///
/// This is part of the common API as both the client and service need to agree
/// on the format of the streaming responses.
#[derive(Serialize, Deserialize, Debug)]
pub enum StreamingDelta<R> {
    /// Represents a response delta from the API
    Delta(R),
    Comment(String),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AnnotatedDelta<R> {
    pub delta: R,
    pub id: Option<String>,
    pub event: Option<String>,
    pub comment: Option<String>,
}

trait OpenAISamplingOptionsProvider {
    fn get_temperature(&self) -> Option<f32>;

    fn get_top_p(&self) -> Option<f32>;

    fn get_frequency_penalty(&self) -> Option<f32>;

    fn get_presence_penalty(&self) -> Option<f32>;

    fn nvext(&self) -> Option<&nvext::NvExt>;
}

trait OpenAIStopConditionsProvider {
    fn get_max_tokens(&self) -> Option<u32>;

    fn get_min_tokens(&self) -> Option<u32>;

    fn get_stop(&self) -> Option<Vec<String>>;

    fn nvext(&self) -> Option<&nvext::NvExt>;
}

impl<T: OpenAISamplingOptionsProvider> SamplingOptionsProvider for T {
    fn extract_sampling_options(&self) -> Result<common::SamplingOptions> {
        // let result = self.validate();
        // if let Err(e) = result {
        //     return Err(format!("Error validating sampling options: {}", e));
        // }

        let mut temperature = validate_range(self.get_temperature(), &TEMPERATURE_RANGE)
            .map_err(|e| anyhow::anyhow!("Error validating temperature: {}", e))?;
        let mut top_p = validate_range(self.get_top_p(), &TOP_P_RANGE)
            .map_err(|e| anyhow::anyhow!("Error validating top_p: {}", e))?;
        let frequency_penalty =
            validate_range(self.get_frequency_penalty(), &FREQUENCY_PENALTY_RANGE)
                .map_err(|e| anyhow::anyhow!("Error validating frequency_penalty: {}", e))?;
        let presence_penalty = validate_range(self.get_presence_penalty(), &PRESENCE_PENALTY_RANGE)
            .map_err(|e| anyhow::anyhow!("Error validating presence_penalty: {}", e))?;

        if let Some(nvext) = self.nvext() {
            let greedy = nvext.greed_sampling.unwrap_or(false);
            if greedy {
                top_p = None;
                temperature = None;
            }
        }

        Ok(common::SamplingOptions {
            n: None,
            best_of: None,
            frequency_penalty,
            presence_penalty,
            repetition_penalty: None,
            temperature,
            top_p,
            top_k: None,
            min_p: None,
            seed: None,
            use_beam_search: None,
            length_penalty: None,
        })
    }
}

impl<T: OpenAIStopConditionsProvider> StopConditionsProvider for T {
    fn extract_stop_conditions(&self) -> Result<common::StopConditions> {
        let max_tokens = self.get_max_tokens();
        let min_tokens = self.get_min_tokens();
        let stop = self.get_stop();

        if let Some(stop) = &stop {
            if stop.len() > 4 {
                anyhow::bail!("stop conditions must be less than 4")
            }
        }

        let mut ignore_eos = None;

        if let Some(nvext) = self.nvext() {
            ignore_eos = nvext.ignore_eos;
        }

        Ok(common::StopConditions {
            max_tokens,
            min_tokens,
            stop,
            stop_token_ids_hidden: None,
            ignore_eos,
        })
    }
}

/// Common structure for chat completion responses; the only delta is the type of choices which differs
/// between streaming and non-streaming requests.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GenericCompletionResponse<C>
// where
//     C: Serialize + Clone,
{
    /// A unique identifier for the chat completion.
    pub id: String,

    /// A list of chat completion choices. Can be more than one if n is greater than 1.
    pub choices: Vec<C>,

    /// The Unix timestamp (in seconds) of when the chat completion was created.
    pub created: u64,

    /// The model used for the chat completion.
    pub model: String,

    /// The object type, which is `chat.completion` if the type of `Choice` is `ChatCompletionChoice`,
    /// or is `chat.completion.chunk` if the type of `Choice` is `ChatCompletionChoiceDelta`.
    pub object: String,

    pub usage: Option<async_openai::types::CompletionUsage>,

    /// This fingerprint represents the backend configuration that the model runs with.
    ///
    /// Can be used in conjunction with the seed request parameter to understand when backend changes
    /// have been made that might impact determinism.
    ///
    /// NIM Compatibility:
    /// This field is not supported by the NIM; however it will be added in the future.
    /// The optional nature of this field will be relaxed when it is supported.
    pub system_fingerprint: Option<String>,
    // TODO() - add NvResponseExtention
}

// todo - move to common location
fn validate_range<T>(value: Option<T>, range: &(T, T)) -> Result<Option<T>>
where
    T: PartialOrd + Display,
{
    if value.is_none() {
        return Ok(None);
    }
    let value = value.unwrap();
    if value < range.0 || value > range.1 {
        anyhow::bail!("Value {} is out of range [{}, {}]", value, range.0, range.1);
    }
    Ok(Some(value))
}

// todo - move to common location
/// scale value in `src` range to `dst` range
pub fn scale_value<T>(value: &T, src: &(T, T), dst: &(T, T)) -> Result<T>
where
    T: Copy
        + PartialOrd
        + Add<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Div<Output = T>
        + From<f32>,
{
    let dst_range = dst.1 - dst.0;
    let src_range = src.1 - src.0;
    if dst_range == T::from(0.0) {
        anyhow::bail!("dst range is 0");
    }
    if src_range == T::from(0.0) {
        anyhow::bail!("src range is 0");
    }
    let value_scaled = (*value - src.0) / src_range;
    Ok(dst.0 + (value_scaled * dst_range))
}

pub trait DeltaGeneratorExt<ResponseType: Send + Sync + 'static + std::fmt::Debug>:
    Send + Sync + 'static
{
    fn choice_from_postprocessor(
        &mut self,
        response: common::llm_backend::BackendOutput,
    ) -> Result<ResponseType>;

    /// Gets the current prompt token count (Input Sequence Length).
    fn get_isl(&self) -> Option<u32>;
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_validate_range() {
        assert_eq!(validate_range(Some(0.5), &(0.0, 1.0)).unwrap(), Some(0.5));
        assert_eq!(validate_range(Some(0.0), &(0.0, 1.0)).unwrap(), Some(0.0));
        assert_eq!(validate_range(Some(1.0), &(1.0, 1.0)).unwrap(), Some(1.0));
        assert_eq!(validate_range(Some(1_i32), &(1, 1)).unwrap(), Some(1));
        assert_eq!(
            validate_range(Some(1.1), &(0.0, 1.0))
                .unwrap_err()
                .to_string(),
            "Value 1.1 is out of range [0, 1]"
        );
        assert_eq!(
            validate_range(Some(-0.1), &(0.0, 1.0))
                .unwrap_err()
                .to_string(),
            "Value -0.1 is out of range [0, 1]"
        );
    }

    #[test]
    fn test_scaled_value() {
        assert_eq!(scale_value(&0.5, &(0.0, 1.0), &(0.0, 2.0)).unwrap(), 1.0);
        assert_eq!(scale_value(&0.0, &(0.0, 1.0), &(0.0, 2.0)).unwrap(), 0.0);
        assert_eq!(scale_value(&-1.0, &(-2.0, 2.0), &(1.0, 2.0)).unwrap(), 1.25);
        assert!(scale_value(&1.0, &(1.0, 1.0), &(0.0, 2.0)).is_err());
    }
}
