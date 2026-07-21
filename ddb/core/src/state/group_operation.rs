use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::state::GroupId;

/// Serializes membership transitions with debugger operations for one group.
///
/// A session is visible in GroupMgr before it becomes routable. Holding this
/// gate through activation makes every group operation observe the session
/// either before registration or after its router and breakpoint projections
/// are complete.
#[derive(Default)]
pub(crate) struct GroupOperationCoordinator {
    gates: DashMap<GroupId, Arc<Mutex<()>>>,
}

impl GroupOperationCoordinator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn lock(&self, group_id: GroupId) -> OwnedMutexGuard<()> {
        self.gates
            .entry(group_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
            .lock_owned()
            .await
    }

    pub(crate) async fn lock_many(
        &self,
        group_ids: impl IntoIterator<Item = GroupId>,
    ) -> Vec<OwnedMutexGuard<()>> {
        let mut group_ids = group_ids.into_iter().collect::<Vec<_>>();
        group_ids.sort_unstable();
        group_ids.dedup();

        let mut guards = Vec::with_capacity(group_ids.len());
        for group_id in group_ids {
            guards.push(self.lock(group_id).await);
        }
        guards
    }

    pub(crate) fn remove_group(&self, group_id: GroupId) {
        self.gates.remove(&group_id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{sync::mpsc, time::timeout};

    use super::*;

    #[tokio::test]
    async fn same_group_operations_are_serialized() {
        let coordinator = Arc::new(GroupOperationCoordinator::new());
        let first = coordinator.lock(GroupId::new(7)).await;
        let (events, mut observed) = mpsc::unbounded_channel();

        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move {
                events.send("waiting").unwrap();
                let _guard = coordinator.lock(GroupId::new(7)).await;
                events.send("acquired").unwrap();
            }
        });

        assert_eq!(observed.recv().await, Some("waiting"));
        assert!(timeout(Duration::from_millis(20), observed.recv())
            .await
            .is_err());

        drop(first);
        assert_eq!(
            timeout(Duration::from_millis(100), observed.recv())
                .await
                .unwrap(),
            Some("acquired")
        );
        waiter.await.unwrap();
    }

    #[tokio::test]
    async fn different_groups_do_not_block_each_other() {
        let coordinator = Arc::new(GroupOperationCoordinator::new());
        let _first = coordinator.lock(GroupId::new(7)).await;

        timeout(
            Duration::from_millis(100),
            coordinator.lock(GroupId::new(8)),
        )
        .await
        .expect("an unrelated group should not share the gate");
    }
}
