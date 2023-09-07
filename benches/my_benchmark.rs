use std::rc::Rc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use deno_unsync::AsyncRefCell;
use deno_unsync::other::AsyncRefCell as CoreAsyncRefCell;
use criterion::async_executor::FuturesExecutor;

fn criterion_benchmark(c: &mut Criterion) {
  c.bench_function("new read", |b| {
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
  c.bench_function("new mut", |b| {
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

  c.bench_function("core read", |b| {
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
  c.bench_function("core mut", |b| {
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
