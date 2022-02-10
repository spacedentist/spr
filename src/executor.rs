thread_local! {
    static EXECUTOR: async_executor::LocalExecutor<'static> =
        async_executor::LocalExecutor::new();
}

pub fn spawn<F>(future: F) -> async_executor::Task<F::Output>
where
    F: std::future::Future + 'static,
{
    EXECUTOR.with(move |executor| executor.spawn(future))
}

pub fn run<T, F>(future: F) -> T
where
    T: 'static,
    F: std::future::Future<Output = T> + 'static,
{
    EXECUTOR.with(move |executor| {
        futures_lite::future::block_on(executor.run(executor.spawn(future)))
    })
}
