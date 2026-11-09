//! Shadows of tiled windows. Neovim marks a window as a panel with the `neovide.window_shadow`
//! notification, and the panel then casts the floating window shadow to one side, over the
//! windows next to it.

use skia_safe::Canvas;
use skia_safe::Color4f;
use skia_safe::Paint;
use skia_safe::Path;
use skia_safe::Point3;
use skia_safe::RRect;
use skia_safe::Rect;

use super::RendererSettings;
use super::rendered_layer::cast_shadow;
use crate::units::GridScale;
use crate::units::PixelPos;
use crate::units::PixelRect;
use crate::units::to_skia_rect;

/// The side of a panel where its shadow falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelShadow {
    Left,
    Right,
    Top,
    Bottom,
}

impl PanelShadow {
    pub fn parse(side: &str) -> Option<Self> {
        match side {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            _ => None,
        }
    }

    /// The direction from the panel to the light, which is on the other side of the shadow.
    /// `angle` is the angle between the light and the screen normal, in radians.
    fn light(self, angle: f32) -> Point3 {
        let (sin, cos) = angle.sin_cos();
        match self {
            Self::Left => Point3::new(sin, 0., cos),
            Self::Right => Point3::new(-sin, 0., cos),
            Self::Top => Point3::new(0., sin, cos),
            Self::Bottom => Point3::new(0., -sin, cos),
        }
    }

    fn is_horizontal(self) -> bool { matches!(self, Self::Left | Self::Right) }
}

/// Joins the panels that cast a shadow to the same side and are at most one cell apart, such as
/// two panels with a status line or a separator between them. Each group casts one shadow from
/// its bounding box, so that no shadow falls into the gaps between its panels.
pub fn group_panels(
    panels: &[(PixelRect<f32>, PanelShadow)],
    grid_scale: GridScale,
) -> Vec<(PixelRect<f32>, PanelShadow)> {
    let gap_x = grid_scale.width() + 1.0;
    let gap_y = grid_scale.height() + 1.0;
    let near = |a: PixelRect<f32>, b: PixelRect<f32>| {
        a.min.x <= b.max.x + gap_x
            && b.min.x <= a.max.x + gap_x
            && a.min.y <= b.max.y + gap_y
            && b.min.y <= a.max.y + gap_y
    };

    let mut groups: Vec<(PixelRect<f32>, PanelShadow)> = Vec::new();
    for &(mut region, side) in panels {
        while let Some(index) =
            groups.iter().position(|&(other, other_side)| other_side == side && near(region, other))
        {
            let (other, _) = groups.swap_remove(index);
            region = PixelRect::new(
                PixelPos::new(region.min.x.min(other.min.x), region.min.y.min(other.min.y)),
                PixelPos::new(region.max.x.max(other.max.x), region.max.y.max(other.max.y)),
            );
        }
        groups.push((region, side));
    }
    groups
}

/// The rect that casts the shadow of a group. An end of the group that is less than one cell
/// from the edge of the content goes on past that edge, so that the shadow does not fade out
/// before the edge.
fn caster_rect(
    region: PixelRect<f32>,
    side: PanelShadow,
    content: PixelRect<f32>,
    grid_scale: GridScale,
) -> Rect {
    let mut rect = to_skia_rect(&region);
    if side.is_horizontal() {
        let reach = content.size().height;
        if region.min.y - content.min.y < grid_scale.height() {
            rect.top -= reach;
        }
        if content.max.y - region.max.y < grid_scale.height() {
            rect.bottom += reach;
        }
    } else {
        let reach = content.size().width;
        if region.min.x - content.min.x < grid_scale.width() {
            rect.left -= reach;
        }
        if content.max.x - region.max.x < grid_scale.width() {
            rect.right += reach;
        }
    }
    rect
}

/// The part of the content on the shadow side of a group.
fn shadow_area(region: PixelRect<f32>, side: PanelShadow, content: PixelRect<f32>) -> Rect {
    let mut area = to_skia_rect(&content);
    match side {
        PanelShadow::Left => area.right = region.min.x,
        PanelShadow::Right => area.left = region.max.x,
        PanelShadow::Top => area.bottom = region.min.y,
        PanelShadow::Bottom => area.top = region.max.y,
    }
    area
}

/// Adds the cell next to a group on its shadow side: the separator column, or the status line
/// row. The shadow then starts at the next window, and the cell is a margin between the panel
/// text and the shadow. The new edge is on a whole pixel, so that the edge line is sharp.
fn with_separator(
    region: PixelRect<f32>,
    side: PanelShadow,
    grid_scale: GridScale,
) -> PixelRect<f32> {
    let mut region = region;
    match side {
        PanelShadow::Left => region.min.x = (region.min.x - grid_scale.width()).round(),
        PanelShadow::Right => region.max.x = (region.max.x + grid_scale.width()).round(),
        PanelShadow::Top => region.min.y = (region.min.y - grid_scale.height()).round(),
        PanelShadow::Bottom => region.max.y = (region.max.y + grid_scale.height()).round(),
    }
    region
}

/// The line along the shadow side of a group, inside the group, so that the shadow starts
/// where the line ends.
fn edge_line(region: PixelRect<f32>, side: PanelShadow, width: f32) -> Rect {
    let mut line = to_skia_rect(&region);
    match side {
        PanelShadow::Left => line.right = line.left + width,
        PanelShadow::Right => line.left = line.right - width,
        PanelShadow::Top => line.bottom = line.top + width,
        PanelShadow::Bottom => line.top = line.bottom - width,
    }
    line
}

/// Draws the shadows of the panels, and a line where each shadow starts. Call it after the
/// tiled windows and before the floating windows, so that the shadows fall on the tiled windows
/// only. `line_width` is in physical pixels, and 0 draws no line.
pub fn draw_panel_shadows(
    root_canvas: &Canvas,
    panels: &[(PixelRect<f32>, PanelShadow)],
    settings: &RendererSettings,
    grid_scale: GridScale,
    content: PixelRect<f32>,
    line_width: f32,
) {
    let angle = settings.light_angle_degrees.to_radians();
    let opacity = settings.panel_shadow_line_opacity.clamp(0.0, 1.0);
    let line_paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, opacity), None);
    for (region, side) in group_panels(panels, grid_scale) {
        let region = with_separator(region, side, grid_scale);
        let area = shadow_area(region, side, content);
        if area.is_empty() {
            continue;
        }
        let caster = caster_rect(region, side, content, grid_scale);
        root_canvas.save();
        root_canvas.clip_rect(area, None, Some(false));
        cast_shadow(
            root_canvas,
            &Path::rrect(RRect::new_rect(caster), None),
            settings.floating_z_height,
            side.light(angle),
        );
        root_canvas.restore();
        if line_width > 0.0 && opacity > 0.0 {
            root_canvas.draw_rect(edge_line(region, side, line_width), &line_paint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::PixelSize;

    fn grid_scale() -> GridScale { GridScale::new(PixelSize::new(8.0, 16.0)) }

    /// A region in cells of `grid_scale`.
    fn cells(left: f32, top: f32, right: f32, bottom: f32) -> PixelRect<f32> {
        PixelRect::new(
            PixelPos::new(left * 8.0, top * 16.0),
            PixelPos::new(right * 8.0, bottom * 16.0),
        )
    }

    #[test]
    fn panels_with_a_status_line_between_them_cast_one_shadow() {
        let upper = cells(0.0, 0.0, 30.0, 20.0);
        let lower = cells(0.0, 21.0, 30.0, 40.0);
        let groups =
            group_panels(&[(upper, PanelShadow::Right), (lower, PanelShadow::Right)], grid_scale());
        assert_eq!(groups, vec![(cells(0.0, 0.0, 30.0, 40.0), PanelShadow::Right)]);
    }

    #[test]
    fn a_bar_and_a_panel_with_a_separator_between_them_cast_one_shadow() {
        let bar = cells(0.0, 0.0, 3.0, 40.0);
        let panel = cells(4.0, 0.0, 30.0, 20.0);
        let groups =
            group_panels(&[(bar, PanelShadow::Right), (panel, PanelShadow::Right)], grid_scale());
        assert_eq!(groups, vec![(cells(0.0, 0.0, 30.0, 40.0), PanelShadow::Right)]);
    }

    #[test]
    fn a_group_joins_panels_that_only_touch_through_another_panel() {
        let first = cells(0.0, 0.0, 10.0, 10.0);
        let last = cells(0.0, 22.0, 10.0, 30.0);
        let middle = cells(0.0, 11.0, 10.0, 21.0);
        let groups = group_panels(
            &[
                (first, PanelShadow::Right),
                (last, PanelShadow::Right),
                (middle, PanelShadow::Right),
            ],
            grid_scale(),
        );
        assert_eq!(groups, vec![(cells(0.0, 0.0, 10.0, 30.0), PanelShadow::Right)]);
    }

    #[test]
    fn panels_far_apart_or_with_other_sides_cast_their_own_shadows() {
        let left = cells(0.0, 0.0, 30.0, 40.0);
        let right = cells(100.0, 0.0, 130.0, 40.0);
        let next_to_left = cells(31.0, 0.0, 50.0, 40.0);
        let groups = group_panels(
            &[
                (left, PanelShadow::Right),
                (right, PanelShadow::Left),
                (next_to_left, PanelShadow::Left),
            ],
            grid_scale(),
        );
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn a_full_height_panel_casts_past_the_top_and_the_bottom() {
        let content = cells(0.0, 0.0, 130.0, 40.5);
        let rect =
            caster_rect(cells(0.0, 0.0, 30.0, 40.0), PanelShadow::Right, content, grid_scale());
        assert!(rect.top < 0.0);
        assert!(rect.bottom > content.max.y);
        assert_eq!((rect.left, rect.right), (0.0, 240.0));

        let rect =
            caster_rect(cells(0.0, 5.0, 30.0, 20.0), PanelShadow::Right, content, grid_scale());
        assert_eq!((rect.top, rect.bottom), (80.0, 320.0));
    }

    #[test]
    fn the_shadow_starts_after_the_separator() {
        let panel = cells(0.0, 0.0, 30.0, 20.0);
        assert_eq!(
            with_separator(panel, PanelShadow::Right, grid_scale()),
            cells(0.0, 0.0, 31.0, 20.0)
        );
        assert_eq!(
            with_separator(panel, PanelShadow::Top, grid_scale()),
            cells(0.0, -1.0, 30.0, 20.0)
        );
    }

    #[test]
    fn the_separator_edge_is_on_a_whole_pixel() {
        let scale = GridScale::new(PixelSize::new(9.6, 24.0));
        let panel = PixelRect::new(PixelPos::new(0.0, 0.0), PixelPos::new(288.0, 480.0));
        assert_eq!(with_separator(panel, PanelShadow::Right, scale).max.x, 298.0);
    }

    #[test]
    fn the_edge_line_ends_where_the_shadow_starts() {
        let content = cells(0.0, 0.0, 130.0, 40.0);
        let region = cells(0.0, 0.0, 31.0, 20.0);
        let line = edge_line(region, PanelShadow::Right, 1.0);
        assert_eq!((line.left, line.right, line.top, line.bottom), (247.0, 248.0, 0.0, 320.0));
        assert_eq!(line.right, shadow_area(region, PanelShadow::Right, content).left);

        let region = cells(0.0, 21.0, 130.0, 40.0);
        let line = edge_line(region, PanelShadow::Top, 1.0);
        assert_eq!((line.top, line.bottom), (336.0, 337.0));
        assert_eq!(line.top, shadow_area(region, PanelShadow::Top, content).bottom);
    }

    #[test]
    fn the_shadow_area_is_on_the_shadow_side_of_the_panel() {
        let content = cells(0.0, 0.0, 130.0, 40.0);
        let panel = cells(100.0, 0.0, 130.0, 40.0);
        let area = shadow_area(panel, PanelShadow::Left, content);
        assert_eq!((area.left, area.right), (0.0, 800.0));
        assert!(shadow_area(panel, PanelShadow::Right, content).is_empty());
    }
}
