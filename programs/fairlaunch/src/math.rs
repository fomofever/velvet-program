//! Pure, side-effect-free math. Everything that decides who gets what lives here
//! so it can be unit tested without a validator.
//!
//! Curve: constant product on *virtual* reserves (pump.fun style).
//!   k = virtual_sol * virtual_token
//! All arithmetic is u128 with checked ops; rounding always favours the pool.

pub const BPS_DENOM: u64 = 10_000;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MathError {
    Overflow,
    ZeroAmount,
}

type R<T> = Result<T, MathError>;

fn m(x: Option<u128>) -> R<u128> {
    x.ok_or(MathError::Overflow)
}

fn to_u64(x: u128) -> R<u64> {
    u64::try_from(x).map_err(|_| MathError::Overflow)
}

/// Result of a buy against the curve.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct BuyQuote {
    /// Tokens the buyer receives.
    pub tokens_out: u64,
    /// SOL (after fees) actually consumed by the curve. <= sol_in.
    /// Smaller than sol_in only when the curve runs out of tokens.
    pub sol_used: u64,
    /// True if this buy exhausted the curve's real token reserves.
    pub completes_curve: bool,
}

/// Buy with `sol_in` lamports (already net of fees).
/// `real_tokens` caps how many tokens can actually leave the curve.
pub fn quote_buy(
    virtual_sol: u64,
    virtual_token: u64,
    real_tokens: u64,
    sol_in: u64,
) -> R<BuyQuote> {
    if sol_in == 0 {
        return Err(MathError::ZeroAmount);
    }
    let vs = virtual_sol as u128;
    let vt = virtual_token as u128;
    let k = m(vs.checked_mul(vt))?;
    let new_vs = m(vs.checked_add(sol_in as u128))?;
    // ceil(k / new_vs) so the pool keeps the rounding dust
    let new_vt = m(k.checked_add(new_vs - 1))? / new_vs;
    let raw_out = vt.saturating_sub(new_vt);

    if raw_out < real_tokens as u128 {
        return Ok(BuyQuote {
            tokens_out: to_u64(raw_out)?,
            sol_used: sol_in,
            completes_curve: false,
        });
    }

    // Demand exceeds what's left: sell exactly `real_tokens` and compute the
    // SOL needed for that, rounded up. The rest is refunded.
    let tokens_out = real_tokens as u128;
    let sol_used = sol_for_exact_tokens(vs, vt, tokens_out)?;
    Ok(BuyQuote {
        tokens_out: to_u64(tokens_out)?,
        sol_used: to_u64(sol_used.min(sol_in as u128))?,
        completes_curve: true,
    })
}

/// Lamports needed to take exactly `tokens_out` from the curve (rounded up).
fn sol_for_exact_tokens(vs: u128, vt: u128, tokens_out: u128) -> R<u128> {
    let k = m(vs.checked_mul(vt))?;
    let new_vt = m(vt.checked_sub(tokens_out))?;
    if new_vt == 0 {
        return Err(MathError::Overflow);
    }
    let new_vs = m(k.checked_add(new_vt - 1))? / new_vt;
    m(new_vs.checked_sub(vs))
}

/// Gross lamports out for selling `tokens_in` (before fees). Rounded down.
pub fn quote_sell(virtual_sol: u64, virtual_token: u64, tokens_in: u64) -> R<u64> {
    if tokens_in == 0 {
        return Err(MathError::ZeroAmount);
    }
    let vs = virtual_sol as u128;
    let vt = virtual_token as u128;
    let k = m(vs.checked_mul(vt))?;
    let new_vt = m(vt.checked_add(tokens_in as u128))?;
    let new_vs = m(k.checked_add(new_vt - 1))? / new_vt; // ceil keeps more SOL in pool
    to_u64(vs.saturating_sub(new_vs))
}

/// Anti-snipe fee schedule. Linear decay from `max_bps` at `start_slot` down to
/// `base_bps` at `start_slot + decay_slots`, flat afterwards.
pub fn fee_bps_at(
    slot: u64,
    start_slot: u64,
    decay_slots: u64,
    max_bps: u16,
    base_bps: u16,
) -> u16 {
    let (max_bps, base_bps) = (max_bps as u64, base_bps as u64);
    if decay_slots == 0 || max_bps <= base_bps {
        return base_bps as u16;
    }
    let elapsed = slot.saturating_sub(start_slot);
    if elapsed >= decay_slots {
        return base_bps as u16;
    }
    let span = max_bps - base_bps;
    // fee = max - span * elapsed / decay  (rounded so the fee never undershoots)
    let reduction = span * elapsed / decay_slots;
    (max_bps - reduction) as u16
}

/// Fee amount, rounded up (protocol never under-collects).
pub fn fee_amount(amount: u64, fee_bps: u16) -> R<u64> {
    let num = m((amount as u128).checked_mul(fee_bps as u128))?;
    let d = BPS_DENOM as u128;
    to_u64(m(num.checked_add(d - 1))? / d)
}

/// Pro-rata share for batch auction claims, rounded down.
/// Dust stays in the vault, so the sum of all claims never exceeds `pool`.
pub fn pro_rata(pool: u64, part: u64, total: u64) -> R<u64> {
    if total == 0 {
        return Ok(0);
    }
    let num = m((pool as u128).checked_mul(part as u128))?;
    to_u64(num / total as u128)
}

/// Split a collected fee into (to_holders, to_treasury).
/// The base part of the fee sends `holder_fee_bps / base_fee_bps` of itself to holders;
/// the cover-charge premium above base sends `cover_to_holders_bps` of itself to holders.
pub fn split_fee(fee: u64, fee_bps: u16, base_fee_bps: u16, holder_fee_bps: u16, cover_to_holders_bps: u16) -> R<(u64, u64)> {
    if fee == 0 || fee_bps == 0 {
        return Ok((0, fee));
    }
    let fee128 = fee as u128;
    let base_part = if fee_bps <= base_fee_bps { fee128 } else { m(fee128.checked_mul(base_fee_bps as u128))? / fee_bps as u128 };
    let premium = fee128 - base_part;
    let from_base = if base_fee_bps == 0 { 0 } else { m(base_part.checked_mul(holder_fee_bps.min(base_fee_bps) as u128))? / base_fee_bps as u128 };
    let from_premium = m(premium.checked_mul(cover_to_holders_bps.min(10_000) as u128))? / 10_000;
    let holders = to_u64(from_base + from_premium)?;
    Ok((holders, fee - holders))
}

/// Creator tokens unlocked at `now` under linear vesting.
pub fn vested(total: u64, start: i64, secs: i64, now: i64) -> u64 {
    if secs <= 0 {
        return total;
    }
    let elapsed = now.saturating_sub(start).clamp(0, secs) as u128;
    ((total as u128) * elapsed / secs as u128) as u64
}

/// Split `amount` by bps shares; the remainder goes to the last bucket.
pub fn split3(amount: u64, a_bps: u16, b_bps: u16) -> (u64, u64, u64) {
    let a = (amount as u128 * a_bps as u128 / 10_000) as u64;
    let b = (amount as u128 * b_bps as u128 / 10_000) as u64;
    (a, b, amount - a - b)
}

/// LP tokens for a Raydium CPMM deposit of at most `a` and `b`, given the pool's
/// tradeable reserves `ra`, `rb` and its LP supply. Raydium rounds the required
/// amounts up, so the result is checked against that and backed off if needed.
/// Returns (lp, a_needed, b_needed); all zero if nothing can be deposited.
pub fn lp_for_deposit(a: u64, b: u64, ra: u64, rb: u64, supply: u64) -> (u64, u64, u64) {
    if a == 0 || b == 0 || ra == 0 || rb == 0 || supply == 0 {
        return (0, 0, 0);
    }
    let s = supply as u128;
    let need = |lp: u128, r: u64| -> u128 { (lp * r as u128 + s - 1) / s };
    let mut lp = core::cmp::min(a as u128 * s / ra as u128, b as u128 * s / rb as u128);
    while lp > 0 && (need(lp, ra) > a as u128 || need(lp, rb) > b as u128) {
        lp -= 1;
    }
    if lp == 0 || lp > u64::MAX as u128 {
        return (0, 0, 0);
    }
    (lp as u64, need(lp, ra) as u64, need(lp, rb) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lp_deposit_never_needs_more_than_offered() {
        // Pool with 100 SOL and 200M tokens, LP supply 4.47e12.
        let (ra, rb, supply) = (100_000_000_000u64, 200_000_000_000_000u64, 4_472_135_954_999u64);
        for (a, b) in [(1_000_000_000u64, 1_000_000_000_000u64), (5_000, 7), (123_456_789, 999_999_999_999), (1, 1)] {
            let (lp, na, nb) = lp_for_deposit(a, b, ra, rb, supply);
            assert!(na <= a && nb <= b, "{a} {b} -> {lp} {na} {nb}");
            if lp > 0 {
                // Raydium's own ceiling math gives the same amounts.
                assert_eq!(na as u128, (lp as u128 * ra as u128 + supply as u128 - 1) / supply as u128);
            }
        }
        assert_eq!(lp_for_deposit(0, 5, ra, rb, supply), (0, 0, 0));
        assert_eq!(lp_for_deposit(5, 5, 0, rb, supply), (0, 0, 0));
    }

    const VS: u64 = 30_000_000_000; // 30 SOL virtual
    const VT: u64 = 1_073_000_000_000_000; // 1.073B tokens (6dp) virtual
    const RT: u64 = 793_100_000_000_000; // 793.1M real

    #[test]
    fn buy_then_sell_never_profits() {
        let q = quote_buy(VS, VT, RT, 1_000_000_000).unwrap();
        assert!(!q.completes_curve);
        let vs2 = VS + q.sol_used;
        let vt2 = VT - q.tokens_out;
        let back = quote_sell(vs2, vt2, q.tokens_out).unwrap();
        assert!(back <= 1_000_000_000, "round trip must not create SOL: {back}");
    }

    #[test]
    fn k_never_decreases() {
        let k0 = VS as u128 * VT as u128;
        let q = quote_buy(VS, VT, RT, 7_777_777_777).unwrap();
        let k1 = (VS + q.sol_used) as u128 * (VT - q.tokens_out) as u128;
        assert!(k1 >= k0);
        let s = quote_sell(VS + q.sol_used, VT - q.tokens_out, q.tokens_out / 3).unwrap();
        let k2 = (VS + q.sol_used - s) as u128 * (VT - q.tokens_out + q.tokens_out / 3) as u128;
        assert!(k2 >= k1);
    }

    #[test]
    fn huge_buy_clamps_and_refunds() {
        let q = quote_buy(VS, VT, RT, 1_000_000_000_000).unwrap(); // 1000 SOL
        assert!(q.completes_curve);
        assert_eq!(q.tokens_out, RT);
        assert!(q.sol_used < 1_000_000_000_000);
        // with these params ~85 SOL fills the curve
        assert!(q.sol_used > 80_000_000_000 && q.sol_used < 90_000_000_000, "{}", q.sol_used);
    }

    #[test]
    fn one_big_buy_equals_batch_split() {
        // The batch auction's core promise: buying as one aggregate order gives
        // every participant the same average price regardless of order.
        let total = 10_000_000_000u64;
        let q = quote_buy(VS, VT, RT, total).unwrap();
        let a = pro_rata(q.tokens_out, 3_000_000_000, total).unwrap();
        let b = pro_rata(q.tokens_out, 7_000_000_000, total).unwrap();
        assert!(a + b <= q.tokens_out);
        // same price per lamport (within rounding)
        let pa = a as u128 * 7;
        let pb = b as u128 * 3;
        assert!(pa.abs_diff(pb) <= 10);
    }

    #[test]
    fn sequential_first_buyer_beats_later_buyer() {
        // Shows what the batch auction prevents: without it, slot order = price.
        let first = quote_buy(VS, VT, RT, 3_000_000_000).unwrap();
        let second = quote_buy(VS + first.sol_used, VT - first.tokens_out, RT - first.tokens_out, 3_000_000_000).unwrap();
        assert!(first.tokens_out > second.tokens_out);
    }

    #[test]
    fn fee_decay_schedule() {
        assert_eq!(fee_bps_at(100, 100, 150, 5000, 100), 5000);
        assert_eq!(fee_bps_at(175, 100, 150, 5000, 100), 2550);
        assert_eq!(fee_bps_at(249, 100, 150, 5000, 100), 133);
        assert_eq!(fee_bps_at(250, 100, 150, 5000, 100), 100);
        assert_eq!(fee_bps_at(99_999, 100, 150, 5000, 100), 100);
        // monotonic non-increasing
        let mut prev = u16::MAX;
        for s in 100..300 {
            let f = fee_bps_at(s, 100, 150, 5000, 100);
            assert!(f <= prev);
            prev = f;
        }
    }

    #[test]
    fn fee_rounds_up() {
        assert_eq!(fee_amount(1, 100).unwrap(), 1);
        assert_eq!(fee_amount(10_000, 100).unwrap(), 100);
        assert_eq!(fee_amount(0, 100).unwrap(), 0);
    }

    #[test]
    fn pro_rata_never_over_allocates() {
        let pool = 123_456_789u64;
        let parts = [1u64, 2, 3, 5, 8, 13, 21, 34, 55, 89];
        let total: u64 = parts.iter().sum();
        let sum: u64 = parts.iter().map(|p| pro_rata(pool, *p, total).unwrap()).sum();
        assert!(sum <= pool);
        assert!(pool - sum < parts.len() as u64);
    }

    #[test]
    fn fee_split_base_and_cover() {
        // 1% fee, 0.25% to holders → a quarter of the fee
        assert_eq!(split_fee(10_000, 100, 100, 25, 10_000).unwrap(), (2_500, 7_500));
        // 50% fee: base part is 1/50 of it, premium 49/50 goes fully to holders
        let (h, t) = split_fee(5_000_000, 5_000, 100, 25, 10_000).unwrap();
        assert_eq!(h + t, 5_000_000);
        assert_eq!(h, 4_900_000 + 25_000);
        // half the premium to holders
        let (h2, _) = split_fee(5_000_000, 5_000, 100, 25, 5_000).unwrap();
        assert_eq!(h2, 2_450_000 + 25_000);
        assert_eq!(split_fee(0, 100, 100, 25, 10_000).unwrap(), (0, 0));
    }

    #[test]
    fn vesting_is_linear_and_capped() {
        assert_eq!(vested(1000, 100, 0, 50), 1000);
        assert_eq!(vested(1000, 100, 100, 50), 0);
        assert_eq!(vested(1000, 100, 100, 150), 500);
        assert_eq!(vested(1000, 100, 100, 10_000), 1000);
    }

    #[test]
    fn split3_conserves() {
        let (a, b, c) = split3(1_000_001, 2_500, 2_500);
        assert_eq!(a + b + c, 1_000_001);
        assert_eq!(a, 250_000);
    }

    #[test]
    fn zero_amounts_rejected() {
        assert_eq!(quote_buy(VS, VT, RT, 0), Err(MathError::ZeroAmount));
        assert_eq!(quote_sell(VS, VT, 0), Err(MathError::ZeroAmount));
    }
}
