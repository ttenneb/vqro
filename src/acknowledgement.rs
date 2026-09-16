//! Client-neutral per-subject sequence acknowledgement.
//!
//! Cursors are presentation-local. They are never evidence that shared runtime state changed.

use std::collections::HashMap;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

pub const ACKNOWLEDGEMENT_CONTRACT_V1: &str = "host.acknowledgement.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SequenceObservation<K> {
    pub subject: K,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PresentationEvidence<K> {
    pub contract: String,
    pub epoch: String,
    pub projection_revision: u64,
    pub focused: bool,
    pub observations: Vec<SequenceObservation<K>>,
}

#[derive(Clone, Debug)]
pub struct AcknowledgementCursor<K> {
    epoch: Option<String>,
    acknowledged: HashMap<K, u64>,
}

impl<K> Default for AcknowledgementCursor<K> {
    fn default() -> Self {
        Self {
            epoch: None,
            acknowledged: HashMap::new(),
        }
    }
}

impl<K> AcknowledgementCursor<K>
where
    K: Clone + Eq + Hash,
{
    pub fn establish_baseline(
        &mut self,
        epoch: &str,
        observations: impl IntoIterator<Item = SequenceObservation<K>>,
    ) -> bool {
        if self.epoch.as_deref() == Some(epoch) {
            return false;
        }
        self.epoch = Some(epoch.to_owned());
        self.acknowledged.clear();
        self.acknowledged.extend(
            observations
                .into_iter()
                .map(|observation| (observation.subject, observation.sequence)),
        );
        true
    }

    pub fn acknowledge(
        &mut self,
        evidence: &PresentationEvidence<K>,
        expected_projection_revision: u64,
    ) -> bool {
        if evidence.contract != ACKNOWLEDGEMENT_CONTRACT_V1 {
            return false;
        }
        self.acknowledge_observations(
            &evidence.epoch,
            evidence.projection_revision,
            expected_projection_revision,
            evidence.focused,
            evidence
                .observations
                .iter()
                .map(|observation| (&observation.subject, observation.sequence)),
        )
    }

    pub fn acknowledge_observations<'a>(
        &mut self,
        epoch: &str,
        projection_revision: u64,
        expected_projection_revision: u64,
        focused: bool,
        observations: impl IntoIterator<Item = (&'a K, u64)>,
    ) -> bool
    where
        K: 'a,
    {
        if !focused
            || self.epoch.as_deref() != Some(epoch)
            || projection_revision != expected_projection_revision
        {
            return false;
        }
        let mut changed = false;
        for (subject, sequence) in observations {
            if self
                .acknowledged
                .get(subject)
                .is_some_and(|acknowledged| *acknowledged >= sequence)
            {
                continue;
            }
            self.acknowledged.insert(subject.clone(), sequence);
            changed = true;
        }
        changed
    }

    pub fn is_acknowledged(&self, subject: &K, sequence: u64) -> bool {
        self.acknowledged
            .get(subject)
            .is_some_and(|acknowledged| *acknowledged >= sequence)
    }

    pub fn retain_subjects(&mut self, mut retain: impl FnMut(&K) -> bool) {
        self.acknowledged.retain(|subject, _| retain(subject));
    }

    pub fn epoch(&self) -> Option<&str> {
        self.epoch.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(subject: &str, sequence: u64) -> SequenceObservation<String> {
        SequenceObservation {
            subject: subject.into(),
            sequence,
        }
    }

    #[test]
    fn acknowledgement_is_epoch_revision_and_focus_fenced() {
        let mut cursor = AcknowledgementCursor::default();
        cursor.establish_baseline("boot-1", [observation("terminal-1", 4)]);
        let mut evidence = PresentationEvidence {
            contract: ACKNOWLEDGEMENT_CONTRACT_V1.into(),
            epoch: "boot-1".into(),
            projection_revision: 2,
            focused: true,
            observations: vec![observation("terminal-1", 5)],
        };
        assert!(!cursor.acknowledge(&evidence, 1));
        evidence.focused = false;
        assert!(!cursor.acknowledge(&evidence, 2));
        evidence.focused = true;
        assert!(cursor.acknowledge(&evidence, 2));
        assert!(cursor.is_acknowledged(&"terminal-1".into(), 5));
    }

    #[test]
    fn a_new_epoch_resets_the_cursor_and_subject_retention_is_explicit() {
        let mut cursor = AcknowledgementCursor::default();
        cursor.establish_baseline(
            "boot-1",
            [observation("terminal-1", 7), observation("terminal-2", 3)],
        );
        assert!(cursor.establish_baseline("boot-2", [observation("terminal-1", 1)]));
        assert!(!cursor.is_acknowledged(&"terminal-2".into(), 3));
        cursor.retain_subjects(|subject| subject == "terminal-2");
        assert!(!cursor.is_acknowledged(&"terminal-1".into(), 1));
    }
}
