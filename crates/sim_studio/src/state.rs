//! The studio's top-level modes (`sim_studio_ux.md` §5.2) and the activity rail that switches
//! between them (§5.4).

use bevy::prelude::*;

/// What the studio window is doing. Systems are gated with `in_state(...)`; the campaign monitor
/// and results explorer are windows of `sim_explorer`, not states here (§6.2).
#[derive(States, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Reflect)]
pub enum AppState {
    /// Open or create a workspace, open a crash dump or a campaign.
    #[default]
    Launcher,
    /// Author a scenario and its parameters and requirements; export `.scn.ron` (S2).
    ScenarioBuilder,
    /// Run one `(param_set_id, seed)` interactively in-process (S3-1).
    LocalRun,
    /// Configure a sweep and submit a campaign (S6).
    CampaignSetup,
    /// Replay a completed run from Tier 3 snapshots or by re-simulating it (S4).
    RunReplay,
    /// Scrub a crashed run's flight-recorder frames (S4).
    CrashReplay,
}

impl AppState {
    /// The name shown in the title bar and breadcrumb.
    pub fn title(self) -> &'static str {
        match self {
            AppState::Launcher => "Launcher",
            AppState::ScenarioBuilder => "Scenario Builder",
            AppState::LocalRun => "Local Run",
            AppState::CampaignSetup => "Campaign Setup",
            AppState::RunReplay => "Run Replay",
            AppState::CrashReplay => "Crash Replay",
        }
    }

    /// The build step that brings the screen; `None` once it is built.
    pub fn arrives_in(self) -> Option<&'static str> {
        match self {
            AppState::Launcher | AppState::ScenarioBuilder | AppState::LocalRun => None,
            AppState::RunReplay | AppState::CrashReplay => Some("S4"),
            AppState::CampaignSetup => Some("S6"),
        }
    }
}

/// The left activity rail: one item per workspace area, keys 1–4 (§9.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Reflect)]
pub enum Rail {
    /// B: the Scenario Builder.
    Builder,
    /// C: Campaign Setup.
    Campaigns,
    /// R: Results, which open in `sim_explorer` (IB-18).
    Results,
    /// P: Run or Crash Replay.
    Replay,
}

impl Rail {
    /// Rail order, top to bottom.
    pub const ALL: [Rail; 4] = [Rail::Builder, Rail::Campaigns, Rail::Results, Rail::Replay];

    /// The rail letter.
    pub fn letter(self) -> &'static str {
        match self {
            Rail::Builder => "B",
            Rail::Campaigns => "C",
            Rail::Results => "R",
            Rail::Replay => "P",
        }
    }

    /// The full name, for the tooltip and screen readers.
    pub fn name(self) -> &'static str {
        match self {
            Rail::Builder => "Scenario Builder",
            Rail::Campaigns => "Campaign Setup",
            Rail::Results => "Results (opens the explorer)",
            Rail::Replay => "Replay",
        }
    }

    /// The keyboard shortcut digit.
    pub fn key(self) -> KeyCode {
        match self {
            Rail::Builder => KeyCode::Digit1,
            Rail::Campaigns => KeyCode::Digit2,
            Rail::Results => KeyCode::Digit3,
            Rail::Replay => KeyCode::Digit4,
        }
    }

    /// Whether `state` is this item's area (the item is highlighted).
    pub fn is_active(self, state: AppState) -> bool {
        matches!(
            (self, state),
            (
                Rail::Builder,
                AppState::ScenarioBuilder | AppState::LocalRun
            ) | (Rail::Campaigns, AppState::CampaignSetup)
                | (Rail::Replay, AppState::RunReplay | AppState::CrashReplay)
        )
    }
}

/// What the rail needs to know to enable an item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RailContext {
    /// A workspace is open.
    pub workspace: bool,
    /// A completed run is loaded for replay.
    pub run: bool,
    /// A crash dump is open.
    pub crash: bool,
}

/// Where a rail item leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailTarget {
    /// Switch to this state.
    State(AppState),
    /// Hand off to `sim_explorer` (IB-18 `OpenCampaign`).
    Explorer,
}

impl Rail {
    /// Where the item leads in this context, or why it is disabled (shown on hover, §5.4).
    pub fn target(self, cx: RailContext) -> Result<RailTarget, &'static str> {
        match self {
            Rail::Builder if cx.workspace => Ok(RailTarget::State(AppState::ScenarioBuilder)),
            Rail::Builder => Err("No workspace open"),
            Rail::Campaigns if cx.workspace => Ok(RailTarget::State(AppState::CampaignSetup)),
            Rail::Campaigns => Err("No workspace open"),
            Rail::Results if cx.workspace => Ok(RailTarget::Explorer),
            Rail::Results => Err("No workspace open"),
            Rail::Replay if cx.crash => Ok(RailTarget::State(AppState::CrashReplay)),
            Rail::Replay if cx.run => Ok(RailTarget::State(AppState::RunReplay)),
            Rail::Replay => Err("No run or crash dump open"),
        }
    }
}
