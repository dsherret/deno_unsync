use std::borrow::BorrowMut;
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::future::Future;
use std::marker::PhantomData;
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
  pub fn try_acquire(&mut self, is_reader: bool) -> bool {
    if self.is_writing
      || is_reader && !self.pending.is_empty()
      || !is_reader && self.reader_count > 0
    {
      false
    } else {
      if is_reader {
        self.reader_count += 1;
      } else {
        self.is_writing = true;
      }

      true
    }
  }

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
pub struct AsyncRefCell<T> {
  state: Rc<UnsafeCell<State>>,
  inner: UnsafeCell<T>,
}

impl<T> Default for AsyncRefCell<T>
where
  T: Default,
{
  fn default() -> Self {
    Self::new(T::default())
  }
}

impl<T> AsyncRefCell<T> {
  pub fn new(value: T) -> Self {
    Self {
      state: Default::default(),
      inner: UnsafeCell::new(value),
    }
  }

  pub async fn borrow(&self) -> AsyncRefCellBorrow<T> {
    let should_await = unsafe {
      let state = &mut *self.state.get();
      !state.try_acquire(true)
    };
    if should_await {
      let future = AcquireFuture {
        cell: self,
        is_reader: true,
        drop_flag: Default::default(),
      };
      future.await
    }

    AsyncRefCellBorrow {
      state: self.state.clone(),
      value: self.inner.get(),
      _phantom: PhantomData::default(),
    }
  }

  pub async fn borrow_mut(&self) -> AsyncRefCellBorrowMut<T> {
    let should_await = unsafe {
      let state = &mut *self.state.get();
      !state.try_acquire(false)
    };
    if should_await {
      let future = AcquireFuture {
        cell: self,
        is_reader: false,
        drop_flag: Default::default(),
      };
      future.await
    }

    AsyncRefCellBorrowMut {
      state: self.state.clone(),
      value: self.inner.get(),
      _phantom: PhantomData::default(),
    }
  }
}

struct AcquireFuture<'a, T> {
  cell: &'a AsyncRefCell<T>,
  is_reader: bool,
  drop_flag: Option<Rc<Flag>>,
}

impl<'a, T> Drop for AcquireFuture<'a, T> {
  fn drop(&mut self) {
    if let Some(flag) = &self.drop_flag {
      flag.raise();
    }
  }
}

impl<'a, T> Future for AcquireFuture<'a, T> {
  type Output = ();

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    unsafe {
      let state = &mut *self.cell.state.get();

      if state.try_acquire(self.is_reader) {
        Poll::Ready(())
      } else {
        let drop_flag = self.drop_flag.get_or_insert_with(Default::default).clone();
        state.pending.push_back(PendingWaker {
          is_reader: self.is_reader,
          waker: cx.waker().clone(),
          future_dropped: drop_flag,
        });
        Poll::Pending
      }
    }
  }
}

#[derive(Debug)]
pub struct AsyncRefCellBorrow<'a, T> {
  state: Rc<UnsafeCell<State>>,
  value: *const T,
  _phantom: PhantomData<&'a T>,
}

impl<'a, T> Drop for AsyncRefCellBorrow<'a, T> {
  fn drop(&mut self) {
    unsafe {
      let state = &mut *self.state.get();

      state.reader_count -= 1;

      if state.reader_count == 0 {
        state.wake_pending();
      }
    }
  }
}

impl<'a, T> Deref for AsyncRefCellBorrow<'a, T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    unsafe { &*self.value }
  }
}

#[derive(Debug)]
pub struct AsyncRefCellBorrowMut<'a, T> {
  state: Rc<UnsafeCell<State>>,
  value: *mut T,
  _phantom: PhantomData<&'a T>,
}

impl<'a, T> Drop for AsyncRefCellBorrowMut<'a, T> {
  fn drop(&mut self) {
    unsafe {
      let state = &mut *self.state.get();

      state.is_writing = false;
      state.wake_pending();
    }
  }
}

impl<'a, T> Deref for AsyncRefCellBorrowMut<'a, T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    unsafe { &*self.value }
  }
}

impl<'a, T> DerefMut for AsyncRefCellBorrowMut<'a, T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    unsafe { &mut *self.value }
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[derive(Default)]
  struct Thing {
    touch_count: usize,
    _private: (),
  }

  impl Thing {
    pub fn look(&self) -> usize {
      self.touch_count
    }

    pub fn touch(&mut self) -> usize {
      self.touch_count += 1;
      self.touch_count
    }
  }

  #[tokio::test]
  async fn async_ref_cell_borrow() {
    let cell = AsyncRefCell::<Thing>::default();

    let fut1 = cell.borrow();
    let fut2 = cell.borrow_mut();
    let fut3 = cell.borrow();
    let fut4 = cell.borrow();
    let fut5 = cell.borrow();
    let fut6 = cell.borrow();
    let fut7 = cell.borrow_mut();
    let fut8 = cell.borrow();

    assert_eq!(fut1.await.look(), 0);

    assert_eq!(fut2.await.touch(), 1);

    {
      let ref5 = fut5.await;
      let ref4 = fut4.await;
      let ref3 = fut3.await;
      let ref6 = fut6.await;
      assert_eq!(ref3.look(), 1);
      assert_eq!(ref4.look(), 1);
      assert_eq!(ref5.look(), 1);
      assert_eq!(ref6.look(), 1);
    }

    {
      let mut ref7 = fut7.await;
      assert_eq!(ref7.look(), 1);
      assert_eq!(ref7.touch(), 2);
    }

    {
      let ref8 = fut8.await;
      assert_eq!(ref8.look(), 2);
    }
  }
}
