use std::rc::Rc;

use criterion::async_executor::FuturesExecutor;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use deno_unsync::other::AsyncRefCell as CoreAsyncRefCell;
use deno_unsync::AsyncRefCell;

fn criterion_benchmark(c: &mut Criterion) {
  c.bench_function("new read sequential", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = AsyncRefCell::new(1);
      for _ in 0..10_000 {
        let borrow = cell.borrow().await;
        assert_eq!(*borrow, 1);
      }
    })
  });
  c.bench_function("new read contention", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = AsyncRefCell::new(1);
      let mut borrows = Vec::with_capacity(10_000);
      for _ in 0..borrows.capacity() {
        borrows.push(cell.borrow());
      }
      for borrow in borrows {
        let borrow = borrow.await;
        assert_eq!(*borrow, 1);
      }
    })
  });
  c.bench_function("new mut sequential", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = AsyncRefCell::new(1);
      for _ in 0..10_000 {
        let mut borrow = cell.borrow_mut().await;
        *borrow = 2;
        assert_eq!(*borrow, 2);
      }
    })
  });
  c.bench_function("new mut contention", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = AsyncRefCell::new(1);
      let mut borrows = Vec::with_capacity(10_000);
      for _ in 0..borrows.capacity() {
        borrows.push(cell.borrow_mut());
      }
      for borrow in borrows {
        let mut borrow = borrow.await;
        *borrow = 2;
        assert_eq!(*borrow, 2);
      }
    })
  });

  c.bench_function("core read sequential", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = Rc::new(CoreAsyncRefCell::new(1));
      for _ in 0..10_000 {
        let borrow = cell.borrow().await;
        assert_eq!(*borrow, 1);
      }
    })
  });
  c.bench_function("core read contention", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = Rc::new(CoreAsyncRefCell::new(1));
      let mut borrows = Vec::with_capacity(10_000);
      for _ in 0..borrows.capacity() {
        borrows.push(cell.borrow());
      }
      for borrow in borrows {
        let borrow = borrow.await;
        assert_eq!(*borrow, 1);
      }
    })
  });
  c.bench_function("core mut sequential", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = Rc::new(CoreAsyncRefCell::new(1));
      for _ in 0..10_000 {
        let mut borrow = cell.borrow_mut().await;
        *borrow = 2;
        assert_eq!(*borrow, 2);
      }
    })
  });
  c.bench_function("core mut contention", |b| {
    b.to_async(FuturesExecutor).iter(|| async move {
      let cell = Rc::new(CoreAsyncRefCell::new(1));
      let mut borrows = Vec::with_capacity(10_000);
      for _ in 0..borrows.capacity() {
        borrows.push(cell.borrow_mut());
      }
      for borrow in borrows {
        let mut borrow = borrow.await;
        *borrow = 2;
        assert_eq!(*borrow, 2);
      }
    })
  });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
