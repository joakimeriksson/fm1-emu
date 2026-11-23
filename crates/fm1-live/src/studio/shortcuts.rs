//! File dialogs open on key release, after an idle frame, to avoid reopening on repeats.
#[derive(Default)]
pub struct ReleaseShortcut {
    ready: bool,
    pressed: bool,
}
impl ReleaseShortcut {
    pub fn update(&mut self, down: bool) -> bool {
        if !self.ready {
            self.ready = !down;
            return false;
        }
        let fire = self.pressed && !down;
        self.pressed = down;
        fire
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn ignores_startup_hold_and_repeats() {
        let mut key = super::ReleaseShortcut::default();
        for down in [true, true, false, false, true, true] {
            assert!(!key.update(down));
        }
        assert!(key.update(false));
        assert!(!key.update(false));
    }
}
