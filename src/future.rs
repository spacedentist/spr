/*
 * Copyright (c) Radical HQ, Ltd.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::executor::spawn;
use std::collections::HashMap;
use std::future::Future as StdFuture;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

pub struct Future<T: 'static> {
    inner: FutureInner<T>,
}

pub struct SharedFuture<T: Clone + 'static> {
    inner: Option<Arc<Mutex<PromiseInner<T>>>>,
    index: Option<u32>,
}

pub struct Promise<T: 'static> {
    inner: Arc<Mutex<PromiseInner<T>>>,
}

enum FutureInner<T: 'static> {
    StdFuture(Pin<Box<dyn StdFuture<Output = T> + 'static>>),
    Task(async_executor::Task<T>),
    Promise(Arc<Mutex<PromiseInner<T>>>),
    Value(Box<T>),
    Invalid,
}

struct PromiseInner<T: 'static> {
    result: Option<T>,
    wakers: HashMap<u32, Waker>,
    max_index: u32,
    dropped: bool,
}

#[derive(Debug, PartialEq, Clone, thiserror::Error)]
pub enum FutureError {
    #[error("broken promise")]
    BrokenPromise,
    #[error("promise in invalid state")]
    InvalidStatePromise,
    #[error("poisoned mutex")]
    PoisonedMutex,
}

impl<T: 'static> Promise<T> {
    pub fn set(&self, value: T) -> Result<(), FutureError> {
        let wakers = self
            .inner
            .lock()
            .map_err(|_| FutureError::PoisonedMutex)?
            .set_value(value)?;
        for waker in wakers {
            waker.1.wake();
        }

        Ok(())
    }
}

impl<T: 'static> Drop for Promise<T> {
    fn drop(&mut self) {
        let mut lock = self.inner.lock().expect("mutex poisoned");
        lock.dropped = true;
        if lock.result.is_some() {
            return;
        }
        let wakers = std::mem::take(&mut lock.wakers);
        drop(lock);
        for waker in wakers {
            waker.1.wake();
        }
    }
}

impl<T: 'static> PromiseInner<T> {
    fn new() -> Self {
        Self {
            result: None,
            wakers: Default::default(),
            dropped: false,
            max_index: 0,
        }
    }

    fn set_value(
        &mut self,
        value: T,
    ) -> Result<HashMap<u32, Waker>, FutureError> {
        if self.result.is_some() {
            return Err(FutureError::InvalidStatePromise);
        }
        self.result = Some(value);
        Ok(std::mem::take(&mut self.wakers))
    }
}

impl<T: 'static> StdFuture for Future<T> {
    type Output = Result<T, FutureError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        if let FutureInner::Value(_) = self.inner {
            let inner =
                std::mem::replace(&mut self.inner, FutureInner::Invalid);
            if let FutureInner::Value(value) = inner {
                return Poll::Ready(Ok(*value));
            }
        }

        let value: Result<T, FutureError> = match self.inner {
            FutureInner::StdFuture(ref mut fut) => {
                match fut.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(value) => Ok(value),
                }
            }
            FutureInner::Task(ref mut task) => match Pin::new(task).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(value) => Ok(value),
            },
            FutureInner::Promise(ref arc) => match arc.lock() {
                Err(_) => Err(FutureError::PoisonedMutex),
                Ok(mut lock) => {
                    let result = std::mem::replace(&mut lock.result, None);
                    if let Some(value) = result {
                        Ok(value)
                    } else if lock.dropped {
                        Err(FutureError::BrokenPromise)
                    } else {
                        lock.wakers.insert(0, cx.waker().clone());
                        return Poll::Pending;
                    }
                }
            },
            _ => return Poll::Pending,
        };

        self.inner = FutureInner::Invalid;

        Poll::Ready(value)
    }
}

impl<T: 'static> Future<T> {
    pub fn new<F>(fut: F) -> Self
    where
        F: StdFuture<Output = T> + 'static,
    {
        Self {
            inner: FutureInner::<T>::StdFuture(Box::pin(fut)),
        }
    }

    pub fn spawn(mut self) -> Self {
        let inner =
            std::mem::replace(&mut self.inner, FutureInner::<T>::Invalid);

        if let FutureInner::<T>::StdFuture(fut) = inner {
            Self {
                inner: FutureInner::<T>::Task(spawn(fut)),
            }
        } else {
            Self { inner }
        }
    }

    pub fn new_promise() -> (Promise<T>, Self) {
        let inner = Arc::new(Mutex::new(PromiseInner::new()));
        let inner_clone = inner.clone();

        (
            Promise::<T> { inner },
            Self {
                inner: FutureInner::<T>::Promise(inner_clone),
            },
        )
    }

    pub fn ready(value: T) -> Self {
        Self {
            inner: FutureInner::<T>::Value(Box::new(value)),
        }
    }
}

impl<T: 'static + Clone> Future<T> {
    pub fn shared(mut self) -> SharedFuture<T> {
        let inner = std::mem::replace(&mut self.inner, FutureInner::Invalid);
        match inner {
            FutureInner::Promise(inner) => SharedFuture {
                inner: Some(inner),
                index: Some(0),
            },
            FutureInner::StdFuture(fut) => {
                let (p, f) = Future::<T>::new_promise();
                spawn(async move { p.set(fut.await) }).detach();
                f.shared()
            }
            FutureInner::Task(task) => {
                let (p, f) = Future::<T>::new_promise();
                spawn(async move { p.set(task.await) }).detach();
                f.shared()
            }
            FutureInner::Value(value) => SharedFuture {
                inner: Some(Arc::new(Mutex::new(PromiseInner {
                    result: Some(*value),
                    dropped: true,
                    wakers: Default::default(),
                    max_index: 0,
                }))),
                index: None,
            },
            FutureInner::Invalid => SharedFuture {
                inner: None,
                index: None,
            },
        }
    }
}

impl<T> Drop for Future<T>
where
    T: 'static,
{
    fn drop(&mut self) {
        let inner =
            std::mem::replace(&mut self.inner, FutureInner::<T>::Invalid);

        match inner {
            FutureInner::StdFuture(fut) => {
                spawn(fut).detach();
            }
            FutureInner::Task(task) => {
                task.detach();
            }
            _ => {}
        }
    }
}

impl<T: Clone + 'static> StdFuture for SharedFuture<T> {
    type Output = Result<T, FutureError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let mut lock = match self.inner {
            Some(ref arc) => arc,
            None => {
                return Poll::Ready(Err(FutureError::InvalidStatePromise));
            }
        }
        .lock()
        .map_err(|_| FutureError::PoisonedMutex)?;

        if let Some(ref value) = lock.result {
            return Poll::Ready(Ok(value.clone()));
        }

        if lock.dropped {
            return Poll::Ready(Err(FutureError::BrokenPromise));
        }

        let index = if let Some(index) = self.index {
            index
        } else {
            lock.max_index += 1;
            lock.max_index
        };

        lock.wakers.insert(index, cx.waker().clone());
        drop(lock);
        self.index = Some(index);

        Poll::Pending
    }
}

impl<T: Clone + 'static> Clone for SharedFuture<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            index: None,
        }
    }
}

impl<T: Clone + 'static> Drop for SharedFuture<T>
where
    T: 'static,
{
    fn drop(&mut self) {
        if let Some(index) = self.index {
            if let Some(ref arc) = self.inner {
                if let Ok(mut inner) = arc.lock() {
                    inner.wakers.remove(&index);
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// TESTS

#[cfg(test)]
mod tests {
    use super::{Future, FutureError};
    use crate::executor::run;

    #[test]
    fn ready() {
        run(async {
            let f = Future::ready(123);
            assert_eq!(f.await.unwrap(), 123);
        })
    }

    #[test]
    fn future() {
        run(async {
            let f = Future::new(async { 123 });
            assert_eq!(f.await.unwrap(), 123);
        })
    }

    #[test]
    fn task() {
        run(async {
            let f = Future::new(async { 123 }).spawn();
            assert_eq!(f.await.unwrap(), 123);
        })
    }

    #[test]
    fn channel_ok() {
        run(async {
            let (p, f) = Future::<i32>::new_promise();
            let _ = p.set(3);
            assert_eq!(f.await.unwrap(), 3);
        })
    }

    #[test]
    fn promise_and_task() {
        run(async {
            let (p, f) = Future::<i32>::new_promise();
            let f = Future::new(async { f.await }).spawn();
            p.set(123).unwrap();
            assert_eq!(f.await.unwrap().unwrap(), 123);
        })
    }

    #[test]
    fn channel_hung_up() {
        run(async {
            let (p, f) = Future::<i32>::new_promise();
            drop(p);
            assert_eq!(f.await.err().unwrap(), FutureError::BrokenPromise);
        })
    }

    #[test]
    fn shared_promise() {
        run(async {
            let (p, f) = Future::<i32>::new_promise();
            let s1 = f.shared();
            let s2 = s1.clone();
            let s3 = s1.clone();
            assert!(p.set(123).is_ok());
            assert_eq!(s1.await, Ok(123));
            assert_eq!(s2.await, Ok(123));
            let s4 = s3.clone();
            assert_eq!(s3.await, Ok(123));
            assert_eq!(s4.await, Ok(123));
        })
    }
}
