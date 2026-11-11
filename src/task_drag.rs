//! GPU reorder previews. Neighbor motion runs in DWM; the application has no animation frame timer.
use crate::{
    usage_meter::compositor::Compositor,
    visual::{Surface, theme},
};
use anyhow::Result;
use std::time::Instant;
use windows::Win32::Graphics::{
    Direct2D::Common::D2D_RECT_F, DirectComposition::IDCompositionVisual,
};
use windows_sys::Win32::Foundation::{HWND, RECT};
pub const SETTLE_MS: u32 = 180;
const DURATION: f64 = 0.14;
#[derive(Clone, Copy, Debug)]
struct Motion {
    from: f64,
    to: f64,
    start: Instant,
}
impl Motion {
    fn at(self, now: Instant) -> f64 {
        let t = (now.saturating_duration_since(self.start).as_secs_f64() / DURATION).clamp(0., 1.);
        self.from + (self.to - self.from) * (1. - (1. - t).powi(3))
    }
    fn still(x: f64, now: Instant) -> Self {
        Self {
            from: x,
            to: x,
            start: now,
        }
    }
}
pub struct Shot {
    id: usize,
    rect: RECT,
    pixels: Vec<u32>,
    hash: u64,
}
pub unsafe fn capture(
    surface: &Surface,
    width: i32,
    rectangles: &[(usize, RECT)],
    glass: bool,
) -> Vec<Shot> {
    unsafe {
        let pixels = surface.straight_pixels(glass);
        let background = theme::colorref(theme::palette().background);
        rectangles
            .iter()
            .filter(|(_, r)| r.right > r.left && r.bottom > r.top)
            .map(|(id, rect)| {
                let mut image = Vec::with_capacity(
                    ((rect.right - rect.left) * (rect.bottom - rect.top)) as usize,
                );
                let mut hash = 0xcbf29ce484222325u64;
                for y in rect.top..rect.bottom {
                    for x in rect.left..rect.right {
                        let pixel = pixels.get((y * width + x) as usize).copied().unwrap_or(0);
                        let pixel = if pixel & 0xffffff == background {
                            0
                        } else {
                            pixel
                        };
                        hash ^= pixel as u64;
                        hash = hash.wrapping_mul(0x100000001b3);
                        image.push(pixel);
                    }
                }
                Shot {
                    id: *id,
                    rect: *rect,
                    pixels: image,
                    hash,
                }
            })
            .collect()
    }
}
struct Node {
    id: usize,
    visual: IDCompositionVisual,
    motion: Motion,
    rect: RECT,
    hash: u64,
}
pub struct Layer {
    gpu: Compositor,
    nodes: Vec<Node>,
    dragged: Option<usize>,
    bounds: (i32, i32),
    pub commits: u64,
    seed: Vec<(usize, i32)>,
}
impl Layer {
    pub unsafe fn new(hwnd: HWND, seed: Vec<(usize, i32)>) -> Result<Self> {
        unsafe {
            Ok(Self {
                gpu: Compositor::new_layer(hwnd, false)?,
                nodes: vec![],
                dragged: None,
                bounds: (0, 0),
                commits: 0,
                seed,
            })
        }
    }
    unsafe fn move_node(
        gpu: &Compositor,
        node: &mut Node,
        target: f64,
        snap: bool,
        now: Instant,
    ) -> Result<()> {
        unsafe {
            if !snap && (node.motion.to - target).abs() < 0.01 {
                return Ok(());
            }
            if snap {
                node.visual.SetOffsetX2(target as f32)?;
                node.motion = Motion::still(target, now);
                return Ok(());
            }
            let from = node.motion.at(now);
            let delta = target - from;
            let animation = gpu.device.CreateAnimation()?;
            animation.AddCubic(
                0.,
                from as f32,
                (3. * delta / DURATION) as f32,
                (-3. * delta / DURATION.powi(2)) as f32,
                (delta / DURATION.powi(3)) as f32,
            )?;
            animation.End(DURATION, target as f32)?;
            node.visual.SetOffsetX(&animation)?;
            node.motion = Motion {
                from,
                to: target,
                start: now,
            };
            Ok(())
        }
    }
    pub unsafe fn sync(
        &mut self,
        shots: Vec<Shot>,
        dragged: Option<(usize, i32)>,
        height: i32,
    ) -> Result<()> {
        unsafe {
            let now = Instant::now();
            self.dragged = dragged.map(|p| p.0);
            self.bounds = (
                shots.iter().map(|s| s.rect.left).min().unwrap_or(0),
                shots.iter().map(|s| s.rect.right).max().unwrap_or(0),
            );
            self.gpu.root.SetClip2(&D2D_RECT_F {
                left: self.bounds.0 as f32,
                top: 0.,
                right: self.bounds.1 as f32,
                bottom: height as f32,
            })?;
            for i in (0..self.nodes.len()).rev() {
                if !shots.iter().any(|s| s.id == self.nodes[i].id) {
                    let node = self.nodes.remove(i);
                    self.gpu.root.RemoveVisual(&node.visual)?;
                }
            }
            for shot in shots {
                let w = shot.rect.right - shot.rect.left;
                let h = shot.rect.bottom - shot.rect.top;
                let target = if let Some((id, left)) = dragged
                    && id == shot.id
                {
                    left.clamp(self.bounds.0, (self.bounds.1 - w).max(self.bounds.0)) as f64
                } else {
                    shot.rect.left as f64
                };
                let index = if let Some(i) = self.nodes.iter().position(|n| n.id == shot.id) {
                    if self.nodes[i].hash != shot.hash
                        || self.nodes[i].rect.right - self.nodes[i].rect.left != w
                        || self.nodes[i].rect.bottom - self.nodes[i].rect.top != h
                    {
                        let visual = self.gpu.image(w, h, &shot.pixels)?;
                        let current = self.nodes[i].motion.at(now);
                        visual.SetOffsetX2(current as f32)?;
                        self.gpu.root.RemoveVisual(&self.nodes[i].visual)?;
                        self.gpu.root.AddVisual(&visual, false, None)?;
                        self.nodes[i].visual = visual;
                        self.nodes[i].hash = shot.hash;
                        self.nodes[i].motion = Motion::still(current, now);
                    }
                    self.nodes[i].rect = shot.rect;
                    i
                } else {
                    let visual = self.gpu.image(w, h, &shot.pixels)?;
                    let initial = self
                        .seed
                        .iter()
                        .find(|(id, _)| *id == shot.id)
                        .map(|(_, left)| *left as f64)
                        .unwrap_or(target);
                    visual.SetOffsetX2(initial as f32)?;
                    self.gpu.root.AddVisual(&visual, false, None)?;
                    self.nodes.push(Node {
                        id: shot.id,
                        visual,
                        motion: Motion::still(initial, now),
                        rect: shot.rect,
                        hash: shot.hash,
                    });
                    self.nodes.len() - 1
                };
                let node = &mut self.nodes[index];
                node.visual.SetOffsetY2(shot.rect.top as f32)?;
                Self::move_node(&self.gpu, node, target, self.dragged == Some(node.id), now)?;
            }
            if let Some(id) = self.dragged
                && let Some(node) = self.nodes.iter().find(|n| n.id == id)
            {
                self.gpu.root.RemoveVisual(&node.visual)?;
                self.gpu.root.AddVisual(&node.visual, false, None)?;
            }
            self.gpu.device.Commit()?;
            self.commits += 1;
            Ok(())
        }
    }
    pub unsafe fn follow(&mut self, id: usize, left: i32) -> Result<()> {
        unsafe {
            if let Some(node) = self.nodes.iter_mut().find(|n| n.id == id) {
                let width = node.rect.right - node.rect.left;
                let left = left.clamp(self.bounds.0, (self.bounds.1 - width).max(self.bounds.0));
                if (node.motion.to - left as f64).abs() > 0.1 {
                    Self::move_node(&self.gpu, node, left as f64, true, Instant::now())?;
                    self.gpu.device.Commit()?;
                    self.commits += 1;
                }
            }
            Ok(())
        }
    }
    pub fn moving_neighbours(&self, dragged: usize) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.id != dragged && (n.motion.from - n.motion.to).abs() > 0.1)
            .count()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retargeting_preserves_the_current_position_and_finishes_exactly() {
        let now = Instant::now();
        let first = Motion {
            from: 100.,
            to: 0.,
            start: now,
        };
        let midway = now + std::time::Duration::from_millis(60);
        let second = Motion {
            from: first.at(midway),
            to: 180.,
            start: midway,
        };
        assert_eq!(first.at(midway), second.at(midway));
        assert!(second.from > 0. && second.from < 100.);
        assert_eq!(
            second.at(midway + std::time::Duration::from_millis(140)),
            180.
        );
    }
}
