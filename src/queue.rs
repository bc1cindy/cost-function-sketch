//! The wallet's outstanding obligations.

use crate::intent::{IntentId, IntentWithPolicy};

/// All the information the wallet has about what the user wants to do.
#[derive(Clone)]
pub struct Queue(pub(crate) Vec<IntentWithPolicy>);

impl Queue {
    pub fn new(intents: impl IntoIterator<Item = IntentWithPolicy>) -> Self {
        Queue(intents.into_iter().collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn get(&self, id: IntentId) -> Option<&IntentWithPolicy> {
        self.0.get(id.0)
    }

    pub fn iter(&self) -> impl Iterator<Item = &IntentWithPolicy> {
        self.0.iter()
    }

    /// Every position in the queue, in order.
    pub fn ids(&self) -> impl Iterator<Item = IntentId> {
        (0..self.0.len()).map(IntentId)
    }
}
