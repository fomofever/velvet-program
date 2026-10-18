// Derived from raydium-io/raydium-cp-swap (Apache-2.0), commit 0f86d46:
// programs/cp-swap/src/curve/calculator.rs.
// Modified by Velvet: one pair per pool (SOL and the coin) and the fee is always charged in SOL:
// on a buy it comes off the SOL going in (as in the original), on a sell it comes off the SOL
// coming out. The constant-product step itself is the original `swap_base_input_without_fees`.

use crate::curve::{constant_product::ConstantProductCurve, fees::Fees};

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct SwapResult {
    /// What the user receives (tokens on a buy, SOL on a sell).
    pub amount_out: u64,
    /// Whole fee, in SOL.
    pub trade_fee: u64,
    /// Part of the fee that leaves the pool later (holders, creator, Velvet), in SOL.
    pub protocol_fee: u64,
    /// SOL and token reserves after the swap, protocol fees excluded.
    pub new_sol: u128,
    pub new_token: u128,
}

pub struct CurveCalculator;

impl CurveCalculator {
    /// Buy: `sol_in` SOL in, coin out. Reserves exclude protocol fees owed.
    pub fn buy(sol_in: u64, sol: u128, token: u128, trade_fee_rate: u64, protocol_fee_rate: u64) -> Option<SwapResult> {
        if sol == 0 || token == 0 || sol_in == 0 { return None; }
        let amount = u128::from(sol_in);
        let trade_fee = Fees::trading_fee(amount, trade_fee_rate)?;
        let protocol_fee = Fees::protocol_fee(trade_fee, protocol_fee_rate)?;
        let in_less_fee = amount.checked_sub(trade_fee)?;
        let out = ConstantProductCurve::swap_base_input_without_fees(in_less_fee, sol, token);
        if out == 0 || out >= token { return None; }
        Some(SwapResult {
            amount_out: u64::try_from(out).ok()?,
            trade_fee: u64::try_from(trade_fee).ok()?,
            protocol_fee: u64::try_from(protocol_fee).ok()?,
            // the LP part of the fee stays in the pool; the protocol part is set aside
            new_sol: sol.checked_add(amount)?.checked_sub(protocol_fee)?,
            new_token: token.checked_sub(out)?,
        })
    }

    /// Sell: `token_in` coin in, SOL out; the fee comes off the SOL out.
    pub fn sell(token_in: u64, sol: u128, token: u128, trade_fee_rate: u64, protocol_fee_rate: u64) -> Option<SwapResult> {
        if sol == 0 || token == 0 || token_in == 0 { return None; }
        let gross = ConstantProductCurve::swap_base_input_without_fees(u128::from(token_in), token, sol);
        if gross == 0 || gross >= sol { return None; }
        let trade_fee = Fees::trading_fee(gross, trade_fee_rate)?;
        let protocol_fee = Fees::protocol_fee(trade_fee, protocol_fee_rate)?;
        let out = gross.checked_sub(trade_fee)?;
        if out == 0 { return None; }
        Some(SwapResult {
            amount_out: u64::try_from(out).ok()?,
            trade_fee: u64::try_from(trade_fee).ok()?,
            protocol_fee: u64::try_from(protocol_fee).ok()?,
            new_sol: sol.checked_sub(gross)?.checked_add(trade_fee - protocol_fee)?,
            new_token: token.checked_add(u128::from(token_in))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const RATE: u64 = 3_500;
    const PROTO: u64 = 428_571; // 15/35 of the fee

    #[test]
    fn k_never_decreases_and_fees_add_up() {
        let (mut sol, mut tok) = (85_000_000_000u128, 300_000_000_000_000u128);
        let mut seed = 7u64;
        let mut rnd = || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
        for i in 0..20_000 {
            let k0 = sol * tok;
            let r = if i % 2 == 0 {
                let amt = 1 + rnd() % 20_000_000_000;
                CurveCalculator::buy(amt, sol, tok, RATE, PROTO)
            } else {
                let amt = 1 + rnd() % (tok as u64 / 20);
                CurveCalculator::sell(amt, sol, tok, RATE, PROTO)
            };
            if let Some(r) = r {
                assert!(r.new_sol * r.new_token >= k0, "k decreased at step {i}");
                assert!(r.protocol_fee <= r.trade_fee);
                sol = r.new_sol; tok = r.new_token;
            }
        }
    }

    #[test]
    fn fee_is_0_35_percent_of_the_sol_side() {
        let r = CurveCalculator::buy(1_000_000_000, 85_000_000_000, 300_000_000_000_000, RATE, PROTO).unwrap();
        assert_eq!(r.trade_fee, 3_500_000);
        assert_eq!(r.protocol_fee, 1_499_998);
        let s = CurveCalculator::sell(1_000_000_000_000, 85_000_000_000, 300_000_000_000_000, RATE, PROTO).unwrap();
        let gross = s.amount_out + s.trade_fee;
        assert_eq!(s.trade_fee as u128, (gross as u128 * 3_500 + 999_999) / 1_000_000);
    }

    #[test]
    fn a_round_trip_never_profits() {
        let (sol, tok) = (85_000_000_000u128, 300_000_000_000_000u128);
        for amt in [1u64, 1_000, 1_000_000, 1_000_000_000, 20_000_000_000] {
            let b = CurveCalculator::buy(amt, sol, tok, RATE, PROTO).unwrap_or(SwapResult { amount_out: 0, trade_fee: 0, protocol_fee: 0, new_sol: sol, new_token: tok });
            if b.amount_out == 0 { continue; }
            let s = CurveCalculator::sell(b.amount_out, b.new_sol, b.new_token, RATE, PROTO);
            if let Some(s) = s { assert!(s.amount_out < amt, "buy {amt} sell back {}", s.amount_out); }
        }
    }
}
