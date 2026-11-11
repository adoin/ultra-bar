use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Labels,
    Icons,
    Overflow,
}

#[derive(Debug)]
pub struct Layout {
    pub mode: Mode,
    pub visible: Range<usize>,
    pub leading: Range<usize>,
    pub trailing: Range<usize>,
    pub widths: Vec<i32>,
    pub overflow_width: i32,
}

/// Preserve a contiguous interval around the active application, like Git Agent
/// tabs. Hidden applications retain their order on both sides.
#[cfg(test)]
fn arrange(preferred: &[i32], available: i32, dpi: i32, icons_only: bool, anchor: usize) -> Layout {
    arrange_with_slot(preferred, available, dpi, icons_only, anchor, 48)
}

#[cfg(test)]
pub fn arrange_with_slot(
    preferred: &[i32],
    available: i32,
    dpi: i32,
    icons_only: bool,
    anchor: usize,
    slot: i32,
) -> Layout {
    let icon = (slot * dpi / 96).max(1);
    let overflow = (32 * dpi / 96).max(1);
    arrange_physical(preferred, available, icons_only, anchor, icon, overflow)
}
pub fn arrange_physical(
    preferred: &[i32],
    available: i32,
    icons_only: bool,
    anchor: usize,
    icon: i32,
    overflow: i32,
) -> Layout {
    let n = preferred.len();
    let available = available.max(0);
    let all = |mode, widths| Layout {
        mode,
        visible: 0..n,
        leading: 0..0,
        trailing: n..n,
        widths,
        overflow_width: overflow,
    };
    if !icons_only && preferred.iter().map(|w| i64::from(*w)).sum::<i64>() <= i64::from(available) {
        return all(Mode::Labels, preferred.to_vec());
    }
    if n as i64 * i64::from(icon) <= i64::from(available) {
        return all(Mode::Icons, vec![icon; n]);
    }
    // Even an extremely narrow screen has one menu through which all tasks remain reachable.
    if available < icon + overflow * 2 {
        return Layout {
            mode: Mode::Overflow,
            visible: 0..0,
            leading: 0..0,
            trailing: 0..n,
            widths: vec![],
            overflow_width: available.min(overflow),
        };
    }
    let anchor = anchor.min(n.saturating_sub(1));
    let (mut start, mut end) = (anchor, anchor + 1);
    let fits = |start: usize, end: usize| {
        (end - start) as i64 * i64::from(icon)
            + if start > 0 { i64::from(overflow) } else { 0 }
            + if end < n { i64::from(overflow) } else { 0 }
            <= i64::from(available)
    };
    loop {
        let left = start > 0 && fits(start - 1, end);
        let right = end < n && fits(start, end + 1);
        match (left, right) {
            (true, true) if anchor - start < end - anchor => start -= 1,
            (true, true) => end += 1,
            (true, false) => start -= 1,
            (false, true) => end += 1,
            (false, false) => break,
        }
    }
    Layout {
        mode: Mode::Overflow,
        visible: start..end,
        leading: 0..start,
        trailing: end..n,
        widths: vec![icon; end - start],
        overflow_width: overflow,
    }
}

pub fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from != to && from < items.len() && to < items.len() {
        let item = items.remove(from);
        items.insert(to, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_size_fits_more_icons_without_changing_overflow_order() {
        let normal = arrange_with_slot(&[180; 10], 400, 96, true, 5, 48);
        let small = arrange_with_slot(&[180; 10], 400, 96, true, 5, 36);
        assert_eq!(normal.mode, Mode::Overflow);
        assert_eq!(small.mode, Mode::Icons);
        assert_eq!(small.visible, 0..10);
        assert!(small.widths.iter().all(|width| *width == 36));
    }
    #[test]
    fn three_modes_and_preference() {
        let widths = vec![160; 8];
        assert_eq!(arrange(&widths, 1280, 96, false, 3).mode, Mode::Labels);
        assert_eq!(arrange(&widths, 500, 96, false, 3).mode, Mode::Icons);
        assert_eq!(arrange(&widths, 250, 96, false, 3).mode, Mode::Overflow);
        assert_eq!(arrange(&widths, 1280, 96, true, 3).mode, Mode::Icons);
    }
    #[test]
    fn both_sides_collapse_around_active_without_losing_items() {
        let layout = arrange(&[160; 12], 260, 96, false, 6);
        assert!(layout.visible.contains(&6));
        assert!(!layout.leading.is_empty() && !layout.trailing.is_empty());
        assert_eq!(
            layout
                .leading
                .chain(layout.visible)
                .chain(layout.trailing)
                .collect::<Vec<_>>(),
            (0..12).collect::<Vec<_>>()
        );
    }
    #[test]
    fn ends_and_high_dpi_fit() {
        for dpi in [96, 120, 144, 192] {
            for anchor in 0..30 {
                let available = 400 * dpi / 96;
                let layout = arrange(&[300; 30], available, dpi, true, anchor);
                assert!(layout.visible.contains(&anchor));
                let used = layout.widths.iter().sum::<i32>()
                    + i32::from(!layout.leading.is_empty()) * layout.overflow_width
                    + i32::from(!layout.trailing.is_empty()) * layout.overflow_width;
                assert!(used <= available);
            }
        }
    }
    #[test]
    fn narrow_empty_and_large_lists_remain_accessible() {
        assert!(arrange(&[], 0, 96, false, 99).visible.is_empty());
        let layout = arrange(&[100; 100], 35, 96, true, 50);
        assert_eq!(layout.trailing, 0..100);
        assert!(layout.overflow_width <= 35);
        let layout = arrange(&[100; 100], 600, 96, true, 90);
        assert!(layout.visible.contains(&90));
        assert_eq!(
            layout.leading.len() + layout.visible.len() + layout.trailing.len(),
            100
        );
    }
    #[test]
    fn reordering_preserves_identity_and_linear_order() {
        let mut ids = vec![1, 2, 3, 4, 5];
        move_item(&mut ids, 0, 3);
        assert_eq!(ids, [2, 3, 4, 1, 5]);
        move_item(&mut ids, 4, 0);
        assert_eq!(ids, [5, 2, 3, 4, 1]);
        move_item(&mut ids, 80, 0);
        assert_eq!(ids, [5, 2, 3, 4, 1]);
    }
}
