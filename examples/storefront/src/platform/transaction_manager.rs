use leaf::prelude::*;

/// A `@Component` [`TransactionManager`] wrapping the framework's in-memory manager —
/// what a `#[transactional]` method's auto-installed interceptor demarcates against.
/// Exposes begin/commit/rollback counts (test-only) so the demarcation is assertable.
#[derive(Debug)]
pub struct LocalTransactionManager {
    inner: leaf_tx::InMemoryTransactionManager,
}
register_component!(LocalTransactionManager);

impl LocalTransactionManager {
    fn new() -> Self {
        LocalTransactionManager { inner: leaf_tx::InMemoryTransactionManager::new() }
    }

    #[cfg(test)]
    pub fn begins(&self) -> usize {
        self.inner.begins()
    }
    #[cfg(test)]
    pub fn commits(&self) -> usize {
        self.inner.commits()
    }
    #[cfg(test)]
    pub fn rollbacks(&self) -> usize {
        self.inner.rollbacks()
    }
}

impl TransactionManager for LocalTransactionManager {
    fn begin<'a>(
        &'a self,
        def: &'a leaf::core::TxDefinition,
        cx: &'a leaf::core::ResolveCtx<'a>,
    ) -> leaf::core::BoxFuture<'a, Result<leaf::core::TxState, LeafError>> {
        self.inner.begin(def, cx)
    }

    fn commit(&self, st: leaf::core::TxState) -> leaf::core::BoxFuture<'_, Result<(), LeafError>> {
        self.inner.commit(st)
    }

    fn rollback(&self, st: leaf::core::TxState) -> leaf::core::BoxFuture<'_, Result<(), LeafError>> {
        self.inner.rollback(st)
    }

    fn synchronizations<'a>(
        &'a self,
        st: &'a leaf::core::TxState,
    ) -> &'a leaf::core::TxSyncRegistry {
        self.inner.synchronizations(st)
    }
}
