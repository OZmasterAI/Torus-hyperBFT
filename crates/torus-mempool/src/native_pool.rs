use torus_types::{NativeAction, SignedNativeAction};

/// Simple native action pool with priority ordering.
pub(crate) struct NativePool {
    actions: Vec<SignedNativeAction>,
}

impl NativePool {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
        }
    }

    pub fn size(&self) -> usize {
        self.actions.len()
    }

    pub fn insert(&mut self, action: SignedNativeAction) {
        self.actions.push(action);
    }

    /// Drain up to `limit` actions in priority order.
    /// Cancellations first (highest priority), then everything else.
    pub fn drain(&mut self, limit: usize) -> Vec<SignedNativeAction> {
        self.actions
            .sort_by_key(|a| if is_cancel(&a.action) { 0u8 } else { 1 });
        let count = limit.min(self.actions.len());
        self.actions.drain(..count).collect()
    }

    /// Re-insert a batch of actions (e.g., after block reorg).
    pub fn reinsert(&mut self, actions: Vec<SignedNativeAction>) {
        self.actions.extend(actions);
    }
}

fn is_cancel(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::CancelOrder { .. } | NativeAction::CancelAllOrders { .. }
    )
}
