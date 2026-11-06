//! Non-modal notifications at the lower right (`sim_studio_ux.md` §6): each [`Toast`] shows for
//! a few seconds and stays in the [`ToastLog`], which the Console tab lists.

use std::time::Duration;

use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::prelude::*;

use crate::theme::{Token, token};
use crate::widgets::{label, mark};

/// How a notification reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Reflect)]
pub enum ToastLevel {
    /// Something happened.
    Info,
    /// Something finished well.
    Success,
    /// Worth a look.
    Warning,
    /// Something failed.
    Error,
}

impl ToastLevel {
    /// The mark shown with it, so the level never rests on color alone (§10.1).
    pub fn mark(self) -> &'static str {
        match self {
            ToastLevel::Info => "ℹ",
            ToastLevel::Success => "✓",
            ToastLevel::Warning => "⚠",
            ToastLevel::Error => "✖",
        }
    }

    /// Its color token.
    pub fn token(self) -> Token {
        match self {
            ToastLevel::Info => token::ACCENT,
            ToastLevel::Success => token::STATUS_PASS,
            ToastLevel::Warning => token::STATUS_WARN,
            ToastLevel::Error => token::STATUS_FAIL,
        }
    }

    /// How long it stays on screen: errors stay longer.
    pub fn lifetime(self) -> Duration {
        match self {
            ToastLevel::Error => Duration::from_secs(10),
            ToastLevel::Warning => Duration::from_secs(7),
            _ => Duration::from_secs(4),
        }
    }
}

/// Show a notification.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct Toast {
    /// How it reads.
    pub level: ToastLevel,
    /// One sentence.
    pub text: String,
}

impl Toast {
    /// An informational notice.
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Info,
            text: text.into(),
        }
    }
    /// A success notice.
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Success,
            text: text.into(),
        }
    }
    /// A warning.
    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Warning,
            text: text.into(),
        }
    }
    /// An error.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            level: ToastLevel::Error,
            text: text.into(),
        }
    }
}

/// Every notification of the session, oldest first (the Console tab's history), capped.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct ToastLog {
    /// The notices, with the app time they were shown.
    pub entries: Vec<(Duration, Toast)>,
}

impl ToastLog {
    /// Notices kept.
    pub const CAP: usize = 500;

    fn push(&mut self, at: Duration, toast: Toast) {
        if self.entries.len() == Self::CAP {
            self.entries.remove(0);
        }
        self.entries.push((at, toast));
    }
}

/// The lower-right column the toasts stack in.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ToastStack;

/// One toast on screen, with the time it goes away.
#[derive(Component, Clone, Copy, Debug)]
pub struct ToastNode {
    /// App time to despawn it.
    pub until: Duration,
}

/// Toasts on screen at once; older ones go first.
pub const MAX_ON_SCREEN: usize = 4;

/// Log every toast, and show it when a [`ToastStack`] exists (a headless app only logs).
pub(crate) fn show_toasts(
    mut toasts: MessageReader<Toast>,
    time: Res<Time<Real>>,
    mut log: ResMut<ToastLog>,
    stack: Query<(Entity, Option<&Children>), With<ToastStack>>,
    nodes: Query<(), With<ToastNode>>,
    mut commands: Commands,
) {
    let now = time.elapsed();
    let stack = stack.single().ok();
    let mut on_screen = stack
        .and_then(|(_, c)| c)
        .map(|c| c.iter().filter(|e| nodes.contains(*e)).collect::<Vec<_>>())
        .unwrap_or_default();
    for toast in toasts.read() {
        log.push(now, toast.clone());
        let Some((stack, _)) = stack else {
            continue;
        };
        while on_screen.len() >= MAX_ON_SCREEN {
            commands.entity(on_screen.remove(0)).despawn();
        }
        let until = now + toast.level.lifetime();
        let id = commands
            .spawn_scene(toast_card(toast.clone()))
            .insert((ToastNode { until }, ChildOf(stack)))
            .id();
        on_screen.push(id);
    }
}

pub(crate) fn expire_toasts(
    time: Res<Time<Real>>,
    nodes: Query<(Entity, &ToastNode)>,
    mut commands: Commands,
) {
    let now = time.elapsed();
    for (e, n) in &nodes {
        if now >= n.until {
            commands.entity(e).despawn();
        }
    }
}

fn toast_card(toast: Toast) -> impl Scene {
    let level = toast.level;
    bsn! {
        Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(8),
            padding: px(10),
            max_width: px(420),
            border_radius: {BorderRadius::all(px(6))},
            border: px(1),
        }
        ThemeBackgroundColor(token::BG_PANEL)
        BorderColor::all(Color::NONE)
        Children [
            @mark(level.mark(), level.token())
            --
            @label(toast.text.clone())
        ]
    }
}

/// The toast column, spawned once over the whole window.
pub fn toast_stack() -> impl Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            right: px(16),
            bottom: px(40),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: px(8),
            align_items: AlignItems::End,
        }
        GlobalZIndex(100)
        ToastStack
    }
}
