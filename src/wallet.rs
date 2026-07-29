//! What the wallet owns, and how actions change it.

use std::collections::HashSet;

use bitcoin::{Amount, OutPoint, Script, SignedAmount, TxOut};
use psbt_v2::v2::{DetermineLockTimeError, Psbt, Signer};

use crate::queue::Queue;

/// A coin the wallet can spend.
#[derive(Clone, Debug)]
pub struct Utxo {
    pub outpoint: OutPoint,
    pub txout: TxOut,
}

/// What the wallet owns and what it has been asked to do.
///
/// The state a plan is simulated against. Excludes time and feerate: those are
/// observations of the outside world, supplied alongside rather than baked in.
#[derive(Clone)]
pub struct WalletState {
    /// Coins available to fund or complete an [`crate::intent::Intent`].
    utxos: Vec<Utxo>,
    /// Intents not yet satisfied.
    queue: Queue,
}

impl WalletState {
    pub fn new(utxos: impl IntoIterator<Item = Utxo>, queue: Queue) -> Self {
        WalletState {
            utxos: utxos.into_iter().collect(),
            queue,
        }
    }

    pub fn utxos(&self) -> &[Utxo] {
        &self.utxos
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Total value of spendable coins.
    pub fn balance(&self) -> Amount {
        self.utxos.iter().map(|u| u.txout.value).sum()
    }

    /// How much value left the wallet getting from here to `simulated`.
    ///
    /// Positive when the wallet pays out, negative when it receives. Neither change nor
    /// fees are special cases: change is a coin in `simulated` that never left, and fees
    /// are value that left and came back as nothing.
    pub fn objective_cost(&self, simulated: &WalletState) -> SignedAmount {
        let before = self.balance().to_sat() as i64;
        let after = simulated.balance().to_sat() as i64;
        SignedAmount::from_sat(before - after)
    }

    /// Spend the coins `psbt` consumes, credit the ones it pays us.
    ///
    /// Outputs are named by their position, so whatever order the caller built them in is
    /// the order simulated. Fine for scoring; a transaction going on the wire wants a
    /// canonical order first.
    pub fn broadcast(
        &self,
        psbt: &Psbt,
        is_mine: impl Fn(&Script) -> bool,
    ) -> Result<WalletState, SimulationError> {
        // NOTE: correct only while every input is SegWit. A legacy input's scriptSig is
        // part of the txid, so for those this predicts outpoints that signing will
        // change. Worth enforcing rather than assuming.
        //
        // Not `Psbt::id()`: that zeroes sequence numbers, so it identifies the PSBT
        // across updates rather than naming the transaction that will confirm.
        let tx = Signer::new(psbt.clone())
            .map_err(SimulationError::LockTime)?
            .unsigned_tx();
        let txid = tx.compute_txid();

        let spent: HashSet<OutPoint> = tx.input.iter().map(|txin| txin.previous_output).collect();

        let surviving = self
            .utxos
            .iter()
            .filter(|u| !spent.contains(&u.outpoint))
            .cloned();

        let created = tx
            .output
            .iter()
            .enumerate()
            .filter(|(_, txout)| is_mine(&txout.script_pubkey))
            .map(|(vout, txout)| Utxo {
                outpoint: OutPoint::new(txid, vout as u32),
                txout: txout.clone(),
            });

        Ok(WalletState {
            utxos: surviving.chain(created).collect(),
            // TODO: drop the intents this transaction satisfies. Deciding which queue
            // entries a transaction realizes is its own problem.
            queue: self.queue.clone(),
        })
    }
}

/// Why a simulation could not be carried out.
#[derive(Debug)]
pub enum SimulationError {
    /// The inputs disagree about lock time, so no transaction can be formed.
    LockTime(DetermineLockTimeError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{ScriptBuf, Txid, hashes::Hash};
    use psbt_v2::v2::{Global, Input, Output};

    fn utxo(vout: u32, sats: u64) -> Utxo {
        Utxo {
            outpoint: OutPoint::new(Txid::all_zeros(), vout),
            txout: TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: ScriptBuf::new(),
            },
        }
    }

    fn state(utxos: Vec<Utxo>) -> WalletState {
        WalletState::new(utxos, Queue::new([]))
    }

    #[test]
    fn paying_with_change_costs_payment_plus_fee() {
        // Spend a 100k coin, pay 30k out, 69k comes back as change: 31k left the wallet.
        let before = state(vec![utxo(0, 100_000)]);
        let after = state(vec![utxo(1, 69_000)]);

        assert_eq!(
            before.objective_cost(&after),
            SignedAmount::from_sat(31_000)
        );
    }

    #[test]
    fn receiving_is_negative_cost() {
        let before = state(vec![]);
        let after = state(vec![utxo(0, 50_000)]);

        assert_eq!(
            before.objective_cost(&after),
            SignedAmount::from_sat(-50_000)
        );
    }

    /// Spend a 100k coin, pay 30k to a stranger, take 69k back as change. The wallet is
    /// out 31k: the payment plus a 1k fee.
    #[test]
    fn broadcasting_spends_inputs_and_credits_change() {
        let change_spk = ScriptBuf::from_bytes(vec![0x51]); // OP_TRUE, stands in for ours
        let stranger_spk = ScriptBuf::from_bytes(vec![0x52]);

        let coin = utxo(0, 100_000);
        let before = state(vec![coin.clone(), utxo(1, 5_000)]);

        let psbt = Psbt {
            global: Global::default(),
            inputs: vec![Input::new(&coin.outpoint)],
            outputs: vec![
                Output::new(TxOut {
                    value: Amount::from_sat(30_000),
                    script_pubkey: stranger_spk,
                }),
                Output::new(TxOut {
                    value: Amount::from_sat(69_000),
                    script_pubkey: change_spk.clone(),
                }),
            ],
        };

        let after = before.broadcast(&psbt, |spk| *spk == *change_spk).unwrap();

        // The spent coin is gone, the untouched one survives, change is credited.
        assert_eq!(after.utxos().len(), 2);
        assert_eq!(after.balance(), Amount::from_sat(74_000));
        assert_eq!(
            before.objective_cost(&after),
            SignedAmount::from_sat(31_000)
        );

        // Change is spendable at a real outpoint, not a placeholder.
        let change = after
            .utxos()
            .iter()
            .find(|u| u.txout.value == Amount::from_sat(69_000));
        assert_eq!(change.unwrap().outpoint.vout, 1);
    }

    #[test]
    fn consolidating_costs_only_the_fee() {
        // Two coins merged into one; nothing left the wallet but the fee.
        let before = state(vec![utxo(0, 40_000), utxo(1, 60_000)]);
        let after = state(vec![utxo(2, 99_500)]);

        assert_eq!(before.objective_cost(&after), SignedAmount::from_sat(500));
    }
}
