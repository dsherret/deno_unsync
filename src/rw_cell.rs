use std::cell::Ref;
use std::cell::RefCell;
use std::cell::RefMut;
use std::collections::VecDeque;
use std::future::Future;
use std::ops::Deref;
use std::ops::DerefMut;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use crate::Flag;

#[derive(Debug)]
struct PendingWaker {
  is_reader: bool,
  waker: Waker,
  future_dropped: Rc<Flag>,
}

#[derive(Debug, Default)]
struct State {
  pending: VecDeque<PendingWaker>,
  is_writing: bool,
  reader_count: usize,
}

impl State {
  pub fn wake_pending(&mut self) {
    let mut found_pending = Vec::new();
    while let Some(pending) = self.pending.pop_front() {
      if !pending.future_dropped.is_raised() {
        let is_reader = pending.is_reader;
        if is_reader {
          found_pending.push(pending);
        } else if found_pending.is_empty() {
          found_pending.push(pending);
          break; // found a writer, exit
        } else {
          // there were already pending readers, store back this writer
          self.pending.push_front(pending);
          break;
        }
      }
    }

    for pending in found_pending {
      pending.waker.wake();
    }
  }
}

/// An unsync read-write cell that handles read and write requests in order.
/// Read requests can proceed at the same time as another read request, but
/// write requests must wait for all other requests to finish before proceeding.
#[derive(Debug)]
pub struct AsyncRwCell<T> {
  state: Rc<RefCell<State>>,
  inner: RefCell<T>,
}

impl<T> Default for AsyncRwCell<T>
where
  T: Default,
{
  fn default() -> Self {
    Self::new(T::default())
  }
}

impl<T> AsyncRwCell<T> {
  pub fn new(value: T) -> Self {
    Self {
      state: Default::default(),
      inner: RefCell::new(value),
    }
  }

  pub async fn read(&self) -> AsyncRwCellReadPermit<T> {
    AcquireFuture {
      is_reader: true,
      state: self.state.clone(),
      drop_flag: Default::default(),
    }
    .await;

    AsyncRwCellReadPermit {
      state: self.state.clone(),
      value: self.inner.borrow(),
    }
  }

  pub async fn write(&self) -> AsyncRwCellWritePermit<T> {
    AcquireFuture {
      is_reader: false,
      state: self.state.clone(),
      drop_flag: Default::default(),
    }
    .await;

    AsyncRwCellWritePermit {
      state: self.state.clone(),
      value: self.inner.borrow_mut(),
    }
  }
}

struct AcquireFuture {
  is_reader: bool,
  state: Rc<RefCell<State>>,
  drop_flag: Rc<Flag>,
}

impl Drop for AcquireFuture {
  fn drop(&mut self) {
    self.drop_flag.raise();
  }
}

impl Future for AcquireFuture {
  type Output = ();

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let mut state = self.state.borrow_mut();

    if state.is_writing
      || self.is_reader && !state.pending.is_empty()
      || !self.is_reader && state.reader_count > 0
    {
      state.pending.push_back(PendingWaker {
        is_reader: self.is_reader,
        waker: cx.waker().clone(),
        future_dropped: self.drop_flag.clone(),
      });
      Poll::Pending
    } else {
      if self.is_reader {
        state.reader_count += 1;
      } else {
        state.is_writing = true;
      }

      Poll::Ready(())
    }
  }
}

#[derive(Debug)]
pub struct AsyncRwCellReadPermit<'a, T> {
  state: Rc<RefCell<State>>,
  value: Ref<'a, T>,
}

impl<'a, T> Drop for AsyncRwCellReadPermit<'a, T> {
  fn drop(&mut self) {
    let mut state = self.state.borrow_mut();

    state.reader_count -= 1;

    if state.reader_count == 0 {
      state.wake_pending();
    }
  }
}

impl<'a, T> Deref for AsyncRwCellReadPermit<'a, T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    &self.value
  }
}

#[derive(Debug)]
pub struct AsyncRwCellWritePermit<'a, T> {
  state: Rc<RefCell<State>>,
  value: RefMut<'a, T>,
}

impl<'a, T> Drop for AsyncRwCellWritePermit<'a, T> {
  fn drop(&mut self) {
    let mut state = self.state.borrow_mut();

    state.is_writing = false;
    state.wake_pending();
  }
}

impl<'a, T> Deref for AsyncRwCellWritePermit<'a, T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    &self.value
  }
}

impl<'a, T> DerefMut for AsyncRwCellWritePermit<'a, T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    self.value.deref_mut()
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn should_allow_multiple_readers() {
    let mut rw_cell = AsyncRwCell::new(0);
  }
}