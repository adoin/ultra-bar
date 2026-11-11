/// Capture the active application before mouse capture or a popup can move focus.
#[derive(Clone, Copy)]
pub struct TaskClick {
    active_at_press: bool,
}

#[derive(Debug, PartialEq)]
pub enum Operation {
    Minimize,
    Activate,
}

pub fn foreground_candidate(direct: usize, gui: usize, observed: usize) -> usize {
    [direct, gui, observed]
        .into_iter()
        .find(|window| *window != 0)
        .unwrap_or(0)
}

impl TaskClick {
    pub fn begin(target_owner: usize, foreground_owner: usize, minimized: bool) -> Self {
        Self {
            active_at_press: target_owner != 0 && target_owner == foreground_owner && !minimized,
        }
    }
    pub fn release(self, currently_minimized: bool) -> Operation {
        if self.active_at_press && !currently_minimized {
            Operation::Minimize
        } else {
            Operation::Activate
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn null_foreground_uses_observation_and_real_focus_changes_override_it() {
        assert_eq!(foreground_candidate(0, 0, 10), 10);
        assert_eq!(foreground_candidate(0, 20, 10), 20);
        assert_eq!(foreground_candidate(30, 20, 10), 30);
        let remembered = foreground_candidate(0, 0, 10);
        assert_eq!(
            TaskClick::begin(10, remembered, false).release(false),
            Operation::Minimize
        );
    }
    #[test]
    fn active_window_still_minimizes_when_taskbar_takes_focus_before_release() {
        let press = TaskClick::begin(10, 10, false);
        // The foreground at release may be a taskbar/tooltip HWND; it must not
        // replace the application's state captured at the start of this click.
        assert_eq!(press.release(false), Operation::Minimize);
    }
    #[test]
    fn background_and_minimized_windows_activate_instead_of_minimizing() {
        assert_eq!(
            TaskClick::begin(10, 20, false).release(false),
            Operation::Activate
        );
        assert_eq!(
            TaskClick::begin(10, 10, true).release(true),
            Operation::Activate
        );
        assert_eq!(
            TaskClick::begin(10, 10, false).release(true),
            Operation::Activate
        );
        assert_eq!(
            TaskClick::begin(0, 0, false).release(false),
            Operation::Activate
        );
    }
}
