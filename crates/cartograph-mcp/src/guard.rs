//! Every query runs through [`Guard::run`]: on a blocking thread (a panic there becomes a
//! `JoinError`, so `catch_unwind` comes for free and the server survives it), under a
//! timeout, and behind a semaphore that bounds how many queries run (and how much memory
//! they hold) at once. A query that times out keeps its thread until it ends, but it keeps
//! its slot too, so runaway queries cannot pile up beyond `max_concurrent`.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, PartialEq)]
pub enum GuardError {
    /// The query panicked; the message is the panic payload.
    Panicked(String),
    /// It did not finish (or get a slot) in time.
    TimedOut(Duration),
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardError::Panicked(m) => write!(f, "internal error (panic): {m}"),
            GuardError::TimedOut(d) => write!(f, "timed out after {} s", d.as_secs_f32()),
        }
    }
}

#[derive(Clone)]
pub struct Guard {
    slots: Arc<Semaphore>,
    timeout: Duration,
}

impl Guard {
    pub fn new(max_concurrent: usize, timeout: Duration) -> Guard {
        Guard {
            slots: Arc::new(Semaphore::new(max_concurrent.max(1))),
            timeout,
        }
    }

    /// Run `f` on the blocking pool with a slot, within the timeout.
    pub async fn run<T, F>(&self, f: F) -> Result<T, GuardError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let slots = self.slots.clone();
        let work = async move {
            let permit = slots
                .acquire_owned()
                .await
                .map_err(|_| GuardError::Panicked("semaphore closed".into()))?;
            let handle = tokio::task::spawn_blocking(move || {
                // The slot is released when the query really ends, even after a timeout.
                let _permit = permit;
                f()
            });
            match handle.await {
                Ok(v) => Ok(v),
                Err(e) if e.is_panic() => Err(GuardError::Panicked(
                    cartograph_server::panic_message(e.into_panic().as_ref()),
                )),
                Err(e) => Err(GuardError::Panicked(e.to_string())),
            }
        };
        match tokio::time::timeout(self.timeout, work).await {
            Ok(r) => r,
            Err(_) => Err(GuardError::TimedOut(self.timeout)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Timeouts of a second or more: sub-second ones failed on a loaded machine.
    #[tokio::test(flavor = "multi_thread")]
    async fn ok_panic_and_timeout() {
        let g = Guard::new(2, Duration::from_secs(1));
        assert_eq!(g.run(|| 41 + 1).await, Ok(42));
        let r: Result<(), _> = g.run(|| panic!("boom in a query")).await;
        assert_eq!(r, Err(GuardError::Panicked("boom in a query".into())));
        let r = g.run(|| std::thread::sleep(Duration::from_secs(3))).await;
        assert!(matches!(r, Err(GuardError::TimedOut(_))));
        // Still serving after all of that.
        assert_eq!(g.run(|| "alive").await, Ok("alive"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slots_bound_concurrency() {
        let g = Guard::new(1, Duration::from_secs(1));
        let g2 = g.clone();
        let slow =
            tokio::spawn(
                async move { g2.run(|| std::thread::sleep(Duration::from_secs(3))).await },
            );
        tokio::time::sleep(Duration::from_millis(200)).await;
        // The only slot is busy past this request's timeout.
        let r = g.run(|| 1).await;
        assert!(matches!(r, Err(GuardError::TimedOut(_))));
        assert!(matches!(slow.await.unwrap(), Err(GuardError::TimedOut(_))));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(g.run(|| 2).await, Ok(2));
    }
}
