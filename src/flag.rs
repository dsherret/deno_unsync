use std::cell::RefCell;

/// A flag with interior mutability that can be raised or lowered.
/// Useful for indicating if an event has occurred.
#[derive(Debug, Default)]
pub struct Flag(RefCell<bool>);

impl Flag {
  /// Raise the flag.
  pub fn raise(&self) {
    *self.0.borrow_mut() = true;
  }

  /// Lower the flag.
  pub fn lower(&self) {
    *self.0.borrow_mut() = false;
  }

  /// Gets if the flag is raised.
  pub fn is_raised(&self) -> bool {
    *self.0.borrow()
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn test_raise_lower() {
    let flag = Flag::default();
    assert!(!flag.is_raised());
    flag.raise();
    assert!(flag.is_raised());
    flag.raise();
    assert!(flag.is_raised());
    flag.lower();
    assert!(!flag.is_raised());
    flag.lower();
    assert!(!flag.is_raised());
  }
}
