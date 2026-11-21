//! Job queue for the scan workers: jobs for on-screen shots come first.
// pattern: Functional Core

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;

use photosort_core::model::{ByteRange, FileKind, ShotId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Job {
    Parse {
        shot: ShotId,
        file: usize,
        path: PathBuf,
        kind: FileKind,
    },
    Preview {
        shot: ShotId,
        path: PathBuf,
        range: ByteRange,
        orientation: Option<u16>,
    },
}

impl Job {
    fn shot(&self) -> ShotId {
        match self {
            Job::Parse { shot, .. } | Job::Preview { shot, .. } => *shot,
        }
    }
}

/// One shot's queued jobs. Previews are served before parses.
#[derive(Default)]
struct ShotJobs {
    previews: VecDeque<Job>,
    parses: VecDeque<Job>,
}

/// Pending jobs per shot, plus the on-screen shots in display order.
#[derive(Default)]
pub(crate) struct JobQueue {
    pending: BTreeMap<ShotId, ShotJobs>,
    visible: Vec<ShotId>,
    len: usize,
}

impl ShotJobs {
    fn is_empty(&self) -> bool {
        self.previews.is_empty() && self.parses.is_empty()
    }
}

impl JobQueue {
    pub(crate) fn push(&mut self, job: Job) {
        let shot = job.shot();
        let jobs = self.pending.entry(shot).or_default();
        if matches!(job, Job::Preview { .. }) {
            jobs.previews.push_back(job);
        } else {
            jobs.parses.push_back(job);
        }
        self.len += 1;
    }

    pub(crate) fn set_visible(&mut self, visible: Vec<ShotId>) {
        self.visible = visible;
    }

    /// Jobs of visible shots in visible order, then the lowest shot's jobs.
    pub(crate) fn pop(&mut self) -> Option<Job> {
        // Pending entries are never empty, so a visible shot found here has a job.
        let shot = self
            .visible
            .iter()
            .copied()
            .find(|shot| self.pending.contains_key(shot))
            .or_else(|| self.pending.keys().next().copied())?;
        self.take_from(shot)
    }

    fn take_from(&mut self, shot: ShotId) -> Option<Job> {
        let jobs = self.pending.get_mut(&shot)?;
        let job = jobs
            .previews
            .pop_front()
            .or_else(|| jobs.parses.pop_front())?;
        if jobs.is_empty() {
            self.pending.remove(&shot);
        }
        self.len -= 1;
        Some(job)
    }

    /// Drops every pending job and the visible list.
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.visible.clear();
        self.len = 0;
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// True when a preview job for `shot` is queued.
    pub(crate) fn contains_preview(&self, shot: ShotId) -> bool {
        self.pending
            .get(&shot)
            .is_some_and(|jobs| !jobs.previews.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;

    fn parse(shot: u32, file: usize) -> Job {
        Job::Parse {
            shot: ShotId(shot),
            file,
            path: PathBuf::from(format!("{shot}-{file}.jpg")),
            kind: FileKind("jpeg"),
        }
    }

    fn preview(shot: u32) -> Job {
        Job::Preview {
            shot: ShotId(shot),
            path: PathBuf::from(format!("{shot}.jpg")),
            range: ByteRange { offset: 0, len: 1 },
            orientation: None,
        }
    }

    fn drain(queue: &mut JobQueue) -> Vec<Job> {
        std::iter::from_fn(|| queue.pop()).collect()
    }

    #[test]
    fn without_a_visible_set_jobs_pop_in_shot_id_order() {
        let mut queue = JobQueue::default();
        queue.push(parse(5, 0));
        queue.push(parse(1, 0));
        queue.push(parse(3, 0));
        queue.push(parse(1, 1));
        assert_eq!(
            drain(&mut queue),
            vec![parse(1, 0), parse(1, 1), parse(3, 0), parse(5, 0)]
        );
    }

    #[test]
    fn visible_shots_pop_first_in_visible_order() {
        let mut queue = JobQueue::default();
        queue.push(parse(1, 0));
        queue.push(parse(2, 0));
        queue.push(parse(3, 0));
        queue.push(parse(7, 0));
        queue.push(parse(7, 1));
        queue.set_visible(vec![ShotId(7), ShotId(3)]);
        assert_eq!(
            drain(&mut queue),
            vec![
                parse(7, 0),
                parse(7, 1),
                parse(3, 0),
                parse(1, 0),
                parse(2, 0)
            ]
        );
    }

    #[test]
    fn a_preview_pops_before_the_parse_of_the_same_shot() {
        let mut queue = JobQueue::default();
        queue.push(parse(4, 0));
        queue.push(preview(4));
        queue.set_visible(vec![ShotId(4)]);
        assert_eq!(drain(&mut queue), vec![preview(4), parse(4, 0)]);
    }

    #[test]
    fn a_preview_pops_before_the_parse_without_a_visible_set() {
        let mut queue = JobQueue::default();
        queue.push(parse(4, 0));
        queue.push(preview(4));
        assert_eq!(drain(&mut queue), vec![preview(4), parse(4, 0)]);
    }

    #[test]
    fn clear_empties_the_queue() {
        let mut queue = JobQueue::default();
        queue.push(parse(1, 0));
        queue.push(preview(2));
        queue.set_visible(vec![ShotId(2)]);
        queue.clear();
        assert_eq!(queue.len(), 0);
        assert!(!queue.contains_preview(ShotId(2)));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn pop_on_an_empty_queue_is_none_even_with_a_visible_set() {
        let mut queue = JobQueue::default();
        queue.set_visible(vec![ShotId(9)]);
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn len_counts_the_jobs_still_pending() {
        let mut queue = JobQueue::default();
        queue.push(parse(1, 0));
        queue.push(parse(2, 0));
        queue.push(preview(2));
        assert_eq!(queue.len(), 3);
        assert!(queue.pop().is_some());
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn contains_preview_reports_a_queued_preview_for_that_shot_only() {
        let mut queue = JobQueue::default();
        queue.push(preview(2));
        queue.push(parse(3, 0));
        assert!(queue.contains_preview(ShotId(2)));
        assert!(!queue.contains_preview(ShotId(3)));
        assert_eq!(queue.pop(), Some(preview(2)));
        assert!(!queue.contains_preview(ShotId(2)));
    }

    /// Job `i` is unique, so a set comparison also counts each job once.
    fn job(i: usize, shot: u32, is_preview: bool) -> Job {
        if is_preview {
            Job::Preview {
                shot: ShotId(shot),
                path: PathBuf::from(format!("{i}")),
                range: ByteRange {
                    offset: i as u64,
                    len: 1,
                },
                orientation: None,
            }
        } else {
            Job::Parse {
                shot: ShotId(shot),
                file: i,
                path: PathBuf::from(format!("{i}")),
                kind: FileKind("jpeg"),
            }
        }
    }

    proptest! {
        #[test]
        fn every_pushed_job_is_popped_exactly_once(
            specs in prop::collection::vec((0u32..8, any::<bool>()), 0..60),
            visible in prop::collection::vec(0u32..8, 0..8),
            pops_before_visible_change in 0usize..60,
            pops_before_second_half in 0usize..60,
        ) {
            let jobs: Vec<Job> = specs
                .iter()
                .enumerate()
                .map(|(i, &(shot, is_preview))| job(i, shot, is_preview))
                .collect();
            let split = jobs.len() / 2;
            let mut queue = JobQueue::default();
            let mut popped = Vec::new();

            for j in &jobs[..split] {
                queue.push(j.clone());
            }
            for _ in 0..pops_before_visible_change {
                popped.extend(queue.pop());
            }
            queue.set_visible(visible.iter().map(|&s| ShotId(s)).collect());
            for _ in 0..pops_before_second_half {
                popped.extend(queue.pop());
            }
            for j in &jobs[split..] {
                queue.push(j.clone());
            }
            popped.extend(drain(&mut queue));

            prop_assert_eq!(queue.len(), 0);
            prop_assert_eq!(popped.len(), jobs.len());
            let popped_set: HashSet<Job> = popped.into_iter().collect();
            let pushed_set: HashSet<Job> = jobs.into_iter().collect();
            prop_assert_eq!(popped_set, pushed_set);
        }
    }
}
