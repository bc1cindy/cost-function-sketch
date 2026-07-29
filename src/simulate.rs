//! Turning a leaf of the tree into a wallet state.
//!
//! A [`Plan`] is only a grouping. Scoring it means funding each batch, building the
//! transactions, and folding them over the wallet — leaving a [`WalletState`] the cost
//! function can compare against the one we started from.

use bitcoin::{Amount, Script};
use concurrent_psbt::output::{OutputUniqueIdExt, UniqueId};
use psbt_v2::v2::{Input, Output, Psbt};

use crate::decision_tree::{Batch, Plan};
use crate::selection::{CoinSelector, FundingError, SelectionParams};
use crate::wallet::{SimulationError, WalletState};

#[derive(Debug)]
pub enum RealizeError {
    Funding(FundingError),
    Simulation(SimulationError),
}

impl Plan {
    /// The wallet state this plan would generate
    ///
    /// Batches are funded in tree order against the coins still available, so a coin
    /// claimed by one is gone for the next. Only plans whose batches contend for the same
    /// coins are sensitive to that order; treating utxo exclusivity as a real constraint
    /// is a much larger machine.
    pub fn realize(
        &self,
        state: &WalletState,
        params: &SelectionParams,
        selector: &dyn CoinSelector,
        is_mine: impl Fn(&Script) -> bool + Copy,
    ) -> Result<WalletState, RealizeError> {
        let mut current = state.clone();

        for batch in &self.batches {
            let psbt = batch.to_funded_psbt(&current, params, selector)?;
            current = current
                .broadcast(&psbt, is_mine)
                .map_err(RealizeError::Simulation)?;
        }

        Ok(current)
    }
}

impl Batch {
    /// The transaction that realizes this batch: its intents joined, then funded.
    ///
    /// Outputs come out in join order rather than a canonical one. That is enough to
    /// simulate — vouts only have to be self-consistent for one batch to spend another's
    /// change — but it is not what should go on the wire. Sorting belongs wherever the
    /// winning plan becomes real transactions.
    pub fn to_funded_psbt(
        &self,
        state: &WalletState,
        params: &SelectionParams,
        selector: &dyn CoinSelector,
    ) -> Result<Psbt, RealizeError> {
        let funding = self
            .fund(state.queue(), state.utxos(), params, selector)
            .map_err(RealizeError::Funding)?;

        let mut psbt = self
            .to_unordered_psbt(state.queue())
            .map_err(|err| RealizeError::Funding(FundingError::Batch(err)))?
            .into_psbt();

        for utxo in &funding.inputs {
            psbt.inputs.push(Input::new(&utxo.outpoint));
            psbt.global.input_count += 1;
        }

        if let Some(change) = funding.change {
            // Outputs need a unique ID before they can be keyed in an `OutputSet`.
            // TODO: this is an awkward two step process. We should be able to do this in one step (Output::new_with_generated_unique_id)
            // or a builder pattern if there are more fields to set.
            let mut output = Output::new(change);
            output.set_unique_id(UniqueId::generate());
            psbt.outputs.push(output);
            psbt.global.output_count += 1;
        }

        Ok(psbt)
    }
}

impl WalletState {
    /// What this plan costs, in satoshis that left the wallet.
    ///
    /// Purely objective: cost of the blockspace this transction will take up.
    /// TODO: add privacy and timing costs.
    pub fn plan_cost(
        &self,
        plan: &Plan,
        params: &SelectionParams,
        selector: &dyn CoinSelector,
        is_mine: impl Fn(&Script) -> bool + Copy,
    ) -> Result<bitcoin::SignedAmount, RealizeError> {
        let after = plan.realize(self, params, selector, is_mine)?;
        Ok(self.objective_cost(&after))
    }
}

/// A change output below this is not worth creating at typical feerates.
pub const DEFAULT_DUST_LIMIT: Amount = Amount::from_sat(546);

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{FeeRate, OutPoint, ScriptBuf, TxOut, Txid, hashes::Hash};
    use concurrent_psbt::tx::UnorderedPsbt;
    use psbt_v2::v2::{Global, Psbt};

    use crate::intent::{Intent, IntentWithPolicy};
    use crate::queue::Queue;
    use crate::refinement::Refined;
    use crate::selection::{CoinGrinder, MAX_STANDARD_TX_WEIGHT};
    use crate::wallet::Utxo;
    use crate::{IntentId, Plan};

    fn p2tr(byte: u8) -> ScriptBuf {
        let mut spk = vec![0x51, 0x20];
        spk.extend_from_slice(&[byte; 32]);
        ScriptBuf::from_bytes(spk)
    }

    /// A payment of `sats` to a script nobody but the payee owns, with nothing funding it.
    fn payment(byte: u8, sats: u64) -> IntentWithPolicy {
        let mut output = Output::new(TxOut {
            value: Amount::from_sat(sats),
            script_pubkey: p2tr(byte),
        });
        output.set_unique_id(UniqueId::generate());

        let psbt = Psbt {
            global: Global {
                input_count: 0,
                output_count: 1,
                ..Global::default()
            },
            inputs: vec![],
            outputs: vec![output],
        };

        IntentWithPolicy {
            inner: Intent::OnChain(UnorderedPsbt::try_from_psbt(psbt).unwrap()),
            payoff_curve: vec![],
            always_make_private: false,
        }
    }

    fn coin(vout: u32, sats: u64) -> Utxo {
        Utxo {
            outpoint: OutPoint::new(Txid::all_zeros(), vout),
            txout: TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: p2tr(0xcc),
            },
        }
    }

    fn params() -> SelectionParams {
        SelectionParams {
            feerate: FeeRate::from_sat_per_vb(10).unwrap(),
            long_term_feerate: FeeRate::from_sat_per_vb(10).unwrap(),
            change_script: p2tr(0xcc),
            dust_limit: DEFAULT_DUST_LIMIT,
            max_selection_weight: MAX_STANDARD_TX_WEIGHT,
        }
    }

    /// The whole chain: a leaf of the tree becomes transactions, those become a new wallet
    /// state, and the difference is what the plan cost.
    #[test]
    fn a_leaf_prices_out_to_payments_plus_fees() {
        let queue = Queue::new([payment(0xaa, 50_000), payment(0xbb, 30_000)]);
        let state = WalletState::new([coin(0, 500_000)], queue);
        let params = params();
        let is_mine = |spk: &bitcoin::Script| spk == params.change_script.as_script();

        let plans = Refined {
            unilateral: Vec::new(),
            interactive: vec![IntentId(0), IntentId(1)],
        }
        .enumerate()
        .unwrap();

        let costs: Vec<_> = plans
            .plans()
            .iter()
            .map(|plan| {
                state
                    .plan_cost(plan, &params, &CoinGrinder, is_mine)
                    .unwrap()
                    .to_sat()
            })
            .collect();

        // Both plans move the same 80,000 sat to the payees; they differ only in fees.
        assert_eq!(costs.len(), 2);
        for cost in &costs {
            assert!(*cost > 80_000, "cost {cost} does not cover the payments");
            assert!(
                *cost < 85_000,
                "cost {cost} is implausibly high for two payments"
            );
        }
    }

    /// One transaction pays overhead and change once; two pay for both twice.
    #[test]
    fn batching_beats_splitting() {
        let queue = Queue::new([payment(0xaa, 50_000), payment(0xbb, 30_000)]);
        let state = WalletState::new([coin(0, 500_000)], queue);
        let params = params();
        let is_mine = |spk: &bitcoin::Script| spk == params.change_script.as_script();

        let together = Plan {
            batches: vec![Batch {
                intents: vec![IntentId(0), IntentId(1)],
            }],
        };
        let apart = Plan {
            batches: vec![
                Batch {
                    intents: vec![IntentId(0)],
                },
                Batch {
                    intents: vec![IntentId(1)],
                },
            ],
        };

        let batched = state
            .plan_cost(&together, &params, &CoinGrinder, is_mine)
            .unwrap();
        let split = state
            .plan_cost(&apart, &params, &CoinGrinder, is_mine)
            .unwrap();

        assert!(
            batched < split,
            "batching cost {batched} should beat splitting at {split}"
        );
    }
}
