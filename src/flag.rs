use std::cell::Cell;

/// A flag with interior mutability that can be raised or lowered.
/// Useful for indicating if an event has occurred.
#[derive(Debug, Default)]
pub struct Flag(Cell<bool>);

impl Flag {
  /// Raise the flag.
  pub fn raise(&self) {
    self.0.set(true)
  }

  /// Lower the flag.
  pub fn lower(&self) {
    self.0.set(false)
  }

  /// Gets if the flag is raised.
  pub fn is_raised(&self) -> bool {
    self.0.get()
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
