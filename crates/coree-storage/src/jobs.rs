// SPDX-License-Identifier: MIT OR Apache-2.0

//! Non-blocking background job runner (`JobPool`) backed by `rayon`, `std::sync::mpsc`,
//! cooperative `Arc<AtomicBool>` cancellation, and `Arc<AtomicU8>` progress (0..=100)
//! so the desktop UI stays locked at 60fps during heavy export, trace, or `.cdr` parsing.

use crate::error::{StorageError, StorageResult};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Monotonic identifier assigned to each submitted background job.
pub type JobId = u64;

/// Lifecycle state of a background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobState {
    Queued = 0,
    Running = 1,
    Completed = 2,
    Cancelled = 3,
    Failed = 4,
}

impl JobState {
    fn from_u8(val: u8) -> Self {
        match val {
            0 => Self::Queued,
            1 => Self::Running,
            2 => Self::Completed,
            3 => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

/// Terminal outcome delivered through a [`JobHandle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome<T> {
    Completed(T),
    Cancelled,
    Failed(String),
}

/// Cooperative cancellation & progress context passed into each background job closure.
#[derive(Debug, Clone)]
pub struct JobContext {
    cancel_flag: Arc<AtomicBool>,
    progress_pct: Arc<AtomicU8>,
}

impl JobContext {
    /// Returns `true` if the UI/caller requested cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }

    /// Updates job completion progress (`0..=100`).
    pub fn set_progress(&self, percent: u8) {
        self.progress_pct.store(percent.min(100), Ordering::Release);
    }

    /// Reads current progress percentage (`0..=100`).
    #[must_use]
    pub fn progress(&self) -> u8 {
        self.progress_pct.load(Ordering::Acquire)
    }
}

/// Handle returned to the UI thread for polling progress, cancelling, and collecting results
/// without blocking the render loop.
pub struct JobHandle<T> {
    id: JobId,
    name: String,
    cancel_flag: Arc<AtomicBool>,
    progress_pct: Arc<AtomicU8>,
    state: Arc<AtomicU8>,
    rx: mpsc::Receiver<JobOutcome<T>>,
}

impl<T> std::fmt::Debug for JobHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state())
            .field("progress", &self.progress())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl<T> JobHandle<T> {
    /// Unique job identifier.
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    /// Human-readable job label (for status bar / progress toast).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Requests cooperative cancellation of this job.
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
    }

    /// Returns `true` if cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }

    /// Current progress percentage (`0..=100`).
    #[must_use]
    pub fn progress(&self) -> u8 {
        self.progress_pct.load(Ordering::Acquire)
    }

    /// Current lifecycle [`JobState`].
    #[must_use]
    pub fn state(&self) -> JobState {
        JobState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Returns `true` if the job has reached a terminal state (`Completed`, `Cancelled`, or `Failed`).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(
            self.state(),
            JobState::Completed | JobState::Cancelled | JobState::Failed
        )
    }

    /// Non-blocking poll for completion (safe to call every UI frame at 60fps).
    pub fn try_recv(&self) -> Option<JobOutcome<T>> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                if self.is_cancelled() {
                    Some(JobOutcome::Cancelled)
                } else {
                    Some(JobOutcome::Failed("worker channel disconnected".into()))
                }
            }
        }
    }

    /// Blocks until the background job finishes and returns its [`JobOutcome`].
    pub fn wait(self) -> JobOutcome<T> {
        match self.rx.recv() {
            Ok(outcome) => outcome,
            Err(_) => {
                if self.is_cancelled() {
                    JobOutcome::Cancelled
                } else {
                    JobOutcome::Failed("worker channel disconnected".into())
                }
            }
        }
    }

    /// Blocks up to `timeout` waiting for completion; returns `Err(self)` if still running.
    pub fn wait_timeout(self, timeout: Duration) -> Result<JobOutcome<T>, Self> {
        match self.rx.recv_timeout(timeout) {
            Ok(outcome) => Ok(outcome),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(self),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(if self.is_cancelled() {
                JobOutcome::Cancelled
            } else {
                JobOutcome::Failed("worker channel disconnected".into())
            }),
        }
    }
}

/// Rayon-backed background worker pool for CPU-intensive design & export tasks.
pub struct JobPool {
    pool: rayon::ThreadPool,
    next_id: AtomicU64,
}

impl JobPool {
    /// Creates a background worker pool with an explicit thread count (minimum 1).
    pub fn new(num_threads: usize) -> StorageResult<Self> {
        let threads = num_threads.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|idx| format!("coree-job-{idx}"))
            .build()
            .map_err(|e| StorageError::ThreadPool(e.to_string()))?;
        Ok(Self {
            pool,
            next_id: AtomicU64::new(1),
        })
    }

    /// Creates a pool sized for responsive desktop usage (`available_parallelism - 1`, min 2).
    pub fn default_pool() -> StorageResult<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let worker_threads = cpus.saturating_sub(1).max(2);
        Self::new(worker_threads)
    }

    /// Returns the number of worker threads owned by this pool.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.pool.current_num_threads()
    }

    /// Submits a background job closure to the rayon pool and immediately returns a non-blocking [`JobHandle`].
    pub fn submit<T, F>(&self, name: impl Into<String>, task: F) -> JobHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&JobContext) -> Result<T, String> + Send + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let progress_pct = Arc::new(AtomicU8::new(0));
        let state = Arc::new(AtomicU8::new(JobState::Queued as u8));
        let (tx, rx) = mpsc::channel();

        let ctx = JobContext {
            cancel_flag: Arc::clone(&cancel_flag),
            progress_pct: Arc::clone(&progress_pct),
        };
        let state_worker = Arc::clone(&state);

        self.pool.spawn(move || {
            if ctx.is_cancelled() {
                state_worker.store(JobState::Cancelled as u8, Ordering::Release);
                let _ = tx.send(JobOutcome::Cancelled);
                return;
            }
            state_worker.store(JobState::Running as u8, Ordering::Release);

            let outcome = match catch_unwind(AssertUnwindSafe(|| task(&ctx))) {
                Ok(Ok(value)) => {
                    if ctx.is_cancelled() {
                        state_worker.store(JobState::Cancelled as u8, Ordering::Release);
                        JobOutcome::Cancelled
                    } else {
                        ctx.set_progress(100);
                        state_worker.store(JobState::Completed as u8, Ordering::Release);
                        JobOutcome::Completed(value)
                    }
                }
                Ok(Err(msg)) => {
                    if ctx.is_cancelled() {
                        state_worker.store(JobState::Cancelled as u8, Ordering::Release);
                        JobOutcome::Cancelled
                    } else {
                        state_worker.store(JobState::Failed as u8, Ordering::Release);
                        JobOutcome::Failed(msg)
                    }
                }
                Err(panic_payload) => {
                    state_worker.store(JobState::Failed as u8, Ordering::Release);
                    let reason = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        format!("worker panicked: {s}")
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        format!("worker panicked: {s}")
                    } else {
                        "worker panicked".to_owned()
                    };
                    JobOutcome::Failed(reason)
                }
            };

            let _ = tx.send(outcome);
        });

        JobHandle {
            id,
            name: name.into(),
            cancel_flag,
            progress_pct,
            state,
            rx,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn job_pool_executes_progress_and_completes() {
        let pool = JobPool::new(2).unwrap();
        let handle = pool.submit("Export PDF", |ctx| {
            for step in 1..=5 {
                ctx.set_progress(step * 20);
            }
            Ok(42usize)
        });

        assert_eq!(handle.name(), "Export PDF");
        let outcome = handle.wait();
        assert_eq!(outcome, JobOutcome::Completed(42));
    }

    #[test]
    fn job_pool_supports_cooperative_cancellation() {
        let pool = JobPool::new(2).unwrap();
        let started = Arc::new(Barrier::new(2));
        let started_worker = Arc::clone(&started);

        let handle = pool.submit("Vector Trace", move |ctx| {
            started_worker.wait();
            for i in 0..10_000 {
                if ctx.is_cancelled() {
                    return Err("cancelled".into());
                }
                ctx.set_progress(((i % 100) as u8).min(99));
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok("finished")
        });

        started.wait();
        handle.cancel();
        assert!(handle.is_cancelled());
        let outcome = handle.wait();
        assert_eq!(outcome, JobOutcome::Cancelled);
    }

    #[test]
    fn job_pool_isolates_worker_panics_into_failed_outcome() {
        let pool = JobPool::new(1).unwrap();
        let handle = pool.submit::<(), _>("Faulty Task", |_ctx| {
            panic!("simulated codec bug");
        });
        match handle.wait() {
            JobOutcome::Failed(msg) => assert!(msg.contains("simulated codec bug")),
            other => panic!("expected Failed outcome, got {other:?}"),
        }
    }
}
