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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorrowKind {
  Read,
  Write,
}

impl BorrowKind {
  pub fn is_read(&self) -> bool {
    *self == Self::Read
  }

  pub fn is_write(&self) -> bool {
    *self == Self::Write
  }
}

#[derive(Debug)]
struct PendingWaker {
  kind: BorrowKind,
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
  pub fn try_acquire_sync(&mut self, kind: BorrowKind) -> bool {
    self.pending.is_empty() && self.try_acquire(kind)
  }

  pub fn try_acquire(&mut self, kind: BorrowKind) -> bool {
    if self.is_writing
      || kind.is_read() && !self.pending.is_empty()
      || kind.is_write() && self.reader_count > 0
    {
      false
    } else {
      if kind.is_read() {
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
        if pending.kind.is_read() {
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

  pub fn borrow(&self) -> impl Future<Output = AsyncRefCellBorrow<T>> {
    AcquireBorrowFuture {
      cell: self,
      drop_flag: Default::default(),
    }
  }

  pub fn try_borrow(&self) -> Option<AsyncRefCellBorrow<T>> {
    let acquired =
      unsafe { (*self.state.get()).try_acquire_sync(BorrowKind::Read) };
    if acquired {
      Some(self.create_borrow())
    } else {
      None
    }
  }

  fn create_borrow(&self) -> AsyncRefCellBorrow<T> {
    AsyncRefCellBorrow {
      state: self.state.clone(),
      value: self.inner.get(),
      _phantom: PhantomData::default(),
    }
  }

  pub fn borrow_mut(&self) -> impl Future<Output = AsyncRefCellBorrowMut<T>> {
    AcquireBorrowMutFuture {
      cell: self,
      drop_flag: Default::default(),
    }
  }

  pub fn try_borrow_mut(&self) -> Option<AsyncRefCellBorrowMut<T>> {
    let acquired =
      unsafe { (*self.state.get()).try_acquire_sync(BorrowKind::Write) };
    if acquired {
      Some(self.create_borrow_mut())
    } else {
      None
    }
  }

  fn create_borrow_mut(&self) -> AsyncRefCellBorrowMut<T> {
    AsyncRefCellBorrowMut {
      state: self.state.clone(),
      value: self.inner.get(),
      _phantom: PhantomData::default(),
    }
  }
}

struct AcquireBorrowFuture<'a, T> {
  cell: &'a AsyncRefCell<T>,
  drop_flag: Option<Rc<Flag>>,
}

impl<'a, T> Drop for AcquireBorrowFuture<'a, T> {
  fn drop(&mut self) {
    if let Some(flag) = &self.drop_flag {
      flag.raise();
    }
  }
}

impl<'a, T> Future for AcquireBorrowFuture<'a, T> {
  type Output = AsyncRefCellBorrow<'a, T>;

  fn poll(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Self::Output> {
    unsafe {
      let state = &mut *self.cell.state.get();

      if state.try_acquire(BorrowKind::Read) {
        Poll::Ready(self.cell.create_borrow())
      } else {
        let future_dropped =
          self.drop_flag.get_or_insert(Default::default()).clone();
        state.pending.push_back(PendingWaker {
          kind: BorrowKind::Read,
          waker: cx.waker().clone(),
          future_dropped,
        });
        Poll::Pending
      }
    }
  }
}

struct AcquireBorrowMutFuture<'a, T> {
  cell: &'a AsyncRefCell<T>,
  drop_flag: Option<Rc<Flag>>,
}

impl<'a, T> Drop for AcquireBorrowMutFuture<'a, T> {
  fn drop(&mut self) {
    if let Some(flag) = &self.drop_flag {
      flag.raise();
    }
  }
}

impl<'a, T> Future for AcquireBorrowMutFuture<'a, T> {
  type Output = AsyncRefCellBorrowMut<'a, T>;

  fn poll(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Self::Output> {
    unsafe {
      let state = &mut *self.cell.state.get();

      if state.try_acquire(BorrowKind::Write) {
        Poll::Ready(self.cell.create_borrow_mut())
      } else {
        let future_dropped =
          self.drop_flag.get_or_insert(Default::default()).clone();
        state.pending.push_back(PendingWaker {
          kind: BorrowKind::Write,
          waker: cx.waker().clone(),
          future_dropped,
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

    // The `try_borrow` and `try_borrow_mut` methods should always return `None`
    // if there's a queue of async borrowers.
    assert!(cell.try_borrow().is_none());
    assert!(cell.try_borrow_mut().is_none());

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

  #[test]
  fn async_ref_cell_try_borrow() {
    let cell = AsyncRefCell::<Thing>::default();

    {
      let ref1 = cell.try_borrow().unwrap();
      assert_eq!(ref1.look(), 0);
      assert!(cell.try_borrow_mut().is_none());
    }

    {
      let mut ref2 = cell.try_borrow_mut().unwrap();
      assert_eq!(ref2.touch(), 1);
      assert!(cell.try_borrow().is_none());
      assert!(cell.try_borrow_mut().is_none());
    }

    {
      let ref3 = cell.try_borrow().unwrap();
      let ref4 = cell.try_borrow().unwrap();
      let ref5 = cell.try_borrow().unwrap();
      let ref6 = cell.try_borrow().unwrap();
      assert_eq!(ref3.look(), 1);
      assert_eq!(ref4.look(), 1);
      assert_eq!(ref5.look(), 1);
      assert_eq!(ref6.look(), 1);
      assert!(cell.try_borrow_mut().is_none());
    }

    {
      let mut ref7 = cell.try_borrow_mut().unwrap();
      assert_eq!(ref7.look(), 1);
      assert_eq!(ref7.touch(), 2);
      assert!(cell.try_borrow().is_none());
      assert!(cell.try_borrow_mut().is_none());
    }

    {
      let ref8 = cell.try_borrow().unwrap();
      assert_eq!(ref8.look(), 2);
      assert!(cell.try_borrow_mut().is_none());
      assert!(cell.try_borrow().is_some());
    }
  }
}
