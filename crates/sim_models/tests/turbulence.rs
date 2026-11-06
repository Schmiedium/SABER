//! Dryden turbulence (verification §7.1, statistical rung): a long seeded realisation against the
//! MIL-F-8785C / MIL-HDBK-1797 spectra and their autocorrelations, and the plugin's wind through
//! the kernel (gust axes per frame, interpolation within a tick, seeding, setup errors).
//!
//! Statistical tolerances are five standard deviations of the estimator, computed from the
//! analytic spectrum (Bartlett's formula for autocovariances, the χ² spread of a Welch average for
//! the spectrum), so a correct generator fails with probability below 1e-6 and the fixed seed makes
//! the result reproducible.

use std::f64::consts::PI;

use glam::DVec3;
use sim_core::{
    MassProps, PointMass3, RunSeed, SetupError, SimAppBuilder, SimConfig, SimEnvironment, SimId,
    SimRng, SimWorld, WorldFrame,
};
use sim_models::environment::dryden::{DrydenSpec, DrydenTurbulence};
use sim_models::environment::{Atmosphere, DRYDEN_STREAM, Gravity, Wind};
use sim_schema::Frame;

const V: f64 = 50.0;
const SIGMA: [f64; 3] = [1.5, 1.2, 0.8];
const SCALE: [f64; 3] = [200.0, 100.0, 40.0];
/// `V dt / L` is 0.125, 0.25 and 0.625: both of the generator's `J_k` branches are used.
const DT: f64 = 0.5;
const N: usize = 1 << 19;

fn spec() -> DrydenSpec {
    DrydenSpec {
        airspeed: V,
        intensity: SIGMA,
        scale: SCALE,
    }
}

/// The MIL spectra, one-sided over spatial frequency `Ω`, rad/m.
fn mil_psd(axis: usize, omega: f64) -> f64 {
    let (s, l) = (SIGMA[axis], SCALE[axis]);
    let x = (l * omega).powi(2);
    if axis == 0 {
        s * s * 2.0 * l / PI / (1.0 + x)
    } else {
        s * s * l / PI * (1.0 + 3.0 * x) / (1.0 + x).powi(2)
    }
}

/// The autocorrelation in time, from the same spectra (MIL-F-8785C).
fn mil_correlation(axis: usize, tau: f64) -> f64 {
    let (s, l) = (SIGMA[axis], SCALE[axis]);
    let x = V * tau.abs() / l;
    if axis == 0 {
        s * s * (-x).exp()
    } else {
        s * s * (1.0 - 0.5 * x) * (-x).exp()
    }
}

/// Spectral density of the sampled sequence, two-sided per rad/s (integrates to σ² over
/// `±π/dt`): the MIL spectrum folded onto the sampling band (Poisson summation), with the `1/ω²`
/// tail beyond `K` images added in closed form.
fn sampled_psd(axis: usize, w: f64) -> f64 {
    let two_sided = |w: f64| mil_psd(axis, w.abs() / V) / (2.0 * V);
    let ws = 2.0 * PI / DT;
    const K: i64 = 20_000;
    let mut s = 0.0;
    for k in -K..=K {
        s += two_sided(w + k as f64 * ws);
    }
    // Far images: two_sided(ω) → c/ω² with c = σ²V/(πL) for u and 3σ²V/(2πL) for v and w.
    let (sg, l) = (SIGMA[axis], SCALE[axis]);
    let c = if axis == 0 { 1.0 } else { 1.5 } * sg * sg * V / (PI * l);
    s + 2.0 * c / (ws * ws) / K as f64
}

fn realisation(seed: u64) -> [Vec<f64>; 3] {
    let mut rng = SimRng::new(0, seed).stream(DRYDEN_STREAM);
    let mut g = DrydenTurbulence::new(&spec(), DT).unwrap();
    g.start(&mut rng);
    let mut out = [
        Vec::with_capacity(N),
        Vec::with_capacity(N),
        Vec::with_capacity(N),
    ];
    for _ in 0..N {
        for (o, v) in out.iter_mut().zip(g.gust()) {
            o.push(v);
        }
        g.step(&mut rng);
    }
    out
}

#[test]
fn the_realisation_has_the_dryden_autocorrelation() {
    for (axis, x) in realisation(11).iter().enumerate() {
        let c = |lag: i64| mil_correlation(axis, lag as f64 * DT);
        for lag in [0usize, 1, 2, 4, 8, 16] {
            let n = N - lag;
            let est = x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum::<f64>() / n as f64;
            // Bartlett: var ≈ (1/n) Σ_k [c(k)² + c(k + l) c(k − l)].
            let l = lag as i64;
            let var = (-4000..=4000)
                .map(|k| c(k) * c(k) + c(k + l) * c(k - l))
                .sum::<f64>()
                / n as f64;
            let want = c(l);
            assert!(
                (est - want).abs() < 5.0 * var.sqrt(),
                "axis {axis} lag {lag}: {est} vs {want} (σ {})",
                var.sqrt()
            );
        }
    }
}

/// In-place radix-2 FFT of `(re, im)`.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

#[test]
fn the_realisation_has_the_dryden_spectrum() {
    const M: usize = 1024;
    const SEGMENTS: usize = N / M;
    const BAND: usize = 16;
    let window: Vec<f64> = (0..M)
        .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / M as f64).cos())
        .collect();
    let w2: f64 = window.iter().map(|w| w * w).sum();
    let bins = [8, 32, 96, 224, 400];
    for (axis, x) in realisation(12).iter().enumerate() {
        // Welch: Hann-windowed periodograms of non-overlapping segments, averaged.
        let mut power = vec![0.0; M / 2];
        let (mut re, mut im) = (vec![0.0; M], vec![0.0; M]);
        for segment in x.chunks_exact(M) {
            for ((r, v), w) in re.iter_mut().zip(segment).zip(&window) {
                *r = v * w;
            }
            im.fill(0.0);
            fft(&mut re, &mut im);
            for (k, p) in power.iter_mut().enumerate() {
                *p += (re[k] * re[k] + im[k] * im[k]) * DT / (2.0 * PI * w2);
            }
        }
        for p in &mut power {
            *p /= SEGMENTS as f64;
        }
        for &b in &bins {
            let range = b..b + BAND;
            let est = range.clone().map(|k| power[k]).sum::<f64>() / BAND as f64;
            let want = range
                .map(|k| sampled_psd(axis, 2.0 * PI * k as f64 / (M as f64 * DT)))
                .sum::<f64>()
                / BAND as f64;
            // Each Welch bin is χ²₂ₖ/2K-distributed; Hann's neighbours are correlated, so count
            // half the band as independent.
            let rel_sigma = 1.0 / ((SEGMENTS * BAND / 2) as f64).sqrt();
            assert!(
                (est / want - 1.0).abs() < 5.0 * rel_sigma,
                "axis {axis} bins {b}..: {est:e} vs {want:e} ({:.2} %)",
                100.0 * (est / want - 1.0)
            );
        }
    }
}

#[test]
fn the_folded_spectrum_integrates_to_the_intensity() {
    // The oracle itself: Parseval on the folded MIL spectrum gives σ² (midpoint rule, smooth
    // periodic integrand).
    let n = 4096;
    let dw = 2.0 * PI / DT / n as f64;
    for (axis, sigma) in SIGMA.iter().enumerate() {
        let total: f64 = (0..n)
            .map(|k| sampled_psd(axis, -PI / DT + (k as f64 + 0.5) * dw) * dw)
            .sum();
        let want = sigma * sigma;
        assert!((total / want - 1.0).abs() < 1e-6, "axis {axis}: {total}");
    }
}

fn world(frame: Frame, wind: Wind) -> SimWorld {
    let mut b = SimAppBuilder::new(SimConfig::new(0.02, 10_000));
    sim_models::add_library(&mut b);
    let mut sim = b.build().unwrap();
    sim.world_mut().insert_resource(WorldFrame(frame));
    sim.spawn(SimId(100), (Gravity::None, Atmosphere::Ussa1976, wind));
    sim.spawn(
        SimId(1),
        (
            PointMass3 {
                position: DVec3::new(0.0, 0.0, -1000.0),
                velocity: DVec3::new(V, 0.0, 0.0),
            },
            MassProps::point(1.0),
            sim_models::ballistics::PointMassAero {
                diameter: 0.5,
                cd0: sim_models::ballistics::Coef::Constant(0.3),
            },
        ),
    );
    sim
}

fn dryden(intensity: DVec3, heading: f64) -> Wind {
    Wind::Dryden {
        mean: DVec3::new(3.0, -1.0, 0.5),
        airspeed: V,
        heading,
        intensity,
        scale: DVec3::from_array(SCALE),
    }
}

fn wind_at(sim: &SimWorld, t: f64) -> DVec3 {
    sim.world()
        .resource::<SimEnvironment>()
        .0
        .sample(DVec3::ZERO, t)
        .wind
}

#[test]
fn the_plugin_blows_the_seeded_gusts_along_the_stated_axes() {
    let mean = DVec3::new(3.0, -1.0, 0.5);
    let dt = 0.02;
    // (frame, heading, intensity, the world direction that intensity blows along)
    let cases = [
        (Frame::Ned, 0.5 * PI, DVec3::X, DVec3::Y), // u east
        (Frame::Ned, 0.0, DVec3::Y, DVec3::Y),      // v right of north: east
        (Frame::Ned, 0.0, DVec3::Z, DVec3::Z),      // w down
        (Frame::Enu, 0.0, DVec3::X, DVec3::Y),      // u north
        (Frame::Enu, 0.0, DVec3::Y, DVec3::X),      // v east
        (Frame::Enu, 0.0, DVec3::Z, -DVec3::Z),     // w down
    ];
    for (frame, heading, intensity, along) in cases {
        let mut sim = world(frame, dryden(intensity, heading));
        sim.start(RunSeed::new(4, 9)).unwrap();
        // The same stream, drawn independently.
        let s = DrydenSpec {
            airspeed: V,
            intensity: intensity.to_array(),
            scale: SCALE,
        };
        let mut rng = SimRng::new(4, 9).stream(DRYDEN_STREAM);
        let mut g = DrydenTurbulence::new(&s, dt).unwrap();
        g.start(&mut rng);
        let magnitude = |g: &DrydenTurbulence| g.gust().iter().sum::<f64>();
        let mut before = magnitude(&g);
        assert!((wind_at(&sim, 0.0) - (mean + along * before)).length() < 1e-15);
        for tick in 0..20 {
            sim.step();
            g.step(&mut rng);
            let after = magnitude(&g);
            let t0 = tick as f64 * dt;
            for (s, w) in [
                (0.0, before),
                (0.25, 0.75 * before + 0.25 * after),
                (1.0, after),
            ] {
                let got = wind_at(&sim, t0 + s * dt);
                let want = mean + along * w;
                assert!(
                    (got - want).length() < 1e-14,
                    "{frame:?} {intensity}: tick {tick} +{s}: {got} vs {want}"
                );
            }
            before = after;
        }
    }
}

#[test]
fn turbulent_runs_repeat_for_a_seed_and_differ_between_seeds() {
    let run = |seed: u64| {
        let mut sim = world(Frame::Ned, dryden(DVec3::new(2.0, 2.0, 2.0), 0.3));
        sim.start(RunSeed::new(0, seed)).unwrap();
        for _ in 0..200 {
            sim.step();
        }
        let e = sim.body(SimId(1)).unwrap();
        sim.world().get::<PointMass3>(e).unwrap().velocity
    };
    assert_eq!(
        run(1).to_array().map(f64::to_bits),
        run(1).to_array().map(f64::to_bits)
    );
    assert_ne!(run(1), run(2));
}

#[test]
fn setup_rejects_bad_turbulence() {
    let bad = [
        (Frame::Ecef, dryden(DVec3::ONE, 0.0)),
        (
            Frame::Ned,
            Wind::Dryden {
                mean: DVec3::ZERO,
                airspeed: 0.0,
                heading: 0.0,
                intensity: DVec3::ONE,
                scale: DVec3::ONE,
            },
        ),
        (
            Frame::Ned,
            Wind::Dryden {
                mean: DVec3::ZERO,
                airspeed: V,
                heading: 0.0,
                intensity: DVec3::new(1.0, -1.0, 1.0),
                scale: DVec3::ONE,
            },
        ),
        (
            Frame::Ned,
            Wind::Dryden {
                mean: DVec3::ZERO,
                airspeed: V,
                heading: 0.0,
                intensity: DVec3::ONE,
                scale: DVec3::new(1.0, 0.0, 1.0),
            },
        ),
    ];
    for (frame, wind) in bad {
        let mut b = SimAppBuilder::new(SimConfig::new(0.02, 10));
        sim_models::add_library(&mut b);
        let mut sim = b.build().unwrap();
        sim.world_mut().insert_resource(WorldFrame(frame));
        sim.spawn(SimId(100), wind);
        let r = sim.start(RunSeed::new(0, 0));
        assert!(
            matches!(
                r,
                Err(SetupError::Model {
                    model: "environment",
                    ..
                })
            ),
            "{frame:?} {wind:?}: {r:?}"
        );
    }
    // A zero intensity needs no scale.
    let mut sim = world(
        Frame::Ned,
        Wind::Dryden {
            mean: DVec3::ZERO,
            airspeed: V,
            heading: 0.0,
            intensity: DVec3::new(0.0, 0.0, 1.0),
            scale: DVec3::new(0.0, 0.0, 30.0),
        },
    );
    sim.start(RunSeed::new(0, 0)).unwrap();
}
