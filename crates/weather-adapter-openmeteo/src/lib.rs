use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use domain_core::{ForecastBundle, ForecastSample};
use serde::Deserialize;

#[derive(Clone)]
pub struct OpenMeteoClient {
    forecast_base_url: String,
    historical_base_url: String,
    geocoding_base_url: String,
    http: reqwest::Client,
}

impl OpenMeteoClient {
    pub fn new(
        forecast_base_url: impl Into<String>,
        historical_base_url: impl Into<String>,
        geocoding_base_url: impl Into<String>,
    ) -> Self {
        Self {
            forecast_base_url: forecast_base_url.into(),
            historical_base_url: historical_base_url.into(),
            geocoding_base_url: geocoding_base_url.into(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn fetch_forecast_for_city(
        &self,
        city: &str,
        target_date: NaiveDate,
    ) -> Result<ForecastBundle> {
        let place = self.geocode(city).await?;
        self.fetch_forecast(city, place.latitude, place.longitude, target_date)
            .await
    }

    pub async fn fetch_historical_forecast_for_city(
        &self,
        city: &str,
        target_date: NaiveDate,
    ) -> Result<ForecastBundle> {
        let place = self.geocode(city).await?;
        self.fetch_historical_forecast(city, place.latitude, place.longitude, target_date)
            .await
    }

    pub async fn fetch_forecast(
        &self,
        city: &str,
        latitude: f64,
        longitude: f64,
        target_date: NaiveDate,
    ) -> Result<ForecastBundle> {
        let url = format!(
            "{}/v1/forecast?latitude={latitude}&longitude={longitude}&hourly=temperature_2m,precipitation&forecast_days=7&timezone=UTC",
            self.forecast_base_url.trim_end_matches('/')
        );
        let payload = self
            .http
            .get(url)
            .send()
            .await
            .context("failed to request Open-Meteo forecast")?
            .error_for_status()
            .context("Open-Meteo forecast returned non-success status")?
            .json::<ForecastResponse>()
            .await
            .context("failed to decode Open-Meteo forecast")?;

        self.map_response_to_bundle(city, latitude, longitude, target_date, payload)
    }

    pub async fn fetch_historical_forecast(
        &self,
        city: &str,
        latitude: f64,
        longitude: f64,
        target_date: NaiveDate,
    ) -> Result<ForecastBundle> {
        let date = target_date.format("%Y-%m-%d");
        let url = format!(
            "{}/v1/forecast?latitude={latitude}&longitude={longitude}&hourly=temperature_2m,precipitation&start_date={date}&end_date={date}&timezone=UTC",
            self.historical_base_url.trim_end_matches('/')
        );
        let payload = self
            .http
            .get(url)
            .send()
            .await
            .context("failed to request Open-Meteo historical forecast")?
            .error_for_status()
            .context("Open-Meteo historical forecast returned non-success status")?
            .json::<ForecastResponse>()
            .await
            .context("failed to decode Open-Meteo historical forecast")?;

        self.map_response_to_bundle(city, latitude, longitude, target_date, payload)
    }

    fn map_response_to_bundle(
        &self,
        city: &str,
        latitude: f64,
        longitude: f64,
        target_date: NaiveDate,
        payload: ForecastResponse,
    ) -> Result<ForecastBundle> {
        let mut samples = Vec::new();
        for ((time, temp), precip) in payload
            .hourly
            .time
            .iter()
            .zip(payload.hourly.temperature_2m.iter())
            .zip(payload.hourly.precipitation.iter())
        {
            let valid_for = DateTime::parse_from_rfc3339(&format!("{time}:00+00:00"))
                .map(|dt| dt.with_timezone(&Utc))
                .or_else(|_| {
                    NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H:%M")
                        .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
                })
                .context("failed to parse Open-Meteo hourly timestamp")?;
            if valid_for.date_naive() != target_date {
                continue;
            }
            samples.push(ForecastSample {
                source: "open-meteo".to_string(),
                issued_at: Utc::now(),
                valid_for,
                temperature_c: Some(*temp),
                precipitation_mm: Some(*precip),
            });
        }

        if samples.is_empty() {
            bail!("no forecast samples returned for {city} on {target_date}");
        }

        Ok(ForecastBundle {
            city: city.to_string(),
            latitude,
            longitude,
            issued_at: Utc::now(),
            samples,
        })
    }

    async fn geocode(&self, city: &str) -> Result<GeocodeResult> {
        let url = format!(
            "{}/v1/search?name={}&count=1&language=en&format=json",
            self.geocoding_base_url.trim_end_matches('/'),
            city.replace(' ', "%20"),
        );
        let payload = self
            .http
            .get(url)
            .send()
            .await
            .context("failed to request Open-Meteo geocoding")?
            .error_for_status()
            .context("Open-Meteo geocoding returned non-success status")?
            .json::<GeocodeResponse>()
            .await
            .context("failed to decode geocoding response")?;

        payload
            .results
            .into_iter()
            .next()
            .with_context(|| format!("city not found in geocoding API: {city}"))
    }
}

#[derive(Debug, Deserialize)]
struct GeocodeResponse {
    results: Vec<GeocodeResult>,
}

#[derive(Debug, Deserialize)]
struct GeocodeResult {
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Deserialize)]
struct ForecastResponse {
    hourly: HourlyForecast,
}

#[derive(Debug, Deserialize)]
struct HourlyForecast {
    time: Vec<String>,
    temperature_2m: Vec<f64>,
    precipitation: Vec<f64>,
}
