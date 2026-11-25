//! `Assets.Catalog`: every model the studio can link, from the built-in library, the workspace's
//! `models/` folder and the library roots, keyed by `(source, id)` (subsystems §5.10). Read-only:
//! importing and deleting are the importer's job.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::builtin;
use crate::hash::ContentHash;
use crate::manifest::{MANIFEST_FILE, VisualAssetManifest};
use crate::thumb::THUMB_FILE;
use crate::visual::{AssetSource, VisualAssetRef, VisualSection};

/// Where a model's file is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// In this folder (beside its `model.ron`).
    Dir(PathBuf),
    /// Inside this binary (the primitives' fallback when the built-in folder is not installed).
    Embedded(&'static [u8]),
}

/// One model in the catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogEntry {
    /// Where it is listed.
    pub source: AssetSource,
    /// Its manifest.
    pub manifest: VisualAssetManifest,
    /// Where its file is.
    pub location: Location,
}

impl CatalogEntry {
    /// A link to this model as it is now (with its hash).
    pub fn reference(&self) -> VisualAssetRef {
        VisualAssetRef {
            source: self.source.clone(),
            id: self.manifest.id.clone(),
            version: self.manifest.version,
            content_hash: Some(self.manifest.content_hash),
        }
    }

    /// The model file's path, when it is on disk.
    pub fn file_path(&self) -> Option<PathBuf> {
        match &self.location {
            Location::Dir(dir) => Some(dir.join(&self.manifest.file)),
            Location::Embedded(_) => None,
        }
    }

    /// The thumbnail's path, when the model is in a folder (it may not exist yet).
    pub fn thumb_path(&self) -> Option<PathBuf> {
        match &self.location {
            Location::Dir(dir) => Some(dir.join(THUMB_FILE)),
            Location::Embedded(_) => None,
        }
    }

    /// The thumbnail's PNG bytes: the file beside the model, else the one embedded for a
    /// built-in.
    pub fn thumb(&self) -> Option<Vec<u8>> {
        if let Some(b) = self.thumb_path().and_then(|p| std::fs::read(p).ok()) {
            return Some(b);
        }
        (self.source == AssetSource::Builtin)
            .then(|| builtin::thumb(&self.manifest.id))
            .flatten()
            .map(<[u8]>::to_vec)
    }

    /// The entities that link this model, by `SimId`, in a `visual:` section.
    pub fn users(&self, section: &VisualSection) -> Vec<u32> {
        section
            .models
            .iter()
            .filter(|(_, m)| m.asset.source == self.source && m.asset.id == self.manifest.id)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The model file's bytes and whether they still match the manifest's hash.
    pub fn load(&self) -> std::io::Result<Loaded> {
        let bytes: Cow<'static, [u8]> = match &self.location {
            Location::Dir(dir) => Cow::Owned(std::fs::read(dir.join(&self.manifest.file))?),
            Location::Embedded(b) => Cow::Borrowed(b),
        };
        let hash = ContentHash::of(&bytes);
        Ok(Loaded {
            matches_manifest: hash == self.manifest.content_hash,
            hash,
            bytes,
        })
    }
}

/// A model file read from the catalogue.
#[derive(Clone, Debug)]
pub struct Loaded {
    /// Its bytes.
    pub bytes: Cow<'static, [u8]>,
    /// Their hash.
    pub hash: ContentHash,
    /// Whether the hash is the manifest's: when not, the file changed since it was imported.
    pub matches_manifest: bool,
}

/// What a link resolves to.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution<'a> {
    /// The model, with what differs from the link.
    Found(&'a CatalogEntry, Vec<Notice>),
    /// Nothing under that source and id.
    Missing,
}

/// A difference between a link and the model it finds; drawn anyway, with a lint warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A built-in's version moved on since the link was made.
    OtherVersion {
        /// The version linked.
        linked: u32,
        /// The version installed.
        installed: u32,
    },
    /// The model's file is not the one that was linked.
    HashChanged {
        /// The hash linked.
        linked: ContentHash,
        /// The manifest's hash now.
        now: ContentHash,
    },
}

/// A manifest that could not be listed.
#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    /// Its folder.
    pub dir: PathBuf,
    /// Why.
    pub reason: String,
}

/// The models the studio can link.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
    /// Folders that looked like models but could not be listed.
    pub skipped: Vec<Skipped>,
    /// Models whose file no longer matches their manifest ([`Catalog::verify_files`]).
    stale: Vec<(AssetSource, String)>,
}

impl Catalog {
    /// An empty catalogue.
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in library: this build's manifests, with files from the installed folder (or the
    /// repository's, in a development build) and the embedded primitives.
    pub fn with_builtins() -> Self {
        let mut c = Self::new();
        c.add_builtins(builtin::dir().as_deref());
        c
    }

    /// Add the built-ins, with files looked for in `dir`.
    pub fn add_builtins(&mut self, dir: Option<&Path>) {
        for b in builtin::BUILTINS {
            let manifest = match VisualAssetManifest::from_ron(b.manifest) {
                Ok(m) => m,
                Err(e) => {
                    self.skipped.push(Skipped {
                        dir: PathBuf::from(b.id),
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            let on_disk = dir
                .map(|d| d.join(b.id))
                .filter(|d| d.join(&manifest.file).is_file());
            let location = match (on_disk, b.glb) {
                (Some(d), _) => Location::Dir(d),
                (None, Some(bytes)) => Location::Embedded(bytes),
                (None, None) => {
                    self.skipped.push(Skipped {
                        dir: PathBuf::from(b.id),
                        reason: "the built-in folder is not installed".into(),
                    });
                    continue;
                }
            };
            self.entries.push(CatalogEntry {
                source: AssetSource::Builtin,
                manifest,
                location,
            });
        }
    }

    /// Add a workspace's `models/<slug>/` folders (ids `models/<slug>`).
    pub fn add_workspace(&mut self, root: &Path) {
        self.add_folders(&root.join("models"), AssetSource::Workspace, "models/");
    }

    /// Add a library root's `<slug>/` folders (ids `<slug>`), listed under the root's folder name.
    pub fn add_library(&mut self, root: &Path) {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        self.add_folders(root, AssetSource::Library(name), "");
    }

    fn add_folders(&mut self, dir: &Path, source: AssetSource, prefix: &str) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut dirs: Vec<PathBuf> = read
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join(MANIFEST_FILE).is_file())
            .collect();
        dirs.sort();
        for d in dirs {
            let slug = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let id = format!("{prefix}{slug}");
            let parsed = std::fs::read_to_string(d.join(MANIFEST_FILE))
                .map_err(|e| e.to_string())
                .and_then(|t| VisualAssetManifest::from_ron(&t).map_err(|e| e.to_string()));
            match parsed {
                Ok(mut manifest) => {
                    // The folder decides the id, so a copied folder is not a duplicate.
                    manifest.id = id;
                    if self.get(&source, &manifest.id).is_none() {
                        self.entries.push(CatalogEntry {
                            source: source.clone(),
                            manifest,
                            location: Location::Dir(d),
                        });
                    }
                }
                Err(reason) => self.skipped.push(Skipped { dir: d, reason }),
            }
        }
    }

    /// Hash every model file on disk once, noting those that changed since they were imported
    /// (or cannot be read), so the Library and lint can say so without reading files again.
    pub fn verify_files(&mut self) {
        self.stale = self
            .entries
            .iter()
            .filter(|e| matches!(e.location, Location::Dir(_)))
            .filter(|e| e.load().map_or(true, |l| !l.matches_manifest))
            .map(|e| (e.source.clone(), e.manifest.id.clone()))
            .collect();
    }

    /// Whether a model's file changed since it was imported, as of [`Catalog::verify_files`].
    pub fn file_changed(&self, entry: &CatalogEntry) -> bool {
        self.stale
            .iter()
            .any(|(s, id)| *s == entry.source && *id == entry.manifest.id)
    }

    /// Every model listed.
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// One model by source and id.
    pub fn get(&self, source: &AssetSource, id: &str) -> Option<&CatalogEntry> {
        self.entries
            .iter()
            .find(|e| &e.source == source && e.manifest.id == id)
    }

    /// The model a link names, with what differs from the link.
    pub fn resolve(&self, r: &VisualAssetRef) -> Resolution<'_> {
        let Some(entry) = self.get(&r.source, &r.id) else {
            return Resolution::Missing;
        };
        let mut notices = Vec::new();
        if r.source == AssetSource::Builtin && r.version != entry.manifest.version {
            notices.push(Notice::OtherVersion {
                linked: r.version,
                installed: entry.manifest.version,
            });
        }
        if let Some(linked) = r.content_hash
            && linked != entry.manifest.content_hash
        {
            notices.push(Notice::HashChanged {
                linked,
                now: entry.manifest.content_hash,
            });
        }
        Resolution::Found(entry, notices)
    }
}
