// Derived from raydium-io/raydium-cp-swap (Apache-2.0), commit 0f86d46:
// programs/cp-swap/src/curve/constant_product.rs.
// Modified by Velvet: removed the LP-token conversions (Velvet Pools have no LP tokens) and
// swap-base-output; `swap_base_input_without_fees` is unchanged.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConstantProductCurve;

impl ConstantProductCurve {
    /// Constant product swapping function
    /// Constant product swap ensures x * y = constant
    /// The constant product swap calculation, factored out of its class for reuse.
    ///
    /// This is guaranteed to work for all values such that:
    ///  - 1 <= swap_source_amount * swap_destination_amount <= u128::MAX
    ///  - 1 <= source_amount <= u64::MAX
    pub fn swap_base_input_without_fees(
        source_amount: u128,
        swap_source_amount: u128,
        swap_destination_amount: u128,
    ) -> u128 {
        // (x + delta_x) * (y - delta_y) = x * y
        // delta_y = (delta_x * y) / (x + delta_x)
        let numerator = source_amount.checked_mul(swap_destination_amount).unwrap();
        let denominator = swap_source_amount.checked_add(source_amount).unwrap();
        let destinsation_amount_swapped = numerator.checked_div(denominator).unwrap();
        destinsation_amount_swapped
    }
}
