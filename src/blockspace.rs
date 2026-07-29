//! Blockspace accounting.
//!
//! What it costs, in satoshis, to put something in a block. Everything here falls out of
//! consensus rules and the feerate, with no appeal to what the user prefers.

use bitcoin::{Amount, FeeRate, SignedAmount, TxOut, VarInt, Weight};

use crate::wallet::Utxo;

impl Utxo {
    /// Weight an input spending this coin adds, witness included.
    ///
    /// `None` for anything whose spend size depends on a policy we don't have (P2WSH,
    /// legacy, script-path taproot).
    pub fn input_weight(&self) -> Option<Weight> {
        // Base for every input: 36 outpoint + 1 empty scriptSig + 4 sequence.
        const BASE_VB: u64 = 41;

        let witness_wu = if self.txout.script_pubkey.is_p2tr() {
            66 // 1 item + 1 len + 64 byte Schnorr sig (key path spend)
        } else if self.txout.script_pubkey.is_p2wpkh() {
            108 // 2 items + (1 + 72) sig + (1 + 33) pubkey
        } else {
            return None;
        };

        Some(Weight::from_vb(BASE_VB)? + Weight::from_wu(witness_wu))
    }

    /// Value minus the blockspace needed to spend it — the number coin selection adds up.
    ///
    /// Negative means the coin costs more to spend than it's worth: dust, at this feerate.
    pub fn effective_value(&self, feerate: FeeRate) -> Option<SignedAmount> {
        let spend_cost = feerate.fee_wu(self.input_weight()?)?;
        Some(self.txout.value.to_signed().ok()? - spend_cost.to_signed().ok()?)
    }
}

/// Blockspace accounting for an output the wallet is considering creating.
pub trait EffectiveCost {
    /// Value plus the blockspace needed to create it: the mirror of
    /// [`Utxo::effective_value`], and what coin selection has to cover.
    ///
    /// Always knowable, unlike an input's weight — an output is just a value and a
    /// scriptPubKey, with no spending policy involved.
    fn effective_cost(&self, feerate: FeeRate) -> Option<Amount>;
}

impl EffectiveCost for TxOut {
    fn effective_cost(&self, feerate: FeeRate) -> Option<Amount> {
        let creation_cost = feerate.fee_wu(self.weight())?;
        self.value.checked_add(creation_cost)
    }
}

/// Weight a transaction carries before its inputs and outputs: version, lock time, the
/// two count fields, and the SegWit marker and flag.
///
/// 42 WU (10.5 vB) upward as the counts pass a one byte varint. Nobody owns this — it is
/// the shared cost that makes batching cheaper than separate transactions.
///
/// Assumes SegWit, like the rest of this crate.
pub fn overhead_weight(input_count: usize, output_count: usize) -> Weight {
    // 4 version + 4 lock time, plus the two counts, all non-witness.
    let base_vb = 8 + VarInt(input_count as u64).size() + VarInt(output_count as u64).size();

    // Marker and flag are witness bytes: 2 WU, not 8.
    Weight::from_vb_unchecked(base_vb as u64) + Weight::from_wu(2)
}

/// How many satoshis a batch is short, with every part charged for its blockspace.
///
/// Positive is underfunded — coin selection must find that much more effective value.
/// Negative is surplus, which becomes change or fee. Zero funds it exactly, changeless.
///
/// `None` if any input's spend size is unknown, or the arithmetic overflows.
pub fn deficit(inputs: &[Utxo], outputs: &[TxOut], feerate: FeeRate) -> Option<SignedAmount> {
    let mut deficit = feerate
        .fee_wu(overhead_weight(inputs.len(), outputs.len()))?
        .to_signed()
        .ok()?;

    for output in outputs {
        deficit = deficit.checked_add(output.effective_cost(feerate)?.to_signed().ok()?)?;
    }

    for input in inputs {
        deficit = deficit.checked_sub(input.effective_value(feerate)?)?;
    }

    Some(deficit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{OutPoint, ScriptBuf, Txid, hashes::Hash};

    fn utxo_with(spk: Vec<u8>, sats: u64) -> Utxo {
        Utxo {
            outpoint: OutPoint::new(Txid::all_zeros(), 0),
            txout: TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: ScriptBuf::from_bytes(spk),
            },
        }
    }

    fn p2tr_spk() -> Vec<u8> {
        let mut spk = vec![0x51, 0x20]; // OP_1 PUSH32
        spk.extend_from_slice(&[0xab; 32]);
        spk
    }

    fn p2tr_utxo(sats: u64) -> Utxo {
        utxo_with(p2tr_spk(), sats)
    }

    #[test]
    fn p2tr_effective_value_discounts_the_spend() {
        let coin = p2tr_utxo(100_000);

        // 41 vB base + 66 WU witness = 230 WU = 57.5 vB.
        assert_eq!(coin.input_weight().unwrap(), Weight::from_wu(230));

        // At 10 sat/vB that spend costs 575 sat.
        let feerate = FeeRate::from_sat_per_vb(10).unwrap();
        assert_eq!(
            coin.effective_value(feerate).unwrap(),
            SignedAmount::from_sat(99_425)
        );
    }

    /// A coin worth less than its own input is dust: spending it loses money.
    #[test]
    fn dust_has_negative_effective_value() {
        let coin = p2tr_utxo(400);
        let feerate = FeeRate::from_sat_per_vb(10).unwrap();

        assert_eq!(
            coin.effective_value(feerate).unwrap(),
            SignedAmount::from_sat(-175)
        );
    }

    /// Script types whose spend size depends on a policy we don't have.
    #[test]
    fn unknown_script_type_has_no_known_weight() {
        assert!(utxo_with(vec![], 100_000).input_weight().is_none());
    }

    #[test]
    fn p2tr_effective_cost_includes_the_output() {
        let txout = TxOut {
            value: Amount::from_sat(30_000),
            script_pubkey: ScriptBuf::from_bytes(p2tr_spk()),
        };

        // 8 value + 1 length + 34 script = 43 vB.
        assert_eq!(txout.weight(), Weight::from_vb(43).unwrap());

        // At 10 sat/vB the output costs 430 sat to create, on top of its value.
        let feerate = FeeRate::from_sat_per_vb(10).unwrap();
        assert_eq!(
            txout.effective_cost(feerate).unwrap(),
            Amount::from_sat(30_430)
        );
    }

    /// A coin's effective value and an output's effective cost have to agree, or coin
    /// selection can't balance anything: paying a coin straight through to an identical
    /// output leaves exactly the transaction overhead unfunded.
    #[test]
    fn effective_value_and_cost_are_mirrors() {
        let feerate = FeeRate::from_sat_per_vb(10).unwrap();
        let coin = p2tr_utxo(100_000);

        let payment = TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: coin.txout.script_pubkey.clone(),
        };

        // Spending 100k into a 100k output is short by exactly the input's 575 sat and
        // the output's 430 sat.
        let have = coin.effective_value(feerate).unwrap();
        let need = payment
            .effective_cost(feerate)
            .unwrap()
            .to_signed()
            .unwrap();

        assert_eq!(need - have, SignedAmount::from_sat(1_005));
    }

    #[test]
    fn overhead_is_ten_and_a_half_vbytes() {
        // 8 base + 1 input count + 1 output count = 10 vB, plus 2 WU marker and flag.
        assert_eq!(overhead_weight(1, 2), Weight::from_wu(42));

        // Counts past 252 need a 3 byte varint.
        assert_eq!(overhead_weight(253, 2), Weight::from_wu(50));
    }

    /// Overhead is the whole reason batching beats separate transactions: two intents in
    /// one transaction pay for it once instead of twice.
    #[test]
    fn batching_pays_overhead_once() {
        let feerate = FeeRate::from_sat_per_vb(10).unwrap();

        // Two separate transactions, one input and two outputs each.
        let separate = feerate.fee_wu(overhead_weight(1, 2)).unwrap()
            + feerate.fee_wu(overhead_weight(1, 2)).unwrap();

        // One batched transaction: two inputs, three outputs (both payments, one change).
        let batched = feerate.fee_wu(overhead_weight(2, 3)).unwrap();

        assert_eq!(separate, Amount::from_sat(210));
        assert_eq!(batched, Amount::from_sat(105));
    }
}
