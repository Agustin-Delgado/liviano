//! A thread of its own for Credential Manager. A read can take a fifth of a
//! second (the first one after a while, or after writes) and every write
//! does, so the UI thread never calls it: it hands a job here and gets the
//! result back as a message. Jobs run one at a time, in the order given.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        mpsc::{self, Sender},
    },
};

type Job<R> = Box<dyn FnOnce() -> R + Send>;

pub struct Worker<R: Send + 'static> {
    jobs: Sender<Job<R>>,
    done: Arc<Mutex<VecDeque<R>>>,
}

impl<R: Send + 'static> Worker<R> {
    /// Starts the thread. `ready` runs on it after each job, once the result
    /// can be taken (the app posts itself a message there).
    pub fn new(name: &str, ready: impl Fn() + Send + 'static) -> Option<Self> {
        let (jobs, inbox) = mpsc::channel::<Job<R>>();
        let done = Arc::new(Mutex::new(VecDeque::new()));
        let results = done.clone();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                // Ends when the worker (the sender) is dropped.
                for job in inbox {
                    let result = job();
                    if let Ok(mut done) = results.lock() {
                        done.push_back(result);
                    }
                    ready();
                }
            })
            .ok()?;
        Some(Self { jobs, done })
    }

    pub fn run(&self, job: impl FnOnce() -> R + Send + 'static) {
        let _ = self.jobs.send(Box::new(job));
    }

    /// The results ready now, oldest first.
    pub fn take(&self) -> Vec<R> {
        self.done.lock().map(|mut d| d.drain(..).collect()).unwrap_or_default()
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/vault_worker.rs"]
#[rustfmt::skip]
mod tests;
