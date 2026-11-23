//! The Launcher's recent workspaces and campaigns (`sim_studio_ux.md` §7.1), kept per user in
//! `~/.saber/studio/recent.ron` (`$SABER_HOME` replaces `~/.saber`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sim_schema::{FileHeader, Semver};

/// `FileHeader::format` of the recent list.
pub const RECENT_FORMAT: &str = "studio-recent";
/// Its schema version.
pub const RECENT_SCHEMA_VERSION: Semver = Semver::new(1, 0, 0);
/// Entries kept per list.
pub const RECENT_CAP: usize = 12;

/// The per-user SABER folder: `$SABER_HOME`, else `~/.saber`.
pub fn saber_home() -> Option<PathBuf> {
    std::env::var_os("SABER_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".saber")))
}

/// Where the recent list lives by default.
pub fn default_recent_path() -> Option<PathBuf> {
    saber_home().map(|h| h.join("studio").join("recent.ron"))
}

/// A workspace or campaign the user opened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentItem {
    /// Its folder.
    pub path: PathBuf,
    /// Its name (workspace name or campaign id).
    pub name: String,
    /// When it was last opened, Unix seconds.
    pub opened_unix_s: u64,
}

/// The recent list: most recent first, one entry per folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentList {
    /// Format `studio-recent`.
    pub header: FileHeader,
    /// Workspaces.
    #[serde(default)]
    pub workspaces: Vec<RecentItem>,
    /// Campaign output folders.
    #[serde(default)]
    pub campaigns: Vec<RecentItem>,
}

impl Default for RecentList {
    fn default() -> Self {
        Self {
            header: FileHeader::new(
                RECENT_FORMAT,
                RECENT_SCHEMA_VERSION,
                sim_schema::SABER_VERSION,
            ),
            workspaces: Vec::new(),
            campaigns: Vec::new(),
        }
    }
}

/// Paths compare by their canonical form when the folder exists, so `a/../b` and `b` are one entry.
fn same_place(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn touch(list: &mut Vec<RecentItem>, path: &Path, name: &str, now: u64) {
    list.retain(|i| !same_place(&i.path, path));
    list.insert(
        0,
        RecentItem {
            path: path.to_owned(),
            name: name.to_owned(),
            opened_unix_s: now,
        },
    );
    list.truncate(RECENT_CAP);
}

impl RecentList {
    /// Read the list; a missing, unreadable or foreign file gives an empty list (the list is a
    /// convenience, never a reason to refuse to start).
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match ron::from_str::<Self>(&text) {
            Ok(list)
                if list
                    .header
                    .check(RECENT_FORMAT, RECENT_SCHEMA_VERSION)
                    .is_ok() =>
            {
                list
            }
            _ => Self::default(),
        }
    }

    /// Write the list, making its folder.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("ron.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Put a workspace at the top.
    pub fn touch_workspace(&mut self, path: &Path, name: &str, now: u64) {
        touch(&mut self.workspaces, path, name, now);
    }

    /// Put a campaign at the top.
    pub fn touch_campaign(&mut self, path: &Path, name: &str, now: u64) {
        touch(&mut self.campaigns, path, name, now);
    }

    /// Forget a workspace (it was moved or deleted).
    pub fn forget_workspace(&mut self, path: &Path) {
        self.workspaces.retain(|i| !same_place(&i.path, path));
    }
}
