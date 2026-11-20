//! Ownership of leader blocking work. Scope handles are weak lease references;
//! registered jobs retain the lease until their function/captures are destroyed.
//! Only the two audited read-only registry snapshots may use the carve-out.
use crate::election::ElectionLease;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinError, JoinHandle};

#[derive(Debug)]
pub enum BlockingError {
    Closed,
    NoRuntime,
    Join(JoinError),
}
impl std::fmt::Display for BlockingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "context lease is closing; blocking work not started"),
            Self::NoRuntime => write!(f, "blocking work requires a Tokio runtime"),
            Self::Join(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for BlockingError {}

struct LeaseLifetime {
    _lease: Option<ElectionLease>,
}
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Queued,
    Running,
}
struct Entry {
    phase: Phase,
    abort: Option<AbortHandle>,
}
#[derive(Default)]
struct State {
    closing: bool,
    next: u64,
    jobs: HashMap<u64, Entry>,
}
struct Inner {
    state: Mutex<State>,
    changed: Notify,
    lease: Weak<LeaseLifetime>,
}

/// Opaque context work scope. Keeping an idle scope handle cannot keep a
/// retired lease alive or admit new jobs after its owner closes it.
#[derive(Clone)]
pub struct BlockingScope {
    inner: Arc<Inner>,
}

pub(crate) struct WorkOwner {
    scope: BlockingScope,
    // Config/resources are dropped before this field's final lease reference.
    _lifetime: Arc<LeaseLifetime>,
}
impl WorkOwner {
    pub(crate) fn new(lease: Option<ElectionLease>) -> Self {
        let lifetime = Arc::new(LeaseLifetime { _lease: lease });
        let scope = BlockingScope {
            inner: Arc::new(Inner {
                state: Mutex::default(),
                changed: Notify::new(),
                lease: Arc::downgrade(&lifetime),
            }),
        };
        Self {
            scope,
            _lifetime: lifetime,
        }
    }
    pub(crate) fn scope(&self) -> BlockingScope {
        self.scope.clone()
    }
    pub(crate) fn factory<T>(&self, make: impl FnOnce() -> T) -> T {
        CURRENT.sync_scope(Some(self.scope()), make)
    }
}
impl Drop for WorkOwner {
    fn drop(&mut self) {
        self.scope.close();
    }
}

tokio::task_local! { static CURRENT: Option<BlockingScope>; }

/// The current factory/dispatch/watch scope, not a file descriptor.
pub fn current_scope() -> Option<BlockingScope> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}

// Explicit field order: destroy the actual future before its lease reference,
// including cancellation of a watcher callback while its task is unwinding.
struct BoundFuture<F> {
    future: Pin<Box<F>>,
    _lease: Arc<LeaseLifetime>,
}
impl<F: Future> Future for BoundFuture<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(cx)
    }
}

/// Propagate ownership to an asynchronously spawned watcher probe/callback.
/// Pass an async block so no configuration is invoked before scope admission.
///
/// # Errors
/// A closed/retired context cannot begin another scoped operation.
pub async fn in_scope<F: Future>(
    scope: Option<BlockingScope>,
    future: F,
) -> Result<F::Output, BlockingError> {
    match scope {
        None => Ok(CURRENT.scope(None, future).await),
        Some(scope) => {
            let lease = {
                let state = scope
                    .inner
                    .state
                    .lock()
                    .expect("blocking registry poisoned");
                if state.closing {
                    return Err(BlockingError::Closed);
                }
                scope.inner.lease.upgrade().ok_or(BlockingError::Closed)?
            };
            Ok(CURRENT
                .scope(
                    Some(scope),
                    BoundFuture {
                        future: Box::pin(future),
                        _lease: lease,
                    },
                )
                .await)
        }
    }
}

impl BlockingScope {
    pub(crate) fn close(&self) {
        let aborts = {
            let mut state = self.inner.state.lock().expect("blocking registry poisoned");
            state.closing = true;
            state
                .jobs
                .values()
                .filter(|entry| entry.phase == Phase::Queued)
                .filter_map(|entry| entry.abort.clone())
                .collect::<Vec<_>>()
        };
        // Never abort under the registry mutex: cancellation can drop a queued
        // function synchronously, and its registration removes itself here.
        for abort in aborts {
            abort.abort();
        }
    }
    fn closed(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("blocking registry poisoned")
            .closing
    }
    pub(crate) async fn wait(&self) {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .inner
                .state
                .lock()
                .expect("blocking registry poisoned")
                .jobs
                .is_empty()
            {
                return;
            }
            changed.await;
        }
    }
    fn spawn<F, R>(
        &self,
        runtime: &tokio::runtime::Handle,
        function: F,
    ) -> Result<JoinHandle<Result<R, BlockingError>>, BlockingError>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (id, lease) = {
            let mut state = self.inner.state.lock().expect("blocking registry poisoned");
            if state.closing {
                return Err(BlockingError::Closed);
            }
            let lease = self.inner.lease.upgrade().ok_or(BlockingError::Closed)?;
            let id = state.next;
            state.next = state
                .next
                .checked_add(1)
                .expect("blocking job ids exhausted");
            state.jobs.insert(
                id,
                Entry {
                    phase: Phase::Queued,
                    abort: None,
                },
            );
            (id, lease)
        };
        let job = RegisteredFunction {
            function: Some(function),
            registration: Registration {
                scope: self.clone(),
                id,
                _lease: lease,
            },
        };
        // Spawn outside the mutex. A shut-down runtime may immediately drop the
        // closure; registration was already published and can remove itself.
        let handle = runtime.spawn_blocking(move || job.run());
        let abort = handle.abort_handle();
        let cancel = {
            let mut state = self.inner.state.lock().expect("blocking registry poisoned");
            let closing = state.closing;
            if let Some(entry) = state.jobs.get_mut(&id) {
                entry.abort = Some(abort.clone());
                closing && entry.phase == Phase::Queued
            } else {
                false
            }
        };
        if cancel {
            abort.abort();
        }
        Ok(handle)
    }
}

struct Registration {
    scope: BlockingScope,
    id: u64,
    _lease: Arc<LeaseLifetime>,
}
impl Registration {
    fn start(&self) -> bool {
        let mut state = self
            .scope
            .inner
            .state
            .lock()
            .expect("blocking registry poisoned");
        if state.closing {
            return false;
        }
        state
            .jobs
            .get_mut(&self.id)
            .expect("registered blocking job")
            .phase = Phase::Running;
        true
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let removed = self
            .scope
            .inner
            .state
            .lock()
            .expect("blocking registry poisoned")
            .jobs
            .remove(&self.id);
        drop(removed);
        self.scope.inner.changed.notify_waiters();
    }
}
// Function/captures drop BEFORE registration/lease, including queued cancellation.
struct RegisteredFunction<F> {
    function: Option<F>,
    registration: Registration,
}
impl<F> RegisteredFunction<F> {
    fn run<R>(mut self) -> Result<R, BlockingError>
    where
        F: FnOnce() -> R,
    {
        if !self.registration.start() {
            return Err(BlockingError::Closed);
        }
        let scope = self.registration.scope.clone();
        let function = self.function.take().expect("blocking function once");
        Ok(CURRENT.sync_scope(Some(scope), function))
    }
}
struct AbortQueuedOnDrop(AbortHandle);
impl Drop for AbortQueuedOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn await_job<R: Send + 'static>(
    job: Result<JoinHandle<Result<R, BlockingError>>, BlockingError>,
    cancel: bool,
) -> impl Future<Output = Result<R, BlockingError>> + Send {
    // Created synchronously: even a returned future that is never polled aborts
    // its queued job on Drop. Running registered functions retain their lease.
    let abort = if cancel {
        job.as_ref()
            .ok()
            .map(|job| AbortQueuedOnDrop(job.abort_handle()))
    } else {
        None
    };
    async move {
        let _abort = abort;
        job?.await.map_err(BlockingError::Join)?
    }
}

/// Register process-starting/state-writing leader work before it is queued.
/// Already-running work retains the lease until actual completion; queued
/// work is cancelled on teardown or parent cancellation. Outside a context,
/// this preserves ordinary standalone spawn_blocking behavior.
pub fn spawn_blocking<F, R>(function: F) -> impl Future<Output = Result<R, BlockingError>> + Send
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let scope = current_scope();
    let job = match tokio::runtime::Handle::try_current() {
        Err(_) => Err(BlockingError::NoRuntime),
        Ok(runtime) => match &scope {
            Some(scope) => scope.spawn(&runtime, function),
            None => Ok(runtime.spawn_blocking(move || Ok(function()))),
        },
    };
    await_job(job, scope.is_some())
}

/// The precise carve-out for the two audited read-only registry snapshots.
/// No process start, durable mutation or shared publication is permitted in
/// this closure. Running computation does not retain the lease, but a queued
/// snapshot is cancelled before it can start after context teardown.
pub fn read_only_snapshot<F, R>(
    function: F,
) -> impl Future<Output = Result<R, BlockingError>> + Send
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let scope = current_scope();
    let cancel = scope.is_some();
    let job = match tokio::runtime::Handle::try_current() {
        Err(_) => Err(BlockingError::NoRuntime),
        Ok(runtime) => Ok(runtime.spawn_blocking(move || {
            if scope.as_ref().is_some_and(BlockingScope::closed) {
                return Err(BlockingError::Closed);
            }
            Ok(CURRENT.sync_scope(scope, function))
        })),
    };
    await_job(job, cancel)
}
