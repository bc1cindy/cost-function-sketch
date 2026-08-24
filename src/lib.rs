//! The wallet holds a queue of [`Intent`]s rather than broadcasting the instant it is
//! asked. It enumerates the ways they could be realized, simulates each, and scores the
//! resulting wallet states in satoshis. 
//!
//! - [`intent`]: what the user wants, and how much they care
//! - [`queue`]: the outstanding obligations
//! - [`refinement`]: pulling out the intents that can't wait for peers. e.g due too soon to be considered for batching
//! - [`decision_tree`]: which intents share a transaction
//! - [`selection`]: which coins fund a batch, behind a swappable [`CoinSelector`]
//! - [`simulate`]: turning a leaf into a wallet state, and pricing it
//! - [`wallet`]: what the wallet owns, and how state transitions change it
//! - [`blockspace`]: what it costs to put something in a block

// Pipeline:
// Get the powerset of outcomes
// Refine over the set of outcomes
// collect tree of mutually exclusive actions we can take. Satifiying sets of outcomes
// Result of the sequence => you simulate the result of the action and get the delta wallet state.
// Given we have a tree of actions, the cost function will pick the least costly branch and will assign timestamp to execute
// Unless new information is presented.
// Generating the tree is combinatorical explosion.
// 1. Approx. optimization. Random sampling
// 2. Brute force systmatically go thru the space and order it.
// Feed into cost function -> pos or negative satoshis score

pub mod blockspace;
pub mod decision_tree;
pub mod intent;
pub mod privacy;
pub mod queue;
pub mod refinement;
pub mod selection;
pub mod simulate;
pub mod wallet;

pub use blockspace::{EffectiveCost, deficit, overhead_weight};
pub use decision_tree::{
    Batch, BatchError, BatchId, DecisionTree, Edge, EnumerationError, MAX_EXHAUSTIVE_POOL, Plan,
    plan_count,
};
pub use intent::{
    FixedPaymentInstructions, INFINITE_COST, Intent, IntentId, IntentWithPolicy, PeerIdentity,
};
pub use queue::Queue;
pub use refinement::Refined;
pub use selection::{
    CoinGrinder, CoinSelector, Funding, FundingError, FundingRequest,
    MAX_STANDARD_TX_WEIGHT, SelectionError, SelectionParams,
};
pub use simulate::{DEFAULT_DUST_LIMIT, RealizeError};
pub use wallet::{SimulationError, Utxo, WalletState};
