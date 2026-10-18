// Derived from raydium-io/raydium-cp-swap (Apache-2.0), commit 0f86d46: programs/cp-swap/src/curve.
// Modified by Velvet: kept only the constant-product swap math and fee rounding; fees are always
// taken on the SOL side (see calculator.rs).
pub mod calculator;
pub mod constant_product;
pub mod fees;
