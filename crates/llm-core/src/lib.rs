use anyhow::{Context, Result};
use domain_core::{ForecastBundle, LlmInsight, Market, PosteriorEstimate};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct LlmAnalyst {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
}

impl LlmAnalyst {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            model: model.into(),
            api_key,
        }
    }

    pub fn enabled(&self) -> bool {
        self.api_key.is_some()
    }

    pub async fn analyze(
        &self,
        market: &Market,
        forecast: &ForecastBundle,
        posterior: &PosteriorEstimate,
    ) -> Result<Option<LlmInsight>> {
        let Some(api_key) = &self.api_key else {
            return Ok(None);
        };

        let request = ChatCompletionsRequest {
            model: self.model.clone(),
            response_format: Some(ResponseFormat {
                format_type: "json_object".to_string(),
            }),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: "You are a constrained trading analyst. Return JSON with keys: summary (string), caution_flags (array of strings).".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: format!(
                        "Market: {}\nQuestion: {}\nPosterior probability_yes: {:.4}\nConfidence: {:.4}\nForecast sample count: {}\nReturn JSON only.",
                        market.market_id,
                        market.question,
                        posterior.probability_yes,
                        posterior.confidence,
                        forecast.samples.len()
                    ),
                },
            ],
        };

        let response = self
            .client
            .post(format!(
                "{}/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .json(&request)
            .send()
            .await
            .context("failed to call OpenAI chat completions")?
            .error_for_status()
            .context("OpenAI returned non-success status")?
            .json::<ChatCompletionsResponse>()
            .await
            .context("failed to decode OpenAI response")?;

        let raw = response
            .choices
            .first()
            .map(|choice| choice.message.content.clone())
            .context("missing LLM choice content")?;
        let parsed = serde_json::from_str::<StructuredInsight>(&raw)
            .with_context(|| format!("failed to parse structured LLM JSON: {raw}"))?;

        Ok(Some(LlmInsight {
            market_id: market.market_id.clone(),
            summary: parsed.summary,
            caution_flags: parsed.caution_flags,
        }))
    }
}

#[derive(Debug, Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct StructuredInsight {
    summary: String,
    caution_flags: Vec<String>,
}
