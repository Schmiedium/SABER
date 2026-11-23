//! The Launcher (`sim_studio_ux.md` §7.1): recent workspaces, starting a new one from a domain
//! pack, recent campaigns, and opening a workspace, crash dump or campaign folder by path.
//!
//! Paths are typed (or pasted) into a field: a native file dialog is a later addition.

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::scene::{CommandsSceneExt, bsn, bsn_list};
use sim_studio_ui::prelude::*;
use sim_studio_ui::widgets::{Fields, KitActivated};

use super::{action, child};
use crate::actions::{LauncherCampaigns, Recent, StudioCommand};
use crate::clock;
use crate::state::AppState;
use crate::workspace::Pack;

/// Field ids on this screen.
pub mod field {
    /// Where a new workspace goes (its parent folder).
    pub const LOCATION: &str = "launcher.location";
    /// The new workspace's name.
    pub const NAME: &str = "launcher.name";
    /// A path to open.
    pub const OPEN: &str = "launcher.open";
}

/// The Launcher's root node.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct LauncherRoot;

fn default_location() -> String {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("saber").display().to_string())
        .unwrap_or_else(|| "saber".to_owned())
}

fn screen() -> impl bevy::scene::Scene {
    bsn! {
        Node {
            width: percent(100),
            height: percent(100),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: px(16),
            padding: px(32),
            overflow: Overflow::scroll_y(),
        }
        Background(token::BG_APP)
        LauncherRoot
    }
}

fn grow_panel() -> impl bevy::scene::Scene {
    bsn! {
        @panel()
        Node {
            flex_grow: 1.0,
            flex_basis: px(0),
            min_width: px(320),
        }
    }
}

/// Build the Launcher.
pub fn spawn(mut commands: Commands, recent: Res<Recent>, campaigns: Res<LauncherCampaigns>) {
    let now = clock::unix_now();
    let root = commands
        .spawn_scene(screen())
        .insert(DespawnOnExit(AppState::Launcher))
        .id();

    let header = child(&mut commands, root, column());
    child(&mut commands, header, title("SABER"));
    child(
        &mut commands,
        header,
        muted(
            "System analysis by physics simulation: model a system, simulate it, check it against its requirements.",
        ),
    );
    child(&mut commands, header, muted(sim_schema::build_id()));

    let top = child(&mut commands, root, row());
    commands.entity(top).insert(Node {
        display: Display::Flex,
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Stretch,
        column_gap: px(16),
        flex_wrap: FlexWrap::Wrap,
        ..default()
    });

    // Recent workspaces.
    let recent_panel = child(&mut commands, top, grow_panel());
    child(&mut commands, recent_panel, heading("Recent workspaces"));
    if recent.list.workspaces.is_empty() {
        child(
            &mut commands,
            recent_panel,
            muted("None yet. Start a new workspace, or open one by its folder below."),
        );
    }
    for item in &recent.list.workspaces {
        let a11y = format!("Open workspace {}", item.name);
        let line = format!(
            "{}   {}   {}",
            item.name,
            clock::ago(now, item.opened_unix_s),
            item.path.display()
        );
        action(
            &mut commands,
            recent_panel,
            plain_row(a11y, bsn_list! { @label(line) }),
            StudioCommand::OpenWorkspace(item.path.clone()),
        );
    }

    // Start new.
    let new_panel = child(&mut commands, top, grow_panel());
    child(&mut commands, new_panel, heading("Start new"));
    let name_row = child(&mut commands, new_panel, row());
    child(&mut commands, name_row, label("Name"));
    child(
        &mut commands,
        name_row,
        text_field(field::NAME, "my_workspace", "New workspace name"),
    );
    let loc_row = child(&mut commands, new_panel, row());
    child(&mut commands, loc_row, label("In"));
    child(
        &mut commands,
        loc_row,
        text_field(
            field::LOCATION,
            default_location(),
            "Folder the new workspace goes in",
        ),
    );
    for pack in Pack::ALL {
        let r = child(&mut commands, new_panel, row());
        commands
            .spawn_scene(button(pack.label()))
            .insert(ChildOf(r))
            .observe(
                move |_: On<KitActivated>,
                      fields: Fields,
                      mut out: MessageWriter<StudioCommand>| {
                    let name = fields.get(field::NAME).unwrap_or_default();
                    let location = fields.get(field::LOCATION).unwrap_or_default();
                    out.write(StudioCommand::NewWorkspace {
                        root: PathBuf::from(location).join(&name),
                        name,
                        pack,
                    });
                },
            );
        child(&mut commands, r, muted(pack.blurb()));
    }

    // Recent campaigns.
    let camp_panel = child(&mut commands, root, panel());
    child(&mut commands, camp_panel, heading("Recent campaigns"));
    if campaigns.0.is_empty() {
        child(
            &mut commands,
            camp_panel,
            muted("None yet. Open a campaign's output folder below."),
        );
    }
    for card in &campaigns.0 {
        match card {
            Ok(c) => {
                let (status, words) = c.status();
                let line = format!(
                    "{}   {}",
                    c.id,
                    if c.crashes == 1 {
                        "1 crash".to_owned()
                    } else {
                        format!("{} crashes", c.crashes)
                    }
                );
                let path = c.dir.display().to_string();
                action(
                    &mut commands,
                    camp_panel,
                    plain_row(
                        format!("Open campaign {}: {words}", c.id),
                        bsn_list! {
                            @label(line)
                            --
                            @status_text(status, words.clone())
                            --
                            @muted(path)
                        },
                    ),
                    StudioCommand::OpenCampaign(c.dir.clone()),
                );
            }
            Err((dir, e)) => {
                let r = child(&mut commands, camp_panel, row());
                child(
                    &mut commands,
                    r,
                    status_text(Status::Warning, dir.display().to_string()),
                );
                child(&mut commands, r, muted(e.clone()));
            }
        }
    }

    // Open by path.
    let open_panel = child(&mut commands, root, panel());
    child(&mut commands, open_panel, heading("Open"));
    let open_row = child(&mut commands, open_panel, row());
    child(
        &mut commands,
        open_row,
        text_field(
            field::OPEN,
            "",
            "Path to a workspace folder, a crash dump (.zst) or a campaign output folder",
        ),
    );
    let buttons = child(&mut commands, open_panel, row());
    type Make = fn(PathBuf) -> StudioCommand;
    let opens: [(&str, Make); 3] = [
        ("Open workspace", StudioCommand::OpenWorkspace),
        ("Open crash dump", StudioCommand::OpenCrash),
        ("Open campaign folder", StudioCommand::OpenCampaign),
    ];
    for (text, make) in opens {
        commands
            .spawn_scene(button(text))
            .insert(ChildOf(buttons))
            .observe(
                move |_: On<KitActivated>,
                      fields: Fields,
                      mut out: MessageWriter<StudioCommand>,
                      mut toasts: MessageWriter<Toast>| {
                    match fields.get(field::OPEN).filter(|p| !p.is_empty()) {
                        Some(p) => {
                            out.write(make(PathBuf::from(p)));
                        }
                        None => {
                            toasts.write(Toast::warning("Type or paste a path first"));
                        }
                    }
                },
            );
    }
}
