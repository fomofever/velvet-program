use anchor_lang::prelude::*;

use crate::math::MathError;

#[error_code]
pub enum FairError {
    #[msg("Math overflow")]
    Overflow,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Invalid config parameters")]
    InvalidConfig,
    #[msg("Name, symbol or URI too long")]
    MetadataTooLong,
    #[msg("Auction window outside allowed bounds")]
    InvalidAuctionWindow,
    #[msg("Creator buy exceeds the allowed maximum")]
    CreatorBuyTooLarge,
    #[msg("Opening auction is closed")]
    AuctionClosed,
    #[msg("Opening auction is still running")]
    AuctionRunning,
    #[msg("Commit exceeds per-wallet cap")]
    CommitCapExceeded,
    #[msg("Auction already settled")]
    AlreadySettled,
    #[msg("Auction not settled yet; curve trading is locked")]
    NotSettled,
    #[msg("Bonding curve is complete")]
    CurveComplete,
    #[msg("Output below minimum (slippage)")]
    Slippage,
    #[msg("Wrong treasury account")]
    WrongTreasury,
    #[msg("Only the creator can do this")]
    NotCreator,
    #[msg("Launch was cancelled: the line closed below its minimum")]
    LaunchCancelled,
    #[msg("Launch was not cancelled")]
    NotCancelled,
    #[msg("Minimum raise is above the house limit")]
    MinRaiseTooHigh,
    #[msg("Vesting period is above the house limit")]
    VestTooLong,
    #[msg("Only the reward distributor can do this")]
    NotDistributor,
    #[msg("Reward recipient is not a holder of this coin")]
    BadRewardAccount,
    #[msg("Payout exceeds the reward pool")]
    RewardOverdraw,
    #[msg("Curve is not complete yet")]
    NotComplete,
    #[msg("Already graduated")]
    AlreadyGraduated,
    #[msg("Not graduated yet")]
    NotGraduated,
    #[msg("Wrong Raydium account")]
    WrongPoolAccount,
    #[msg("Nothing to claim yet")]
    NothingToClaim,
    #[msg("Not enough SOL in the curve to graduate")]
    GraduationUnderfunded,
    #[msg("No config change is queued")]
    NoPendingChange,
    #[msg("The timelock on this config change hasn't passed yet")]
    TimelockNotReady,
    #[msg("Raydium program, AMM config and locker can't be changed")]
    ImmutableField,
    #[msg("Only the proposed new authority can accept")]
    NotPendingAuthority,
    #[msg("Too soon for the next reward epoch")]
    EpochTooSoon,
    #[msg("This reward epoch's payout window has closed")]
    EpochWindowClosed,
    #[msg("Reward epoch out of order")]
    BadEpoch,
    #[msg("Payout is above the recipient's share of the epoch")]
    RewardAboveShare,
    #[msg("Payout exceeds this epoch's budget")]
    EpochBudgetExceeded,
    #[msg("This buy would take your wallet above 3% of the supply while the coin is bonding")]
    WalletCapExceeded,
    #[msg("Launch approval is missing, malformed or doesn't match this launch")]
    BadApproval,
    #[msg("Launch approval has expired")]
    ApprovalExpired,
    #[msg("Wrong storage wallet")]
    WrongStorageWallet,
}

impl From<MathError> for FairError {
    fn from(e: MathError) -> Self {
        match e {
            MathError::Overflow => FairError::Overflow,
            MathError::ZeroAmount => FairError::ZeroAmount,
        }
    }
}

pub trait MathExt<T> {
    fn fx(self) -> Result<T>;
}

impl<T> MathExt<T> for core::result::Result<T, MathError> {
    fn fx(self) -> Result<T> {
        self.map_err(|e| error!(FairError::from(e)))
    }
}
