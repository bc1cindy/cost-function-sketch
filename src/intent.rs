//! What the user wants, and how much they care.
//!
//! An [`Intent`] says what outcome the user wants, not which transaction produces it.
//! That looseness is what gives the planner anything to decide.

use std::time::Instant;

use bitcoin::{Amount, ScriptBuf, SignedAmount};
use concurrent_psbt::tx::UnorderedPsbt;

/// Where a payment request wants to be paid, resolved to a single on-chain output.
///
/// TODO: pull in from the `bitcoin-payment-instructions` crate. A real request may offer
/// several rails (BOLT 11, BOLT 12, on-chain, silent payments), name no amount, or carry
/// a maximum. Each turns a constant here into a choice, which belongs in the tree rather
/// than in this struct.
#[derive(Clone)]
pub struct FixedPaymentInstructions {
    pub script_pubkey: ScriptBuf,
    pub amount: Amount,
}

// TODO: define once the fungi peer/session layer exists
#[derive(Clone)]
pub struct PeerIdentity;

/**
 * Queue: payment obligations either incoming or outgoing.
 * Deadline (time priority).
 * Amount in sat
 * Address to send to
 *
 * Elements of scheduling: spend this coin, create this coin. TxIn | TxOut.
 *
 * outcomes: one atomic set of elements. i.e these coins should be spent and these should be created
 * bundles of elements are
 *
 * Some outcomes must happen together otherwise must not.
 * The condition is general wether or not they are linkable.
 *
 * Fee rates, peer avaialbility, deadlines looming.
 *
 * queue entry: unordered psbt with deadline + metadata about constraints
 */

/// Position in the [`crate::queue::Queue`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct IntentId(pub usize);

/// An Intent is something that the user wants to happen, which can be realized in potentially more than one way.
#[derive(Clone)]
pub enum Intent {
    // BIP 321 or similar, may or may not be handled in fungi protocol
    PaymentRequest(FixedPaymentInstructions), // bitcoin-payment-instructions crate

    /// Implies net-settlement in a funcgi session
    Interactive(PeerIdentity, Amount), // fungi native interactive payment

    /// consensus level state transition a user wants to see happen.
    /// this unordered PSBT may be imbalanced (e.g. just a TxOut, or a TxIn without any change output)
    OnChain(UnorderedPsbt),
}

#[derive(Clone)]
pub struct IntentWithPolicy {
    pub(crate) inner: Intent,

    /// time elements and payoff associated with acting at those times
    pub(crate) payoff_curve: Vec<(Instant, bitcoin::Amount)>,

    /// If you are coming up to a deadline are you willing to sacrifice privacy to make this payment. This can be interperted in different ways (e.g linking this payment output with others)
    pub(crate) always_make_private: bool,
}

/// Negative infinity past a hard deadline. Bigger than the supply, small enough to sum
/// thousands without overflowing.
pub const INFINITE_COST: SignedAmount = SignedAmount::from_sat(-2_100_000_000_000_000);

impl IntentWithPolicy {
    /// What realizing this intent is worth if it happens at `at`.
    pub fn payoff(&self, at: Instant) -> SignedAmount {
        let Some(&(first_at, first_payoff)) = self.payoff_curve.first() else {
            return SignedAmount::ZERO;
        };

        if at <= first_at {
            return signed(first_payoff);
        }

        for pair in self.payoff_curve.windows(2) {
            let (start_at, start_payoff) = pair[0];
            let (end_at, end_payoff) = pair[1];

            if at <= end_at {
                return interpolate(
                    start_at,
                    signed(start_payoff),
                    end_at,
                    signed(end_payoff),
                    at,
                );
            }
        }

        let &(_, last_payoff) = self.payoff_curve.last().expect("checked non-empty");
        if self.always_make_private {
            signed(last_payoff)
        } else {
            INFINITE_COST
        }
    }

    /// Last moment the curve declares anything about.
    pub fn deadline(&self) -> Option<Instant> {
        self.payoff_curve.last().map(|&(at, _)| at)
    }
}

fn signed(amount: Amount) -> SignedAmount {
    amount.to_signed().unwrap_or(SignedAmount::MAX)
}

fn interpolate(
    start_at: Instant,
    start: SignedAmount,
    end_at: Instant,
    end: SignedAmount,
    at: Instant,
) -> SignedAmount {
    let span = end_at.saturating_duration_since(start_at).as_nanos();
    if span == 0 {
        return end;
    }

    let elapsed = at.saturating_duration_since(start_at).as_nanos();
    let delta = (end.to_sat() - start.to_sat()) as i128;

    SignedAmount::from_sat(start.to_sat() + (delta * elapsed as i128 / span as i128) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Where the payment lands doesn't matter here — only when it happens does.
    fn request() -> FixedPaymentInstructions {
        FixedPaymentInstructions {
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            amount: Amount::from_sat(50_000),
        }
    }

    /// Worth 100k now, decaying to nothing over an hour.
    fn decaying(start: Instant, always_make_private: bool) -> IntentWithPolicy {
        IntentWithPolicy {
            inner: Intent::PaymentRequest(request()),
            payoff_curve: vec![
                (start, Amount::from_sat(100_000)),
                (start + Duration::from_secs(3600), Amount::ZERO),
            ],
            always_make_private,
        }
    }

    #[test]
    fn interpolates_between_breakpoints() {
        let start = Instant::now();
        let intent = decaying(start, false);

        assert_eq!(
            intent.payoff(start + Duration::from_secs(1800)),
            SignedAmount::from_sat(50_000)
        );
        assert_eq!(
            intent.payoff(start + Duration::from_secs(2700)),
            SignedAmount::from_sat(25_000)
        );
    }

    #[test]
    fn flat_before_the_curve_starts() {
        let start = Instant::now();

        assert_eq!(
            decaying(start, false).payoff(start - Duration::from_secs(600)),
            SignedAmount::from_sat(100_000)
        );
    }

    /// Must-pay intents fall off a cliff; privacy-sensitive ones just stop improving.
    #[test]
    fn divergence_past_the_deadline_follows_privacy_sensitivity() {
        let start = Instant::now();
        let late = start + Duration::from_secs(7200);

        assert_eq!(decaying(start, false).payoff(late), INFINITE_COST);
        assert_eq!(decaying(start, true).payoff(late), SignedAmount::ZERO);
    }

    #[test]
    fn no_curve_means_no_preference() {
        let intent = IntentWithPolicy {
            inner: Intent::PaymentRequest(request()),
            payoff_curve: vec![],
            always_make_private: false,
        };

        assert_eq!(intent.payoff(Instant::now()), SignedAmount::ZERO);
        assert_eq!(intent.deadline(), None);
    }
}
