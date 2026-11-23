//! `--resume` (IB-04, IB-07, seq §12): what earlier attempts committed, read from the summaries
//! footers alone, and the plan for the next attempt.
//!
//! Committed parts are the source of truth: a run is done exactly when its `run_id` is in a
//! summaries file under its final name in the shared output. Each footer gives the part's run ranges
//! and counts, and its header the campaign, scene, requirement set and build it was written by;
//! resume refuses parts of anything else. `_MANIFEST.json` is only an index: its earlier attempts
//! are kept, attempts it does not list (a job that died before writing it) are rebuilt from the
//! parts, and it says whether the crash policy stopped the campaign.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;

use serde_json::{Value, json};
use sim_schema::io::{
    CRASH_COUNTS_KEY, DUMP_COUNTS_KEY, MANIFEST_HASH_KEY, RUN_IDS_KEY, STATUS_COUNTS_KEY, Table,
    read_parquet_footer,
};
use sim_schema::{DiskEnum, RangeSet, SimStatus};

use crate::ExitCode;
use crate::campaign::Campaign;
use crate::protocol::{Refusal, Tally};

/// One committed part of an earlier attempt.
#[derive(Clone, Debug, PartialEq)]
pub struct PriorPart {
    /// Its attempt.
    pub attempt: u16,
    /// The rank that wrote it.
    pub rank: u32,
    /// Its number within the attempt.
    pub part: u32,
    /// Its files in the shared output, summaries last.
    pub files: Vec<String>,
    /// Its runs.
    pub run_ids: RangeSet,
}

/// What earlier attempts left, and the attempt to run next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prior {
    /// The attempt to run.
    pub attempt: u16,
    /// Runs in committed parts.
    pub committed: RangeSet,
    /// Counts restored from the committed parts' footers (no verdict counts: footers do not carry
    /// them).
    pub tally: Tally,
    /// Every committed part.
    pub parts: Vec<PriorPart>,
    /// The earlier attempts for `_MANIFEST.json`: the previous index's, then any rebuilt from parts.
    pub attempts: Vec<Value>,
    /// `run_id`s of kept crash dumps, as the previous index listed them.
    pub dump_run_ids: Vec<u64>,
    /// The crash policy is off for this attempt (`--override-crash-policy` after an abort).
    pub override_crash_policy: bool,
}

/// How a job treats what earlier attempts left in the shared output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// A new campaign: refuse an output directory that already holds parts or `_MANIFEST.json`,
    /// so attempt 0 never overwrites anything.
    #[default]
    Fresh,
    /// `--resume`: a new attempt over the runs not yet committed.
    Resume,
    /// `--resume --override-crash-policy`: also continue a campaign the crash policy stopped, with
    /// the abort rules off.
    ResumeOverride,
}

/// What the job runs: attempt 0 over every run, or [`plan`]'s next attempt.
pub fn prepare(campaign: &Campaign, mode: Mode) -> Result<Prior, Refusal> {
    match mode {
        Mode::Fresh => fresh(campaign).map(|()| Prior::default()),
        Mode::Resume => plan(campaign, false),
        Mode::ResumeOverride => plan(campaign, true),
    }
}

/// Refuse a fresh campaign whose output directory is not new.
fn fresh(campaign: &Campaign) -> Result<(), Refusal> {
    let out = campaign
        .shared_out()
        .map_err(|e| refuse(ExitCode::Config, "Config", e.to_string()))?;
    let Ok(entries) = std::fs::read_dir(&out) else {
        return Ok(());
    };
    let taken = entries.filter_map(Result::ok).find_map(|e| {
        let name = e.file_name().into_string().ok()?;
        (name == "_MANIFEST.json" || parse_part_name(&name).is_some()).then_some(name)
    });
    match taken {
        None => Ok(()),
        Some(name) => Err(refuse(
            ExitCode::Config,
            "OutputExists",
            format!(
                "{} already holds a campaign ({name}); continue it with --resume, or give this campaign a new output.shared_out",
                out.display()
            ),
        )),
    }
}

/// A part file's name: `node_{rank}_a{attempt}_p{part}_{table}`, with `.tmp` or `.partial` allowed.
pub fn parse_part_name(name: &str) -> Option<(u32, u16, u32, &str)> {
    let rest = name.strip_prefix("node_")?;
    let (rank, rest) = rest.split_once("_a")?;
    let (attempt, rest) = rest.split_once("_p")?;
    let (part, table) = rest.split_once('_')?;
    Some((
        rank.parse().ok()?,
        attempt.parse().ok()?,
        part.parse().ok()?,
        table,
    ))
}

fn refuse(exit: ExitCode, kind: &str, error: String) -> Refusal {
    Refusal {
        exit: exit.code(),
        kind: kind.to_owned(),
        error,
    }
}

fn hex(h: u64) -> String {
    format!("{h:016x}")
}

/// Read what earlier attempts committed to the campaign's shared output and plan the next one.
pub fn plan(campaign: &Campaign, override_crash_policy: bool) -> Result<Prior, Refusal> {
    let out = campaign
        .shared_out()
        .map_err(|e| refuse(ExitCode::Config, "Config", e.to_string()))?;
    let mut prior = Prior {
        override_crash_policy,
        ..Prior::default()
    };
    let entries = match std::fs::read_dir(&out) {
        Ok(d) => d,
        // Nothing ran yet: the first attempt.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(prior),
        Err(e) => {
            return Err(refuse(
                ExitCode::Io,
                "ResumeRead",
                format!("{}: {e}", out.display()),
            ));
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    // The previous index, if it reads.
    let index: Option<Value> = std::fs::read_to_string(out.join("_MANIFEST.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let indexed: BTreeSet<u64> = index
        .as_ref()
        .and_then(|m| m["attempts"].as_array())
        .map(|a| a.iter().filter_map(|x| x["attempt"].as_u64()).collect())
        .unwrap_or_default();

    let mut last_attempt: Option<u16> = indexed.iter().max().map(|&a| a as u16);
    for name in &names {
        if let Some((_, a, _, _)) = parse_part_name(name) {
            last_attempt = Some(last_attempt.map_or(a, |l| l.max(a)));
        }
    }
    prior.attempt = match last_attempt {
        None => 0,
        Some(a) => a
            .checked_add(1)
            .ok_or_else(|| refuse(ExitCode::Config, "Resume", "no attempt numbers left".into()))?,
    };

    // Every committed part, checked against this campaign.
    let want_req = campaign.requirements.as_ref().map(|r| r.hash());
    let want_scene = campaign.scenario.scene_hash();
    for name in &names {
        let Some((rank, attempt, part, "summaries.parquet")) = parse_part_name(name) else {
            continue;
        };
        let path = out.join(name);
        let bad = |what: String| {
            refuse(
                ExitCode::Io,
                "ResumeRead",
                format!("{}: {what}", path.display()),
            )
        };
        let file = File::open(&path).map_err(|e| bad(e.to_string()))?;
        let footer = read_parquet_footer(file, Table::Summaries).map_err(|e| bad(e.to_string()))?;
        let h = &footer.header;
        let mismatch = |what: &str, found: String, expected: String| {
            refuse(
                ExitCode::Config,
                "ResumeMismatch",
                format!(
                    "{name} was written for {what} {found}, this campaign has {expected}; a changed campaign is a new campaign"
                ),
            )
        };
        if h.campaign_id.as_deref() != Some(campaign.config.campaign.id.as_str()) {
            return Err(mismatch(
                "campaign",
                format!("{:?}", h.campaign_id),
                format!("{:?}", campaign.config.campaign.id),
            ));
        }
        if h.scene_hash != Some(want_scene) {
            return Err(mismatch(
                "scene_hash",
                h.scene_hash.map_or("none".into(), hex),
                hex(want_scene),
            ));
        }
        if h.req_set_hash != want_req {
            return Err(mismatch(
                "req_set_hash",
                h.req_set_hash.map_or("none".into(), hex),
                want_req.map_or("none".into(), hex),
            ));
        }
        if let Some(m) = footer.get(MANIFEST_HASH_KEY)
            && m != hex(campaign.manifest.hash)
        {
            return Err(mismatch(
                "sweep manifest",
                m.to_owned(),
                hex(campaign.manifest.hash),
            ));
        }
        if h.build_id != campaign.build_id {
            return Err(refuse(
                ExitCode::BuildMismatch,
                "BuildMismatch",
                format!(
                    "{name} was written by {:?}, this is {:?}; results are bit-identical only within one build (D1), so resume with that build or run a new campaign",
                    h.build_id, campaign.build_id
                ),
            ));
        }
        let run_ids: RangeSet = footer
            .get(RUN_IDS_KEY)
            .and_then(|v| serde_json::from_str::<Vec<[u64; 2]>>(v).ok())
            .map(RangeSet::from)
            .ok_or_else(|| bad(format!("no readable {RUN_IDS_KEY} footer")))?;
        restore_counts(&mut prior.tally, &footer, &run_ids).map_err(bad)?;
        prior.committed.extend(&run_ids);
        let stem = format!("node_{rank}_a{attempt}_p{part}_");
        let mut files: Vec<String> = [
            "events.parquet",
            "snapshots.parquet",
            "crashes.zst",
            "impacts.parquet",
        ]
        .iter()
        .map(|t| format!("{stem}{t}"))
        .filter(|f| names.binary_search(f).is_ok())
        .collect();
        files.push(name.clone());
        prior.parts.push(PriorPart {
            attempt,
            rank,
            part,
            files,
            run_ids,
        });
    }

    // The index's attempts, then any attempt it does not list, rebuilt from its parts.
    if let Some(m) = &index {
        if let Some(a) = m["attempts"].as_array() {
            prior.attempts.extend(a.iter().cloned());
        }
        if let Some(d) = m["crash_dump_run_ids"].as_array() {
            prior.dump_run_ids = d.iter().filter_map(Value::as_u64).collect();
        }
        if m["status"] == "aborted" && !override_crash_policy {
            let reason = m["abort"]["reason"].as_str().unwrap_or("crash policy");
            let detail = m["abort"]["detail"].as_str().unwrap_or("");
            return Err(refuse(
                ExitCode::CrashPolicy,
                "Aborted",
                format!(
                    "the crash policy stopped this campaign ({reason}: {detail}); fix the scenario and run it as a new campaign, or resume with --override-crash-policy"
                ),
            ));
        }
    }
    let mut rebuilt: BTreeMap<u16, BTreeMap<u32, Vec<Value>>> = BTreeMap::new();
    for p in prior
        .parts
        .iter()
        .filter(|p| !indexed.contains(&u64::from(p.attempt)))
    {
        rebuilt
            .entry(p.attempt)
            .or_default()
            .entry(p.rank)
            .or_default()
            .push(json!({
                "part": p.part,
                "files": p.files,
                "run_ids": p.run_ids,
            }));
    }
    for (attempt, ranks) in rebuilt {
        let ranks: Vec<Value> = ranks
            .into_iter()
            .map(|(rank, parts)| json!({"rank": rank, "parts": parts}))
            .collect();
        prior
            .attempts
            .push(json!({"attempt": attempt, "rebuilt_from_footers": true, "ranks": ranks}));
    }
    Ok(prior)
}

/// Add one part's footer counts to the restored tally.
fn restore_counts(
    tally: &mut Tally,
    footer: &sim_schema::io::TableFooter,
    run_ids: &RangeSet,
) -> Result<(), String> {
    tally.runs += run_ids.len();
    let read = |key: &str| -> Result<BTreeMap<String, u64>, String> {
        match footer.get(key) {
            None => Ok(BTreeMap::new()),
            Some(v) => serde_json::from_str(v).map_err(|e| format!("{key}: {e}")),
        }
    };
    for (k, v) in read(CRASH_COUNTS_KEY)? {
        let id: u32 = k
            .parse()
            .map_err(|_| format!("{CRASH_COUNTS_KEY}: key {k:?}"))?;
        *tally.crashes_by_param_set.entry(id).or_insert(0) += v;
    }
    for (k, v) in read(DUMP_COUNTS_KEY)? {
        let id: u32 = k
            .parse()
            .map_err(|_| format!("{DUMP_COUNTS_KEY}: key {k:?}"))?;
        *tally.dumps_by_param_set.entry(id).or_insert(0) += v as u32;
    }
    for (name, v) in read(STATUS_COUNTS_KEY)? {
        let status = SimStatus::VALUES
            .iter()
            .find(|s| format!("{s:?}") == name)
            .ok_or_else(|| format!("{STATUS_COUNTS_KEY}: unknown status {name:?}"))?;
        let i = status.to_u8() as usize;
        if tally.status_counts.len() <= i {
            tally
                .status_counts
                .resize(SimStatus::VALUES.len().max(i + 1), 0);
        }
        tally.status_counts[i] += v;
        if *status == SimStatus::SetupError {
            tally.setup_errors += v;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_names_parse() {
        assert_eq!(
            parse_part_name("node_3_a12_p0_summaries.parquet"),
            Some((3, 12, 0, "summaries.parquet"))
        );
        assert_eq!(
            parse_part_name("node_0_a0_p7_crashes.zst.tmp"),
            Some((0, 0, 7, "crashes.zst.tmp"))
        );
        assert_eq!(parse_part_name("_MANIFEST.json"), None);
        assert_eq!(parse_part_name("node_x_a0_p0_summaries.parquet"), None);
        assert_eq!(parse_part_name("node_0_a0_summaries.parquet"), None);
    }
}
