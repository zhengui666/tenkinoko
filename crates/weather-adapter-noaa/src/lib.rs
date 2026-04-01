use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, Utc};
use domain_core::{ForecastBundle, ForecastSample, ObservationSnapshot};
use serde::Deserialize;

#[derive(Clone)]
pub struct NoaaClient {
    base_url: String,
    http: reqwest::Client,
}

impl NoaaClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .user_agent("tenkinoko/0.1.0")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub async fn fetch_hourly_forecast(
        &self,
        city: &str,
        latitude: f64,
        longitude: f64,
        target_date: NaiveDate,
    ) -> Result<ForecastBundle> {
        let point = self.fetch_point(latitude, longitude).await?;
        let payload = self
            .http
            .get(point.properties.forecast_hourly)
            .send()
            .await
            .context("failed to request NOAA hourly forecast")?
            .error_for_status()
            .context("NOAA hourly forecast returned non-success status")?
            .json::<ForecastPeriodsResponse>()
            .await
            .context("failed to decode NOAA hourly forecast")?;

        let mut samples = Vec::new();
        for period in payload.properties.periods {
            let valid_for = DateTime::parse_from_rfc3339(&period.start_time)
                .context("failed to parse NOAA forecast timestamp")?
                .with_timezone(&Utc);
            if valid_for.date_naive() != target_date {
                continue;
            }

            let temperature_c = match period.temperature_unit.as_deref() {
                Some("F") => Some((period.temperature - 32.0) * 5.0 / 9.0),
                Some("C") | None => Some(period.temperature),
                Some(other) => bail!("unsupported NOAA temperature unit {other}"),
            };

            samples.push(ForecastSample {
                source: "noaa".to_string(),
                issued_at: Utc::now(),
                valid_for,
                temperature_c,
                precipitation_mm: None,
            });
        }

        if samples.is_empty() {
            bail!("no NOAA hourly forecast samples returned for {city} on {target_date}");
        }

        Ok(ForecastBundle {
            city: city.to_string(),
            latitude,
            longitude,
            issued_at: Utc::now(),
            samples,
        })
    }

    pub async fn fetch_latest_observation(
        &self,
        city: &str,
        latitude: f64,
        longitude: f64,
    ) -> Result<ObservationSnapshot> {
        let point = self.fetch_point(latitude, longitude).await?;
        let stations = self
            .http
            .get(point.properties.observation_stations)
            .send()
            .await
            .context("failed to request NOAA observation stations")?
            .error_for_status()
            .context("NOAA observation stations returned non-success status")?
            .json::<ObservationStationsResponse>()
            .await
            .context("failed to decode NOAA stations response")?;
        let station_url = stations
            .observation_stations
            .first()
            .cloned()
            .context("NOAA returned no observation stations")?;
        let latest = self
            .http
            .get(format!("{station_url}/observations/latest"))
            .send()
            .await
            .context("failed to request NOAA latest observation")?
            .error_for_status()
            .context("NOAA latest observation returned non-success status")?
            .json::<LatestObservationResponse>()
            .await
            .context("failed to decode NOAA latest observation")?;

        Ok(ObservationSnapshot {
            city: city.to_string(),
            station_id: latest.properties.station.and_then(last_path_segment),
            observed_at: DateTime::parse_from_rfc3339(&latest.properties.timestamp)
                .context("failed to parse NOAA observation timestamp")?
                .with_timezone(&Utc),
            temperature_c: latest.properties.temperature.and_then(|value| value.value),
            precipitation_mm: latest
                .properties
                .precipitation_last_hour
                .and_then(|value| value.value),
            raw_source: "noaa".to_string(),
        })
    }

    async fn fetch_point(&self, latitude: f64, longitude: f64) -> Result<PointResponse> {
        let url = format!(
            "{}/points/{latitude},{longitude}",
            self.base_url.trim_end_matches('/')
        );
        self.http
            .get(url)
            .send()
            .await
            .context("failed to request NOAA points endpoint")?
            .error_for_status()
            .context("NOAA points endpoint returned non-success status")?
            .json::<PointResponse>()
            .await
            .context("failed to decode NOAA points response")
    }
}

fn last_path_segment(url: String) -> Option<String> {
    url.rsplit('/').next().map(ToOwned::to_owned)
}

#[derive(Debug, Deserialize)]
struct PointResponse {
    properties: PointProperties,
}

#[derive(Debug, Deserialize)]
struct PointProperties {
    #[serde(rename = "forecastHourly")]
    forecast_hourly: String,
    #[serde(rename = "observationStations")]
    observation_stations: String,
}

#[derive(Debug, Deserialize)]
struct ForecastPeriodsResponse {
    properties: ForecastPeriods,
}

#[derive(Debug, Deserialize)]
struct ForecastPeriods {
    periods: Vec<ForecastPeriod>,
}

#[derive(Debug, Deserialize)]
struct ForecastPeriod {
    #[serde(rename = "startTime")]
    start_time: String,
    temperature: f64,
    #[serde(rename = "temperatureUnit")]
    temperature_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ObservationStationsResponse {
    #[serde(rename = "observationStations")]
    observation_stations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LatestObservationResponse {
    properties: LatestObservationProperties,
}

#[derive(Debug, Deserialize)]
struct LatestObservationProperties {
    station: Option<String>,
    timestamp: String,
    temperature: Option<UnitValue>,
    #[serde(rename = "precipitationLastHour")]
    precipitation_last_hour: Option<UnitValue>,
}

#[derive(Debug, Deserialize)]
struct UnitValue {
    value: Option<f64>,
}
