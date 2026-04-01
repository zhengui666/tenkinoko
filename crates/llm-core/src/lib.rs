use std::collections::BTreeSet;

use anyhow::{Context, Result};
use domain_core::{
    ComparisonOp, ForecastBundle, LlmInsight, Market, PosteriorEstimate, WeatherMarketKind,
    WeatherMetric,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Clone)]
pub struct LlmAnalyst {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTask {
    PreTradeInsight,
    RuleParsing,
    ForecastDivergence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleParseInsight {
    pub summary: String,
    pub caution_flags: Vec<String>,
    pub settlement_basis: Vec<String>,
    pub ambiguities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForecastDivergenceInsight {
    pub summary: String,
    pub caution_flags: Vec<String>,
    pub divergence_drivers: Vec<String>,
    pub confidence_note: Option<String>,
}

#[derive(Debug)]
struct PromptPackage {
    task: PromptTask,
    system: String,
    user: String,
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
        self.analyze_pre_trade(market, forecast, posterior).await
    }

    pub async fn analyze_pre_trade(
        &self,
        market: &Market,
        forecast: &ForecastBundle,
        posterior: &PosteriorEstimate,
    ) -> Result<Option<LlmInsight>> {
        let prompt = build_pre_trade_prompt(market, forecast, posterior);
        let Some(structured) = self.request_structured::<StructuredInsight>(&prompt).await? else {
            return Ok(None);
        };

        Ok(Some(LlmInsight {
            market_id: market.market_id.clone(),
            summary: normalize_summary(&structured),
            caution_flags: collect_flag_set(&[
                &structured.caution_flags,
                structured.source_divergence.as_deref().unwrap_or(&[]),
                structured.rule_risk.as_deref().unwrap_or(&[]),
                structured.data_quality.as_deref().unwrap_or(&[]),
            ]),
        }))
    }

    pub async fn parse_market_rules(
        &self,
        market: &Market,
        raw_market_text: &str,
    ) -> Result<Option<RuleParseInsight>> {
        let prompt = build_rule_parsing_prompt(market, raw_market_text);
        let Some(structured) = self.request_structured::<StructuredRuleParse>(&prompt).await? else {
            return Ok(None);
        };

        Ok(Some(RuleParseInsight {
            summary: structured.summary.trim().to_string(),
            caution_flags: collect_flag_set(&[
                &structured.caution_flags,
                structured.ambiguities.as_deref().unwrap_or(&[]),
            ]),
            settlement_basis: normalize_short_list(structured.settlement_basis),
            ambiguities: normalize_short_list(structured.ambiguities),
        }))
    }

    pub async fn explain_forecast_divergence(
        &self,
        market: &Market,
        forecast: &ForecastBundle,
        posterior: &PosteriorEstimate,
    ) -> Result<Option<ForecastDivergenceInsight>> {
        let prompt = build_forecast_divergence_prompt(market, forecast, posterior);
        let Some(structured) = self
            .request_structured::<StructuredDivergenceInsight>(&prompt)
            .await?
        else {
            return Ok(None);
        };

        Ok(Some(ForecastDivergenceInsight {
            summary: structured.summary.trim().to_string(),
            caution_flags: collect_flag_set(&[
                &structured.caution_flags,
                structured.divergence_drivers.as_deref().unwrap_or(&[]),
                structured.data_quality.as_deref().unwrap_or(&[]),
            ]),
            divergence_drivers: normalize_short_list(structured.divergence_drivers),
            confidence_note: structured
                .confidence_note
                .map(|note| note.trim().to_string())
                .filter(|note| !note.is_empty()),
        }))
    }

    async fn request_structured<T>(&self, prompt: &PromptPackage) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let Some(api_key) = self.api_key.as_deref() else {
            return Ok(None);
        };

        let request = ChatCompletionsRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: prompt.system.clone(),
                },
                Message {
                    role: "user".to_string(),
                    content: prompt.user.clone(),
                },
            ],
            response_format: ResponseFormat {
                r#type: "json_object".to_string(),
            },
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url.trim_end_matches('/')))
            .bearer_auth(api_key)
            .json(&request)
            .send()
            .await
            .with_context(|| format!("request llm completion for task {:?}", prompt.task))?
            .error_for_status()
            .with_context(|| format!("llm completion returned error for task {:?}", prompt.task))?;

        let payload: ChatCompletionsResponse = response
            .json()
            .await
            .with_context(|| format!("decode llm response envelope for task {:?}", prompt.task))?;

        let raw_content = payload
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .with_context(|| format!("llm response missing choice for task {:?}", prompt.task))?;

        let structured = serde_json::from_str::<T>(&raw_content)
            .with_context(|| format!("decode llm json for task {:?}: {raw_content}", prompt.task))?;

        Ok(Some(structured))
    }
}

fn build_pre_trade_prompt(
    market: &Market,
    forecast: &ForecastBundle,
    posterior: &PosteriorEstimate,
) -> PromptPackage {
    PromptPackage {
        task: PromptTask::PreTradeInsight,
        system: pre_trade_system_prompt().to_string(),
        user: format!(
            concat!(
                "Task: analyze this short-horizon weather market snapshot as a constrained explanatory sidecar.\n\n",
                "What matters most:\n",
                "1. Market-rule ambiguity or settlement edge cases.\n",
                "2. Forecast sparsity, staleness, or internal inconsistency.\n",
                "3. Whether posterior confidence looks fragile relative to the evidence.\n",
                "4. Any reason a human operator should distrust a naive reading of the snapshot.\n\n",
                "Market snapshot:\n",
                "- market_id: {market_id}\n",
                "- city: {city}\n",
                "- station_id: {station_id}\n",
                "- target_date: {target_date}\n",
                "- market_kind: {market_kind}\n",
                "- market_active: {market_active}\n",
                "- market_expires_at: {market_expires_at}\n\n",
                "Forecast snapshot:\n",
                "- forecast_city: {forecast_city}\n",
                "- forecast_issued_at: {forecast_issued_at}\n",
                "- forecast_sample_count: {forecast_sample_count}\n\n",
                "Posterior snapshot:\n",
                "- posterior_market_id: {posterior_market_id}\n",
                "- probability_yes: {probability_yes:.4}\n",
                "- fair_value: {fair_value:.4}\n",
                "- confidence: {confidence:.4}\n",
                "- sample_size: {sample_size}\n",
                "- model_rationale: {rationale}\n\n",
                "Restrictions:\n",
                "- Do not recommend trading actions.\n",
                "- Do not infer prices, fill quality, or risk approval.\n",
                "- If data is insufficient, say so explicitly in caution_flags.\n",
                "- Return valid JSON only.\n"
            ),
            market_id = market.market_id,
            city = market.spec.city,
            station_id = market.spec.station_id.as_deref().unwrap_or("UNKNOWN"),
            target_date = market.spec.target_date,
            market_kind = describe_market_kind(&market.spec.kind),
            market_active = market.active,
            market_expires_at = market
                .expires_at
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_else(|| "NONE".to_string()),
            forecast_city = forecast.city,
            forecast_issued_at = forecast.issued_at.to_rfc3339(),
            forecast_sample_count = forecast.samples.len(),
            posterior_market_id = posterior.market_id,
            probability_yes = posterior.probability_yes,
            fair_value = posterior.fair_value,
            confidence = posterior.confidence,
            sample_size = posterior.sample_size,
            rationale = posterior.rationale,
        ),
    }
}

fn build_rule_parsing_prompt(market: &Market, raw_market_text: &str) -> PromptPackage {
    PromptPackage {
        task: PromptTask::RuleParsing,
        system: rule_parsing_system_prompt().to_string(),
        user: format!(
            concat!(
                "Task: parse the market wording into a strict rule-risk summary for a Polymarket weather contract.\n\n",
                "Structured market context:\n",
                "- market_id: {market_id}\n",
                "- city: {city}\n",
                "- station_id: {station_id}\n",
                "- target_date: {target_date}\n",
                "- structured_market_kind: {market_kind}\n\n",
                "Raw market text:\n",
                "{raw_market_text}\n\n",
                "Instruction details:\n",
                "- Extract settlement dependencies, especially station, metric, threshold, comparison direction, and timing window.\n",
                "- Highlight wording that could be misread by an operator or produce settlement surprises.\n",
                "- If the raw text omits a crucial settlement dependency, say so.\n",
                "- Return valid JSON only.\n"
            ),
            market_id = market.market_id,
            city = market.spec.city,
            station_id = market.spec.station_id.as_deref().unwrap_or("UNKNOWN"),
            target_date = market.spec.target_date,
            market_kind = describe_market_kind(&market.spec.kind),
            raw_market_text = raw_market_text.trim(),
        ),
    }
}

fn build_forecast_divergence_prompt(
    market: &Market,
    forecast: &ForecastBundle,
    posterior: &PosteriorEstimate,
) -> PromptPackage {
    PromptPackage {
        task: PromptTask::ForecastDivergence,
        system: forecast_divergence_system_prompt().to_string(),
        user: format!(
            concat!(
                "Task: explain forecast disagreement risk for this weather market snapshot.\n\n",
                "Market context:\n",
                "- market_id: {market_id}\n",
                "- city: {city}\n",
                "- station_id: {station_id}\n",
                "- target_date: {target_date}\n",
                "- market_kind: {market_kind}\n\n",
                "Forecast context:\n",
                "- forecast_city: {forecast_city}\n",
                "- forecast_issued_at: {forecast_issued_at}\n",
                "- forecast_sample_count: {forecast_sample_count}\n\n",
                "Posterior context:\n",
                "- probability_yes: {probability_yes:.4}\n",
                "- confidence: {confidence:.4}\n",
                "- model_rationale: {rationale}\n\n",
                "Instruction details:\n",
                "- Focus on possible divergence drivers, stale inputs, location mismatch, and threshold sensitivity.\n",
                "- If disagreement cannot be assessed from the provided snapshot, say that explicitly.\n",
                "- Do not output generic weather commentary.\n",
                "- Return valid JSON only.\n"
            ),
            market_id = market.market_id,
            city = market.spec.city,
            station_id = market.spec.station_id.as_deref().unwrap_or("UNKNOWN"),
            target_date = market.spec.target_date,
            market_kind = describe_market_kind(&market.spec.kind),
            forecast_city = forecast.city,
            forecast_issued_at = forecast.issued_at.to_rfc3339(),
            forecast_sample_count = forecast.samples.len(),
            probability_yes = posterior.probability_yes,
            confidence = posterior.confidence,
            rationale = posterior.rationale,
        ),
    }
}

fn pre_trade_system_prompt() -> &'static str {
    concat!(
        "You are a constrained analyst for real-money short-horizon weather prediction markets.\n",
        "You are not a trader, not a risk approver, and not an execution engine.\n\n",
        "Business constraints:\n",
        "- Venue: Polymarket weather markets only.\n",
        "- Horizon: 15 minutes to 1 day, usually 45 minutes to 12 hours.\n",
        "- The quantitative posterior already exists. You only explain fragility, ambiguity, and evidence quality.\n",
        "- Never recommend buy, sell, hold, size, urgency, or risk overrides.\n\n",
        "Output contract:\n",
        "- Return valid JSON only.\n",
        "- Required keys: summary, caution_flags.\n",
        "- Optional keys: source_divergence, rule_risk, data_quality, weather_drivers, confidence_note.\n",
        "- summary: one compact paragraph, <= 90 words.\n",
        "- caution_flags and all optional list fields: arrays of short strings.\n",
        "- Use UPPER_SNAKE_CASE for machine-usable caution flags when possible.\n\n",
        "Reasoning policy:\n",
        "- Prefer rule ambiguity, forecast disagreement, stale data, and threshold sensitivity.\n",
        "- Do not invent missing observations, exchange rules, or executable prices.\n",
        "- If inputs are insufficient, surface insufficiency explicitly.\n"
    )
}

fn rule_parsing_system_prompt() -> &'static str {
    concat!(
        "You are a market-rules parser for Polymarket weather contracts.\n",
        "Your task is not to predict weather and not to recommend trades.\n\n",
        "You must convert raw contract wording into strict, reviewable structured output.\n",
        "Focus on settlement dependencies, threshold interpretation, observation source implications, and wording ambiguity.\n\n",
        "Output contract:\n",
        "- Return valid JSON only.\n",
        "- Required keys: summary, caution_flags, settlement_basis, ambiguities.\n",
        "- summary: <= 80 words.\n",
        "- settlement_basis: array of short strings describing what must be true for settlement.\n",
        "- ambiguities: array of short strings describing missing or risky wording.\n",
        "- caution_flags: array of short UPPER_SNAKE_CASE strings.\n\n",
        "Do not claim certainty when the raw text is incomplete.\n"
    )
}

fn forecast_divergence_system_prompt() -> &'static str {
    concat!(
        "You are a forecast-divergence analyst for short-horizon weather markets.\n",
        "You explain why forecast evidence may disagree or why disagreement cannot be measured from the snapshot.\n",
        "You do not recommend trades or size positions.\n\n",
        "Output contract:\n",
        "- Return valid JSON only.\n",
        "- Required keys: summary, caution_flags.\n",
        "- Optional keys: divergence_drivers, data_quality, confidence_note.\n",
        "- summary: <= 90 words.\n",
        "- divergence_drivers and data_quality: arrays of short strings.\n",
        "- caution_flags: array of short UPPER_SNAKE_CASE strings.\n\n",
        "Prefer location mismatch, threshold sensitivity, stale forecasts, sparse coverage, and model-rationale fragility.\n"
    )
}

fn describe_market_kind(kind: &WeatherMarketKind) -> String {
    match kind {
        WeatherMarketKind::DailyHigh { threshold_c } => {
            format!("DAILY_HIGH contract with threshold_c={threshold_c:.2}")
        }
        WeatherMarketKind::DailyLow { threshold_c } => {
            format!("DAILY_LOW contract with threshold_c={threshold_c:.2}")
        }
        WeatherMarketKind::Threshold {
            metric,
            op,
            threshold,
        } => {
            format!(
                "THRESHOLD contract with metric={} op={} threshold={threshold:.2}",
                describe_metric(metric),
                describe_comparison_op(op),
            )
        }
    }
}

fn describe_metric(metric: &WeatherMetric) -> &'static str {
    match metric {
        WeatherMetric::HighTempC => "HIGH_TEMP_C",
        WeatherMetric::LowTempC => "LOW_TEMP_C",
        WeatherMetric::PrecipitationMm => "PRECIPITATION_MM",
    }
}

fn describe_comparison_op(op: &ComparisonOp) -> &'static str {
    match op {
        ComparisonOp::Above => "ABOVE",
        ComparisonOp::Below => "BELOW",
    }
}

fn normalize_summary(structured: &StructuredInsight) -> String {
    let mut summary = structured.summary.trim().to_string();

    if summary.is_empty() {
        summary = "LLM returned an empty summary; treat this insight as unreliable.".to_string();
    }

    if let Some(confidence_note) = structured.confidence_note.as_deref() {
        let confidence_note = confidence_note.trim();
        if !confidence_note.is_empty() && !summary.contains(confidence_note) {
            summary.push_str(" Confidence note: ");
            summary.push_str(confidence_note);
        }
    }

    if let Some(weather_drivers) = structured.weather_drivers.as_ref() {
        let drivers = weather_drivers
            .iter()
            .map(|driver| driver.trim())
            .filter(|driver| !driver.is_empty())
            .collect::<Vec<_>>();
        if !drivers.is_empty() {
            summary.push_str(" Drivers: ");
            summary.push_str(&drivers.join(", "));
        }
    }

    summary
}

fn collect_flag_set(groups: &[&[String]]) -> Vec<String> {
    let mut seen = BTreeSet::new();

    for group in groups {
        for value in *group {
            let normalized = normalize_flag(value);
            if !normalized.is_empty() {
                seen.insert(normalized);
            }
        }
    }

    seen.into_iter().take(8).collect()
}

fn normalize_short_list(values: Option<Vec<String>>) -> Vec<String> {
    values
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_flag(flag: &str) -> String {
    let mut normalized = String::with_capacity(flag.len());
    let mut last_was_sep = false;

    for ch in flag.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_uppercase());
            last_was_sep = false;
        } else if !normalized.is_empty() && !last_was_sep {
            normalized.push('_');
            last_was_sep = true;
        }
    }

    normalized.trim_matches('_').to_string()
}

#[derive(Debug, Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<Message>,
    response_format: ResponseFormat,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    r#type: String,
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
    #[serde(default)]
    caution_flags: Vec<String>,
    #[serde(default)]
    source_divergence: Option<Vec<String>>,
    #[serde(default)]
    rule_risk: Option<Vec<String>>,
    #[serde(default)]
    data_quality: Option<Vec<String>>,
    #[serde(default)]
    weather_drivers: Option<Vec<String>>,
    #[serde(default)]
    confidence_note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StructuredRuleParse {
    summary: String,
    #[serde(default)]
    caution_flags: Vec<String>,
    #[serde(default)]
    settlement_basis: Option<Vec<String>>,
    #[serde(default)]
    ambiguities: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct StructuredDivergenceInsight {
    summary: String,
    #[serde(default)]
    caution_flags: Vec<String>,
    #[serde(default)]
    divergence_drivers: Option<Vec<String>>,
    #[serde(default)]
    data_quality: Option<Vec<String>>,
    #[serde(default)]
    confidence_note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain_core::{MarketSpec, WeatherMarketKind};

    fn sample_market() -> Market {
        Market {
            market_id: "mkt-1".to_string(),
            condition_id: Some("cond-1".to_string()),
            slug: "nyc-high-apr-2".to_string(),
            question: "Will NYC high exceed 23.5C on 2026-04-02?".to_string(),
            description: Some("Daily temperature contract for New York City.".to_string()),
            resolution_criteria: Some(
                "Resolves using the official station high for the target local calendar day."
                    .to_string(),
            ),
            source_url: Some("https://example.com/rules".to_string()),
            yes_token_id: Some("yes-1".to_string()),
            no_token_id: Some("no-1".to_string()),
            spec: MarketSpec {
                city: "New York".to_string(),
                station_id: Some("KNYC".to_string()),
                target_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 2).expect("valid date"),
                kind: WeatherMarketKind::DailyHigh { threshold_c: 23.5 },
            },
            best_bid: Some(0.54),
            best_ask: Some(0.57),
            active: true,
            expires_at: None,
        }
    }

    fn sample_forecast() -> ForecastBundle {
        ForecastBundle {
            city: "New York".to_string(),
            latitude: 40.7128,
            longitude: -74.0060,
            issued_at: chrono::DateTime::parse_from_rfc3339("2026-04-01T10:00:00Z")
                .expect("valid ts")
                .with_timezone(&chrono::Utc),
            samples: Vec::new(),
        }
    }

    fn sample_posterior() -> PosteriorEstimate {
        PosteriorEstimate {
            market_id: "mkt-1".to_string(),
            probability_yes: 0.61,
            fair_value: 0.58,
            confidence: 0.44,
            sample_size: 12,
            rationale: "Open-Meteo and NOAA remain close but threshold sensitivity is high."
                .to_string(),
        }
    }

    #[test]
    fn pre_trade_prompt_mentions_trading_guardrails() {
        let package = build_pre_trade_prompt(&sample_market(), &sample_forecast(), &sample_posterior());

        assert_eq!(package.task, PromptTask::PreTradeInsight);
        assert!(package.system.contains("not a trader"));
        assert!(package.user.contains("Do not recommend trading actions."));
        assert!(package.user.contains("sample_size: 12"));
    }

    #[test]
    fn rule_parsing_prompt_mentions_settlement_dependencies() {
        let package = build_rule_parsing_prompt(
            &sample_market(),
            "Resolves to YES if the official NYC high exceeds 23.5C.",
        );

        assert_eq!(package.task, PromptTask::RuleParsing);
        assert!(package.system.contains("settlement dependencies"));
        assert!(package.user.contains("Raw market text"));
        assert!(package.user.contains("structured_market_kind"));
    }

    #[test]
    fn normalizes_flags_to_upper_snake_case() {
        assert_eq!(normalize_flag("stale forecast"), "STALE_FORECAST");
        assert_eq!(
            normalize_flag("station/rule mismatch"),
            "STATION_RULE_MISMATCH"
        );
    }

    #[test]
    fn merges_optional_flag_buckets() {
        let flags = collect_flag_set(&[
            &vec!["stale forecast".to_string()],
            &vec!["model disagreement".to_string()],
            &vec!["station/rule mismatch".to_string()],
            &vec!["stale forecast".to_string()],
        ]);

        assert_eq!(
            flags,
            vec![
                "MODEL_DISAGREEMENT".to_string(),
                "STALE_FORECAST".to_string(),
                "STATION_RULE_MISMATCH".to_string()
            ]
        );
    }

    #[test]
    fn normalization_of_short_list_drops_empty_entries() {
        let values = normalize_short_list(Some(vec![
            " station mismatch ".to_string(),
            "".to_string(),
            "threshold sensitivity".to_string(),
        ]));

        assert_eq!(
            values,
            vec![
                "station mismatch".to_string(),
                "threshold sensitivity".to_string()
            ]
        );
    }

    #[test]
    fn structured_insight_requires_non_empty_summary() {
        let structured: StructuredInsight =
            serde_json::from_str(r#"{"summary":"","caution_flags":["RULE_AMBIGUITY"]}"#)
                .expect("json should decode");
        let summary = normalize_summary(&structured);

        assert!(summary.contains("empty summary"));
    }

    #[test]
    fn rule_parse_output_can_be_normalized() {
        let structured: StructuredRuleParse = serde_json::from_str(
            r#"{
                "summary":"Threshold direction is clear, but settlement source is implicit.",
                "caution_flags":["rule ambiguity"],
                "settlement_basis":["official station high", "target date local day"],
                "ambiguities":["source name not explicit"]
            }"#,
        )
        .expect("json should decode");

        assert_eq!(structured.summary, "Threshold direction is clear, but settlement source is implicit.");
        assert_eq!(
            collect_flag_set(&[
                &structured.caution_flags,
                structured.ambiguities.as_deref().unwrap_or(&[])
            ]),
            vec!["RULE_AMBIGUITY".to_string(), "SOURCE_NAME_NOT_EXPLICIT".to_string()]
        );
    }

    #[test]
    fn divergence_prompt_mentions_disagreement_risk() {
        let package =
            build_forecast_divergence_prompt(&sample_market(), &sample_forecast(), &sample_posterior());

        assert_eq!(package.task, PromptTask::ForecastDivergence);
        assert!(package.system.contains("forecast-divergence analyst"));
        assert!(package.user.contains("explain forecast disagreement risk"));
        assert!(package.user.contains("threshold sensitivity"));
    }
}
