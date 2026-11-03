//! Workspaces (`sim_studio_ux.md` §4, §5.1): a folder with `scenarios/`, `requirements/`,
//! `campaigns/` and `scripts/`, and the studio's own state in `saber.workspace.ron` (layout, unit
//! profile, theme, model roots, the scenario that was open).
//!
//! The file carries a [`FileHeader`] with format `workspace`; readers take the same schema major
//! and ignore fields they do not know, so a newer minor still opens (§10.4).

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sim_schema::{FileHeader, HeaderError, Semver};
use sim_studio_ui::dock::DockLayout;
use sim_studio_ui::theme::ThemeMode;

/// The workspace file's name, at the workspace root.
pub const WORKSPACE_FILE: &str = "saber.workspace.ron";
/// `FileHeader::format` of a workspace file.
pub const WORKSPACE_FORMAT: &str = "workspace";
/// The workspace schema this build writes and reads.
pub const WORKSPACE_SCHEMA_VERSION: Semver = Semver::new(1, 0, 0);
/// The folders every workspace has.
pub const FOLDERS: [&str; 4] = ["scenarios", "requirements", "campaigns", "scripts"];

/// The domain pack a workspace started from (§8). Robotics is deferred (D13).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pack {
    /// The environment only.
    #[default]
    Blank,
    /// A spin-stabilised shell with McCoy 6DOF aerodynamics.
    AeroBallistics,
    /// A quad X holding altitude on PID loops.
    Multirotor,
}

/// One starter file a pack writes.
pub struct TemplateFile {
    /// Path under the workspace root.
    pub path: &'static str,
    /// Contents.
    pub text: &'static str,
}

impl Pack {
    /// The Launcher's "Start new" order.
    pub const ALL: [Pack; 3] = [Pack::AeroBallistics, Pack::Multirotor, Pack::Blank];

    /// The Launcher's label.
    pub fn label(self) -> &'static str {
        match self {
            Pack::Blank => "Blank scenario",
            Pack::AeroBallistics => "Aero-ballistics",
            Pack::Multirotor => "Drone (multirotor)",
        }
    }

    /// One line on what it starts with.
    pub fn blurb(self) -> &'static str {
        match self {
            Pack::Blank => "Gravity, the standard atmosphere and calm air; add bodies yourself",
            Pack::AeroBallistics => {
                "A spin-stabilised shell, McCoy 6DOF aerodynamics, apogee and range requirements"
            }
            Pack::Multirotor => {
                "A quad X climbing to a 10 m hold on PID loops, hold-altitude requirements"
            }
        }
    }

    /// The unit profile it starts with.
    pub fn units(self) -> UnitProfile {
        match self {
            Pack::Blank => UnitProfile::Si,
            Pack::AeroBallistics => UnitProfile::Ballistics,
            Pack::Multirotor => UnitProfile::Drone,
        }
    }

    /// The starter scenario, opened when the workspace is created.
    pub fn scenario(self) -> &'static str {
        match self {
            Pack::Blank => "scenarios/blank.scn.ron",
            Pack::AeroBallistics => "scenarios/shell.scn.ron",
            Pack::Multirotor => "scenarios/quad.scn.ron",
        }
    }

    /// The starter files: a scenario and, except for Blank, its requirement set.
    pub fn files(self) -> &'static [TemplateFile] {
        const BLANK: &[TemplateFile] = &[TemplateFile {
            path: "scenarios/blank.scn.ron",
            text: include_str!("../templates/blank.scn.ron"),
        }];
        const AERO: &[TemplateFile] = &[
            TemplateFile {
                path: "scenarios/shell.scn.ron",
                text: include_str!("../templates/shell.scn.ron"),
            },
            TemplateFile {
                path: "requirements/shell.req.ron",
                text: include_str!("../templates/shell.req.ron"),
            },
        ];
        const QUAD: &[TemplateFile] = &[
            TemplateFile {
                path: "scenarios/quad.scn.ron",
                text: include_str!("../templates/quad.scn.ron"),
            },
            TemplateFile {
                path: "requirements/quad.req.ron",
                text: include_str!("../templates/quad.req.ron"),
            },
        ];
        match self {
            Pack::Blank => BLANK,
            Pack::AeroBallistics => AERO,
            Pack::Multirotor => QUAD,
        }
    }
}

/// The workspace's default display units (§9.3). Values are SI internally; the profile only
/// chooses how fields show them (the quantity inputs arrive with the Builder, S2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnitProfile {
    /// SI base units.
    #[default]
    Si,
    /// km, m/s, Mach, deg, mrad (aero-ballistics pack).
    Ballistics,
    /// m, m/s, deg, % throttle (drone pack).
    Drone,
}

impl UnitProfile {
    /// The status bar's wording.
    pub fn label(self) -> &'static str {
        match self {
            UnitProfile::Si => "units: SI",
            UnitProfile::Ballistics => "units: SI (km display)",
            UnitProfile::Drone => "units: SI (m display)",
        }
    }
}

/// `saber.workspace.ron`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceFile {
    /// Format `workspace`.
    pub header: FileHeader,
    /// Shown in the title bar and the Launcher.
    pub name: String,
    /// The pack it started from.
    #[serde(default)]
    pub pack: Pack,
    /// Display units.
    #[serde(default)]
    pub units: UnitProfile,
    /// Dark or light.
    #[serde(default)]
    pub theme: ThemeMode,
    /// Dock region sizes.
    #[serde(default)]
    pub layout: DockLayout,
    /// Team model libraries (`sim_assets`, D17 O6), read in place.
    #[serde(default)]
    pub model_roots: Vec<PathBuf>,
    /// The scenario open when the workspace was last saved, relative to the root.
    #[serde(default)]
    pub open_scenario: Option<String>,
}

/// An open workspace: its root folder and file.
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    /// The folder.
    pub root: PathBuf,
    /// Its `saber.workspace.ron`.
    pub file: WorkspaceFile,
}

/// Why a workspace could not be created, opened or saved.
#[derive(Debug)]
pub enum WorkspaceError {
    /// No `saber.workspace.ron` at the path.
    NotFound(PathBuf),
    /// The folder already is a workspace, or a starter file would be overwritten.
    Exists(PathBuf),
    /// A workspace name must be letters, digits, `-`, `_` or `.`.
    BadName(String),
    /// Reading or writing failed.
    Io(PathBuf, std::io::Error),
    /// The file is not valid RON for a workspace.
    Parse(PathBuf, String),
    /// The header is not a workspace this build reads.
    Header(PathBuf, HeaderError),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceError::NotFound(p) => write!(f, "{}: no {WORKSPACE_FILE} here", p.display()),
            WorkspaceError::Exists(p) => write!(f, "{}: already exists", p.display()),
            WorkspaceError::BadName(n) => write!(
                f,
                "{n:?} is not a workspace name: use letters, digits, '-', '_' or '.'"
            ),
            WorkspaceError::Io(p, e) => write!(f, "{}: {e}", p.display()),
            WorkspaceError::Parse(p, e) => write!(f, "{}: {e}", p.display()),
            WorkspaceError::Header(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// Whether `name` is a valid workspace name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> WorkspaceError + '_ {
    move |e| WorkspaceError::Io(path.to_owned(), e)
}

/// Write `text` to `path` through a temporary file and a rename, so a crash never leaves half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), WorkspaceError> {
    let tmp = path.with_extension("ron.tmp");
    std::fs::write(&tmp, text).map_err(io(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io(path))
}

impl WorkspaceFile {
    /// A new file for a workspace called `name`, started from `pack`.
    pub fn new(name: &str, pack: Pack) -> Self {
        let mut header = FileHeader::new(
            WORKSPACE_FORMAT,
            WORKSPACE_SCHEMA_VERSION,
            sim_schema::SABER_VERSION,
        );
        header.build_id = sim_schema::build_id();
        Self {
            header,
            name: name.to_owned(),
            pack,
            units: pack.units(),
            theme: ThemeMode::default(),
            layout: DockLayout::default(),
            model_roots: Vec::new(),
            open_scenario: Some(pack.scenario().to_owned()),
        }
    }

    /// Parse a file's text and check its header.
    pub fn from_ron(text: &str, path: &Path) -> Result<Self, WorkspaceError> {
        let mut file: Self = ron::from_str(text)
            .map_err(|e| WorkspaceError::Parse(path.to_owned(), e.to_string()))?;
        file.header
            .check(WORKSPACE_FORMAT, WORKSPACE_SCHEMA_VERSION)
            .map_err(|e| WorkspaceError::Header(path.to_owned(), e))?;
        file.layout = file.layout.sanitized();
        Ok(file)
    }

    /// The file's text.
    pub fn to_ron(&self) -> String {
        let body = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .expect("a workspace file serialises");
        format!("// SABER workspace: studio layout and settings (sim_studio_ux.md §5.1).\n{body}\n")
    }
}

impl Workspace {
    /// Create a workspace in `root` (made if missing) from `pack`: the folders, the starter files
    /// and `saber.workspace.ron`. Refuses a folder that already is a workspace and never
    /// overwrites a file.
    pub fn create(root: &Path, name: &str, pack: Pack) -> Result<Self, WorkspaceError> {
        if !valid_name(name) {
            return Err(WorkspaceError::BadName(name.to_owned()));
        }
        let file_path = root.join(WORKSPACE_FILE);
        if file_path.exists() {
            return Err(WorkspaceError::Exists(file_path));
        }
        for t in pack.files() {
            let p = root.join(t.path);
            if p.exists() {
                return Err(WorkspaceError::Exists(p));
            }
        }
        for dir in FOLDERS {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).map_err(io(&d))?;
        }
        for t in pack.files() {
            let p = root.join(t.path);
            std::fs::write(&p, t.text).map_err(io(&p))?;
        }
        let ws = Self {
            root: root.to_owned(),
            file: WorkspaceFile::new(name, pack),
        };
        ws.save()?;
        Ok(ws)
    }

    /// Open the workspace at `path`: its folder or its `saber.workspace.ron`.
    pub fn open(path: &Path) -> Result<Self, WorkspaceError> {
        let (root, file_path) = if path.file_name().is_some_and(|n| n == WORKSPACE_FILE) {
            (
                path.parent().unwrap_or(Path::new(".")).to_owned(),
                path.to_owned(),
            )
        } else {
            (path.to_owned(), path.join(WORKSPACE_FILE))
        };
        if !file_path.is_file() {
            return Err(WorkspaceError::NotFound(root));
        }
        let text = std::fs::read_to_string(&file_path).map_err(io(&file_path))?;
        Ok(Self {
            root,
            file: WorkspaceFile::from_ron(&text, &file_path)?,
        })
    }

    /// Write `saber.workspace.ron`.
    pub fn save(&self) -> Result<(), WorkspaceError> {
        write_atomic(&self.root.join(WORKSPACE_FILE), &self.file.to_ron())
    }

    /// The open scenario's full path.
    pub fn scenario_path(&self) -> Option<PathBuf> {
        self.file.open_scenario.as_ref().map(|s| self.root.join(s))
    }
}
