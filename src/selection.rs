//! Which coins fund a batch.
//! [`CoinSelector`] is the contract; one implementations ship here:
//!
//! - [`CoinGrinder`] minimizes selection weight and always makes change. Always answers.
//!
//! Both target [`FundingRequest::deficit`]. i.e the amount that is needed to fund the batch.
//! some intents may already specify some input amount.

use bitcoin::{Amount, FeeRate, OutPoint, ScriptBuf, SignedAmount, TxOut, Weight};
use bitcoin_coin_selection::errors::SelectionError as SolverError;
use bitcoin_coin_selection::{Spendable, coin_grinder};

use crate::blockspace::deficit;
use crate::decision_tree::{Batch, BatchError};
use crate::queue::Queue;
use crate::wallet::Utxo;

/// Everything a selector needs that isn't wallet state: observations of the outside
/// world, not properties of the wallet.
#[derive(Clone, Debug)]
pub struct SelectionParams {
    /// What we must pay now.
    pub feerate: FeeRate,

    /// What we expect to pay to spend change later. Decides whether change is worth
    /// making at all.
    pub long_term_feerate: FeeRate,

    /// Where change goes, if there is any.
    pub change_script: ScriptBuf,

    /// Change below this is not worth creating.
    pub dust_limit: Amount,

    /// Ceiling on the weight of the selected inputs.
    pub max_selection_weight: Weight,
}

/// The heaviest a standard transaction may be.
pub const MAX_STANDARD_TX_WEIGHT: Weight = Weight::from_wu(400_000);

/// One funding problem, posed without reference to how it gets solved.
pub struct FundingRequest<'a> {
    /// Coins the batch's intents already commit to spending. Context for the arithmetic,
    /// not candidates; a selector must not return them in [`Funding::inputs`].
    pub required: &'a [Utxo],

    /// Coins the selector may draw on. Disjoint from `required`.
    pub available: &'a [Utxo],

    /// What the transaction has to pay, change excluded.
    pub outputs: &'a [TxOut],

    pub params: &'a SelectionParams,
}

impl FundingRequest<'_> {
    /// Effective value a solver has to find. Negative means the required coins already
    /// over-fund the batch.
    ///
    /// Overhead uses the counts known now, so it undercounts by a few vbytes if selection
    /// pushes either count past 253. Not worth iterating to a fixed point.
    pub fn deficit(&self) -> Option<SignedAmount> {
        deficit(self.required, self.outputs, self.params.feerate)
    }
}

/// A strategy for closing a batch's funding gap.
///
/// Implementations may be exact, heuristic, or arbitrary — the planner prices whatever
/// comes back rather than assuming it is optimal.
pub trait CoinSelector {
    fn select(&self, request: FundingRequest<'_>) -> Result<Funding, SelectionError>;
}

/// What a batch spends beyond what its intents commit to, and what it hands back.
#[derive(Clone, Debug, Default)]
pub struct Funding {
    /// Coins the selector added. Excludes [`FundingRequest::required`].
    pub inputs: Vec<Utxo>,

    /// Change, when the surplus is worth an output.
    pub change: Option<TxOut>,
}

/// Why a selector could not fund a request.
#[derive(Debug, PartialEq, Eq)]
pub enum SelectionError {
    /// A coin whose spend size we can't predict see [`Utxo::input_weight`].
    UnknownScriptType,
    /// Not enough effective value on offer to cover the request.
    Insufficient,
    /// Every solution would exceed [`SelectionParams::max_selection_weight`].
    TooHeavy,
    /// Search finished empty. Says nothing about whether a solution exists —
    /// [`BranchAndBound`] reports this routinely.
    NoSolution,
    /// The arithmetic left the range of an amount.
    Overflow,
}

impl From<SolverError> for SelectionError {
    fn from(err: SolverError) -> Self {
        match err {
            SolverError::InsufficentFunds => SelectionError::Insufficient,
            SolverError::MaxWeightExceeded => SelectionError::TooHeavy,
            SolverError::Overflow(_) => SelectionError::Overflow,
            SolverError::IterationLimitReached
            | SolverError::ProgramError
            | SolverError::SolutionNotFound => SelectionError::NoSolution,
        }
    }
}

/// Why a batch could not be funded.
#[derive(Debug)]
pub enum FundingError {
    /// The intents can't be realized by one transaction.
    Batch(BatchError),

    /// The batch spends an outpoint the wallet doesn't hold, so its value is unknown.
    ForeignInput(OutPoint),

    /// The selector declined.
    Selection(SelectionError),
}

impl From<BatchError> for FundingError {
    fn from(err: BatchError) -> Self {
        FundingError::Batch(err)
    }
}

impl From<SelectionError> for FundingError {
    fn from(err: SelectionError) -> Self {
        FundingError::Selection(err)
    }
}

// TODO: move batch funding to the same file where the struct is defined.
impl Batch {
    /// Pose this batch's funding problem to `selector`, splitting `available` into the
    /// coins its intents already commit to and the ones still discretionary.
    pub fn fund(
        &self,
        queue: &Queue,
        available: &[Utxo],
        params: &SelectionParams,
        selector: &dyn CoinSelector,
    ) -> Result<Funding, FundingError> {
        let psbt = self.to_unordered_psbt(queue)?.into_psbt();

        let committed: Vec<OutPoint> = psbt
            .inputs
            .iter()
            .map(|input| OutPoint::new(input.previous_txid, input.spent_output_index))
            .collect();

        let required = committed
            .iter()
            .map(|outpoint| {
                available
                    .iter()
                    .find(|utxo| utxo.outpoint == *outpoint)
                    .cloned()
                    .ok_or(FundingError::ForeignInput(*outpoint))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let discretionary: Vec<Utxo> = available
            .iter()
            .filter(|utxo| !committed.contains(&utxo.outpoint))
            .cloned()
            .collect();

        let outputs: Vec<TxOut> = psbt
            .outputs
            .clone()
            .into_iter()
            .map(|output| TxOut {
                value: output.amount,
                script_pubkey: output.script_pubkey,
            })
            .collect();

        Ok(selector.select(FundingRequest {
            required: &required,
            available: &discretionary,
            outputs: &outputs,
            params,
        })?)
    }
}

/// Branch and bound over weights, minimizing the size of the selection.
///
/// Always makes change, and always answers when the money is there. Right at high
/// feerates; the tradeoff is a fragmented utxo set, since it leaves heavy coins behind.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoinGrinder;

impl CoinSelector for CoinGrinder {
    fn select(&self, request: FundingRequest<'_>) -> Result<Funding, SelectionError> {
        let params = request.params;
        let target = target_of(&request)?;

        let Target::Short(target) = target else {
            return surplus_to_change(&request, target);
        };

        // Only accept selections leaving enough to pay for a change output and still
        // clear dust.
        let change_target = params
            .dust_limit
            .checked_add(fee(params.feerate, change_output_weight(params))?)
            .ok_or(SelectionError::Overflow)?;

        let coins = Coin::from_utxos(request.available)?;

        let (_, selected) = coin_grinder(
            convert::amount(target)?,
            convert::amount(change_target)?,
            convert::weight(params.max_selection_weight),
            convert::feerate(params.feerate)?,
            &coins,
        )?;

        let inputs = Coin::resolve(&selected, request.available);

        // Change is what the selection brought in beyond the target, less the change
        // output's own blockspace. The grinder's contract puts this at or above dust.
        let mut surplus = -target.to_signed().map_err(|_| SelectionError::Overflow)?;
        for utxo in &inputs {
            surplus = surplus
                .checked_add(
                    utxo.effective_value(params.feerate)
                        .ok_or(SelectionError::UnknownScriptType)?,
                )
                .ok_or(SelectionError::Overflow)?;
        }

        Ok(Funding {
            change: change_for(&request, surplus)?,
            inputs,
        })
    }
}

/// Whether a request needs more coins at all.
enum Target {
    /// Short by this much effective value.
    Short(Amount),

    /// The required coins already cover it, with this much to spare.
    Covered(SignedAmount),
}

fn target_of(request: &FundingRequest<'_>) -> Result<Target, SelectionError> {
    let deficit = request.deficit().ok_or(SelectionError::Overflow)?;

    Ok(match deficit.to_unsigned() {
        Ok(short) if short > Amount::ZERO => Target::Short(short),
        _ => Target::Covered(-deficit),
    })
}

/// Already funded: no coin needs choosing, only whether the leftover is worth an output.
fn surplus_to_change(
    request: &FundingRequest<'_>,
    target: Target,
) -> Result<Funding, SelectionError> {
    let Target::Covered(surplus) = target else {
        return Err(SelectionError::NoSolution);
    };

    Ok(Funding {
        inputs: Vec::new(),
        change: change_for(request, surplus)?,
    })
}

/// A change output for `surplus`, or `None` when it is worth less than the blockspace it
/// would take to keep it.
fn change_for(
    request: &FundingRequest<'_>,
    surplus: SignedAmount,
) -> Result<Option<TxOut>, SelectionError> {
    let params = request.params;

    let cost = fee(params.feerate, change_output_weight(params))?
        .to_signed()
        .map_err(|_| SelectionError::Overflow)?;

    let Ok(value) = (surplus - cost).to_unsigned() else {
        return Ok(None);
    };

    Ok((value >= params.dust_limit).then(|| TxOut {
        value,
        script_pubkey: params.change_script.clone(),
    }))
}

/// A candidate coin, reduced to what a solver looks at. Carries its index because the
/// solvers hand back references into the slice they were given, not our coins.
struct Coin {
    value: bitcoin_units::Amount,
    weight: bitcoin_units::Weight,
    index: usize,
}

impl Coin {
    fn from_utxos(utxos: &[Utxo]) -> Result<Vec<Coin>, SelectionError> {
        utxos
            .iter()
            .enumerate()
            .map(|(index, utxo)| {
                Ok(Coin {
                    value: convert::amount(utxo.txout.value)?,
                    weight: convert::weight(
                        utxo.input_weight()
                            .ok_or(SelectionError::UnknownScriptType)?,
                    ),
                    index,
                })
            })
            .collect()
    }

    fn resolve(selected: &[&Coin], utxos: &[Utxo]) -> Vec<Utxo> {
        selected
            .iter()
            .map(|coin| utxos[coin.index].clone())
            .collect()
    }
}

impl Spendable for Coin {
    fn total_weight(&self) -> bitcoin_units::Weight {
        self.weight
    }

    fn value(&self) -> bitcoin_units::Amount {
        self.value
    }
}

/// Weight of the input that will one day spend our change, assuming the same script type.
const SPEND_CHANGE_WEIGHT: Weight = Weight::from_wu(230); // P2TR: 41 vB base + 66 WU witness.

fn change_output_weight(params: &SelectionParams) -> Weight {
    TxOut {
        value: Amount::ZERO,
        script_pubkey: params.change_script.clone(),
    }
    .weight()
}

fn fee(feerate: FeeRate, weight: Weight) -> Result<Amount, SelectionError> {
    feerate.fee_wu(weight).ok_or(SelectionError::Overflow)
}

/// `bitcoin-coin-selection` is built on the 1.0 line of `bitcoin-units`, whose types are
/// distinct from our pinned `bitcoin`'s. Same satoshis and weight units, so the crossing
/// is mechanical.
mod convert {
    use super::SelectionError;

    pub fn amount(amount: bitcoin::Amount) -> Result<bitcoin_units::Amount, SelectionError> {
        bitcoin_units::Amount::from_sat(amount.to_sat()).map_err(|_| SelectionError::Overflow)
    }

    pub fn weight(weight: bitcoin::Weight) -> bitcoin_units::Weight {
        bitcoin_units::Weight::from_wu(weight.to_wu())
    }

    pub fn feerate(feerate: bitcoin::FeeRate) -> Result<bitcoin_units::FeeRate, SelectionError> {
        let per_kwu =
            u32::try_from(feerate.to_sat_per_kwu()).map_err(|_| SelectionError::Overflow)?;

        Ok(bitcoin_units::FeeRate::from_sat_per_kwu(per_kwu))
    }
}
