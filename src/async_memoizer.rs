/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::{collections::HashMap, hash::Hash};

use crate::{
    executor::spawn,
    future::{Future, SharedFuture},
};

pub struct AsyncMemoizer<K, V>
where
    K: Eq + Hash + Clone + 'static,
    V: Clone + 'static,
{
    inner: std::rc::Rc<async_lock::Mutex<Inner<K, V>>>,
}

struct Inner<K, V>
where
    K: Eq + Hash + Clone + 'static,
    V: Clone + 'static,
{
    map: HashMap<K, SharedFuture<V>>,
    func: Box<dyn Fn(K) -> Future<V>>,
}

impl<K, V> AsyncMemoizer<K, V>
where
    K: Eq + Hash + Clone + 'static,
    V: Clone + 'static,
{
    pub fn new<F, Fut>(func: F) -> Self
    where
        F: (Fn(K) -> Fut) + 'static,
        Fut: std::future::Future<Output = V> + 'static,
    {
        let inner = Inner {
            map: HashMap::new(),
            func: Box::new(move |k| Future::new(func(k))),
        };
        Self {
            inner: std::rc::Rc::new(async_lock::Mutex::new(inner)),
        }
    }

    pub fn get(&self, key: K) -> Future<V> {
        let (p, f) = Future::<V>::new_promise();
        let inner = self.inner.clone();

        spawn(async move {
            let shared = {
                let mut inner = inner.lock().await;
                let inner = &mut *inner;

                inner
                    .map
                    .entry(key)
                    .or_insert_with_key({
                        let func = &inner.func;
                        |key| func(key.clone()).shared()
                    })
                    .clone()
            };

            if let Ok(result) = shared.await {
                p.set(result).ok();
            }
        })
        .detach();

        f
    }
}

// ----------------------------------------------------------------------------
// TESTS

#[cfg(test)]
mod tests {
    use super::AsyncMemoizer;
    use crate::{error::Result, executor::run, future::Future};

    #[test]
    fn unit_key() {
        run(async {
            let memoizer = AsyncMemoizer::new(|_: ()| async { 123 });
            assert_eq!(memoizer.get(()).await.unwrap(), 123);
        })
    }

    #[test]
    fn u64_key() {
        run(async {
            let number_of_calls =
                std::rc::Rc::new(std::sync::Mutex::new(0usize));
            let memoizer = AsyncMemoizer::new({
                let number_of_calls = number_of_calls.clone();
                move |number: u64| {
                    let number_of_calls = number_of_calls.clone();
                    async move {
                        let mut lock = number_of_calls.lock().unwrap();
                        (*lock) += 1;

                        number * 2
                    }
                }
            });

            assert_eq!(*number_of_calls.lock().unwrap(), 0);
            assert_eq!(memoizer.get(123).await.unwrap(), 246);
            assert_eq!(*number_of_calls.lock().unwrap(), 1);
            assert_eq!(memoizer.get(1234).await.unwrap(), 2468);
            assert_eq!(*number_of_calls.lock().unwrap(), 2);
            assert_eq!(memoizer.get(123).await.unwrap(), 246);
            assert_eq!(*number_of_calls.lock().unwrap(), 2);
        })
    }

    #[test]
    fn parallel_gets() -> Result<()> {
        run(async {
            #[derive(Clone, Hash, PartialEq, Eq)]
            enum Ott {
                One,
                Two,
                Three,
            }

            let (p1, f1) = Future::<u32>::new_promise();
            let (p2, f2) = Future::<u32>::new_promise();
            let (p3, f3) = Future::<u32>::new_promise();

            let number_of_calls =
                std::rc::Rc::new(std::sync::Mutex::new(0usize));
            let memoizer = AsyncMemoizer::new({
                let number_of_calls = number_of_calls.clone();
                let f1 = f1.shared();
                let f2 = f2.shared();
                let f3 = f3.shared();
                move |key: Ott| {
                    *number_of_calls.lock().unwrap() += 1;
                    match key {
                        Ott::One => f1.clone(),
                        Ott::Two => f2.clone(),
                        Ott::Three => f3.clone(),
                    }
                }
            });

            let memf1_1 = memoizer.get(Ott::One);
            let memf1_2 = memoizer.get(Ott::One);
            let memf2_1 = memoizer.get(Ott::Two);
            let memf3_1 = memoizer.get(Ott::Three);
            let memf2_2 = memoizer.get(Ott::Two);
            let memf3_2 = memoizer.get(Ott::Three);

            p2.set(222)?;
            assert_eq!(memf2_1.await??, 222);
            assert_eq!(memf2_2.await??, 222);
            p3.set(333)?;
            assert_eq!(memf3_1.await??, 333);
            assert_eq!(memf3_2.await??, 333);
            p1.set(111)?;
            assert_eq!(memf1_1.await??, 111);
            assert_eq!(memf1_2.await??, 111);

            assert_eq!(*number_of_calls.lock().unwrap(), 3);

            Ok(())
        })
    }

    #[test]
    fn execute_before_await() -> Result<()> {
        run(async {
            let (p, f) = Future::<u32>::new_promise();
            let p = std::sync::Arc::new(p);

            let memoizer = AsyncMemoizer::new(move |_: ()| {
                let p = p.clone();
                async move {
                    p.set(456).unwrap();
                    123
                }
            });

            // We call memoizer.get, which will call the above lambda, which
            // will call `p.set(456)`. But we are not awaiting the returned
            // future yet.
            let memoizer_get_future = memoizer.get(());

            // We are awaiting the future, which means we are waiting for
            // `p.set` to be called.
            assert_eq!(f.await?, 456);

            // Just check the `memoizer.get` call also returns the expected reult.
            assert_eq!(memoizer_get_future.await.unwrap(), 123);

            Ok(())
        })
    }
}
