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
enum WakerState {
  Pending,
  Waiting(Option<Waker>),
  Ready(Rc<UnsafeCell<State>>),
  Complete,
}

#[derive(Debug)]
struct PendingWaker {
  kind: BorrowKind,
  waker: WakerState,
  future_dropped: bool,
}

impl Drop for PendingWaker {
  fn drop(&mut self) {
    if let WakerState::Ready(state) = &self.waker {
      // the future never got woken up, so release the acquired borrow
      unsafe {
        (*state.get()).release(self.kind, &state);
      }
    }
  }
}

#[derive(Debug, Default)]
struct State {
  pending: VecDeque<Rc<UnsafeCell<PendingWaker>>>,
  is_writing: bool,
  reader_count: usize,
}

impl State {
  pub fn try_acquire_sync(&mut self, kind: BorrowKind) -> bool {
    // don't allow cutting in line if there are pending borrows
    self.pending.is_empty() && self.try_acquire(kind)
  }

  pub fn try_acquire(&mut self, kind: BorrowKind) -> bool {
    if self.is_writing
      || kind.is_read() && !self.pending.is_empty()
      || kind.is_write() && self.reader_count > 0
    {
      false
    } else {
      self.acquire(kind);
      true
    }
  }

  fn acquire(&mut self, kind: BorrowKind) {
    if kind.is_read() {
      self.reader_count += 1;
    } else {
      self.is_writing = true;
    }
  }

  pub fn release(&mut self, kind: BorrowKind, state: &Rc<UnsafeCell<State>>) {
    if kind.is_read() {
      self.reader_count -= 1;

      if self.reader_count == 0 {
        self.wake_pending(state);
      }
    } else {
      self.is_writing = false;
      self.wake_pending(state);
    }
  }

  fn wake_pending(&mut self, state: &Rc<UnsafeCell<State>>) {
    let mut found_pending = false;
    while let Some(pending_cell) = self.pending.pop_front() {
      unsafe {
        let pending = &mut *pending_cell.get();
        if !pending.future_dropped {
          if pending.kind.is_read() || !found_pending {
            match &mut pending.waker {
              WakerState::Complete => {
                // ignore
              }
              WakerState::Pending => {
                self.acquire(pending.kind);
                pending.waker = WakerState::Ready(state.clone());
                found_pending = true;
              }
              WakerState::Ready(_) => {
                unreachable!();
              }
              WakerState::Waiting(waker) => {
                self.acquire(pending.kind);
                waker.take().unwrap().wake();
                pending.waker = WakerState::Ready(state.clone());
                found_pending = true;
              }
            }
            if pending.kind.is_write() && found_pending {
              break; // found a writer, exit
            }
          } else if pending.kind.is_write() {
            debug_assert!(found_pending);
            // there were already pending readers, store back this writer
            self.pending.push_front(pending_cell);
            break;
          }
        }
      }
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
    let maybe_borrow = self.try_borrow();
    AcquireBorrowFuture {
      cell: self,
      borrow_or_pending_item: match maybe_borrow {
        Some(borrow) => BorrowOrPendingWaker::Borrow(Some(borrow)),
        None => {
          let waker = Rc::new(UnsafeCell::new(PendingWaker {
            kind: BorrowKind::Read,
            waker: WakerState::Pending,
            future_dropped: false,
          }));
          unsafe {
            (*self.state.get()).pending.push_back(waker.clone());
          }
          BorrowOrPendingWaker::PendingWaker(waker)
        }
      },
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
    let maybe_borrow = self.try_borrow_mut();
    AcquireBorrowMutFuture {
      cell: self,
      borrow_or_pending_item: match maybe_borrow {
        Some(borrow) => BorrowMutOrPendingWaker::Borrow(Some(borrow)),
        None => {
          let waker = Rc::new(UnsafeCell::new(PendingWaker {
            kind: BorrowKind::Write,
            waker: WakerState::Pending,
            future_dropped: false,
          }));
          unsafe {
            (*self.state.get()).pending.push_back(waker.clone());
          }
          BorrowMutOrPendingWaker::PendingWaker(waker)
        }
      },
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

enum BorrowOrPendingWaker<'a, T> {
  Borrow(Option<AsyncRefCellBorrow<'a, T>>),
  PendingWaker(Rc<UnsafeCell<PendingWaker>>),
}

struct AcquireBorrowFuture<'a, T> {
  cell: &'a AsyncRefCell<T>,
  borrow_or_pending_item: BorrowOrPendingWaker<'a, T>,
}

impl<'a, T> Drop for AcquireBorrowFuture<'a, T> {
  fn drop(&mut self) {
    match &self.borrow_or_pending_item {
      BorrowOrPendingWaker::Borrow(_) => {
        // ignore, will be dropped
      }
      BorrowOrPendingWaker::PendingWaker(pending) => unsafe {
        let pending = &mut *pending.get();
        pending.future_dropped = true;
      },
    }
  }
}

impl<'a, T> Future for AcquireBorrowFuture<'a, T> {
  type Output = AsyncRefCellBorrow<'a, T>;

  fn poll(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Self::Output> {
    match &mut self.borrow_or_pending_item {
      BorrowOrPendingWaker::Borrow(borrow) => {
        Poll::Ready(borrow.take().unwrap())
      }
      BorrowOrPendingWaker::PendingWaker(waker) => unsafe {
        let waker = &mut *waker.get();
        match &waker.waker {
          WakerState::Pending | WakerState::Waiting(_) => {
            waker.waker = WakerState::Waiting(Some(cx.waker().clone()));
            Poll::Pending
          }
          WakerState::Ready(_) => {
            waker.waker = WakerState::Complete;
            Poll::Ready(self.cell.create_borrow())
          }
          WakerState::Complete => {
            unreachable!();
          }
        }
      },
    }
  }
}

enum BorrowMutOrPendingWaker<'a, T> {
  Borrow(Option<AsyncRefCellBorrowMut<'a, T>>),
  PendingWaker(Rc<UnsafeCell<PendingWaker>>),
}

struct AcquireBorrowMutFuture<'a, T> {
  cell: &'a AsyncRefCell<T>,
  borrow_or_pending_item: BorrowMutOrPendingWaker<'a, T>,
}

impl<'a, T> Drop for AcquireBorrowMutFuture<'a, T> {
  fn drop(&mut self) {
    match &mut self.borrow_or_pending_item {
      BorrowMutOrPendingWaker::Borrow(_) => {
        // ignore, will be dropped
      }
      BorrowMutOrPendingWaker::PendingWaker(pending) => unsafe {
        let pending = &mut *pending.get();
        pending.future_dropped = true;
      },
    }
  }
}

impl<'a, T> Future for AcquireBorrowMutFuture<'a, T> {
  type Output = AsyncRefCellBorrowMut<'a, T>;

  fn poll(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Self::Output> {
    match &mut self.borrow_or_pending_item {
      BorrowMutOrPendingWaker::Borrow(borrow) => {
        Poll::Ready(borrow.take().unwrap())
      }
      BorrowMutOrPendingWaker::PendingWaker(waker) => unsafe {
        let waker = &mut *waker.get();
        match &waker.waker {
          WakerState::Pending | WakerState::Waiting(_) => {
            waker.waker = WakerState::Waiting(Some(cx.waker().clone()));
            Poll::Pending
          }
          WakerState::Ready(_) => {
            waker.waker = WakerState::Complete;
            Poll::Ready(self.cell.create_borrow_mut())
          }
          WakerState::Complete => {
            unreachable!();
          }
        }
      },
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
      (*self.state.get()).release(BorrowKind::Read, &self.state);
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
      (*self.state.get()).release(BorrowKind::Write, &self.state);
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
