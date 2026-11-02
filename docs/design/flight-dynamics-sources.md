# SABERS: Flight Dynamics Sources for Aircraft and Drones

Annotated reading list for the fixed-wing and multirotor models in `sim_models`.
It complements McCoy's *Modern Exterior Ballistics*, which covers tube-launched
projectiles in `AeroBallisticsPlugin`.

**Modeling decisions this list assumes** (Alex, 2026-10-30):
- 6DOF rigid body as the default, 3DOF point-mass as an option (also the fast path for fragments).
- Lumped rotor model (thrust/torque from coefficients, no blade-element resolution).
- Rigid arms now. Flexible arms come later.
- No flight software in the loop. Controllers are simple models inside `ControlPlugin`.
- Bit-identical results within one cluster build, not across platforms.

Ratings: **Core** = read first and build from it. **Reference** = use for a
specific term or check. **Later** = for fidelity upgrades not yet in scope.

Edition years are from memory. Check them before citing formally.

---

## 1. Shared foundations (all vehicles)

| Source | Rating | What it covers | How it fits SABERS |
|---|---|---|---|
| Zipfel, *Modeling and Simulation of Aerospace Vehicle Dynamics*, 3rd ed., AIAA Education Series, 2014 | **Core** | Coordinate frames, tensor form of 6DOF equations, and an explicit ladder of 3DOF, 5DOF and 6DOF models for aircraft and missiles. Includes simulation code (CADAC). | The best single text for the 6DOF-with-3DOF-option design. Its 3DOF/6DOF split maps straight onto a `RigidBodyPlugin` that can run either state vector. Bridges McCoy (projectiles) and aircraft in one notation. |
| ANSI/AIAA R-004-1992, *Atmospheric and Space Flight Vehicle Coordinate Systems* | Reference | Standard frame and angle definitions (body, wind, stability, NED, ECEF). | Pin down frame names in `sim_core` once, so aero tables, rotor models and sensors agree. |
| NASA/TM-2015-218675, *Check-cases for Verification of 6-Degree-of-Freedom Flight Vehicle Simulations* (Murri, Jackson, Shelton et al.), Vols. I and II | **Core** | Published check-cases (dropped sphere, tumbling brick, F-16 trims, spinning cylinder, and others) run across six NASA simulation tools, with tabulated reference output. | Ready-made regression tests for `sim_verify`. Run them on every build to confirm the integrator, gravity, rotating Earth and atmosphere before any vehicle model is trusted. |
| AIAA S-119-2011, *Flight Dynamic Model Exchange Standard* (DAVE-ML) | Reference | XML format for aero tables, breakpoints, interpolation and check data. | A good shape for `import::aero_table` and `AeroCoeffs`. Its built-in check cases also feed `sim_verify`. |
| U.S. Standard Atmosphere, 1976 (NOAA/NASA/USAF) | **Core** | Temperature, pressure, density and speed of sound versus altitude. | Default `EnvironmentPlugin` atmosphere. |
| NIMA TR8350.2, *WGS 84* (3rd ed., 2000) | Reference | Ellipsoid, gravity model, Earth rotation. | Needed when long-range ballistics or aircraft runs use a rotating, round Earth. Flat-Earth is fine for most drone runs. |
| MIL-HDBK-1797 (and the older MIL-F-8785C) | Reference | Dryden and von Kármán turbulence spectra and discrete gust models. | Wind/turbulence in `EnvironmentPlugin`. For determinism, seed the turbulence filters from the run seed. |
| Solà, *Quaternion kinematics for the error-state Kalman filter*, arXiv:1711.02508, 2017 | Reference | Careful quaternion algebra, conventions and integration. | Settles Hamilton versus JPL convention and quaternion integration in the 6DOF integrator. |
| Hairer, Lubich, Wanner, *Geometric Numerical Integration*, 2nd ed., Springer, 2006 | Later | Structure-preserving and Lie-group integrators. | Only if RK4 plus quaternion renormalization shows drift in long runs. |

## 2. Fixed-wing aircraft

| Source | Rating | What it covers | How it fits SABERS |
|---|---|---|---|
| Stevens, Lewis, Johnson, *Aircraft Control and Simulation*, 3rd ed., Wiley, 2015 | **Core** | Full nonlinear 6DOF aircraft equations, aero force/moment buildup, trim and linearization, with a complete F-16 model (tables in the appendix) and code. | The working reference for a fixed-wing model. The F-16 data gives a nontrivial, published model to validate against JSBSim and the NASA check-cases. |
| Beard and McLain, *Small Unmanned Aircraft: Theory and Practice*, Princeton UP, 2012 (plus the authors' free supplement and updated chapters online) | **Core** | Small UAV kinematics and dynamics, linear aero coefficient model, wind, sensors, autopilot loops. Builds a simulator chapter by chapter. Includes the Aerosonde parameter set. | The right fidelity for small fixed-wing drones. Its stability-derivative model is a simple first fixed-wing component. Its autopilot chapters are enough for `ControlPlugin` without real flight software. |
| Etkin and Reid, *Dynamics of Flight: Stability and Control*, 3rd ed., Wiley, 1996 | Reference | Classical derivation of the equations of motion and stability derivatives. | Theory behind the derivative model. Good when a derivative's sign or nondimensionalization is in doubt. |
| Phillips, *Mechanics of Flight*, 2nd ed., Wiley, 2010 | Reference | Aerodynamics through 6DOF dynamics in one book, including lifting-line methods and propeller effects. | Useful for estimating coefficients from geometry when no wind-tunnel data exists. |
| Nelson, *Flight Stability and Automatic Control*, 2nd ed., McGraw-Hill, 1998 | Reference | Shorter, approachable treatment of stability derivatives and modes. | A gentler alternative to Etkin for new team members. |
| Allerton, *Principles of Flight Simulation*, Wiley, 2009 | Reference | How real flight simulators are built: integration, tables, engines, atmosphere, real-time structure. | Practical engineering choices for the kernel layout. |
| USAF Stability and Control DATCOM / Digital DATCOM | Reference | Empirical methods for estimating aero derivatives from configuration geometry. | Fills `AeroCoeffs` tables early in design, before test data exists. |
| OpenVSP with VSPAERO (NASA, open source) | Reference | Parametric geometry plus vortex-lattice and panel aero, including stability derivatives. | A local, open tool for generating fixed-wing tables. It runs on the cluster without external services. |

**Decided (2026-10-30):** fixed-wing aircraft get their own `FixedWingPlugin`, kept
separate from `AeroBallisticsPlugin` so the two don't get confused. Both reuse the shared
Mach × alpha × beta table type (`subsystems-and-interfaces.md` §5.2, D9).

## 3. Multirotor drones (quad and hex)

| Source | Rating | What it covers | How it fits SABERS |
|---|---|---|---|
| Mahony, Kumar, Corke, "Multirotor Aerial Vehicles: Modeling, Estimation, and Control of Quadrotor", *IEEE Robotics & Automation Magazine* 19(3), 2012 | **Core** | Rigid-body model, lumped rotor thrust/torque (T = k_T ω², Q = k_Q ω²), blade flapping and induced drag, control structure. | The standard starting point. Its rotor model is exactly the lumped rotor the design chose. |
| Leishman, *Principles of Helicopter Aerodynamics*, 2nd ed., Cambridge UP, 2006 | **Core** | Momentum theory, blade-element momentum theory, inflow, ground effect, vortex ring state, rotor in forward flight. | Physics behind the Ct and Cq curves and how they change with climb rate and advance ratio. Gives the corrections a lumped model should carry. |
| Hoffmann, Huang, Waslander, Tomlin, "Quadrotor Helicopter Flight Dynamics and Control: Theory and Experiment", AIAA GNC Conference, 2007 | **Core** | Measured effects that matter beyond hover: thrust change with airspeed and angle of attack, blade flapping, vortex ring state. | Tells you which lumped-model corrections are worth adding for wind-tolerance and aggressive-maneuver requirements. |
| Bouabdallah, *Design and Control of Quadrotors with Application to Autonomous Flying*, PhD thesis, EPFL, 2007 | Reference | Complete derivation, including rotor gyroscopic terms, hub forces and motor dynamics, with identification on a real vehicle. | Full term-by-term model to check the `RotorcraftPlugin` equations against. |
| Pounds, Mahony, Corke, "Modelling and Control of a Large Quadrotor Robot", *Control Engineering Practice* 18(7), 2010 | Reference | Larger vehicle effects: rotor flapping, blade flexibility, and their influence on stability. | Useful once vehicles get heavier than hobby-scale. |
| Bangura and Mahony, "Nonlinear Dynamic Modeling for High Performance Control of a Quadrotor", ACRA, 2012 | Reference | Rotor drag (H-force) and induced-velocity model in forward flight. | A compact rotor-drag term for the lumped model. |
| Faessler, Franchi, Scaramuzza, "Differential Flatness of Quadrotor Dynamics Subject to Rotor Drag", *IEEE RA-L* 3(2), 2018 | Reference | Linear rotor-drag model with identified coefficients. | Simple, identified drag term. Also the basis for a reference trajectory controller in `ControlPlugin`. |
| Mueller and D'Andrea, "Stability and Control of a Quadrocopter Despite the Complete Loss of One, Two, or Three Propellers", ICRA, 2014 | Later | Behavior and control after rotor loss. | Failure scenarios (motor-out on quad versus hex), which fit SABERS requirement checks well. |
| Cheeseman and Bennett, "The Effect of the Ground on a Helicopter Rotor in Forward Flight", ARC R&M 3021, 1955 | Reference | Classic closed-form ground-effect correction. | One-line ground-effect factor on thrust near the ground. |
| Brandt and Selig, "Propeller Performance Data at Low Reynolds Numbers", AIAA 2011-1255, and the UIUC Propeller Data Site | **Core** (data) | Measured Ct and Cp versus advance ratio for many small propellers. | Real data for `import::rotor_table`. APC also publishes performance files for its propellers. |
| Padfield, *Helicopter Flight Dynamics*, 3rd ed., Wiley, 2018 | Later | Rotorcraft flight dynamics at full-helicopter fidelity. | Only if blade-element or flapping-dynamics rotors come into scope. |

**Motor, ESC and battery:** the sources above treat the motor as a first-order lag
from command to ω. That is enough for the current scope. For the battery-margin metric,
an equivalent-circuit battery model (for example Tremblay and Dessaint, "Experimental
Validation of a Battery Dynamic Model for EV Applications", *World Electric Vehicle
Journal*, 2009) is a common choice. Check that reference before relying on it.

## 4. Open simulators to read and cross-check against

These are for reading equations and comparing outputs, not for linking into SABERS.
All run locally with no external services.

| Model | Rating | What it is | How to use it |
|---|---|---|---|
| JSBSim (LGPL). Berndt, "JSBSim: An Open Source Flight Dynamics Model in C++", AIAA Modeling and Simulation Technologies Conference, 2004 | **Core** | Mature, table-driven 6DOF model with a large aircraft library (C172, F-16 and others), rotating-Earth options and a documented XML model format. | The main cross-check for fixed-wing. Run the same aircraft and initial state in both, compare trajectories. Its XML force buildup is a good template for SABERS components. |
| ArduPilot SITL physics (`libraries/SITL`, for example `SIM_Multicopter`, `SIM_Frame`, `SIM_Plane`) | Reference | Simple, readable multirotor and plane models used for autopilot testing. | Compact reference for frame layouts (quad X, hex X, and others) and motor mixing. |
| PX4 SITL with Gazebo plugins (`gazebo_motor_model` and related) | Reference | Motor model with first-order dynamics, rotor drag and rolling moment. | Another small, readable lumped-rotor implementation. |
| RotorS (Furrer et al., "RotorS: A Modular Gazebo MAV Simulator Framework", in *Robot Operating System (ROS)*, Springer, 2016) | Reference | Origin of the motor model PX4 still uses. | Explains the rotor-drag and rolling-moment terms in the PX4 plugin. |
| RotorPy (Folk, Paulos, Kumar, arXiv:2306.04485, 2023) | **Core** (multirotor) | Python multirotor simulator built for research, with rotor drag, induced drag, wind and documented equations. | The closest open match to SABERS' multirotor scope. Easy to run from the Python `sabers` analysis side as a reference result. |
| Flightmare (Song et al., CoRL 2020) and AirSim (Shah et al., *Field and Service Robotics*, 2017) | Later | Fast quadrotor simulators aimed at learning and vision. | Low priority. Their physics is simpler than RotorPy's. |

## 5. Ballistics bridge (alongside McCoy)

| Source | Rating | Why it is here |
|---|---|---|
| NATO STANAG 4355, *The Modified Point Mass and Five Degrees of Freedom Trajectory Models* | **Core** | The standard 3DOF-plus (modified point mass) model. It is a well-defined target for the fast 3DOF option and for the 30,000-fragment case. Check access, since distribution may be restricted. |
| Carlucci and Jacobson, *Ballistics: Theory and Design of Guns and Ammunition*, 3rd ed., CRC Press, 2018 | Reference | Exterior ballistics chapters in a more modern notation than McCoy, plus launch conditions that set initial states. |
| Missile DATCOM (USAF) | Reference | Aero coefficient estimates for finned projectiles and missiles when no range data exists. |

## 6. Suggested reading order

1. Zipfel (frames, 6DOF and 3DOF structure) and the NASA check-cases (what "correct" means).
2. For airplanes: Beard and McLain for the model, then Stevens, Lewis and Johnson for the F-16 validation case, then JSBSim for cross-checks.
3. For drones: Mahony, Kumar and Corke for the model, Leishman chapters on momentum theory and ground effect, Hoffmann et al. for which corrections matter, then RotorPy for cross-checks and UIUC data for real propellers.
4. STANAG 4355 when building the 3DOF fragment path.
