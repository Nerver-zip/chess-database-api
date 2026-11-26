//! Admission and cancellation for search workloads.

use crate::error::ApiError;
use chess_database_core::Collection;
use chess_database_index::search::control::SearchControl;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const ADMISSION_WAIT: Duration = Duration::from_millis(100);
const QUEUED_SEARCHES: usize = 4;
const QUEUED_AGGREGATES: usize = 2;

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub masters_discovery: bool,
    pub elite_discovery: bool,
    pub masters_game_metadata: bool,
    pub elite_game_metadata: bool,
    pub max_page_size: usize,
    pub concurrency: usize,
    pub deadline: Duration,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            masters_discovery: false,
            elite_discovery: true,
            masters_game_metadata: false,
            elite_game_metadata: true,
            max_page_size: 100,
            concurrency: 2,
            deadline: Duration::from_secs(2),
        }
    }
}

impl SearchOptions {
    pub fn discovery(&self, collection: &Collection) -> bool {
        match collection {
            Collection::MastersOtb => self.masters_discovery,
            Collection::EliteOnline => self.elite_discovery,
        }
    }

    pub fn game_metadata(&self, collection: &Collection) -> bool {
        match collection {
            Collection::MastersOtb => self.masters_game_metadata,
            Collection::EliteOnline => self.elite_game_metadata,
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=100).contains(&self.max_page_size),
            "Search page limit must be between 1 and 100"
        );
        anyhow::ensure!(
            (1..=8).contains(&self.concurrency),
            "Search worker count must be between 1 and 8"
        );
        anyhow::ensure!(
            !self.deadline.is_zero() && self.deadline <= Duration::from_secs(15),
            "Search deadline must be above zero and at most 15 seconds"
        );
        Ok(())
    }
}

pub(crate) struct SearchRuntime {
    pub(crate) options: SearchOptions,
    permits: Arc<Semaphore>,
    search_admission: Arc<Semaphore>,
    aggregate_admission: Arc<Semaphore>,
    aggregate_worker: Arc<Semaphore>,
}

struct CancelOnDrop(SearchControl);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl SearchRuntime {
    pub(crate) fn new(options: SearchOptions) -> anyhow::Result<Self> {
        options.validate()?;
        Ok(Self {
            permits: Arc::new(Semaphore::new(options.concurrency)),
            search_admission: Arc::new(Semaphore::new(options.concurrency + QUEUED_SEARCHES)),
            aggregate_admission: Arc::new(Semaphore::new(1 + QUEUED_AGGREGATES)),
            aggregate_worker: Arc::new(Semaphore::new(1)),
            options,
        })
    }

    pub(crate) async fn run<T, Work>(&self, work: Work) -> Result<T, ApiError>
    where
        T: Send + 'static,
        Work: FnOnce(&SearchControl) -> Result<T, ApiError> + Send + 'static,
    {
        self.run_in_lane(false, work).await
    }

    pub(crate) async fn run_in_lane<T, Work>(
        &self,
        aggregate: bool,
        work: Work,
    ) -> Result<T, ApiError>
    where
        T: Send + 'static,
        Work: FnOnce(&SearchControl) -> Result<T, ApiError> + Send + 'static,
    {
        let admission = if aggregate {
            &self.aggregate_admission
        } else {
            &self.search_admission
        }
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::SearchBusy)?;
        let (aggregate_worker, permit) = tokio::time::timeout(ADMISSION_WAIT, async {
            let aggregate_worker = if aggregate {
                Some(self.aggregate_worker.clone().acquire_owned().await?)
            } else {
                None
            };
            Ok::<_, tokio::sync::AcquireError>((
                aggregate_worker,
                self.permits.clone().acquire_owned().await?,
            ))
        })
        .await
        .map_err(|_| ApiError::SearchBusy)?
        .map_err(|_| ApiError::SearchBusy)?;
        let control = SearchControl::new(self.options.deadline);
        let _cancel = CancelOnDrop(control.clone());
        let worker = tokio::task::spawn_blocking(move || {
            let _permits = (permit, aggregate_worker, admission);
            if control.expired() {
                return Err(ApiError::SearchDeadline);
            }
            let result = work(&control);
            if control.expired() {
                Err(ApiError::SearchDeadline)
            } else {
                result
            }
        });
        tokio::time::timeout(self.options.deadline, worker)
            .await
            .map_err(|_| ApiError::SearchDeadline)?
            .map_err(|error| ApiError::Internal(format!("Search worker failed: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn an_aggregate_leaves_one_worker_for_interactive_search() {
        let runtime = Arc::new(SearchRuntime::new(SearchOptions::default()).unwrap());
        let (started, running) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            async move {
                runtime
                    .run_in_lane(true, move |control| {
                        started.send(()).unwrap();
                        while !control.expired() {
                            std::thread::yield_now();
                        }
                        Ok(())
                    })
                    .await
            }
        });
        running.await.unwrap();
        assert!(runtime.run(|_| Ok(())).await.is_ok());
        assert!(matches!(
            runtime.run_in_lane(true, |_| Ok(())).await,
            Err(ApiError::SearchBusy)
        ));
        task.abort();
    }

    #[tokio::test]
    async fn overload_and_dropped_request_release_the_worker_budget() {
        let runtime = Arc::new(
            SearchRuntime::new(SearchOptions {
                concurrency: 1,
                ..SearchOptions::default()
            })
            .unwrap(),
        );
        let running = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn({
            let runtime = runtime.clone();
            let running = running.clone();
            let observed = observed.clone();
            async move {
                runtime
                    .run(move |control| {
                        running.store(true, Ordering::Release);
                        while !control.expired() {
                            std::thread::yield_now();
                        }
                        observed.store(true, Ordering::Release);
                        Ok(())
                    })
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !running.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            runtime.run(|_| Ok(())).await,
            Err(ApiError::SearchBusy)
        ));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match runtime.run(|_| Ok(())).await {
                    Ok(()) => break,
                    Err(ApiError::SearchBusy) => tokio::task::yield_now().await,
                    result => panic!("unexpected response: {result:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert!(observed.load(Ordering::Acquire));
    }
}
