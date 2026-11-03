//! `StudioDock` v1 (`sim_studio_ux.md` §6, §6.1.5): four fixed regions (left, center, right,
//! bottom) separated by [`Splitter`]s. Drag a splitter to resize its region, double-click it to
//! collapse or restore the region. Sizes live in [`DockLayout`], which the studio saves in
//! `saber.workspace.ron`; free drag-and-drop docking comes later.

use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::picking::cursor::EntityCursor;
use bevy::picking::events::{PointerClick, PointerDrag, PointerDragEnd, PointerDragStart};
use bevy::picking::hover::Hovered;
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, SystemCursorIcon};
use serde::{Deserialize, Serialize};

use crate::theme::token;

/// Where a panel sits in the dock.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Reflect)]
#[reflect(Component, Default)]
pub enum DockRegion {
    /// Outliner, Library, Parameters.
    Left,
    /// The viewport or plot canvas.
    #[default]
    Center,
    /// Inspector, Properties.
    Right,
    /// Timeline, Console, Lint, Requirements, Jobs.
    Bottom,
}

/// A side region that has a size and can collapse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
pub enum DockSide {
    /// The left region (its width).
    #[default]
    Left,
    /// The right region (its width).
    Right,
    /// The bottom region (its height).
    Bottom,
}

impl DockSide {
    /// All three sides.
    pub const ALL: [DockSide; 3] = [DockSide::Left, DockSide::Right, DockSide::Bottom];

    /// The region this side sizes.
    pub fn region(self) -> DockRegion {
        match self {
            DockSide::Left => DockRegion::Left,
            DockSide::Right => DockRegion::Right,
            DockSide::Bottom => DockRegion::Bottom,
        }
    }
}

/// Sizes of the three side regions, logical pixels, and which are collapsed. Saved per workspace.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Reflect, Serialize, Deserialize)]
#[reflect(Resource)]
#[serde(default, deny_unknown_fields)]
pub struct DockLayout {
    /// Left region width.
    pub left: f32,
    /// Right region width.
    pub right: f32,
    /// Bottom region height.
    pub bottom: f32,
    /// Left region hidden.
    pub left_collapsed: bool,
    /// Right region hidden.
    pub right_collapsed: bool,
    /// Bottom region hidden.
    pub bottom_collapsed: bool,
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            left: 280.0,
            right: 320.0,
            bottom: 200.0,
            left_collapsed: false,
            right_collapsed: false,
            bottom_collapsed: false,
        }
    }
}

impl DockLayout {
    /// Smallest size of a side region that is shown, px.
    pub const MIN: f32 = 120.0;
    /// Smallest center region, px, so the viewport never vanishes.
    pub const MIN_CENTER: f32 = 200.0;
    /// Splitter thickness, px.
    pub const SPLITTER: f32 = 4.0;

    /// The stored size of a side.
    pub fn size(&self, side: DockSide) -> f32 {
        match side {
            DockSide::Left => self.left,
            DockSide::Right => self.right,
            DockSide::Bottom => self.bottom,
        }
    }

    fn size_mut(&mut self, side: DockSide) -> &mut f32 {
        match side {
            DockSide::Left => &mut self.left,
            DockSide::Right => &mut self.right,
            DockSide::Bottom => &mut self.bottom,
        }
    }

    /// Whether a side is collapsed.
    pub fn collapsed(&self, side: DockSide) -> bool {
        match side {
            DockSide::Left => self.left_collapsed,
            DockSide::Right => self.right_collapsed,
            DockSide::Bottom => self.bottom_collapsed,
        }
    }

    fn collapsed_mut(&mut self, side: DockSide) -> &mut bool {
        match side {
            DockSide::Left => &mut self.left_collapsed,
            DockSide::Right => &mut self.right_collapsed,
            DockSide::Bottom => &mut self.bottom_collapsed,
        }
    }

    /// The size a side is drawn at: 0 when collapsed.
    pub fn shown(&self, side: DockSide) -> f32 {
        if self.collapsed(side) {
            0.0
        } else {
            self.size(side)
        }
    }

    /// Collapse a shown side, or restore a collapsed one at its stored size.
    pub fn toggle(&mut self, side: DockSide) {
        let c = self.collapsed_mut(side);
        *c = !*c;
    }

    /// Move a side's splitter by `delta` px (screen axes: x right, y down) inside a window of
    /// `window` px, keeping every shown region at least [`Self::MIN`] and the center at least
    /// [`Self::MIN_CENTER`]. Dragging a collapsed side's splitter restores it first.
    pub fn drag(&mut self, side: DockSide, delta: Vec2, window: Vec2) {
        if self.collapsed(side) {
            *self.collapsed_mut(side) = false;
        }
        let grow = match side {
            DockSide::Left => delta.x,
            DockSide::Right => -delta.x,
            DockSide::Bottom => -delta.y,
        };
        let max = self.max_size(side, window);
        let s = self.size_mut(side);
        *s = (*s + grow).clamp(Self::MIN, max.max(Self::MIN));
    }

    /// The largest size a side can take in this window.
    pub fn max_size(&self, side: DockSide, window: Vec2) -> f32 {
        let splitters = 2.0 * Self::SPLITTER;
        match side {
            DockSide::Left => window.x - self.shown(DockSide::Right) - Self::MIN_CENTER - splitters,
            DockSide::Right => window.x - self.shown(DockSide::Left) - Self::MIN_CENTER - splitters,
            DockSide::Bottom => window.y - Self::MIN_CENTER - Self::SPLITTER,
        }
    }

    /// Shrink sides that no longer fit after a window resize (never below [`Self::MIN`]).
    pub fn fit(&mut self, window: Vec2) {
        for side in DockSide::ALL {
            let max = self.max_size(side, window).max(Self::MIN);
            let s = self.size_mut(side);
            *s = s.clamp(Self::MIN, max);
        }
    }

    /// Replace sizes that are not finite or below the minimum (a hand-edited file) with defaults.
    pub fn sanitized(mut self) -> Self {
        let d = Self::default();
        for side in DockSide::ALL {
            let s = self.size_mut(side);
            if !s.is_finite() || *s < Self::MIN {
                *s = d.size(side);
            }
        }
        self
    }
}

/// The drag handle between a side region and the center. Drag to resize; double-click to collapse.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
pub struct Splitter(pub DockSide);

/// Sent when the user resized or collapsed a region (not on every drag step: at drag end and on
/// double-click), so the studio can save the layout.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct DockLayoutChanged(pub DockLayout);

/// The dock's root node. Panels are spawned as children of the [`DockRegion`] nodes.
#[derive(Component, Clone, Copy, Debug, Default, Reflect)]
#[reflect(Component, Default)]
pub struct StudioDock;

fn splitter(side: DockSide) -> impl Scene {
    let horizontal = side == DockSide::Bottom;
    let (w, h) = if horizontal {
        (percent(100), px(DockLayout::SPLITTER))
    } else {
        (px(DockLayout::SPLITTER), percent(100))
    };
    let cursor = if horizontal {
        SystemCursorIcon::RowResize
    } else {
        SystemCursorIcon::ColResize
    };
    let label = match side {
        DockSide::Left => "Resize the left panel",
        DockSide::Right => "Resize the right panel",
        DockSide::Bottom => "Resize the bottom panel",
    };
    bsn! {
        Node {
            width: {w},
            height: {h},
            flex_shrink: 0.0,
        }
        Splitter({side})
        Hovered
        EntityCursor::System({cursor})
        ThemeBackgroundColor(token::BORDER)
        AccessibleLabel({label})
    }
}

/// The dock: a row of left, center and right regions above the bottom region. Spawn panels into
/// the regions by querying [`DockRegion`].
pub fn dock() -> impl Scene {
    bsn! {
        Node {
            flex_grow: 1.0,
            min_height: px(0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
        }
        StudioDock
        Children [
            Node {
                flex_grow: 1.0,
                min_height: px(0),
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
            }
            Children [
                Node {
                    width: px(280),
                    flex_shrink: 0.0,
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::clip(),
                }
                DockRegion::Left
                ThemeBackgroundColor(token::BG_PANEL)
                --
                @splitter(DockSide::Left)
                --
                Node {
                    flex_grow: 1.0,
                    min_width: px(0),
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::clip(),
                }
                DockRegion::Center
                ThemeBackgroundColor(token::BG_APP)
                --
                @splitter(DockSide::Right)
                --
                Node {
                    width: px(320),
                    flex_shrink: 0.0,
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::clip(),
                }
                DockRegion::Right
                ThemeBackgroundColor(token::BG_PANEL)
            ]
            --
            @splitter(DockSide::Bottom)
            --
            Node {
                height: px(200),
                flex_shrink: 0.0,
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                overflow: Overflow::clip(),
            }
            DockRegion::Bottom
            ThemeBackgroundColor(token::BG_PANEL)
        ]
    }
}

/// Size the regions from [`DockLayout`] whenever it changes (or a dock is spawned).
pub(crate) fn apply_layout(
    layout: Res<DockLayout>,
    added: Query<(), Added<StudioDock>>,
    mut regions: Query<(&DockRegion, &mut Node)>,
) {
    if !layout.is_changed() && added.is_empty() {
        return;
    }
    for (region, mut node) in &mut regions {
        let side = match region {
            DockRegion::Left => DockSide::Left,
            DockRegion::Right => DockSide::Right,
            DockRegion::Bottom => DockSide::Bottom,
            DockRegion::Center => continue,
        };
        let size = px(layout.size(side));
        let display = if layout.collapsed(side) {
            Display::None
        } else {
            Display::Flex
        };
        if side == DockSide::Bottom {
            node.height = size;
        } else {
            node.width = size;
        }
        node.display = display;
    }
}

/// Keep the layout inside the window when it shrinks.
pub(crate) fn fit_layout(
    windows: Query<&Window, (With<PrimaryWindow>, Changed<Window>)>,
    mut layout: ResMut<DockLayout>,
) {
    if let Ok(w) = windows.single() {
        let size = Vec2::new(w.width(), w.height());
        let mut fitted = *layout;
        fitted.fit(size);
        if fitted != *layout {
            *layout = fitted;
        }
    }
}

pub(crate) fn on_splitter_drag_start(
    mut drag: On<PointerDragStart>,
    splitters: Query<(), With<Splitter>>,
) {
    if splitters.contains(drag.entity) {
        drag.propagate(false);
    }
}

pub(crate) fn on_splitter_drag(
    mut drag: On<PointerDrag>,
    splitters: Query<&Splitter>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut layout: ResMut<DockLayout>,
) {
    let Ok(Splitter(side)) = splitters.get(drag.entity) else {
        return;
    };
    drag.propagate(false);
    if drag.button != PointerButton::Primary {
        return;
    }
    let window = windows
        .single()
        .map(|w| Vec2::new(w.width(), w.height()))
        .unwrap_or(Vec2::splat(f32::INFINITY));
    layout.drag(*side, drag.delta, window);
}

pub(crate) fn on_splitter_drag_end(
    mut drag: On<PointerDragEnd>,
    splitters: Query<(), With<Splitter>>,
    layout: Res<DockLayout>,
    mut changed: MessageWriter<DockLayoutChanged>,
) {
    if splitters.contains(drag.entity) {
        drag.propagate(false);
        changed.write(DockLayoutChanged(*layout));
    }
}

pub(crate) fn on_splitter_click(
    mut click: On<PointerClick>,
    splitters: Query<&Splitter>,
    mut layout: ResMut<DockLayout>,
    mut changed: MessageWriter<DockLayoutChanged>,
) {
    let Ok(Splitter(side)) = splitters.get(click.entity) else {
        return;
    };
    click.propagate(false);
    if click.button == PointerButton::Primary && click.count == 2 {
        layout.toggle(*side);
        changed.write(DockLayoutChanged(*layout));
    }
}
