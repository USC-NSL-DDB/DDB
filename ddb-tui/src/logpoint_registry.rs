//! The worker registers creation receipts before classifying queued stops.
//! Retired metadata remains available for temporary/deleted breakpoint events.

use super::{Stop, Template};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_LOGPOINTS: usize = 1024;
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
const MAX_STOP_IDENTITIES: usize = 4096;

#[derive(Default)]
pub(super) struct Registry {
    instance: String,
    templates: BTreeMap<String, Template>,
    retired: VecDeque<String>,
    revisions: BTreeMap<String, u64>,
    bytes: usize,
}

impl Registry {
    /// Only an authoritative connection/hydration update may change instances.
    /// A delayed stop or creation receipt cannot switch the registry backwards.
    pub fn reset(&mut self, instance: &str) {
        if self.instance != instance {
            *self = Self {
                instance: instance.into(),
                ..Default::default()
            };
        }
    }

    /// Call before issuing creation, on the same serialized worker that registers
    /// its receipt. Pending creation therefore cannot race another registration.
    pub fn reserve(&mut self, template: &Template) -> Result<(), String> {
        let size = template_bytes(template);
        while self.templates.len() >= MAX_LOGPOINTS
            || self.bytes.saturating_add(size) > MAX_METADATA_BYTES
        {
            let Some(id) = self.retired.pop_front() else {
                return Err(
                    "Logpoint metadata limit reached; remove a logpoint before creating another"
                        .into(),
                );
            };
            if let Some(old) = self.templates.remove(&id) {
                self.bytes -= template_bytes(&old);
            }
        }
        Ok(())
    }

    pub fn register(
        &mut self,
        instance: &str,
        id: String,
        template: Template,
    ) -> Result<(), String> {
        if self.instance.is_empty() || self.instance != instance || id.is_empty() {
            return Err("Logpoint creation belongs to an unavailable DDB instance".into());
        }
        if self.templates.contains_key(&id) {
            return Err("Logpoint identity is already registered".into());
        }
        self.reserve(&template)?;
        self.bytes += template_bytes(&template);
        self.templates.insert(id, template);
        Ok(())
    }

    pub fn retain_active(&mut self, active: &BTreeSet<String>) {
        self.retired.retain(|id| !active.contains(id));
        for id in self.templates.keys() {
            if !active.contains(id) && !self.retired.contains(id) {
                self.retired.push_back(id.clone());
            }
        }
    }

    /// None suppresses stale, duplicate, running, or process-wide peer events.
    /// Some with no template is a normal stop; callers must not auto-continue it.
    pub fn claim(&mut self, stop: Stop) -> Option<(Stop, Option<Template>)> {
        if self.instance.is_empty()
            || stop.server_instance != self.instance
            || !stop.current()
            || stop.state.running
        {
            return None;
        }
        let Some(ddb_api_client::v2::target::Selector::Thread(thread)) = stop
            .state
            .target
            .as_ref()
            .and_then(|target| target.selector.as_ref())
        else {
            return None;
        };
        if stop.state.execution_state_id.is_empty()
            || stop
                .state
                .stop_reason
                .as_ref()
                .and_then(|reason| reason.thread_id.as_ref())
                .is_some_and(|owner| *owner != thread.thread_id)
        {
            return None;
        }
        let key = &stop.state.execution_state_id;
        if self
            .revisions
            .get(key)
            .is_some_and(|previous| *previous >= stop.state.revision)
        {
            return None;
        }
        // Do not evict a deduplication watermark and accidentally execute an old
        // logpoint again. At capacity, leave unfamiliar stops paused instead.
        if self.revisions.len() >= MAX_STOP_IDENTITIES && !self.revisions.contains_key(key) {
            return Some((stop, None));
        }
        self.revisions.insert(key.clone(), stop.state.revision);
        let template = stop
            .state
            .stop_reason
            .as_ref()
            .filter(|reason| reason.kind == ddb_api_client::v2::StopReasonKind::Breakpoint as i32)
            .and_then(|reason| reason.breakpoint_id.as_ref())
            .and_then(|id| self.templates.get(id))
            .cloned();
        Some((stop, template))
    }
}

fn template_bytes(template: &Template) -> usize {
    template
        .parts
        .iter()
        .map(|part| match part {
            super::Part::Text(value) | super::Part::Expression(value) => value.len(),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddb_api_client::v2;
    use std::sync::{atomic::AtomicU64, Arc};

    fn stop(revision: u64) -> Stop {
        Stop {
            state: v2::ExecutionState {
                execution_state_id: "execution".into(),
                revision,
                target: Some(crate::api::thread_target("thread")),
                stop_reason: Some(v2::StopReason {
                    kind: v2::StopReasonKind::Breakpoint as i32,
                    thread_id: Some("thread".into()),
                    breakpoint_id: Some("log".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_instance: "server".into(),
            control_epoch: 1,
            current_epoch: Arc::new(AtomicU64::new(1)),
        }
    }

    fn registry() -> Registry {
        let mut registry = Registry::default();
        registry.reset("server");
        registry
            .register(
                "server",
                "log".into(),
                Template::parse("value {x}").unwrap(),
            )
            .unwrap();
        registry
    }

    #[test]
    fn each_canonical_stop_is_classified_once_and_retired_metadata_survives() {
        let mut registry = registry();
        registry.retain_active(&BTreeSet::new());
        assert!(registry.claim(stop(9)).unwrap().1.is_some());
        assert!(registry.claim(stop(9)).is_none());
        assert!(registry.claim(stop(8)).is_none());
        assert!(registry.claim(stop(10)).unwrap().1.is_some());
        let mut unknown = stop(11);
        unknown.state.stop_reason.as_mut().unwrap().breakpoint_id = Some("ordinary".into());
        assert!(registry.claim(unknown).unwrap().1.is_none());
    }

    #[test]
    fn stale_instances_receipts_and_peer_projections_cannot_claim_a_stop() {
        let mut registry = registry();
        let mut peer = stop(9);
        peer.state.stop_reason.as_mut().unwrap().thread_id = Some("peer".into());
        assert!(registry.claim(peer).is_none());
        assert!(registry.claim(stop(9)).unwrap().1.is_some());
        registry.reset("replacement");
        assert!(registry.claim(stop(10)).is_none());
        assert!(registry
            .register("server", "late".into(), Template::parse("late").unwrap())
            .is_err());
        assert!(registry.templates.is_empty());
    }

    #[test]
    fn active_metadata_is_not_evicted_to_make_room() {
        let mut registry = Registry::default();
        registry.reset("server");
        let template = Template::parse(&"x".repeat(64 * 1024)).unwrap();
        for index in 0..64 {
            registry
                .register("server", format!("log/{index}"), template.clone())
                .unwrap();
        }
        assert!(registry.reserve(&template).is_err());
        registry.retain_active(&BTreeSet::from(["log/0".into()]));
        registry.register("server", "new".into(), template).unwrap();
        assert!(registry.templates.contains_key("log/0"));
        assert!(registry.bytes <= MAX_METADATA_BYTES);
    }

    #[test]
    fn deduplication_capacity_fails_closed_without_forgetting_old_stops() {
        let mut registry = registry();
        assert!(registry.claim(stop(1)).unwrap().1.is_some());
        for index in 1..MAX_STOP_IDENTITIES {
            let mut next = stop(1);
            next.state.execution_state_id = format!("execution/{index}");
            registry.claim(next).unwrap();
        }
        let mut overflow = stop(1);
        overflow.state.execution_state_id = "overflow".into();
        assert!(registry.claim(overflow).unwrap().1.is_none());
        assert!(registry.claim(stop(1)).is_none());
    }
}
