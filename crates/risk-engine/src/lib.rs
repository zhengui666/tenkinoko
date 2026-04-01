use domain_core::{
    Market, MarketRuntimeSnapshot, Position, PositionSide, RiskDecision, RiskState, SignalSide,
    TradeSignal,
};

#[derive(Debug, Clone)]
pub struct RiskEngine {
    pub total_equity_usd: f64,
    pub max_position_pct: f64,
    pub cluster_max_pct: f64,
    pub daily_loss_limit_usd: f64,
    pub market_anomaly_spread_bps: i64,
}

impl RiskEngine {
    pub fn derive_risk_state(
        &self,
        existing_positions: &[Position],
        known_markets: &[Market],
        runtimes: &[MarketRuntimeSnapshot],
        persisted_state: RiskState,
    ) -> RiskState {
        let unrealized_pnl = existing_positions
            .iter()
            .filter(|position| position.exit_reason.is_none())
            .map(|position| {
                let market = known_markets
                    .iter()
                    .find(|market| market.market_id == position.market_id);
                let runtime = runtimes
                    .iter()
                    .filter(|runtime| runtime.market_id == position.market_id)
                    .max_by_key(|runtime| runtime.captured_at);
                let mark =
                    mark_price(position.side, market, runtime).unwrap_or(position.average_price);
                (mark - position.average_price) * position.quantity_shares
            })
            .sum::<f64>();
        let anomalous_market = existing_positions
            .iter()
            .filter(|position| position.exit_reason.is_none())
            .any(|position| {
                runtimes
                    .iter()
                    .filter(|runtime| runtime.market_id == position.market_id)
                    .max_by_key(|runtime| runtime.captured_at)
                    .map(|runtime| {
                        let spread = match position.side {
                            PositionSide::Yes => runtime.yes_spread,
                            PositionSide::No => runtime.no_spread,
                        }
                        .unwrap_or_default();
                        (spread * 10_000.0) as i64 >= self.market_anomaly_spread_bps
                    })
                    .unwrap_or(false)
            });

        if unrealized_pnl <= -self.daily_loss_limit_usd {
            RiskState::EmergencyFlat
        } else if unrealized_pnl <= -(self.daily_loss_limit_usd * 0.5) {
            RiskState::ReduceOnly
        } else if anomalous_market && matches!(persisted_state, RiskState::Normal) {
            RiskState::Cautious
        } else {
            persisted_state
        }
    }

    pub fn evaluate(
        &self,
        market: &Market,
        signal: &TradeSignal,
        state: RiskState,
        existing_positions: &[Position],
        known_markets: &[Market],
    ) -> RiskDecision {
        if matches!(signal.side, SignalSide::Hold) {
            return RiskDecision {
                market_id: signal.market_id.clone(),
                state,
                approved: false,
                capped_size_usd: 0.0,
                reason: "signal is hold".to_string(),
            };
        }

        if matches!(state, RiskState::HaltOpen | RiskState::EmergencyFlat) {
            return RiskDecision {
                market_id: signal.market_id.clone(),
                state,
                approved: false,
                capped_size_usd: 0.0,
                reason: "risk state blocks new exposure".to_string(),
            };
        }

        if matches!(state, RiskState::ReduceOnly) && !matches!(signal.side, SignalSide::Exit) {
            return RiskDecision {
                market_id: signal.market_id.clone(),
                state,
                approved: false,
                capped_size_usd: 0.0,
                reason: "reduce-only mode".to_string(),
            };
        }

        let current_exposure = existing_positions
            .iter()
            .filter(|position| position.market_id == signal.market_id)
            .map(|position| position.size_usd)
            .sum::<f64>();

        let cluster_exposure = existing_positions
            .iter()
            .filter_map(|position| {
                let position_market = known_markets
                    .iter()
                    .find(|candidate| candidate.market_id == position.market_id)?;
                (position_market
                    .spec
                    .city
                    .eq_ignore_ascii_case(&market.spec.city)
                    && position_market.spec.target_date == market.spec.target_date)
                    .then_some(position.size_usd)
            })
            .sum::<f64>();

        if matches!(signal.side, SignalSide::Exit) {
            return RiskDecision {
                market_id: signal.market_id.clone(),
                state,
                approved: current_exposure > 0.0,
                capped_size_usd: current_exposure,
                reason: format!(
                    "exit signal closing {:.2} usd of exposure",
                    current_exposure
                ),
            };
        }

        let hard_cap = self.total_equity_usd * self.max_position_pct;
        let cluster_cap = self.total_equity_usd * self.cluster_max_pct;
        let remaining = (hard_cap - current_exposure).max(0.0);
        let cluster_remaining = (cluster_cap - cluster_exposure).max(0.0);
        let cautious_multiplier = if matches!(state, RiskState::Cautious) {
            0.5
        } else {
            1.0
        };
        let capped_size_usd = remaining
            .min(cluster_remaining)
            .min(signal.max_size_usd * cautious_multiplier);

        RiskDecision {
            market_id: signal.market_id.clone(),
            state,
            approved: capped_size_usd > 0.0,
            capped_size_usd,
            reason: format!(
                "remaining market exposure {:.2}/{:.2}; cluster remaining {:.2}/{:.2}",
                remaining, hard_cap, cluster_remaining, cluster_cap
            ),
        }
    }
}

fn mark_price(
    side: PositionSide,
    market: Option<&Market>,
    runtime: Option<&MarketRuntimeSnapshot>,
) -> Option<f64> {
    match side {
        PositionSide::Yes => runtime
            .and_then(|runtime| runtime.yes_midpoint.or(runtime.yes_last_trade))
            .or_else(|| market.and_then(|market| market.best_bid.or(market.best_ask))),
        PositionSide::No => runtime
            .and_then(|runtime| runtime.no_midpoint.or(runtime.no_last_trade))
            .or_else(|| {
                market.and_then(|market| {
                    market
                        .best_ask
                        .map(|ask| 1.0 - ask)
                        .or_else(|| market.best_bid.map(|bid| 1.0 - bid))
                })
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::RiskEngine;
    use chrono::Utc;
    use domain_core::{
        Market, MarketSpec, Position, PositionSide, RiskState, SignalSide, TradeSignal,
        WeatherMarketKind,
    };
    use uuid::Uuid;

    #[test]
    fn enforces_per_market_cap() {
        let engine = RiskEngine {
            total_equity_usd: 10_000.0,
            max_position_pct: 0.02,
            cluster_max_pct: 0.05,
            daily_loss_limit_usd: 250.0,
            market_anomaly_spread_bps: 600,
        };
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
                target_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).expect("date"),
                kind: WeatherMarketKind::DailyHigh { threshold_c: 20.0 },
            },
            best_bid: None,
            best_ask: None,
            active: true,
            expires_at: None,
        };
        let signal = TradeSignal {
            market_id: "m1".to_string(),
            generated_at: Utc::now(),
            side: SignalSide::BuyYes,
            edge_bps: 400,
            max_size_usd: 250.0,
            reason: "edge".to_string(),
        };
        let positions = vec![Position {
            position_id: Uuid::new_v4(),
            market_id: "m1".to_string(),
            side: PositionSide::Yes,
            quantity_shares: 180.0 / 0.52,
            pending_close_shares: 0.0,
            average_price: 0.52,
            size_usd: 180.0,
            opened_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            exit_reason: None,
        }];

        let decision = engine.evaluate(
            &market,
            &signal,
            RiskState::Normal,
            &positions,
            std::slice::from_ref(&market),
        );
        assert!(decision.approved);
        assert!((decision.capped_size_usd - 20.0).abs() < 1e-6);
    }

    #[test]
    fn enforces_cluster_cap() {
        let engine = RiskEngine {
            total_equity_usd: 10_000.0,
            max_position_pct: 0.02,
            cluster_max_pct: 0.03,
            daily_loss_limit_usd: 250.0,
            market_anomaly_spread_bps: 600,
        };
        let market = Market {
            market_id: "m2".to_string(),
            condition_id: None,
            slug: "slug-2".to_string(),
            question: "q".to_string(),
            description: None,
            resolution_criteria: None,
            source_url: None,
            yes_token_id: None,
            no_token_id: None,
            spec: MarketSpec {
                city: "Boston".to_string(),
                station_id: None,
                target_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).expect("date"),
                kind: WeatherMarketKind::DailyHigh { threshold_c: 20.0 },
            },
            best_bid: None,
            best_ask: None,
            active: true,
            expires_at: None,
        };
        let signal = TradeSignal {
            market_id: "m2".to_string(),
            generated_at: Utc::now(),
            side: SignalSide::BuyYes,
            edge_bps: 400,
            max_size_usd: 250.0,
            reason: "edge".to_string(),
        };
        let positions = vec![Position {
            position_id: Uuid::new_v4(),
            market_id: "m1".to_string(),
            side: PositionSide::Yes,
            quantity_shares: 290.0 / 0.52,
            pending_close_shares: 0.0,
            average_price: 0.52,
            size_usd: 290.0,
            opened_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            exit_reason: None,
        }];
        let known_markets = vec![
            Market {
                market_id: "m1".to_string(),
                ..market.clone()
            },
            market.clone(),
        ];

        let decision = engine.evaluate(
            &market,
            &signal,
            RiskState::Normal,
            &positions,
            &known_markets,
        );
        assert!(decision.approved);
        assert!((decision.capped_size_usd - 10.0).abs() < 1e-6);
    }
}
