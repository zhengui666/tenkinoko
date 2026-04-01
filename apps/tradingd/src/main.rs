use anyhow::Result;
use backtest_engine::{
    execution_replay_from_storage, replay_from_storage, replay_from_storage_obfuscated,
};
use chrono::Utc;
use clap::{Parser, Subcommand};
use config_core::AppConfig;
use domain_core::{SignalSide, TradeSignal};
use execution_engine::{
    ExecutionEngine, ExecutionGateway, PolymarketExecutionGateway, RecordingExecutionGateway,
};
use llm_core::LlmAnalyst;
use polymarket_adapter::PolymarketClient;
use posterior_models::PosteriorEngine;
use risk_engine::RiskEngine;
use scheduler_core::{
    claim_due_jobs, complete_job, fail_job, save_cycle_checkpoint, sleep_until_next_cycle,
};
use signal_engine::SignalEngine;
use std::sync::Arc;
use storage_rocksdb::Storage;
use telegram_bot::TelegramNotifier;
use tracing::{info, warn};
use weather_adapter_noaa::NoaaClient;
use weather_adapter_openmeteo::OpenMeteoClient;

#[derive(Debug, Parser)]
#[command(
    name = "tradingd",
    about = "Single-process weather-market trading daemon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    RunOnce,
    Daemon,
    DiscoverMarkets,
    MarketRuntime,
    PriceHistory,
    BackfillHistoricalForecast { city: String, date: String },
    Replay,
    ReplayObfuscated,
    ReplayExecution,
    ExecutionHealth,
    ReconcileExecution,
    CancelOrder { order_id: String },
    CancelAllOrders,
    TelegramPoll,
    TelegramFlush,
}

#[tokio::main]
async fn main() -> Result<()> {
    telemetry_core::init()?;
    let cli = Cli::parse();
    let config = AppConfig::from_env()?;
    let storage = Storage::open(&config.storage_path)?;

    match cli.command {
        Command::RunOnce => {
            let gateway = build_execution_gateway(&config, &storage).await?;
            run_cycle(&config, &storage, gateway).await?
        }
        Command::Daemon => {
            let gateway = if config.execution.live_enabled() {
                let live_gateway =
                    PolymarketExecutionGateway::connect(&config.execution, storage.clone()).await?;
                let report = live_gateway.health_report().await?;
                info!(
                    blocked = report.blocked,
                    country = %report.country,
                    api_keys = report.api_key_count,
                    open_orders = report.open_orders,
                    recent_trades = report.recent_trades,
                    collateral_balance = %report.collateral_balance,
                    allowance_entries = report.collateral_allowance_entries,
                    "live Polymarket execution gateway ready"
                );
                let recovery_report = live_gateway.recover_pending_orders().await?;
                info!(
                    pending_cancel_orders = recovery_report.pending_cancel_orders,
                    pending_replace_orders = recovery_report.pending_replace_orders,
                    remote_cancel_attempts = recovery_report.remote_cancel_attempts,
                    local_state_fixes = recovery_report.local_state_fixes,
                    replacement_submissions = recovery_report.replacement_submissions,
                    "startup pending-order recovery completed"
                );
                let reconcile_report = live_gateway.reconcile_orders().await?;
                info!(
                    total_local_orders = reconcile_report.total_local_orders,
                    updated_orders = reconcile_report.updated_orders,
                    filled_orders = reconcile_report.filled_orders,
                    cancelled_orders = reconcile_report.cancelled_orders,
                    recent_trades = reconcile_report.recent_trades,
                    "cold-start reconciliation completed"
                );
                let handles = live_gateway.spawn_user_stream_sync().await?;
                info!(
                    task_count = handles.len(),
                    "user websocket sync tasks started"
                );
                let public_market_handles = PolymarketClient::new(
                    config.polymarket_gamma_url.clone(),
                    config.execution.clob_url.clone(),
                )
                .spawn_market_stream_sync(storage.clone())
                .await?;
                info!(
                    task_count = public_market_handles.len(),
                    "public market websocket sync tasks started"
                );
                Arc::new(live_gateway) as Arc<dyn ExecutionGateway + Send + Sync>
            } else {
                Arc::new(RecordingExecutionGateway::new(storage.clone()))
                    as Arc<dyn ExecutionGateway + Send + Sync>
            };
            loop {
                if let Err(error) = run_cycle(&config, &storage, gateway.clone()).await {
                    warn!(error = %error, "decision cycle failed");
                }
                sleep_until_next_cycle(config.cycle_interval_secs).await;
            }
        }
        Command::DiscoverMarkets => {
            let client = PolymarketClient::new(
                config.polymarket_gamma_url.clone(),
                config.execution.clob_url.clone(),
            );
            let markets = client.fetch_weather_markets(config.market_limit).await?;
            for market in markets {
                println!(
                    "{} | {} | {}",
                    market.market_id, market.spec.city, market.question
                );
            }
        }
        Command::ReplayExecution => {
            for row in execution_replay_from_storage(&storage)? {
                println!(
                    "{} | decision_time={} | side={:?} | fill_price={:?} | simulated_pnl_per_share={:?}",
                    row.market_id,
                    row.decision_time,
                    row.side,
                    row.fill_price,
                    row.simulated_pnl_per_share
                );
            }
        }
        Command::MarketRuntime => {
            let client = PolymarketClient::new(
                config.polymarket_gamma_url.clone(),
                config.execution.clob_url.clone(),
            );
            let markets = client.fetch_weather_markets(config.market_limit).await?;
            for market in markets {
                let (runtime, books) = client.fetch_market_runtime(&market).await?;
                storage.save_market_runtime(&runtime)?;
                storage
                    .append_event(&domain_core::Event::MarketRuntimeCaptured(runtime.clone()))?;
                for book in books {
                    storage.save_orderbook_delta(&book)?;
                    storage.append_event(&domain_core::Event::OrderbookCaptured(book))?;
                }
                println!(
                    "{} | yes_mid={:?} | no_mid={:?} | yes_spread={:?} | no_spread={:?}",
                    runtime.market_id,
                    runtime.yes_midpoint,
                    runtime.no_midpoint,
                    runtime.yes_spread,
                    runtime.no_spread
                );
            }
        }
        Command::PriceHistory => {
            let client = PolymarketClient::new(
                config.polymarket_gamma_url.clone(),
                config.execution.clob_url.clone(),
            );
            let markets = client.fetch_weather_markets(config.market_limit).await?;
            for market in markets {
                for series in client.fetch_price_history(&market).await? {
                    storage.save_price_history(&series)?;
                    storage
                        .append_event(&domain_core::Event::PriceHistoryCaptured(series.clone()))?;
                    println!(
                        "{} | token={} | points={}",
                        series.market_id,
                        series.token_id,
                        series.points.len()
                    );
                }
            }
        }
        Command::BackfillHistoricalForecast { city, date } => {
            let target_date = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")?;
            let weather = OpenMeteoClient::new(
                config.openmeteo_base_url.clone(),
                config.openmeteo_historical_base_url.clone(),
                config.openmeteo_geocoding_url.clone(),
            );
            let forecast = weather
                .fetch_historical_forecast_for_city(&city, target_date)
                .await?;
            storage.save_forecast(&city, &forecast)?;
            println!(
                "city={} | date={} | samples={}",
                city,
                target_date,
                forecast.samples.len()
            );
        }
        Command::Replay => {
            let signal_engine = SignalEngine {
                min_edge_bps: config.min_edge_bps,
                fees_bps: config.fees_bps,
                slippage_bps: config.slippage_bps,
                prereso_exit_hours: config.prereso_exit_hours,
            };
            for row in replay_from_storage(&storage, &signal_engine)? {
                println!(
                    "{} | decision_time={} | prob_yes={:.4} | side={:?} | edge_bps={}",
                    row.market_id,
                    row.decision_time,
                    row.posterior.probability_yes,
                    row.signal.side,
                    row.signal.edge_bps
                );
            }
        }
        Command::ReplayObfuscated => {
            let signal_engine = SignalEngine {
                min_edge_bps: config.min_edge_bps,
                fees_bps: config.fees_bps,
                slippage_bps: config.slippage_bps,
                prereso_exit_hours: config.prereso_exit_hours,
            };
            for row in replay_from_storage_obfuscated(&storage, &signal_engine)? {
                println!(
                    "{} | obfuscated_decision_time={} | prob_yes={:.4} | side={:?} | edge_bps={}",
                    row.market_id,
                    row.decision_time,
                    row.posterior.probability_yes,
                    row.signal.side,
                    row.signal.edge_bps
                );
            }
        }
        Command::ExecutionHealth => {
            let gateway =
                PolymarketExecutionGateway::connect(&config.execution, storage.clone()).await?;
            let report = gateway.health_report().await?;
            println!(
                "blocked={} | country={} | api_keys={} | open_orders={} | recent_trades={} | collateral_balance={} | allowance_entries={}",
                report.blocked,
                report.country,
                report.api_key_count,
                report.open_orders,
                report.recent_trades,
                report.collateral_balance,
                report.collateral_allowance_entries
            );
        }
        Command::ReconcileExecution => {
            let gateway =
                PolymarketExecutionGateway::connect(&config.execution, storage.clone()).await?;
            let report = gateway.reconcile_orders().await?;
            println!(
                "total_local_orders={} | updated_orders={} | filled_orders={} | cancelled_orders={} | recent_trades={}",
                report.total_local_orders,
                report.updated_orders,
                report.filled_orders,
                report.cancelled_orders,
                report.recent_trades
            );
        }
        Command::CancelOrder { order_id } => {
            let gateway =
                PolymarketExecutionGateway::connect(&config.execution, storage.clone()).await?;
            let report = gateway.cancel_order(&order_id).await?;
            println!(
                "canceled_count={} | not_canceled_count={}",
                report.canceled_count, report.not_canceled_count
            );
        }
        Command::CancelAllOrders => {
            let gateway =
                PolymarketExecutionGateway::connect(&config.execution, storage.clone()).await?;
            let report = gateway.cancel_all_orders().await?;
            println!(
                "canceled_count={} | not_canceled_count={}",
                report.canceled_count, report.not_canceled_count
            );
        }
        Command::TelegramPoll => {
            let notifier = TelegramNotifier::new(
                config.telegram_bot_token.clone(),
                config.telegram_chat_id.clone(),
                config.telegram_readonly_chat_ids.clone(),
                config.telegram_admin_chat_ids.clone(),
                storage.clone(),
            );
            let processed = notifier.poll_commands_once().await?;
            println!("processed_commands={processed}");
        }
        Command::TelegramFlush => {
            let notifier = TelegramNotifier::new(
                config.telegram_bot_token.clone(),
                config.telegram_chat_id.clone(),
                config.telegram_readonly_chat_ids.clone(),
                config.telegram_admin_chat_ids.clone(),
                storage.clone(),
            );
            let delivered = notifier
                .flush_outbox(config.telegram_max_delivery_attempts)
                .await?;
            println!("delivered_messages={delivered}");
        }
    }

    Ok(())
}

async fn build_execution_gateway(
    config: &AppConfig,
    storage: &Storage,
) -> Result<Arc<dyn ExecutionGateway + Send + Sync>> {
    if config.execution.live_enabled() {
        let gateway =
            PolymarketExecutionGateway::connect(&config.execution, storage.clone()).await?;
        let report = gateway.health_report().await?;
        info!(
            blocked = report.blocked,
            country = %report.country,
            api_keys = report.api_key_count,
            open_orders = report.open_orders,
            recent_trades = report.recent_trades,
            collateral_balance = %report.collateral_balance,
            allowance_entries = report.collateral_allowance_entries,
            "live Polymarket execution gateway ready"
        );
        Ok(Arc::new(gateway))
    } else {
        Ok(Arc::new(RecordingExecutionGateway::new(storage.clone())))
    }
}

async fn run_cycle(
    config: &AppConfig,
    storage: &Storage,
    gateway: Arc<dyn ExecutionGateway + Send + Sync>,
) -> Result<()> {
    let polymarket = PolymarketClient::new(
        config.polymarket_gamma_url.clone(),
        config.execution.clob_url.clone(),
    );
    let weather = OpenMeteoClient::new(
        config.openmeteo_base_url.clone(),
        config.openmeteo_historical_base_url.clone(),
        config.openmeteo_geocoding_url.clone(),
    );
    let noaa = NoaaClient::new(config.noaa_base_url.clone());
    let llm = LlmAnalyst::new(
        config.openai_base_url.clone(),
        config.openai_model.clone(),
        config.openai_api_key.clone(),
    );
    let signal_engine = SignalEngine {
        min_edge_bps: config.min_edge_bps,
        fees_bps: config.fees_bps,
        slippage_bps: config.slippage_bps,
        prereso_exit_hours: config.prereso_exit_hours,
    };
    let risk_engine = RiskEngine {
        total_equity_usd: config.total_equity_usd,
        max_position_pct: config.max_position_pct,
        cluster_max_pct: config.cluster_max_pct,
        daily_loss_limit_usd: config.daily_loss_limit_usd,
        market_anomaly_spread_bps: config.market_anomaly_spread_bps,
    };
    let executor = ExecutionEngine::new(gateway, storage.clone());
    let notifier = TelegramNotifier::new(
        config.telegram_bot_token.clone(),
        config.telegram_chat_id.clone(),
        config.telegram_readonly_chat_ids.clone(),
        config.telegram_admin_chat_ids.clone(),
        storage.clone(),
    );

    let _ = notifier
        .flush_outbox(config.telegram_max_delivery_attempts)
        .await?;
    let _ = notifier.poll_commands_once().await?;
    let forced_exit_market_ids = collect_manual_close_requests(storage)?;
    let markets = polymarket
        .fetch_weather_markets(config.market_limit)
        .await?;
    info!(market_count = markets.len(), "fetched candidate markets");
    let positions = storage.list_positions()?;
    let current_risk = storage
        .get_risk_state()?
        .unwrap_or(domain_core::RiskState::Normal);
    let runtime_snapshots = storage.list_market_runtime()?;
    let mut current_risk =
        risk_engine.derive_risk_state(&positions, &markets, &runtime_snapshots, current_risk);
    if current_risk == domain_core::RiskState::Normal
        && source_divergence_exceeds_threshold(
            &storage.list_forecasts()?,
            config.source_divergence_c,
        )
    {
        current_risk = domain_core::RiskState::Cautious;
    }
    storage.set_risk_state(&current_risk)?;

    for market in &markets {
        if !market.active {
            continue;
        }
        if !config.city_filters.is_empty()
            && !config
                .city_filters
                .iter()
                .any(|city| city.eq_ignore_ascii_case(&market.spec.city))
        {
            continue;
        }

        storage.save_market(market)?;
        storage.append_event(&domain_core::Event::MarketDiscovered(market.clone()))?;
        if let Ok((runtime, books)) = polymarket.fetch_market_runtime(market).await {
            storage.save_market_runtime(&runtime)?;
            storage.append_event(&domain_core::Event::MarketRuntimeCaptured(runtime))?;
            for book in books {
                storage.save_orderbook_delta(&book)?;
                storage.append_event(&domain_core::Event::OrderbookCaptured(book))?;
            }
        }
        if let Ok(histories) = polymarket.fetch_price_history(market).await {
            for series in histories {
                storage.save_price_history(&series)?;
                storage.append_event(&domain_core::Event::PriceHistoryCaptured(series))?;
            }
        }

        let requote_report = executor
            .reprice_stale_orders(market, config.reprice_threshold_bps)
            .await?;
        if requote_report.canceled_orders > 0 || requote_report.reposted_orders > 0 {
            info!(
                market_id = %market.market_id,
                canceled_orders = requote_report.canceled_orders,
                reposted_orders = requote_report.reposted_orders,
                "repriced stale working orders"
            );
        }

        let forecast = weather
            .fetch_forecast_for_city(&market.spec.city, market.spec.target_date)
            .await?;
        let mut merged_forecast = forecast.clone();
        if config.noaa_enabled {
            if let Ok(noaa_forecast) = noaa
                .fetch_hourly_forecast(
                    &market.spec.city,
                    forecast.latitude,
                    forecast.longitude,
                    market.spec.target_date,
                )
                .await
            {
                merged_forecast.samples.extend(noaa_forecast.samples);
            }
            if let Ok(observation) = noaa
                .fetch_latest_observation(&market.spec.city, forecast.latitude, forecast.longitude)
                .await
            {
                storage.save_observation(&market.market_id, &observation)?;
                storage.append_event(&domain_core::Event::ObservationIngested(observation))?;
            }
        }
        storage.save_forecast(&market.market_id, &merged_forecast)?;
        storage.append_event(&domain_core::Event::ForecastIngested(
            merged_forecast.clone(),
        ))?;

        let posterior = PosteriorEngine::estimate(market, &merged_forecast);
        storage.save_posterior(&posterior)?;
        storage.append_event(&domain_core::Event::PosteriorComputed(posterior.clone()))?;

        let llm_insight = llm.analyze(market, &merged_forecast, &posterior).await?;
        if let Some(insight) = llm_insight.as_ref() {
            storage.save_llm_insight(insight)?;
            storage.append_event(&domain_core::Event::LlmInsightComputed(insight.clone()))?;
        }
        let mut signal =
            signal_engine.generate(market, &posterior, llm_insight.as_ref(), &positions);
        let latest_observation = storage
            .list_observations()?
            .into_iter()
            .filter(|observation| observation.city.eq_ignore_ascii_case(&market.spec.city))
            .max_by_key(|observation| observation.observed_at);
        if let Some(observation) = latest_observation {
            let has_open_position = positions.iter().any(|position| {
                position.market_id == market.market_id && position.exit_reason.is_none()
            });
            let mean_temp = merged_forecast
                .samples
                .iter()
                .filter_map(|sample| sample.temperature_c)
                .sum::<f64>()
                / (merged_forecast
                    .samples
                    .iter()
                    .filter(|sample| sample.temperature_c.is_some())
                    .count()
                    .max(1) as f64);
            if has_open_position
                && observation
                    .temperature_c
                    .map(|temp| (temp - mean_temp).abs() >= config.observation_mismatch_c)
                    .unwrap_or(false)
            {
                signal = TradeSignal {
                    market_id: market.market_id.clone(),
                    generated_at: Utc::now(),
                    side: SignalSide::Exit,
                    edge_bps: signal.edge_bps,
                    max_size_usd: signal.max_size_usd,
                    reason: format!(
                        "observation mismatch exceeded {:.1}C; {}",
                        config.observation_mismatch_c, signal.reason
                    ),
                };
            }
        }
        if forced_exit_market_ids
            .iter()
            .any(|market_id| market_id == &market.market_id)
        {
            signal = TradeSignal {
                market_id: market.market_id.clone(),
                generated_at: Utc::now(),
                side: SignalSide::Exit,
                edge_bps: signal.edge_bps,
                max_size_usd: signal.max_size_usd,
                reason: format!("manual close requested; {}", signal.reason),
            };
        }
        storage.save_signal(&signal)?;
        storage.append_event(&domain_core::Event::SignalGenerated(signal.clone()))?;

        let risk = risk_engine.evaluate(market, &signal, current_risk, &positions, &markets);
        storage.save_risk_decision(&risk)?;
        storage.append_event(&domain_core::Event::RiskEvaluated(risk.clone()))?;

        if let Some(order) = executor.execute(market, &signal, &risk, &positions).await? {
            let insight_text = llm_insight
                .as_ref()
                .map(|insight| format!(" | analyst={}", insight.summary))
                .unwrap_or_default();
            notifier
                .notify(&format!(
                    "order submitted | market={} | city={} | side={:?} | size_usd={:.2} | edge_bps={} | venue_order_id={:?}{}",
                    market.market_id,
                    market.spec.city,
                    order.intent.side,
                    order.intent.size_usd,
                    signal.edge_bps,
                    order.venue_order_id,
                    insight_text
                ))
                .await?;
        }
    }

    let _ = save_cycle_checkpoint(storage, "tradingd-daemon", config.cycle_interval_secs)?;

    Ok(())
}

fn collect_manual_close_requests(storage: &Storage) -> Result<Vec<String>> {
    let jobs = claim_due_jobs(storage, 32)?;
    let positions = storage.list_positions()?;
    let mut market_ids = Vec::new();

    for job in jobs {
        if job.kind != "manual_close" {
            fail_job(storage, &job, 300)?;
            continue;
        }
        let Some(position) = positions
            .iter()
            .find(|position| position.position_id.to_string() == job.payload)
        else {
            fail_job(storage, &job, 300)?;
            continue;
        };
        market_ids.push(position.market_id.clone());
        complete_job(storage, &job)?;
    }

    Ok(market_ids)
}

fn source_divergence_exceeds_threshold(
    forecasts: &[domain_core::ForecastBundle],
    threshold_c: f64,
) -> bool {
    let mut source_means = forecasts
        .iter()
        .flat_map(|bundle| bundle.samples.iter())
        .filter_map(|sample| {
            sample
                .temperature_c
                .map(|temp| (sample.source.as_str(), temp))
        })
        .fold(
            std::collections::BTreeMap::<&str, (f64, usize)>::new(),
            |mut acc, (source, temp)| {
                let entry = acc.entry(source).or_insert((0.0, 0));
                entry.0 += temp;
                entry.1 += 1;
                acc
            },
        )
        .into_iter()
        .filter_map(|(_, (sum, count))| (count > 0).then_some(sum / count as f64))
        .collect::<Vec<_>>();
    source_means.sort_by(f64::total_cmp);
    match (source_means.first(), source_means.last()) {
        (Some(min), Some(max)) => (max - min) >= threshold_c,
        _ => false,
    }
}
