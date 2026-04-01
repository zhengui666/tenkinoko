use anyhow::Result;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc};
use domain_core::{DateMapping, ForecastBundle, Market, PosteriorEstimate, TradeSignal};
use posterior_models::PosteriorEngine;
use signal_engine::SignalEngine;
use storage_rocksdb::Storage;
use uuid::Uuid;

#[derive(Debug)]
pub struct ReplayRow {
    pub market_id: String,
    pub posterior: PosteriorEstimate,
    pub signal: TradeSignal,
    pub decision_time: DateTime<Utc>,
}

#[derive(Debug)]
pub struct ExecutionReplayRow {
    pub market_id: String,
    pub side: domain_core::SignalSide,
    pub decision_time: DateTime<Utc>,
    pub fill_price: Option<f64>,
    pub simulated_pnl_per_share: Option<f64>,
}

pub fn replay_from_storage(
    storage: &Storage,
    signal_engine: &SignalEngine,
) -> Result<Vec<ReplayRow>> {
    replay_internal(storage, signal_engine, false)
}

pub fn replay_from_storage_obfuscated(
    storage: &Storage,
    signal_engine: &SignalEngine,
) -> Result<Vec<ReplayRow>> {
    replay_internal(storage, signal_engine, true)
}

pub fn execution_replay_from_storage(storage: &Storage) -> Result<Vec<ExecutionReplayRow>> {
    let signals = storage.list_signals()?;
    let histories = storage.list_price_history()?;
    let mut rows = Vec::new();

    for signal in signals {
        let relevant = histories
            .iter()
            .filter(|history| history.market_id == signal.market_id)
            .flat_map(|history| history.points.iter())
            .filter(|point| point.timestamp >= signal.generated_at)
            .min_by_key(|point| point.timestamp);

        let fill_price = relevant.map(|point| point.price);
        let simulated_pnl_per_share = match signal.side {
            domain_core::SignalSide::BuyYes => fill_price.map(|price| 1.0 - price),
            domain_core::SignalSide::BuyNo => fill_price.map(|price| price),
            domain_core::SignalSide::Exit | domain_core::SignalSide::Hold => None,
        };

        rows.push(ExecutionReplayRow {
            market_id: signal.market_id,
            side: signal.side,
            decision_time: signal.generated_at,
            fill_price,
            simulated_pnl_per_share,
        });
    }

    Ok(rows)
}

fn replay_internal(
    storage: &Storage,
    signal_engine: &SignalEngine,
    obfuscate_dates: bool,
) -> Result<Vec<ReplayRow>> {
    let markets = storage.list_markets()?;
    let forecasts = storage.list_forecasts()?;
    let mut rows = Vec::new();

    for market in markets {
        if let Some(forecast) = forecasts
            .iter()
            .find(|bundle| bundle.city == market.spec.city)
        {
            rows.push(replay_market(
                storage,
                &market,
                forecast,
                signal_engine,
                obfuscate_dates,
            )?);
        }
    }

    Ok(rows)
}

fn replay_market(
    storage: &Storage,
    market: &Market,
    forecast: &ForecastBundle,
    signal_engine: &SignalEngine,
    obfuscate_dates: bool,
) -> Result<ReplayRow> {
    let posterior = PosteriorEngine::estimate(market, forecast);
    let signal = signal_engine.generate(market, &posterior, None, &[]);
    let decision_time = if obfuscate_dates {
        obfuscate_timestamp(storage, forecast.issued_at)?
    } else {
        forecast.issued_at
    };
    Ok(ReplayRow {
        market_id: market.market_id.clone(),
        posterior,
        signal,
        decision_time,
    })
}

fn obfuscate_timestamp(storage: &Storage, real_timestamp: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let real_date = real_timestamp.date_naive();
    let fake_date = if let Some(mapping) = storage
        .list_date_mappings()?
        .into_iter()
        .find(|entry| entry.real_date == real_date)
    {
        mapping.fake_date
    } else {
        let fake_date = remap_date(real_date);
        storage.save_date_mapping(&DateMapping {
            id: Uuid::new_v4(),
            real_date,
            fake_date,
            created_at: Utc::now(),
        })?;
        fake_date
    };
    Ok(Utc
        .with_ymd_and_hms(
            fake_date.year(),
            fake_date.month(),
            fake_date.day(),
            real_timestamp.hour(),
            real_timestamp.minute(),
            real_timestamp.second(),
        )
        .single()
        .expect("obfuscated timestamp"))
}

fn remap_date(real_date: NaiveDate) -> NaiveDate {
    let candidate_year = real_date.year() + 12;
    NaiveDate::from_ymd_opt(candidate_year, real_date.month(), real_date.day())
        .or_else(|| {
            if real_date.month() == 2 && real_date.day() == 29 {
                NaiveDate::from_ymd_opt(candidate_year + 4 - (candidate_year % 4), 2, 29)
            } else {
                None
            }
        })
        .unwrap_or(real_date)
}

#[cfg(test)]
mod tests {
    use super::{remap_date, replay_from_storage_obfuscated};
    use chrono::{Datelike, NaiveDate, Utc};
    use domain_core::{ForecastBundle, ForecastSample, Market, MarketSpec, WeatherMarketKind};
    use signal_engine::SignalEngine;
    use std::time::{SystemTime, UNIX_EPOCH};
    use storage_rocksdb::Storage;

    #[test]
    fn remaps_year_but_preserves_month_day() {
        let real = NaiveDate::from_ymd_opt(2024, 2, 29).expect("date");
        let fake = remap_date(real);
        assert_eq!(fake.month(), 2);
        assert_eq!(fake.day(), 29);
        assert!(fake.year() >= 2036);
    }

    #[test]
    fn obfuscated_replay_persists_date_mapping() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tenkinoko-backtest-date-map-{unique}"));
        let storage = Storage::open(&path).expect("open storage");
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
        storage.save_market(&market).expect("save market");
        storage
            .save_forecast(
                &market.market_id,
                &ForecastBundle {
                    city: "Boston".to_string(),
                    latitude: 0.0,
                    longitude: 0.0,
                    issued_at: Utc::now(),
                    samples: vec![ForecastSample {
                        source: "open-meteo".to_string(),
                        issued_at: Utc::now(),
                        valid_for: Utc::now(),
                        temperature_c: Some(21.0),
                        precipitation_mm: Some(0.0),
                    }],
                },
            )
            .expect("save forecast");

        let rows = replay_from_storage_obfuscated(
            &storage,
            &SignalEngine {
                min_edge_bps: 300,
                fees_bps: 100,
                slippage_bps: 40,
                prereso_exit_hours: 6,
            },
        )
        .expect("replay");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            storage.list_date_mappings().expect("date mappings").len(),
            1
        );
        let _ = std::fs::remove_dir_all(path);
    }
}
