// Copyright 2018-2024 the Deno authors. MIT license.

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;
use std::task::RawWaker;
use std::task::RawWakerVTable;
use std::task::Waker;

use futures_core::Stream;

use crate::Flag;
use crate::UnsyncWaker;

struct SharedState {
  parent_waker: UnsyncWaker,
  indexes_to_poll: UnsafeCell<VecDeque<usize>>,
}

impl SharedState {
  pub fn push_index(&self, index: usize) {
    unsafe {
      let indexes = &mut *self.indexes_to_poll.get();
      indexes.push_back(index);
    }
  }

  pub fn pop_index(&self) -> Option<usize> {
    unsafe {
      let indexes = &mut *self.indexes_to_poll.get();
      indexes.pop_front()
    }
  }
}

struct FutureData<F: std::future::Future> {
  future: F,
  child_state: Rc<ChildWakerState>,
}

/// A ![`Sync`] and ![`Sync`] version of `futures::stream::FuturesUnordered`
/// that is backed by a vector.
///
/// This is useful if you know the number of the futures you
/// want to collect ahead of time.
pub struct VecFuturesUnordered<F: std::future::Future> {
  futures: Vec<Option<FutureData<F>>>,
  shared_state: Rc<SharedState>,
  len: usize,
}

impl<F: std::future::Future> Drop for VecFuturesUnordered<F> {
    fn drop(&mut self) {
      if self.len == 0 {
        // if the length is 0 then we can skip running the destructor
        // for each item in the vector because we know they'll all be
        // None
        let futures = std::mem::take(&mut self.futures);
        std::mem::forget(futures);
      }
    }
}

impl<F: std::future::Future> VecFuturesUnordered<F> {
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      futures: Vec::with_capacity(capacity),
      shared_state: Rc::new(SharedState {
        parent_waker: UnsyncWaker::default(),
        indexes_to_poll: UnsafeCell::new(VecDeque::with_capacity(capacity)),
      }),
      len: 0,
    }
  }

  pub fn is_empty(&self) -> bool {
    self.len == 0
  }

  pub fn len(&self) -> usize {
    self.len
  }

  pub fn push(&mut self, future: F) {
    let index = self.futures.len();
    self.futures.push(Some(FutureData {
      future,
      child_state: Rc::new(ChildWakerState {
        index,
        woken: Flag::raised(),
        shared_state: self.shared_state.clone(),
      }),
    }));
    self.shared_state.push_index(index);
    self.len += 1;
  }
}

impl<F> Stream for VecFuturesUnordered<F>
where
  F: std::future::Future + Unpin,
{
  type Item = F::Output;

  fn poll_next(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Self::Item>> {
    if self.len == 0 {
      return Poll::Ready(None);
    }

    self.shared_state.parent_waker.register(cx.waker());

    while let Some(index) = self.shared_state.pop_index() {
      let future_data = self.futures[index].as_mut().unwrap();
      let was_lowered = future_data.child_state.woken.lower();
      debug_assert!(was_lowered);
      let child_waker = create_child_waker(future_data.child_state.clone());
      let mut child_cx = Context::from_waker(&child_waker);
      let future = Pin::new(&mut future_data.future);
      match future.poll(&mut child_cx) {
        Poll::Ready(output) => {
          self.futures[index] = None;
          self.len -= 1;
          return Poll::Ready(Some(output));
        }
        Poll::Pending => {}
      }
    }

    Poll::Pending
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    let len = self.len();
    (len, Some(len))
  }
}

struct ChildWakerState {
  index: usize,
  woken: Flag,
  shared_state: Rc<SharedState>,
}

fn create_child_waker(state: Rc<ChildWakerState>) -> Waker {
  let raw_waker = RawWaker::new(
    Rc::into_raw(state) as *const (),
    &RawWakerVTable::new(
      clone_waker,
      wake_waker,
      wake_by_ref_waker,
      drop_waker,
    ),
  );
  unsafe { Waker::from_raw(raw_waker) }
}

unsafe fn clone_waker(data: *const ()) -> RawWaker {
  Rc::increment_strong_count(data as *const ChildWakerState);
  RawWaker::new(
    data,
    &RawWakerVTable::new(
      clone_waker,
      wake_waker,
      wake_by_ref_waker,
      drop_waker,
    ),
  )
}

unsafe fn wake_waker(data: *const ()) {
  let state = Rc::from_raw(data as *const ChildWakerState);
  if state.woken.raise() {
    state.shared_state.push_index(state.index);
  }
  state.shared_state.parent_waker.wake();
}

unsafe fn wake_by_ref_waker(data: *const ()) {
  let state = Rc::from_raw(data as *const ChildWakerState);
  if state.woken.raise() {
    state.shared_state.push_index(state.index);
  }
  state.shared_state.parent_waker.wake_by_ref();
  let _ = Rc::into_raw(state); // keep it alive
}

unsafe fn drop_waker(data: *const ()) {
  Rc::decrement_strong_count(data as *const ChildWakerState);
}

#[cfg(test)]
mod test {
  use std::time::Duration;

  use futures::FutureExt;
  use futures::StreamExt;

  use super::*;

  #[tokio::test(flavor = "current_thread")]
  async fn single_future() {
    let mut futures = VecFuturesUnordered::with_capacity(1);
    futures.push(
      async {
        tokio::task::yield_now().await;
        1
      }
      .boxed_local(),
    );

    let first = futures.next().await.unwrap();
    assert_eq!(first, 1);
  }

  #[tokio::test(flavor = "current_thread")]
  async fn many_yielded() {
    let len = 10_000;
    let mut futures = VecFuturesUnordered::with_capacity(len);
    for i in 0..len {
      futures.push(
        async move {
          tokio::task::yield_now().await;
          i
        }
        .boxed_local(),
      );
      assert_eq!(futures.len(), i + 1);
    }
    let mut sum = 0;
    let mut expected_len = len;
    while let Some(value) = futures.next().await {
      sum += value;
      expected_len -= 1;
      assert_eq!(futures.len(), expected_len);
    }
    assert_eq!(sum, 49995000);
  }

  #[cfg(not(miri))]
  #[tokio::test(flavor = "current_thread")]
  async fn completes_first_to_finish_time() {
    let mut futures = VecFuturesUnordered::with_capacity(3);
    futures.push(
      async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        1
      }
      .boxed_local(),
    );
    futures.push(
      async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        2
      }
      .boxed_local(),
    );
    futures.push(
      async {
        tokio::time::sleep(Duration::from_millis(25)).await;
        3
      }
      .boxed_local(),
    );

    let first = futures.next().await.unwrap();
    let second = futures.next().await.unwrap();
    let third = futures.next().await.unwrap();
    assert_eq!(first, 3);
    assert_eq!(second, 2);
    assert_eq!(third, 1);
  }

  #[tokio::test(flavor = "current_thread")]
  async fn completes_first_to_finish_polls() {
    let mut futures = VecFuturesUnordered::with_capacity(3);
    futures.push(
      async {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        1
      }
      .boxed_local(),
    );
    futures.push(
      async {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        2
      }
      .boxed_local(),
    );
    futures.push(
      async {
        tokio::task::yield_now().await;
        3
      }
      .boxed_local(),
    );
    assert_eq!(futures.len(), 3);

    let first = futures.next().await.unwrap();
    assert_eq!(futures.len(), 2);
    let second = futures.next().await.unwrap();
    assert_eq!(futures.len(), 1);
    assert!(!futures.is_empty());
    let third = futures.next().await.unwrap();
    assert_eq!(futures.len(), 0);
    assert!(futures.is_empty());
    assert_eq!(first, 3);
    assert_eq!(second, 2);
    assert_eq!(third, 1);
  }
}
