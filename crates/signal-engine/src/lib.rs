use chrono::Utc;
use domain_core::{LlmInsight, Market, Position, PosteriorEstimate, SignalSide, TradeSignal};

#[derive(Debug, Clone)]
pub struct SignalEngine {
    pub min_edge_bps: i64,
    pub fees_bps: i64,
    pub slippage_bps: i64,
    pub prereso_exit_hours: i64,
}

impl SignalEngine {
    pub fn generate(
        &self,
        market: &Market,
        posterior: &PosteriorEstimate,
        llm: Option<&LlmInsight>,
        positions: &[Position],
    ) -> TradeSignal {
        let ask = market.best_ask.unwrap_or(0.5);
        let bid = market.best_bid.unwrap_or(0.5);
        let net_yes_edge_bps = (((posterior.probability_yes - ask) * 10_000.0) as i64)
            - self.fees_bps
            - self.slippage_bps;
        let net_no_edge_bps = ((((1.0 - posterior.probability_yes) - (1.0 - bid)) * 10_000.0)
            as i64)
            - self.fees_bps
            - self.slippage_bps;
        let strongest_edge = net_yes_edge_bps.max(net_no_edge_bps);
        let has_position = positions.iter().any(|position| {
            position.market_id == market.market_id && position.exit_reason.is_none()
        });
        let should_preresolution_exit = has_position
            && market
                .expires_at
                .map(|expires_at| {
                    expires_at.signed_duration_since(Utc::now()).num_hours()
                        <= self.prereso_exit_hours
                })
                .unwrap_or(false);

        let (side, edge_bps) =
            if net_yes_edge_bps >= self.min_edge_bps && posterior.confidence >= 0.2 {
                (SignalSide::BuyYes, net_yes_edge_bps)
            } else if net_no_edge_bps >= self.min_edge_bps && posterior.confidence >= 0.2 {
                (SignalSide::BuyNo, net_no_edge_bps)
            } else if should_preresolution_exit {
                (SignalSide::Exit, strongest_edge)
            } else if has_position
                && (strongest_edge < self.min_edge_bps / 2 || posterior.confidence < 0.2)
            {
                (SignalSide::Exit, strongest_edge)
            } else {
                (SignalSide::Hold, strongest_edge)
            };

        let mut reason = posterior.rationale.clone();
        if let Some(insight) = llm {
            reason.push_str("; analyst: ");
            reason.push_str(&insight.summary);
        }

        TradeSignal {
            market_id: market.market_id.clone(),
            generated_at: Utc::now(),
            side,
            edge_bps,
            max_size_usd: 250.0,
            reason,
        }
    }
}
