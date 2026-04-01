use domain_core::{
    ComparisonOp, ForecastBundle, Market, PosteriorEstimate, WeatherMarketKind, WeatherMetric,
};

pub struct PosteriorEngine;

impl PosteriorEngine {
    pub fn estimate(market: &Market, forecast: &ForecastBundle) -> PosteriorEstimate {
        let outcomes = forecast
            .samples
            .iter()
            .filter_map(|sample| match &market.spec.kind {
                WeatherMarketKind::DailyHigh { threshold_c } => {
                    sample.temperature_c.map(|temp| temp > *threshold_c)
                }
                WeatherMarketKind::DailyLow { threshold_c } => {
                    sample.temperature_c.map(|temp| temp < *threshold_c)
                }
                WeatherMarketKind::Threshold {
                    metric,
                    op,
                    threshold,
                } => match metric {
                    WeatherMetric::PrecipitationMm => sample
                        .precipitation_mm
                        .map(|value| compare(*op, value, *threshold)),
                    WeatherMetric::HighTempC | WeatherMetric::LowTempC => sample
                        .temperature_c
                        .map(|value| compare(*op, value, *threshold)),
                },
            })
            .collect::<Vec<_>>();

        let sample_size = outcomes.len();
        let yes_count = outcomes.iter().filter(|value| **value).count();
        let probability_yes = if sample_size == 0 {
            0.5
        } else {
            yes_count as f64 / sample_size as f64
        };
        let confidence = if sample_size == 0 {
            0.0
        } else {
            (sample_size as f64 / 24.0).min(1.0)
        };

        PosteriorEstimate {
            market_id: market.market_id.clone(),
            probability_yes,
            fair_value: probability_yes,
            confidence,
            sample_size,
            rationale: format!(
                "Posterior derived from {} hourly forecast samples for {}",
                sample_size, forecast.city
            ),
        }
    }
}

fn compare(op: ComparisonOp, value: f64, threshold: f64) -> bool {
    match op {
        ComparisonOp::Above => value > threshold,
        ComparisonOp::Below => value < threshold,
    }
}

#[cfg(test)]
mod tests {
    use super::PosteriorEngine;
    use chrono::{NaiveDate, Utc};
    use domain_core::{ForecastBundle, ForecastSample, Market, MarketSpec, WeatherMarketKind};

    #[test]
    fn maps_probability_from_hourly_samples() {
        let market = Market {
            market_id: "m1".to_string(),
            condition_id: None,
            slug: "slug".to_string(),
            question: "q".to_string(),
            description: None,
            resolution_criteria: None,
            source_url: None,
            yes_token_id: None,
            no_token_id: None,
            spec: MarketSpec {
                city: "Boston".to_string(),
                station_id: None,
                target_date: NaiveDate::from_ymd_opt(2026, 4, 1).expect("date"),
                kind: WeatherMarketKind::DailyHigh { threshold_c: 20.0 },
            },
            best_bid: Some(0.45),
            best_ask: Some(0.55),
            active: true,
            expires_at: None,
        };
        let issued_at = Utc::now();
        let forecast = ForecastBundle {
            city: "Boston".to_string(),
            latitude: 0.0,
            longitude: 0.0,
            issued_at,
            samples: vec![
                ForecastSample {
                    source: "open-meteo".to_string(),
                    issued_at,
                    valid_for: issued_at,
                    temperature_c: Some(19.0),
                    precipitation_mm: Some(0.0),
                },
                ForecastSample {
                    source: "open-meteo".to_string(),
                    issued_at,
                    valid_for: issued_at,
                    temperature_c: Some(21.0),
                    precipitation_mm: Some(0.0),
                },
                ForecastSample {
                    source: "open-meteo".to_string(),
                    issued_at,
                    valid_for: issued_at,
                    temperature_c: Some(22.0),
                    precipitation_mm: Some(0.0),
                },
            ],
        };

        let posterior = PosteriorEngine::estimate(&market, &forecast);
        assert!((posterior.probability_yes - (2.0 / 3.0)).abs() < 1e-6);
    }
}
