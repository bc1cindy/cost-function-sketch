//! Splitting the queue before generating the tree.
//!
//! Some intents cannot wait for peers. Pulling them out first keeps them out of the
//! combinatorics entirely.

use std::time::{Duration, Instant};

use crate::intent::IntentId;
use crate::queue::Queue;

/// The queue split by whether an intent can still wait to be batched.
pub struct Refined {
    /// Expiring within the horizon and not privacy-sensitive. Each becomes its own
    /// transaction, outside the tree.
    pub unilateral: Vec<IntentId>,

    /// Everything else, still eligible to batch.
    pub interactive: Vec<IntentId>,
}

impl Queue {
    /// Split the queue at `now`, treating anything due within `horizon` as urgent.
    ///
    /// Urgent and not privacy-sensitive goes unilateral: no time to wait for a session,
    /// nothing lost by acting alone.
    ///
    /// Urgent *and* privacy-sensitive stays interactive. Such an intent should be foregone
    /// rather than made unprivately
    pub fn refine(&self, now: Instant, horizon: Duration) -> Refined {
        let mut unilateral = Vec::new();
        let mut interactive = Vec::new();

        for id in self.ids() {
            let intent = self.get(id).expect("id came from this queue");

            let urgent = intent
                .deadline()
                .is_some_and(|deadline| deadline <= now + horizon);

            if urgent && !intent.always_make_private {
                unilateral.push(id);
            } else {
                interactive.push(id);
            }
        }

        Refined {
            unilateral,
            interactive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{Amount, ScriptBuf};

    use crate::intent::{FixedPaymentInstructions, Intent, IntentWithPolicy};

    /// Refinement reads the policy, never the payment, so any well formed one will do.
    fn request() -> FixedPaymentInstructions {
        FixedPaymentInstructions {
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            amount: Amount::from_sat(50_000),
        }
    }

    fn intent(deadline: Option<Instant>, always_make_private: bool) -> IntentWithPolicy {
        IntentWithPolicy {
            inner: Intent::PaymentRequest(request()),
            payoff_curve: deadline
                .map(|at| vec![(at, Amount::from_sat(1_000))])
                .unwrap_or_default(),
            always_make_private,
        }
    }

    #[test]
    fn urgent_and_not_private_goes_unilateral() {
        let now = Instant::now();
        let queue = Queue::new([intent(Some(now + Duration::from_secs(60)), false)]);

        let refined = queue.refine(now, Duration::from_secs(600));

        assert_eq!(refined.unilateral, vec![IntentId(0)]);
        assert!(refined.interactive.is_empty());
    }

    /// Better to miss the deadline than to leak, so it stays batchable.
    #[test]
    fn urgent_but_private_stays_interactive() {
        let now = Instant::now();
        let queue = Queue::new([intent(Some(now + Duration::from_secs(60)), true)]);

        let refined = queue.refine(now, Duration::from_secs(600));

        assert!(refined.unilateral.is_empty());
        assert_eq!(refined.interactive, vec![IntentId(0)]);
    }

    #[test]
    fn distant_deadline_stays_interactive() {
        let now = Instant::now();
        let queue = Queue::new([intent(Some(now + Duration::from_secs(86_400)), false)]);

        let refined = queue.refine(now, Duration::from_secs(600));

        assert!(refined.unilateral.is_empty());
        assert_eq!(refined.interactive, vec![IntentId(0)]);
    }

    /// No curve means no deadline, so nothing is forcing our hand.
    #[test]
    fn no_deadline_stays_interactive() {
        let now = Instant::now();
        let queue = Queue::new([intent(None, false)]);

        let refined = queue.refine(now, Duration::from_secs(600));

        assert!(refined.unilateral.is_empty());
        assert_eq!(refined.interactive, vec![IntentId(0)]);
    }

    #[test]
    fn ids_keep_their_queue_positions() {
        let now = Instant::now();
        let soon = now + Duration::from_secs(60);
        let queue = Queue::new([
            intent(None, false),
            intent(Some(soon), false),
            intent(None, false),
            intent(Some(soon), false),
        ]);

        let refined = queue.refine(now, Duration::from_secs(600));

        assert_eq!(refined.unilateral, vec![IntentId(1), IntentId(3)]);
        assert_eq!(refined.interactive, vec![IntentId(0), IntentId(2)]);
    }
}
