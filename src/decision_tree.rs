use bitcoin::TxOut;
use concurrent_psbt::Join;
use concurrent_psbt::output::{OutputUniqueIdExt, UniqueId};
use concurrent_psbt::tx::UnorderedPsbt;
use psbt_v2::v2::{Global, Output, Psbt};

use crate::intent::{FixedPaymentInstructions, Intent, IntentId};
use crate::queue::Queue;
use crate::refinement::Refined;

/// Position of a batch within a [`Plan`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BatchId(pub usize);

/// Intents realized by one transaction.
///
/// Only the grouping. Funding is not the tree's choice — see [`Batch::fund`] — so the same
/// batch can be priced against whatever coins are left when its turn comes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Batch {
    pub intents: Vec<IntentId>,
}

/// One complete grouping of the interactive pool.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    pub batches: Vec<Batch>,
}

/// Where the next unplaced intent could go.
#[derive(Debug)]
pub enum DecisionTree {
    Branch(Vec<Edge>),
    Leaf(Plan),
}

/// One way to place one intent.
#[derive(Debug)]
pub struct Edge {
    pub intent: IntentId,

    /// The batch it joins. Equal to the number of batches so far when it opens a new one.
    pub batch: BatchId,

    pub subtree: Box<DecisionTree>,
}

/// Largest interactive pool we will enumerate exhaustively.
pub const MAX_EXHAUSTIVE_POOL: usize = 8;

/// How many plans a pool of `n` intents produces: the nth Bell number.
pub fn plan_count(n: usize) -> u128 {
    // Bell triangle: each row starts with the last entry of the row above.
    let mut row = vec![1u128];

    for _ in 0..n {
        let mut next = vec![*row.last().expect("row is never empty")];
        for value in &row {
            let sum = next.last().expect("seeded above").saturating_add(*value);
            next.push(sum);
        }
        row = next;

        // Bell numbers only grow, so once we peg there is nothing left to learn.
        if row[0] == u128::MAX {
            return u128::MAX;
        }
    }

    row[0]
}

impl Refined {
    /// Every way to partition the interactive pool into batches.
    ///
    /// Intents are placed in order, each joining an existing batch or opening a new one at
    /// the end. That restriction gives every partition exactly one path, so there are no
    /// duplicates to filter out.
    ///
    /// Refuses pools larger than [`MAX_EXHAUSTIVE_POOL`].
    pub fn enumerate(&self) -> Result<DecisionTree, EnumerationError> {
        self.enumerate_within(MAX_EXHAUSTIVE_POOL)
    }

    /// [`Refined::enumerate`] with an explicit cap, for callers that know their budget.
    pub fn enumerate_within(&self, limit: usize) -> Result<DecisionTree, EnumerationError> {
        let intents = self.interactive.len();
        if intents > limit {
            return Err(EnumerationError::PoolTooLarge {
                intents,
                plans: plan_count(intents),
                limit,
            });
        }

        Ok(expand(&self.interactive, &mut Vec::new()))
    }
}

/// Why a tree could not be built.
#[derive(Debug, PartialEq, Eq)]
pub enum EnumerationError {
    /// Too many intents to enumerate. Needs a search strategy, not a bigger machine.
    PoolTooLarge {
        intents: usize,
        plans: u128,
        limit: usize,
    },
}

fn expand(unplaced: &[IntentId], batches: &mut Vec<Vec<IntentId>>) -> DecisionTree {
    let Some((intent, rest)) = unplaced.split_first() else {
        return DecisionTree::Leaf(Plan {
            batches: batches
                .iter()
                .map(|intents| Batch {
                    intents: intents.clone(),
                })
                .collect(),
        });
    };

    // Join any existing batch, or open one more.
    let mut edges = Vec::with_capacity(batches.len() + 1);

    for existing in 0..batches.len() {
        batches[existing].push(*intent);
        edges.push(Edge {
            intent: *intent,
            batch: BatchId(existing),
            subtree: Box::new(expand(rest, batches)),
        });
        batches[existing].pop();
    }

    batches.push(vec![*intent]);
    edges.push(Edge {
        intent: *intent,
        batch: BatchId(batches.len() - 1),
        subtree: Box::new(expand(rest, batches)),
    });
    batches.pop();

    DecisionTree::Branch(edges)
}

impl DecisionTree {
    /// How many complete plans the tree holds.
    pub fn count_leaves(&self) -> usize {
        match self {
            DecisionTree::Leaf(_) => 1,
            DecisionTree::Branch(edges) => edges.iter().map(|e| e.subtree.count_leaves()).sum(),
        }
    }

    /// Every plan in the tree.
    pub fn plans(&self) -> Vec<&Plan> {
        match self {
            DecisionTree::Leaf(plan) => vec![plan],
            DecisionTree::Branch(edges) => edges.iter().flat_map(|e| e.subtree.plans()).collect(),
        }
    }
}

impl Batch {
    /// The single transaction realizing every intent in this batch.
    pub fn to_unordered_psbt(&self, queue: &Queue) -> Result<UnorderedPsbt, BatchError> {
        let mut psbts = self
            .intents
            .iter()
            .filter_map(|&id| queue.get(id))
            .filter_map(|entry| match &entry.inner {
                Intent::OnChain(psbt) => Some(Ok(psbt.clone())),
                Intent::PaymentRequest(request) => Some(pay_to(request)),
                Intent::Interactive(..) => None,
            });

        let first = psbts.next().ok_or(BatchError::Empty)??.wrap();

        psbts
            .try_fold(first, |acc, next| Ok(acc.join(next?.wrap())))?
            .try_unwrap()
            .map_err(|_| BatchError::Conflict)
    }
}

/// A payment request as the one output it asks for.
///
/// TODO: assumes it resolves to a single on-chain output. See
/// [`FixedPaymentInstructions`] as it may not be limited to that.
/// This is a util method that should be replace with something more general
fn pay_to(request: &FixedPaymentInstructions) -> Result<UnorderedPsbt, BatchError> {
    let mut output = Output::new(TxOut {
        value: request.amount,
        script_pubkey: request.script_pubkey.clone(),
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

    UnorderedPsbt::try_from_psbt(psbt).map_err(|_| BatchError::Malformed)
}

/// Why a set of intents can't be realized by one transaction.
#[derive(Debug)]
pub enum BatchError {
    /// The intents disagree about something a transaction can only say once, such as how
    /// to spend the same coin.
    Conflict,

    /// A batch of nothing is not a transaction.
    Empty,

    /// An intent could not be lowered to a well formed PSBT at all.
    Malformed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, TxOut, Txid, hashes::Hash};
    use concurrent_psbt::output::{OutputUniqueIdExt, UniqueId};
    use psbt_v2::v2::{Global, Input, Output, Psbt};
    use std::collections::HashSet;

    use crate::intent::IntentWithPolicy;

    /// An intent paying `sats`, spending outpoint `nonce`.
    fn on_chain_intent(nonce: u8, sats: u64) -> IntentWithPolicy {
        on_chain_intent_spending(nonce, nonce as u32, sats)
    }

    fn on_chain_intent_spending(nonce: u8, vout: u32, sats: u64) -> IntentWithPolicy {
        on_chain_intent_with_sequence(nonce, vout, sats, None)
    }

    fn on_chain_intent_with_sequence(
        nonce: u8,
        vout: u32,
        sats: u64,
        sequence: Option<Sequence>,
    ) -> IntentWithPolicy {
        let mut spk = vec![0x51, 0x20];
        spk.extend_from_slice(&[nonce; 32]);

        // Outputs need a unique ID before they can be keyed in an `OutputSet`.
        let mut output = Output::new(TxOut {
            value: Amount::from_sat(sats),
            script_pubkey: ScriptBuf::from_bytes(spk),
        });
        output.set_unique_id(UniqueId::generate());

        let mut input = Input::new(&OutPoint::new(Txid::all_zeros(), vout));
        input.sequence = sequence;

        let psbt = Psbt {
            global: Global {
                input_count: 1,
                output_count: 1,
                ..Global::default()
            },
            inputs: vec![input],
            outputs: vec![output],
        };

        IntentWithPolicy {
            inner: Intent::OnChain(UnorderedPsbt::try_from_psbt(psbt).unwrap()),
            payoff_curve: vec![],
            always_make_private: false,
        }
    }

    /// Nothing urgent, so refinement leaves everything batchable.
    fn pool(n: usize) -> Refined {
        Refined {
            unilateral: Vec::new(),
            interactive: (0..n).map(IntentId).collect(),
        }
    }

    /// Batch both, or keep them apart. Order is not a choice the tree makes.
    #[test]
    fn two_intents_give_two_plans() {
        let tree = pool(2).enumerate().unwrap();
        let plans = tree.plans();

        assert_eq!(plans.len(), 2);
        assert!(plans.contains(&&Plan {
            batches: vec![Batch {
                intents: vec![IntentId(0), IntentId(1)]
            }]
        }));
        assert!(plans.contains(&&Plan {
            batches: vec![
                Batch {
                    intents: vec![IntentId(0)]
                },
                Batch {
                    intents: vec![IntentId(1)]
                },
            ]
        }));
    }

    /// Bell numbers, not Fubini: the tree groups, it does not sequence.
    #[test]
    fn plan_counts_are_bell_numbers() {
        let counts: Vec<usize> = (1..=5)
            .map(|n| pool(n).enumerate().unwrap().count_leaves())
            .collect();

        // https://en.wikipedia.org/wiki/Bell_number
        assert_eq!(counts, vec![1, 2, 5, 15, 52]);
    }

    /// The two ends of the blockspace/privacy spectrum both have to be reachable.
    #[test]
    fn both_extremes_are_present() {
        let tree = pool(4).enumerate().unwrap();
        let plans = tree.plans();

        let all_together = plans.iter().filter(|p| p.batches.len() == 1).count();
        let all_apart = plans.iter().filter(|p| p.batches.len() == 4).count();

        assert_eq!(all_together, 1);
        assert_eq!(all_apart, 1);
    }

    /// Restricted growth means each partition appears once, with no dedup pass.
    #[test]
    fn no_partition_appears_twice() {
        let tree = pool(5).enumerate().unwrap();
        let plans = tree.plans();

        let distinct: HashSet<Vec<Vec<usize>>> = plans
            .iter()
            .map(|p| {
                let mut batches: Vec<Vec<usize>> = p
                    .batches
                    .iter()
                    .map(|b| b.intents.iter().map(|i| i.0).collect())
                    .collect();
                batches.sort();
                batches
            })
            .collect();

        assert_eq!(distinct.len(), plans.len());
    }

    #[test]
    fn an_empty_pool_has_one_empty_plan() {
        let tree = pool(0).enumerate().unwrap();

        assert_eq!(tree.count_leaves(), 1);
        assert_eq!(tree.plans(), vec![&Plan { batches: vec![] }]);
    }

    #[test]
    fn plan_count_matches_the_tree() {
        for n in 0..=6 {
            assert_eq!(
                plan_count(n),
                pool(n).enumerate().unwrap().count_leaves() as u128
            );
        }
    }

    #[test]
    fn plan_count_saturates_rather_than_overflowing() {
        // Bell(30) is ~8.5e23, past u64 but inside u128.
        assert!(plan_count(30) > u64::MAX as u128);
        assert_eq!(plan_count(100_000), u128::MAX);
    }

    /// The cap is what stands between us and a multi-gigabyte tree.
    #[test]
    fn oversized_pools_are_refused() {
        let err = pool(MAX_EXHAUSTIVE_POOL + 1).enumerate().unwrap_err();

        assert_eq!(
            err,
            EnumerationError::PoolTooLarge {
                intents: 9,
                plans: 21_147,
                limit: 8,
            }
        );
    }

    #[test]
    fn the_cap_itself_still_enumerates() {
        let tree = pool(MAX_EXHAUSTIVE_POOL).enumerate().unwrap();

        assert_eq!(tree.count_leaves(), 4_140);
    }

    #[test]
    fn callers_can_set_their_own_budget() {
        assert!(pool(4).enumerate_within(3).is_err());
        assert!(pool(4).enumerate_within(4).is_ok());
    }

    #[test]
    fn batching_joins_disjoint_intents() {
        let queue = Queue::new([on_chain_intent(1, 30_000), on_chain_intent(2, 45_000)]);
        let batch = Batch {
            intents: vec![IntentId(0), IntentId(1)],
        };

        let psbt = batch.to_unordered_psbt(&queue).unwrap();

        assert_eq!(psbt.inputs.len(), 2);
        assert_eq!(psbt.outputs.len(), 2);
    }

    /// A payment request brings an output and no input, so it batches with anything.
    #[test]
    fn batching_lowers_a_payment_request_to_its_output() {
        let request = IntentWithPolicy {
            inner: Intent::PaymentRequest(FixedPaymentInstructions {
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
                amount: Amount::from_sat(20_000),
            }),
            payoff_curve: vec![],
            always_make_private: false,
        };
        let queue = Queue::new([on_chain_intent(1, 30_000), request]);
        let batch = Batch {
            intents: vec![IntentId(0), IntentId(1)],
        };

        let psbt = batch.to_unordered_psbt(&queue).unwrap();

        assert_eq!(psbt.inputs.len(), 1);
        assert_eq!(psbt.outputs.len(), 2);
        assert!(
            psbt.outputs
                .clone()
                .into_iter()
                .any(|out| out.amount == Amount::from_sat(20_000))
        );
    }

    /// Two intents that want to spend one coin *differently* can't share a transaction.
    /// This is now a per-batch question, so it prunes one branch rather than killing the
    /// whole enumeration the way the old enumerator did.
    #[test]
    fn batching_conflicting_intents_fails() {
        let queue = Queue::new([
            on_chain_intent_with_sequence(1, 7, 30_000, Some(Sequence::ENABLE_RBF_NO_LOCKTIME)),
            on_chain_intent_with_sequence(2, 7, 45_000, Some(Sequence::MAX)),
        ]);
        let batch = Batch {
            intents: vec![IntentId(0), IntentId(1)],
        };

        assert!(matches!(
            batch.to_unordered_psbt(&queue),
            Err(BatchError::Conflict)
        ));
    }

    /// Two intents spending the same coin the *same* way merge into one input rather
    /// than conflicting — inputs are keyed by outpoint and the join is idempotent.
    ///
    /// The batch is still wrong: two payments now ride on one coin's worth of funding.
    /// Nothing here catches that, which is a job for coin selection once a batch has to
    /// balance.
    #[test]
    fn identical_spends_of_one_coin_silently_merge() {
        let queue = Queue::new([
            on_chain_intent_spending(1, 7, 30_000),
            on_chain_intent_spending(2, 7, 45_000),
        ]);
        let batch = Batch {
            intents: vec![IntentId(0), IntentId(1)],
        };

        let psbt = batch.to_unordered_psbt(&queue).unwrap();

        assert_eq!(psbt.inputs.len(), 1);
        assert_eq!(psbt.outputs.len(), 2);
    }

    #[test]
    fn an_empty_batch_is_not_a_transaction() {
        let queue = Queue::new([]);
        let batch = Batch { intents: vec![] };

        assert!(matches!(
            batch.to_unordered_psbt(&queue),
            Err(BatchError::Empty)
        ));
    }
}
