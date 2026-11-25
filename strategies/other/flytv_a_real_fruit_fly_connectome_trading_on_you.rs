// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "FlyTV - a real fruit fly connectome trading on your chart" by gsoloviev, https://www.tradingview.com/script/nmnHkA02/
// The original's notice: © gsoloviev
// The original's data notice, verbatim: Data: MaleCNS v1.0, CC BY 4.0 — FlyEM/HHMI Janelia, University of Cambridge, MRC LMB,
// Google Research. Berg et al., doi:10.1101/2025.10.09.680999
// Changed: rewritten by hand in Rust for vike.

//! A spiking network of 1,325 neurons and 17,000 connections, the original's distillation of an adult male fruit fly's
//! connectome, run 10 steps a bar (leaky integrate-and-fire: a potential decays by 0.9 a step, fires at 1 and is then zeroed
//! and silent for one step, each connection's current its signed contact weight over 500, 20 injections of 0.3 noise a step
//! from a fixed-seed generator). Each bar becomes stimuli: the drop, rise and range of the bar drive the looming detectors;
//! the angle of the fast (4-bar) and slow (24-bar) log momentum, each over its running spread, is a bump of current on the
//! compass ring (50 cells) beside a constant goal 45 degrees off (36 goal cells); both momenta up drive the walk inputs,
//! a fast fall in a slow rise the backward inputs, a quiet market the grooming inputs; ten market features pick the
//! Kenyon cells (700) that join the memory code (those above their mean plus 1.04 spreads), and the bar's equity return
//! over the 72-bar return spread is the reward or punishment dopamine, which weakens the memory's active synapses onto the
//! mushroom-body output cells (they recover by 0.2% a bar).
//!
//! The read-out, in the animal's order: 3 or more giant-fiber spikes in a bar are an escape, flat for 24 bars; for the
//! first 200 bars of the network, a spike of the walk command goes long and nothing else changes the wish; after them the
//! compass decides: long while the ring's population vector points within 90 degrees of the goal, flat otherwise; with no
//! ring vector, the backward command's release (its pressure 1 spread over its own mean) goes flat and a walk spike long.
//! Long is all of the account's worth (100% of equity) entered at the bar's close, closed at the bar's close when the wish
//! is flat; both fill at the next open. Commission 0.02% a side; starts with 10,000; version 6's 100% margin.
//!
//! Measured and decided while porting:
//! - **The reference calculates on the last 1200 bars of its run only** (the original's header states it, and the
//!   reference's report counts 1200 bars against the window's 30,656): its first such bar is its first bar, with no
//!   history before it. The rule reads its run's last bar (`vike_tactics::Tactics::window`) and calculates from the bar
//!   1199 bar lengths before it, the length read off two bars in a row; a run with no window (a paper mount) calculates
//!   from its first bar.
//! - The original unpacks its connections 2,000 a bar and runs the network from the bar the last of them is unpacked on,
//!   the 9th calculated bar; here they are unpacked at once, and the network runs from that same bar.
//! - Every comparison of two numbers holds within 1e-10 as the original's do (`above`, `below`, `at_least`, `equal`); the
//!   72-bar spread is the square root of the mean square less the squared mean, over compensated rolling sums
//!   (`vike_tactics::rolling::RollingSum`); every exponential, logarithm, power, cosine, sine and arc tangent is
//!   `libm`'s, in the original's order of operations.
//!
//! Left out, as they decide no order: the brain window, the plots and colours, the steering counters of the four DNa02
//! cells, the output cells' valences, the developer's self-test, the live-bar animation, and the "Version" input's other
//! two modes (the default, the whole brain, is ported).
//!
//! The tables at the end of this file (the groups, the connections and their weights, the dopamine reach, the ring and
//! goal angles, the looming gains and the Kenyon projection) are the original's, digit for digit. They are distilled from
//! the MaleCNS v1.0 connectome, which is data under CC BY 4.0: FlyEM/HHMI Janelia, University of Cambridge, MRC LMB,
//! Google Research; Berg et al., doi:10.1101/2025.10.09.680999.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use std::f64::consts::PI;

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "L";
/// The bars the original calculates, the last of its run.
const CALCULATED_BARS: i64 = 1200;
const NEURONS: usize = 1325;
const CONNECTION_COUNT: usize = 17_000;
const KENYON_CELLS: usize = 700;
/// The original unpacks this many connections a bar and runs the network from the bar that unpacks the last.
const UNPACKED_A_BAR: usize = 2000;
const FIRST_NETWORK_BAR: usize = CONNECTION_COUNT.div_ceil(UNPACKED_A_BAR) - 1;
/// A weight's step: a step of `n` is a weight of `e^(n * WEIGHT_STEP) - 1`, signed.
const WEIGHT_STEP: f64 = 0.2055;
/// The digits every table is written in, 0 to 63.
const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_.";
/// The original's comparisons: two numbers within this of each other are equal.
const TOLERANCE: f64 = 1e-10;

// The groups (`GROUPS`).
const GIANT_FIBER: u8 = 0;
const WALK_COMMAND: u8 = 1;
const BACKWARD_COMMAND: u8 = 2;
const GROOM_COMMAND: u8 = 4;
const LOOMING: u8 = 5;
const KENYON: u8 = 6;
const MUSHROOM_OUTPUT: u8 = 7;
const REWARD_DOPAMINE: u8 = 8;
const PUNISH_DOPAMINE: u8 = 9;
const WALK_INPUT: u8 = 11;
const RING: u8 = 12;
const GOAL: u8 = 13;
const BACKWARD_INPUT: u8 = 16;
const GROOM_INPUT: u8 = 17;

// The original's inputs, at their defaults.
const LOOMING_GAIN: f64 = 4.0;
const DROP_WEIGHT: f64 = 1.0;
const RISE_WEIGHT: f64 = 0.3;
const RANGE_WEIGHT: f64 = 0.5;
const STEPS: usize = 10;
const ESCAPE_SPIKES: i64 = 3;
const ESCAPE_BARS: i64 = 24;
const RING_GAIN: f64 = 1.2;
const RING_WIDTH: f64 = 2.0;
const GOAL_DEGREES: f64 = 45.0;
const GOAL_GAIN: f64 = 1.0;
const FAST: usize = 4;
const SLOW: usize = 24;
const WALK_GAIN: f64 = 1.5;
const BACKWARD_GAIN: f64 = 1.5;
const GROOM_GAIN: f64 = 1.0;
const STEER_DEGREES: f64 = 90.0;
const RELEASE_Z: f64 = 1.0;
const WARM_BARS: i64 = 200;
const KENYON_GAIN: f64 = 0.6;
const KENYON_Z: f64 = 1.04;
const DOPAMINE_GAIN: f64 = 0.8;
const DEPRESSION: f64 = 0.05;
const RECOVERY: f64 = 0.002;
const DECAY: f64 = 0.90;
const THRESHOLD: f64 = 1.0;
const WEIGHT_DIVISOR: f64 = 500.0;
const NOISE_COUNT: usize = 20;
const NOISE: f64 = 0.30;
/// The decays the original keeps in a table, `DECAY` to the powers below this.
const DECAY_TABLE: i64 = 64;
const SPREAD_BARS: usize = 72;
/// The longest look back: the 72-bar return.
const KEEP_CLOSES: usize = 72;

/// The `at`th digit of `table`, 0 for one outside the digits.
fn digit(table: &str, at: usize) -> usize {
    DIGITS.iter().position(|&c| c == table.as_bytes()[at]).unwrap_or(0)
}

/// Two digits from `at`: the first is the 64s.
fn two_digits(table: &str, at: usize) -> usize {
    digit(table, at) * 64 + digit(table, at + 1)
}

/// The original's `a > b`.
fn above(a: f64, b: f64) -> bool {
    a - b > TOLERANCE
}

/// The original's `a < b`.
fn below(a: f64, b: f64) -> bool {
    a - b < -TOLERANCE
}

/// The original's `a >= b`.
fn at_least(a: f64, b: f64) -> bool {
    a >= b || a - b >= -TOLERANCE
}

/// The original's `a == b`.
fn equal(a: f64, b: f64) -> bool {
    let difference = a - b;
    a == b || (-TOLERANCE..=TOLERANCE).contains(&difference)
}

/// A missing value read as 0.
fn nz(x: f64) -> f64 {
    if x.is_nan() { 0.0 } else { x }
}

/// The original's own hyperbolic tangent, its argument held within 20 either way.
fn hyperbolic_tangent(x: f64) -> f64 {
    let x = (-20.0f64).max(20.0f64.min(x));
    let e2 = libm::exp(2.0 * x);
    (e2 - 1.0) / (e2 + 1.0)
}

/// The original's angle of the point `(x, y)`, from its arc tangent.
fn angle_of(y: f64, x: f64) -> f64 {
    if above(x, 0.0) {
        libm::atan(y / x)
    } else if below(x, 0.0) {
        if at_least(y, 0.0) { libm::atan(y / x) + PI } else { libm::atan(y / x) - PI }
    } else if above(y, 0.0) {
        PI / 2.0
    } else if below(y, 0.0) {
        -PI / 2.0
    } else {
        0.0
    }
}

/// `a` wrapped into a half turn either way.
fn wrapped(a: f64) -> f64 {
    a - 2.0 * PI * ((a + PI) / (2.0 * PI)).floor()
}

/// `DECAY` to the power `steps`.
fn decay_power(steps: i64) -> f64 {
    libm::pow(DECAY, steps as f64)
}

/// The running mean and spread of everything given, each call site its own (Welford's).
#[derive(Default)]
struct Running {
    count: f64,
    mean: f64,
    squares: f64,
}

impl Running {
    /// Takes `x` in and gives the sample spread so far once more than `least` values have come, else 0.
    fn add(&mut self, x: f64, least: f64) -> f64 {
        self.count += 1.0;
        let d = x - self.mean;
        self.mean += d / self.count;
        self.squares += d * (x - self.mean);
        if above(self.count, least) { (self.squares / (self.count - 1.0)).sqrt() } else { 0.0 }
    }

    /// `x` over its running spread (from the third value; 0 before or while the spread is nil).
    fn scale(&mut self, x: f64) -> f64 {
        let spread = self.add(x, 2.0);
        if above(spread, 1e-9) { x / spread } else { 0.0 }
    }

    /// How many running spreads `x` lies over the running mean (from the second value).
    fn z(&mut self, x: f64) -> f64 {
        let spread = self.add(x, 1.0);
        if above(spread, 1e-9) { (x - self.mean) / spread } else { 0.0 }
    }
}

/// The neurons' state: decayed lazily, only when something reaches them.
struct Cells {
    potential: Vec<f64>,
    last_step: Vec<i64>,
    fired_at: Vec<i64>,
    /// The cells that fired on this step, in order.
    fired: Vec<usize>,
    decay: Vec<f64>,
}

impl Cells {
    /// `amount` into cell `i` on step `t`: caught up to `t`, then fired and zeroed when it reaches the threshold, unless it
    /// fired on this step (nothing reaches it) or the one before (it only fills).
    fn inject(&mut self, i: usize, amount: f64, t: i64) {
        let fired_at = self.fired_at[i];
        if fired_at == t {
            return;
        }
        let dt = t - self.last_step[i];
        let mut v = self.potential[i];
        if dt > 0 {
            v *= if dt < DECAY_TABLE { self.decay[dt as usize] } else { decay_power(dt) };
            self.last_step[i] = t;
        }
        v += amount;
        if at_least(v, THRESHOLD) && fired_at < t - 1 {
            self.potential[i] = 0.0;
            self.fired_at[i] = t;
            self.fired.push(i);
        } else {
            self.potential[i] = v;
        }
    }

    /// Cell `i` caught up to step `t`.
    fn touch(&mut self, i: usize, t: i64) {
        let dt = t - self.last_step[i];
        if dt > 0 {
            self.potential[i] *= decay_power(dt);
            self.last_step[i] = t;
        }
    }

    /// The mean potential of `cells`, each caught up to step `t`.
    fn pressure(&mut self, cells: &[usize], t: i64) -> f64 {
        let mut sum = 0.0;
        for &i in cells {
            self.touch(i, t);
            sum += self.potential[i];
        }
        sum / cells.len().max(1) as f64
    }
}

/// What drives the network on a bar.
struct Drives {
    looming: f64,
    walk: f64,
    backward: f64,
    groom: f64,
    reward: f64,
    punish: f64,
}

/// What one bar's steps fired.
#[derive(Default)]
struct Spikes {
    giant_fiber: i64,
    walk: i64,
    ring_x: f64,
    ring_y: f64,
}

struct Network {
    groups: Vec<u8>,
    /// Each neuron's connections are `offsets[n]..offsets[n + 1]`.
    offsets: Vec<usize>,
    targets: Vec<usize>,
    weights: Vec<f64>,
    /// Each connection's learned share of its weight (only a Kenyon cell's onto an output cell moves).
    plastic: Vec<f64>,
    effective: Vec<f64>,
    cells: Cells,
    /// The cells that fired on the step before.
    spiked: Vec<usize>,
    looming: Vec<usize>,
    looming_gain: Vec<f64>,
    kenyon: Vec<usize>,
    /// Each neuron's number among the output cells.
    output_of: Vec<usize>,
    reward_reach: Vec<f64>,
    punish_reach: Vec<f64>,
    projection: Vec<f64>,
    /// Every connection of a Kenyon cell onto an output cell.
    kenyon_outputs: Vec<usize>,
    /// Those of the Kenyon cells active on the bar before.
    active_outputs: Vec<usize>,
    drive: Vec<f64>,
    active_kenyon: Vec<usize>,
    ring: Vec<usize>,
    ring_angle: Vec<f64>,
    /// Each ring cell's cosine and sine, by neuron.
    ring_cos: Vec<f64>,
    ring_sin: Vec<f64>,
    ring_current: Vec<f64>,
    goal: Vec<usize>,
    goal_current: Vec<f64>,
    walk_inputs: Vec<usize>,
    backward_inputs: Vec<usize>,
    groom_inputs: Vec<usize>,
    reward_cells: Vec<usize>,
    punish_cells: Vec<usize>,
    backward_cells: Vec<usize>,
    groom_cells: Vec<usize>,
    step: i64,
    random: i64,
}

impl Network {
    /// The network the tables write.
    fn unpack() -> Network {
        let groups: Vec<u8> = (0..NEURONS).map(|i| digit(GROUPS, i) as u8).collect();
        let of_group = |g: u8| -> Vec<usize> { (0..NEURONS).filter(|&i| groups[i] == g).collect() };
        let looming = of_group(LOOMING);
        let looming_gain =
            (0..looming.len()).map(|k| 0.5 + digit(LOOMING_GAINS, k) as f64 / 63.0).collect();
        let goal = of_group(GOAL);
        let goal_rad = GOAL_DEGREES * PI / 180.0;
        let goal_current = (0..goal.len())
            .map(|k| {
                let angle = digit(GOAL_ANGLES, k) as f64 * 2.0 * PI / 64.0;
                GOAL_GAIN * libm::exp(libm::cos(angle - goal_rad) - 1.0)
            })
            .collect();
        let mut output_of = vec![usize::MAX; NEURONS];
        let (mut reward_reach, mut punish_reach) = (Vec::new(), Vec::new());
        for (k, i) in of_group(MUSHROOM_OUTPUT).into_iter().enumerate() {
            output_of[i] = k;
            reward_reach.push(digit(REWARD_WEIGHTS, k) as f64 / 63.0);
            punish_reach.push(digit(PUNISH_WEIGHTS, k) as f64 / 63.0);
        }
        let (mut ring, mut ring_angle) = (Vec::new(), Vec::new());
        let (mut ring_cos, mut ring_sin) = (vec![0.0; NEURONS], vec![0.0; NEURONS]);
        for k in 0..RING_CELLS.len() / 2 {
            let id = two_digits(RING_CELLS, k * 2);
            let angle = digit(RING_ANGLES, k) as f64 * 2.0 * PI / 64.0;
            ring.push(id);
            ring_angle.push(angle);
            ring_cos[id] = libm::cos(angle);
            ring_sin[id] = libm::sin(angle);
        }
        let projection = (0..KENYON_CELLS * 10)
            .map(|j| digit(PROJECTION, j) as f64 / 63.0 * 6.3 - 3.15)
            .collect();
        let (mut targets, mut weights) = (Vec::new(), Vec::new());
        for e in 0..CONNECTION_COUNT {
            targets.push(two_digits(CONNECTIONS, e * 3));
            let step = digit(CONNECTIONS, e * 3 + 2) as i64 - 32;
            let w = if step >= 0 {
                libm::exp(step as f64 * WEIGHT_STEP) - 1.0
            } else {
                -(libm::exp(-step as f64 * WEIGHT_STEP) - 1.0)
            };
            weights.push(w / WEIGHT_DIVISOR);
        }
        let mut offsets = vec![0];
        for i in 0..NEURONS {
            offsets.push(offsets[i] + two_digits(DEGREES, i * 2));
        }
        assert_eq!(offsets[NEURONS], CONNECTION_COUNT, "the degrees sum to the connections");
        let kenyon = of_group(KENYON);
        let kenyon_outputs = kenyon
            .iter()
            .flat_map(|&n| offsets[n]..offsets[n + 1])
            .filter(|&e| groups[targets[e]] == MUSHROOM_OUTPUT)
            .collect();
        Network {
            offsets,
            plastic: vec![1.0; CONNECTION_COUNT],
            effective: weights.clone(),
            weights,
            targets,
            cells: Cells {
                potential: vec![0.0; NEURONS],
                last_step: vec![0; NEURONS],
                fired_at: vec![-9; NEURONS],
                fired: Vec::new(),
                decay: (0..DECAY_TABLE).map(decay_power).collect(),
            },
            spiked: Vec::new(),
            looming,
            looming_gain,
            kenyon,
            output_of,
            reward_reach,
            punish_reach,
            projection,
            kenyon_outputs,
            active_outputs: Vec::new(),
            drive: vec![0.0; KENYON_CELLS],
            active_kenyon: Vec::new(),
            ring_current: vec![0.0; ring.len()],
            ring,
            ring_angle,
            ring_cos,
            ring_sin,
            goal,
            goal_current,
            walk_inputs: of_group(WALK_INPUT),
            backward_inputs: of_group(BACKWARD_INPUT),
            groom_inputs: of_group(GROOM_INPUT),
            reward_cells: of_group(REWARD_DOPAMINE),
            punish_cells: of_group(PUNISH_DOPAMINE),
            backward_cells: of_group(BACKWARD_COMMAND),
            groom_cells: of_group(GROOM_COMMAND),
            groups,
            step: 0,
            random: 42,
        }
    }

    /// The dopamine of the last decision weakens the synapses the memory used on the bar before, then every memory synapse
    /// recovers a little; nothing moves when the memory used none.
    fn learn(&mut self, reward: f64, punish: f64) {
        if self.active_outputs.is_empty() {
            return;
        }
        for &e in &self.active_outputs {
            let output = self.output_of[self.targets[e]];
            let dose = reward * self.reward_reach[output] + punish * self.punish_reach[output];
            self.plastic[e] = 0.1f64.max(1.0f64.min(self.plastic[e] - DEPRESSION * dose));
            self.effective[e] = self.weights[e] * self.plastic[e];
        }
        for &e in &self.kenyon_outputs {
            self.plastic[e] += RECOVERY * (1.0 - self.plastic[e]);
            self.effective[e] = self.weights[e] * self.plastic[e];
        }
    }

    /// The Kenyon cells the bar's ten features light: those driven to their mean plus `KENYON_Z` spreads or more.
    fn code(&mut self, features: &[f64; 10]) {
        for (drive, p) in self.drive.iter_mut().zip(self.projection.as_chunks::<10>().0) {
            let mut d = features[0] * p[0];
            for (f, x) in features.iter().zip(p).skip(1) {
                d += f * x;
            }
            *drive = d;
        }
        let mut mean = 0.0;
        for &d in &self.drive {
            mean += d;
        }
        mean /= KENYON_CELLS as f64;
        let mut spread = 0.0;
        for &d in &self.drive {
            let off = d - mean;
            spread += off * off;
        }
        let spread = (spread / KENYON_CELLS as f64).sqrt() + 1e-9;
        let threshold = mean + KENYON_Z * spread;
        self.active_kenyon.clear();
        for (&drive, &cell) in self.drive.iter().zip(&self.kenyon) {
            if at_least(drive, threshold) {
                self.active_kenyon.push(cell);
            }
        }
    }

    /// The bar's steps.
    fn run(&mut self, d: &Drives) -> Spikes {
        let mut spikes = Spikes::default();
        for _ in 0..STEPS {
            self.step += 1;
            let t = self.step;
            let cells = &mut self.cells;
            cells.fired.clear();
            if above(d.looming, 0.0) {
                for (&i, &gain) in self.looming.iter().zip(&self.looming_gain) {
                    cells.inject(i, d.looming * gain, t);
                }
            }
            for (&i, &current) in self.ring.iter().zip(&self.ring_current) {
                cells.inject(i, current, t);
            }
            for (&i, &current) in self.goal.iter().zip(&self.goal_current) {
                cells.inject(i, current, t);
            }
            for (drive, inputs) in [
                (d.walk, &self.walk_inputs),
                (d.backward, &self.backward_inputs),
                (d.groom, &self.groom_inputs),
            ] {
                if above(drive, 0.0) {
                    for &i in inputs {
                        cells.inject(i, drive, t);
                    }
                }
            }
            for &i in &self.active_kenyon {
                cells.inject(i, KENYON_GAIN, t);
            }
            for (dopamine, dopamine_cells) in
                [(d.reward, &self.reward_cells), (d.punish, &self.punish_cells)]
            {
                if above(dopamine, 0.0) {
                    for &i in dopamine_cells {
                        cells.inject(i, DOPAMINE_GAIN * dopamine, t);
                    }
                }
            }
            for _ in 0..NOISE_COUNT {
                self.random = self.random * 48271 % 2_147_483_647;
                cells.inject((self.random % NEURONS as i64) as usize, NOISE, t);
            }
            for &source in &self.spiked {
                let (from, to) = (self.offsets[source], self.offsets[source + 1]);
                for (&target, &weight) in
                    self.targets[from..to].iter().zip(&self.effective[from..to])
                {
                    cells.inject(target, weight, t);
                }
            }
            self.spiked.clear();
            for &id in &cells.fired {
                self.spiked.push(id);
                match self.groups[id] {
                    GIANT_FIBER => spikes.giant_fiber += 1,
                    WALK_COMMAND => spikes.walk += 1,
                    RING => {
                        spikes.ring_x += self.ring_cos[id];
                        spikes.ring_y += self.ring_sin[id];
                    }
                    _ => {}
                }
            }
        }
        self.active_outputs.clear();
        for &n in &self.active_kenyon {
            for e in self.offsets[n]..self.offsets[n + 1] {
                if self.groups[self.targets[e]] == MUSHROOM_OUTPUT {
                    self.active_outputs.push(e);
                }
            }
        }
        spikes
    }
}

struct S {
    network: Network,
    previous_open: Option<i64>,
    /// The calculated bars so far.
    calculated: usize,
    /// The closes before this bar's, newest first.
    closes: VecDeque<f64>,
    returns: RollingSum,
    squared_returns: RollingSum,
    fast: Running,
    slow: Running,
    backward: Running,
    previous_equity: f64,
    bars_ready: i64,
    cooldown: i64,
    want: f64,
    bump_error: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        network: Network::unpack(),
        previous_open: None,
        calculated: 0,
        closes: VecDeque::with_capacity(KEEP_CLOSES + 1),
        returns: RollingSum::new(SPREAD_BARS),
        squared_returns: RollingSum::new(SPREAD_BARS),
        fast: Running::default(),
        slow: Running::default(),
        backward: Running::default(),
        previous_equity: f64::NAN,
        bars_ready: 0,
        cooldown: 0,
        want: 0.0,
        bump_error: 0.0,
    })
}

impl S {
    /// The close `k` bars back, missing before the first calculated bar.
    fn close_back(&self, k: usize) -> f64 {
        self.closes.get(k - 1).copied().unwrap_or(f64::NAN)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.02), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let length = self.previous_open.map(|open| bar.timestamp - open);
        self.previous_open = Some(bar.timestamp);
        let calculated = match strategy.window() {
            None => true,
            Some((_, last)) => {
                length.is_some_and(|length| bar.timestamp > last - CALCULATED_BARS * length)
            }
        };
        if !calculated {
            return;
        }
        let close = bar.close;
        let before = self.closes.front().copied().unwrap_or(close);
        let bar_return = close / before - 1.0;
        let sum = self.returns.next(bar_return);
        let sum_squares = self.squared_returns.next(bar_return * bar_return);
        let deviation = if sum.is_nan() || sum_squares.is_nan() {
            f64::NAN
        } else {
            let mean = sum / SPREAD_BARS as f64;
            0.0f64.max(sum_squares / SPREAD_BARS as f64 - mean * mean).sqrt()
        };
        let volatility = nz(deviation) + 1e-6;
        let range = (bar.high - bar.low) / before;
        let back = [1, FAST, 12, SLOW, 72].map(|k| self.close_back(k));
        let return_over = |past: f64| nz(close / past - 1.0);
        let fast_momentum = nz(libm::log(close / self.close_back(FAST)));
        let slow_momentum = nz(libm::log(close / self.close_back(SLOW)));
        let z_fast = (-4.0f64).max(4.0f64.min(self.fast.scale(fast_momentum)));
        let z_slow = (-4.0f64).max(4.0f64.min(self.slow.scale(slow_momentum)));
        let heading = angle_of(z_fast, z_slow);
        let goal_rad = GOAL_DEGREES * PI / 180.0;
        let held = if above(strategy.position(), 0.0) { 1.0 } else { 0.0 };
        let equity = strategy.equity();

        if self.calculated >= FIRST_NETWORK_BAR {
            self.bars_ready += 1;
            self.cooldown = (self.cooldown - 1).max(0);
            let previous_equity =
                if self.previous_equity.is_nan() { equity } else { self.previous_equity };
            let equity_return = nz(equity / previous_equity - 1.0);
            let reward = (-3.0f64).max(3.0f64.min(equity_return / volatility));
            let (pleasure, pain) = (0.0f64.max(reward), 0.0f64.max(-reward));
            let net = &mut self.network;
            net.learn(pleasure, pain);
            let feature = |x: f64| hyperbolic_tangent(x / volatility / 3.0);
            net.code(&[
                feature(return_over(back[0])),
                feature(return_over(back[1])),
                feature(return_over(back[2])),
                feature(return_over(back[3])),
                feature(return_over(back[4])),
                feature(range),
                held,
                0.0,
                1.0,
                feature(bar_return),
            ]);
            let drives = Drives {
                looming: (DROP_WEIGHT * 0.0f64.max(-bar_return)
                    + RISE_WEIGHT * 0.0f64.max(bar_return)
                    + RANGE_WEIGHT * 0.0f64.max(range))
                    * LOOMING_GAIN,
                walk: 0.0f64.max(z_fast.min(z_slow)) * WALK_GAIN,
                backward: 0.0f64.max(-z_fast)
                    * 0.0f64.max(hyperbolic_tangent(z_slow))
                    * BACKWARD_GAIN,
                groom: 0.0f64.max(1.0 - (z_fast.abs() + z_slow.abs()) / 2.0) * GROOM_GAIN,
                reward: pleasure,
                punish: pain,
            };
            for (current, &angle) in net.ring_current.iter_mut().zip(&net.ring_angle) {
                *current = RING_GAIN * libm::exp(RING_WIDTH * (libm::cos(angle - heading) - 1.0));
            }
            let spikes = net.run(&drives);
            let backward_pressure = net.cells.pressure(&net.backward_cells, net.step);
            // The grooming command's release changes no wish, but reading it catches its cells up, which the next steps'
            // decays start from.
            net.cells.pressure(&net.groom_cells, net.step);
            let backward_z = self.backward.z(backward_pressure);
            let ring_vector =
                above(spikes.ring_x * spikes.ring_x + spikes.ring_y * spikes.ring_y, 0.0);
            if ring_vector {
                self.bump_error = wrapped(angle_of(spikes.ring_y, spikes.ring_x) - goal_rad);
            }
            let ready = self.bars_ready > WARM_BARS;
            if spikes.giant_fiber >= ESCAPE_SPIKES {
                self.cooldown = ESCAPE_BARS;
            }

            if self.cooldown > 0 {
                self.want = 0.0;
            } else if !ready {
                if spikes.walk >= 1 {
                    self.want = 1.0;
                }
            } else if ring_vector {
                let steer = STEER_DEGREES * PI / 180.0;
                self.want = if below(self.bump_error.abs(), steer) { 1.0 } else { 0.0 };
            } else if at_least(backward_z, RELEASE_Z) {
                self.want = 0.0;
            } else if spikes.walk >= 1 {
                self.want = 1.0;
            }
            let position = strategy.position();
            if above(self.want, 0.0) && equal(position, 0.0) {
                let size = share_of_worth(&strategy.sizing(), 100.0, close);
                strategy.entry(NAME, Long).size(size);
            }
            if equal(self.want, 0.0) && above(position, 0.0) {
                strategy.close(NAME);
            }
        }

        self.closes.push_front(close);
        self.closes.truncate(KEEP_CLOSES);
        self.previous_equity = equity;
        self.calculated += 1;
    }
}

// The tables, as the original writes them (the `//!`'s last paragraph has their licence).
/// Each neuron's count of outgoing connections, two digits each, in neuron order.
const DEGREES: &str = "\
    01000H0P0500000K0w0k040b000701030B0B00050407010q060g0P050f070A0d050D03050B1y030408000B0707\
    0h0K0n0f0401070705040b060l0R0A03090103030A0404090g0F07090G0D2O010H010d0.07050008060g050x07\
    0r0703030808040W0402070p0y0v040u03000m010w000b060x0.0w0x0C03060q0602100t0A0g000t0t0x0q0506\
    0C0r05030s0_0501020303040m0a080E0306020l030w050305050403020404000d05030b0S00020n060E0d0e04\
    0v030X030D0g0g0E0708050304000104040b07020f06050203030304050C0705040203060b0608030B020H0102\
    020R0101010a030X050306070C020504050307030W030T0704040305070S03020T0308080Q0U070H0Y0503020W\
    060203020Z04030303030J020F04040Y01040R02040508010208050T0304060W05050408010105040805070101\
    0S0L05040A040905020C08070504080Z030B0E040803030o0L010G0Z0H0E010G060G03050E0I0P0X04030B0804\
    0G0F060L0E080J0306020H0E050B0701010B060H0U0509040W0G030I0I080C0G020409090A0C0F070F0F030B0H\
    060L0J0N070D0D080E0C04030C0L0H070E0D090A090D0H0G090B09040I0A090B0H0J0I0B0D080F0B0A08080609\
    0B05080F060B06030E0K090E08070E0504080R0G0G08010F0A0G0C0D080C0D0I0A020D0B0H0C0C0A0G04080E0D\
    010B0B0H080G0B080G0F09080F0A090F0H0C06090G0A0B0G060G070E0L090I0K090A0K0D0B0I050D0C0H080705\
    090G0B010B0H0d08080709090G0F0K0G0B0A0A0F0C0709060H0G05090E0C0N0C0G04060I0C0A070D0C0E0V0E0D\
    070E0B0A0H09070C0F080D090D080H0D0F04090B0G080A060C0H09030H0E0G0J0G0H070B09040B0N090A0J020G\
    0E0B010B0A0K0A0F07090G0D0M0A08090A0B0F08090A0H0D070C0A0A08090B0B0C090I0B0C09090K09090D0D0D\
    070F080B08080H0J0F0606080708070G0B0D0C0B090608070B0E0B0D070C070A080708090I080A090C0C010G0G\
    0807090D0B080B0B0A0C090F0D0F0D07080B09090G0B0807090C0605070B0C060A0C0H030E070D0A070E0F0F0B\
    0C0D0D0G0O0F0B0A0b0J0H0C0B0G070B080D0F0K0B0B0807070F0F03090J0I08080C08090K0D080H0E0E030B0B\
    090A040A060A0G0A0I0M0901080709010B0C060F060A0K0D0C0B070A0C080C090C0F0A0C0F0G0H080G09090i09\
    030A090907080B09030E070G0E050A0B080G0A090F08070E070H0g0B080E0C09080A0705060F0G0A070B0G0F09\
    0C040j0E0E0G0B0504090C070A0B0F0M0F0G060C070C08030D080805080906060A08030C0609060A080B080809\
    060D0D0G080907090i090A03090F0A0B0H0908080E0D0C0709070A090J060709080B0A06090C080A060C0I0B08\
    0D0J0H0G0709080G0D0B0G050B09070B0K090C090E09090808070E090i0D06040F0G0I08090207090D090D0808\
    080303080A090806070D0706080A08080A07050J07070906090A0F090609070H0B08040A07090806050F0A090E\
    010A070E0907080G060608030B03080G09040908080C02050G06090D0L05060C0y060t05040401050j0K04010m\
    0p03010505070c0g020d040L030c0i00010m0h0i0e0Z0W0n0d040A020J030A060u01030o060t06010904040x04\
    0307040T0C05030702070B050207020605080m0c0g0m0n0402080A0K0I070A0503080A0o0H08040A0L04040706\
    0m0602040f0g0A0c030F050f0k0a0F0J0606050n03030B0H0907090D000e060406060A0c0s0W010A030708060B\
    070F030r0W0J05050704060A070901090B060703070E0708060A0x040E0H080C0K090G020L0C05020701080403\
    080A0E0I050a0h01070G0307030t0C010A060900";
/// Each neuron's group, one digit each.
const GROUPS: &str = "\
    00777337CC77321777FF771C2C927G7CE9GEH9777FHH7C9CC75HG25CGC77H5797HEHHC9EH77979BCCHH4G9CECG\
    C5FHHGHCE9HCCCBCH4C7C7CGCCCCGHCC55CCGC4CCCC5GGC5ECCG55EE5CC56555CGCH55H755E57C55C647CG7CCE\
    CECE6CC7H755545HEC55CHC5555556H55555C75595755565BBC5C5555G5555E55C5CG5555G655C5G66CB6C5E5C\
    C7E7CE555B6867BC5BC558DBEG5CB55C77BG555B65BBBC6586BDB766BB56CB665DBEC656C6676D6B8666C5B66B\
    66B66667B766C6D7B6C6CBDBC6BC6666BB66666D66766C666666666BB66666666666666676666666666666666D\
    6BC6D66B6666666B8666667666666666686666666H66676666666666666666666666666D66666666666H66666H\
    666B66C6666666666666666B66H666666B6666D666C6C666D66666C666666676666666668666666666866666B6\
    66B666666666666666666666666666666666666666666666666666D6666D666666666666666666666666666766\
    66666666D666666666666666666H6666666H66666666666666666C66666667666666666656666666666D666E66\
    66H6666666656C6B6666D6666666666666666666666C6H6D6666686666H7666666D6666C6666666D6666666666\
    67C6666HH666666666D666DH666866D66656C6D6666D666666666C66E66666676666666666D666666666686666\
    6666666666686666666666666666C6686666676666666688666D86666666666H666666666666666866666B6666\
    B8666676666868666768668867668D87C7CEC5BBC95BCC577HCCCECE7ECC77CCCCCCCCH75779DC75C5C5BG5BCB\
    558C6EE556DH5557F7CCCCCEED77797F56GC7676678ECC85ECC7CB7GCCC77ECCCE7676B873CCCGB7CCC775C678\
    D65CC6995ED676766D856666D6C566C666676C886B6H866667CC7D676EC676664";
/// How strongly the reward dopamine cells reach each mushroom-body output cell, one digit each (of 63).
const REWARD_WEIGHTS: &str = "\
    0z0501002051000000.0900009000A000000000000000000000000000000000000000A0004J00000U0000000CE\
    7000000";
/// How strongly the punishment dopamine cells reach each mushroom-body output cell, one digit each (of 63).
const PUNISH_WEIGHTS: &str = "\
    0080Mg4G021.O18F900E0089800A0000A304133211230221010120001202LA3711E7000L910005190007000014\
    7001080";
/// Each looming detector's gain, one digit each: 0.5 plus the digit over 63.
const LOOMING_GAINS: &str = "\
    nSsi6zmo8SNwfqSEZ4qemMzunCT3AhlzLNUC8UEgSqiKqpOIh9D0oginTa97gUameZZJ2SEQsF4IIgZngQpB16kTAW\
    AiSOJeN67zviHznjSH6v";
/// The compass ring's cells, two digits each.
const RING_CELLS: &str = "\
    090P0V0j0l0m0v151F1M1i1m1.2v3863989IDLDnE6EvFwHWHaHbHiHkHpHsHtHuHvHyI8IaIbIcIrI.J3J4J6JAJB\
    JSJYKHKUKV";
/// Each ring cell's angle, one digit each, in 64ths of a turn.
const RING_ANGLES: &str = "0Tn4QIMxe59DCOWstq3cgKXlup7zA2oSVRtqwaZGmU.fiEkqBG";
/// Each goal cell's angle, one digit each, in 64ths of a turn.
const GOAL_ANGLES: &str = "g9.THA_Hrb.JI2fWMDkbKOYMYJMMk_YlEo7f";
/// The connections, three digits each, ordered by their source neuron: the target (two digits) and the weight's step (one).
const CONNECTIONS: &str = "\
    0Ih07I0LG0eD2mE2xE3UA6HL7lJG3HIZCIiKIj6IkCJ8JJM9JuDKFH0BB0SF0iB1DF1jA2j92zC3UL4ZL4_L5PC7OE\
    9kIGIIHMKHXDHdAHe6I1LIyLJ0KJDCJEDJpLKIL0wl0_h2Xo4Hj53k0B70K719J1AL2mA2xB3UJ3YG5tG9aFBDKDVE\
    HNDHm1I17IsBJMKJbJJwJKTK0NJ0PH0WI1GH1OJ1QJ1bK1cH1dH1fK1kH1oK1pJ1qJ1vH1yH1zJ21G22K24L28H2BK\
    2CL2JL2QJ2SL2qI34C38H3rH4IJ4WB4cL4jK5IJ96JDnJFwJHOLHlKHvJHzLI5FIAHIGJIcLIgFJ2CJ4LJGFJIHJTF\
    JUGJZIJlKJrFK6HKcJ08l0Nh0jj15i1Gm1Mi1Qi1Ym1bl1fl1ph1rn1vm1yl23n24h2Bk2Ck2Jl2am2qk3Oo41o49i\
    4Dm4xm5Ik5MiDLnHOmHQiHhmHkoHolHsrHwpI5jINjIbiIdmIekJ6qJFlJHsJZiJll0Uj0dp0.k1Zj02J03E0QE0SD\
    0iF0kF0nK0wF0_L10K1CF1DK1jH1lL2cG2jG2zG4kIE5LEVLFfKFzLHXKHdDHmKHqKI1LI6KIkEIm6IsLIuKJ8EJDE\
    JEFJX6JpI06h0Rj0Ti0ui1Pi1nq4nhJRp0QF0SC0kI05G0JD0L90TF0UKGDJIn9J9LJRGJc8JhL0Fl0Kr0Lr0kj0.i\
    IXkJMiJQhJctJgpJun05y0Dm0Ik0riJ9o0BF0QJ0kHHmL0Gi0ei1Li26jIZhInpJcv05h0VL17E1GL1OL1QH1XE1bK\
    1cG1dK1fJ1kL1oL1pF1qH1rI1uL1zH21L22K24H28L2AH2BH2JK2QH2SI2qI2rG3AF3OI4DC4xD5IL6LD9IDEyHHQK\
    HRKHSHHWHI5LIAKIGKIaKIeLJ3GJIHJJLJZLJlHK6LKHB0Ci0Rh0Ti0uq1nhJ9l0Nn0lk1Om1cj1dm1km1ol1pl1qk\
    21m22n23i28l2Kh2Qm2Sl2gl2kn2nm34l46j4ji5Aj5yr60rCthFwiHQhHlkHuqHvpHxiHymI5kIGjIghIrhJ4sJTh\
    JUuK6mKcm0BW0FW0KW0SW0XW0kW16W2hW45W8QW9aW9nW9.WBVWD7WDVWDbWEhWF3WGZWG_WHNWHPWHmWI1W01h0Di\
    0Tk0uh1nr02F03J04G0BB0QC0kC0nH0wB0_G10I1CE1DJ1jK1lE2XF2cI2jJ3PJ4ZL4kLAuLFfLFzKGCKHdLHmKHrL\
    I1GI6GIKLIjGIkDIm6J8EJDEJEFJQKJX5JoJJpKJuL05i0Dt0Os0Rs0rr1SiJRj05D0AK0DL0GK0JC0L50eD0rLIXD\
    Jc60to1Gh1Qj1bl1fj1ql1vh1yj21h24j28j2Bk2Jl2oi2qm2wk3At3ro3tp5Ij5qi74hHWoHbmHhiHtqI5kIAjIGi\
    ILoIakJ3qJBhJCnJIiJThJYmJZjJlj0Jp17h1Si34hInp0AW0FW0QW0SW0UW0dW0kW0.WDbWHPWI1WIZWJKW05E0C3\
    IYJ0Js0Oi1ShIYhInr06L0yG1JD1TD1gG1hL1tG20H2WI2yHKiE0AW0GW0UW0cW0dW0eW19W1AW1BW3YW3_W45W4lW\
    4sW55W5SW5WW5hW5lW5nW5xW64W67W6YW6eW6iW6kW6mW6tW6wW70W7LW7PW7YW7dW7mW8AW8MW8UW8cW8lW8tW9HW\
    9KW9QW9TW9XW9bW9iW9vWA3WA7WAMWASWAXWAwWB0WB2WB6WBBWBLWBSWBYWBdWBeWCCWCFWCKWCaWCjWCpWD1WDCW\
    DMWDOWDXWDfWDkWDuWDzWEAWEOWETWEhWEmWF6WF7WFBWFMWFOWFRWFSWFZWFhWFnWG8WGBWGGWGJWGKWGOWGRWGfW\
    GgWGiWGxWH2WH6WH9WHGWHPWHmWI1WIZWIiWIjWIvWIwWIxWJ5WJKWJLWK1WKSW1Bm1Li3Sm0Xm16r9ahIll05B0J9\
    0L70UL0bLIZLJ9JJc806J0yG1JE1TF1gG1hH1tF20K2WF2yGKiD0yk11m1Jq1gn20p2yp31k03l0Bj2ch2zo5PlJEj\
    Jph08l09m0Nk1Mq1bl1ch1fk1im1oh1pl1rl1vk1yl1zk2Ci2Jj2dl2ql35n3Ok41k49m4Tl4fm5Ii5MkCthDLpEer\
    HOjHQjHipHomHphHsoHwlIdlIejIfkJ6jJZlJljJml0AW0BW0FW0KW0QW0SW0XW16W2hW45W64WAZWCTWDVWDbWG_W\
    HNWHmWI1WKTW0Nm0Pj0mh0vl1Om1cm1dn1ko1om1pk1ui1zj21m22h23k28j2Ck2Qk2Sl2kk2nh2vo3eo4Eu4Gk4Il\
    4Wp5Yl5yi74i8Yk96oC5mEvhHQmHuoHxoHyqHziIGjIOkIdiIemIzjI_iJIhJanK6mKcn0Nl0vm1Oh1cm1dk1fh1kl\
    1on1pm1zl21h22j23h2Cj2dj2kk2nj2vi3el4Bj4Im4Wn5Yk5yi8Yk96lC5mCtsEvqHQkHlkHxnHzjI.oJSjJanJmk\
    K6mKAjKVpKcl1ll2co4RjJMh01q14i1Tj1hq1tj2Wq2ihKio0OC0RC0T90rC26KInAJRG0Cj0Oi0ur4dhJ9m01q2Fh\
    4MiCOh0Vq1Fn2Kl2do2nm2wp35i3Oo3ci3rn3ti46i4Dj4jl4xk5Al5Yl5yj63h8YmDnmE6qHWrHajHbpHomHtoHxj\
    I8jIapIckJ3rJAnJBsJClJYqJmi0Dq0Ol0Rq0rj1nhJRh0Nm0ll0mn1Oi1cn1dm1kl1ok1pm1zk21h22i23h2Cj2Qj\
    2Sh2dj2nk2vn4Bi4Ej4In4Wk5Yl5yh8Ym96mC5iCtsEeiEvoHOhHQlHSiHjiHllHuhHxkHykHzmIekI.kJShJakK6m\
    KViKck04B0BE0SA0_A1CA1DF2XB3P64HC4ZJ53E5UK9kLFPLGILHMJHXDI1KI6AIyKJ0LJDCJEFJoLJpLKIKKOL2cq\
    4Rl4lm5lk5nm5tjHehI3iJ8jJwk1Jo20k31l01q2Li2Oi3Mi3Qj3lh3piI9lJ.iI6H0AW1SWHPW1lm2cmGfh18K1TF\
    1hH1tH2WF7hJHfHH_KKNIKiJ0Jn1SiInrJFj0yI1JE1gI31L0yH1JI1gH20G2yIBhEBpLDNJERK08i09j1Gj1Qi1bk\
    1fj1qi1rk1vi1yh23h24i2Bk2Ci2Jk2qj2sj2wj35n3Ol41o4Do4xn5Il5qi5wtHOkHQiHapHhjHkpHolHwkI5kI8m\
    IGjIbqIdiIelJ6lJZlJll0AW0BW0HW0KW0QW0UW0dW0kW3uWHNWHPWHmWIZWJKWKTW0Jr0Oh34i4JhHRiIYkInq0yE\
    11K1JC1gE20F2yD31LDNLITJ0Gi0Ko0bi1Ah1Bh2cm4lh5lnBDjCDjDVhHmnImiIxlJ8hJbh0Kl0bi19i1Bh2ck4lm\
    5ln5nhHmoImhIxnJblJgh0AW0GW0QW0UW0bW0cW0dW0eW19W1AW1LW2hW2uW3YW3_W45W4OW4lW54W5DW5WW5eW5hW\
    5lW5nW5pW64W67W6CW6UW6aW6iW6mW6oW6sW6tW6uW6wW6zW7DW7PW7dW7rW7tW7xW84W8AW8MW8UW8WW8gW8nW8tW\
    8xW94W99W9AW9HW9QW9TW9bW9iW9lWA3WA8WABWAMWAPWARWAXWAbWAvWB0WB5WBHWBJWBRWBVWBmWBnWC4WC6WCDW\
    CFWCPWCRWCWWCaWCfWCpWCzWD1WD5WDDWDOWDUWDZWDfWDkWD_WEPWESWEfWEtWF2WF4WF6WF7WFBWFIWFZWFcWFhW\
    FmWFqWFrWG6WG7WG8WGBWGFWGKWGLWGQWGbWGeWGfWGgWGnWGqWGsWG_WH9WHGWHJWHPWHmWI1WIZWIiWIvWIwWIxW\
    J5WJKWJfWJxWK9WKBWKMWKSWKhWHdh0GW0xW1CW3PW4HW53W5PW5nW6mW7JW7OW8tWETWH9WHdWHeWH.W0En1Oj1Qj\
    1Xo1kj1qh24l28j2Bj2Jh2Kl2Qi2Sj2gj2kh2om2qh46m4jl5Al5Ii60iCdiDnoE6lI5lIAiIGiIcnIrmI_rJAnJBm\
    JChJGlJIjJYiJZkK6jKci08H0NF0PD1OH1QI1bH1cI1dH1fL1kG1oH1pH1qH1rK1vJ1yJ1zH21I22J23J24L28H2BK\
    2JI2KL2QG2SF2kI2qH34G38K3eK46D4EK4IL4WK4cF4jF5AL5IK96CCdKHQJHlDHuGHvHHzKI5GIAJIGFIOLIdKIeK\
    IgFJ2DJ4KJIHJTGJUDJZHJlJK6EKcJ1TD1hF1tF2WF7hK8RLKiE0yJ1JE1gJ20G2yG05E0DF0J90OE0RF0YK0rGJ9A\
    0LW0eWHPWJKWJgWJuW08l09i0Nj0jq1Gl1Qh1bl1ci1fl1io1pj1rh1vj1zh23i24h2Bh2Cl2dk2ql35q41i49m4To\
    4fp5Mk6LjDLoEerHOhHQhHSiHipHonHpjHslIekIfiJGiJZiJejJmn0Jq0Rh17h1SlInr08K09K0NH17K1GH1NE1QH\
    1YD1bI1dJ1fI1kL1oK1pK1qJ1rJ1uI1vH1yJ21K22I23L24H28L2BH2CI2GF2JH2QL2SK2aF2pH2qK35A3tE5HG5IG\
    5wE63H74G98BHOKHQKHkKHoKI5IIGKILGIdKIeKJ6JJCLJFFJHHJIIJZLJlHK6GKcG0DD0OA0R90T90r81nCInG08K\
    0NG0ZE0mK1OL1cI1dH1fJ1kK1mK1oJ1pJ1qK1rJ1yK1zH1.I21K22J23H24I2CJ2HG2QL2qL3oG41J4BH4TE5IJ5ML\
    5yHCtJHOJHQJHgHHnJHxFI5FIAKIGLIdHIeJIzGI.JJIJJSKJZLJaIJlKK6IKAFKVF01q0zi2Lh2Oh3Kh3piIoh0To\
    IntJRw1hk2ilKio0ht0pi0yp1Jt1gp20j31iKih05v0Do0Jx0Op0Rm0ro0urJ9q0po1hp2iiKin1Fr2gl2on2so38o\
    3en3tk41n46j49j4Ik4Tn4Wn4fo5Mk60m96oC5iDnsE6oFwnHhlHvlHwiILmIcqIrqI_hJAqJBoJCiJYl05p0Jn4ch\
    IYh0AWHPW0hk0pt1Jm1Tq1to2ijKit08K0NH0lH1GJ1OI1cI1dI1kJ1oH1pI1rH1vI1yL1zH21H22J23I24L28K2CK\
    2JE2QI2SH2kJ2qK2vF34K3eB46F4EC4GC4cE4jE5AJ5ILC5HHQGHlFHuLI5KIODIdJIeHIzKJ4KJILJUFJZLJlIK6K\
    KcJ08K0NG0VG17D1GF1NH1OJ1QK1XD1bJ1dJ1fI1kL1oE1pH1qH1rF1uJ1vJ1yK1zH22L23K24H28J2AE2BI2GK2JK\
    2QH2SK2kJ2oJ2pL2qH2rD3AD3OK4DE4xC5IK6LF9ICEyGHOLHQLHSFHWLHbIHtFI5HIAGIGIIaHJ3KJIIJZJJlHKHC\
    KcH08K0NJ17J1GG1NB1OK1QK1bH1cK1fK1kE1oK1pK1qJ1rK1uE1vG1yH23J24G28L2BI2CK2GE2JJ2QK2kL2pE2qH\
    2rK35C3tC5HD5IH5wF63C74E98BHOKHSIHaGHkKHoHI5HI8HIAFIGJILDIbHIdKIeLJCKJFKJZIJlHK6GKcI0Em2ek\
    4riHVj08K0NH0WE0tI1OI1QJ1cH1dG1kF1oK1pH1qG1rL1vF1yG1zK21H22J24K28H2BK2CK2JK2QG2SH2kG34K38K\
    3eL3rC4IK4JE4WF5IJ96EDnHFwIHOIHvJI5IIAGIGGIcLIdKIeKIgLIrJJ2CJGEJIIJTGJZIJlJJrBK6GKcJ1Jp20m\
    31l08l0Nl0ji12k1Gm1Mn1Oh1bi1ci1fm1mi1ok1pm1rk1ui1vn1yl1zj1.m23m2Bh2Cn2Jh2dj2qn35r3ei41o49n\
    4Ei4Tp4fr5Ml74jC5mEejHOmHQlHipHonHprHwhIdlIekJetJmlKUnKbj2jK08H0NI15H17K1GG1NF1OK1QG1YI1bG\
    1cK1dG1fH1qH1rI1uI1vF1yG21I22K23H24G2BH2CH2GE2JE2SJ2aK2pE2qF35C3tC5HF5IE5wF63A74E98DHOJHQK\
    HSKHaHHkKHoJI5HI8LIAHIGJILCIbIIdHIeGJFHJIHJZGJlGK6HKcF08k0Nn0mi1Oi1cm1fi1ii1on1pm1yh1zk1.q\
    21i23j2Cj2Hm2do3ep5qi8YjC5oHOiHQiHgsHojHplHxhIdlIenI.nJSrJaiJmqK6jKUpKVlKcl0Dk0Oq0Rn0rn0uj\
    J9h08I0NH0VJ17E1GG1NG1OJ1QF1XB1bI1cF1dH1fK1pE1qF1rK1uH1vJ1yJ1zG23J24H28H2AG2BG2GJ2JJ2QL2kJ\
    2oK2pK2qG2rE3AE3OK4DF4xC5HL5IL6LF9IDEyGHQLHRKHSFHWKHbGHtLI5HIAIIGHIaJJ3HJIGJJKJZKJlGKHDKcK\
    08H0NF0VL17G1GH1OK1QH1XC1bG1cI1dM1fJ1oJ1qI1rI1vI1yJ1zH22H23H24I28J2AH2BG2CL2JH2QL2SH2kL2oI\
    2qF2rF2tG2wM35M3AF3cL4DE4xI5IH5qG6LF9IEEyGHQKHRDHSHHWKHtII5HIAJIGGIdMIeMJ3KJIGJJFJYLJZGJlH\
    K6KKHIKcJ08G0NG0ZD12M1GI1OI1QK1bJ1cG1dH1fF1iK1kJ1mJ1oE1pG1rJ1vH1yG1zI1.L23F28L2BK2CI2HE2JL\
    2SJ2kK2qH3oK41G4TH5IK5YKHOIHQEHgHHnFHxCI5IIAHIGFIdJIeIIfKJIGJSHJZIJaFJeIJlLK6IKAIKUIKVKKbH\
    KcJ08K0NF0PJ0lL1GH1OI1bI1cG1dF1fJ1kJ1oG1pG1qJ1vL1yI1zI21G22H23K28K2BL2CL2JF2KJ2QI2SH2kH2qG\
    2vI34I3eC46F4ED4jC5AL5IJ96IC5HHOLHQIHjLHlDHuIHyJI5LIGIIOBIdHIeHIgFJ2JJIJJTLJUEJZLJlGK6JKcH\
    05I0DE0GI0J60OI0RG0UA0eB0rM27DHPBJ9H1hh2ijKim2sm3cm4Eg6Lk74lHSh08G0NK0WL1GH1OJ1QH1bK1cH1dF\
    1fH1kG1oH1pH1qK1rK1yH1zI21H22J23K28I2BK2QH2kH34B38H3rF4IH4WC4jJ5IH96CDnKFwHHOLHvHHzJI5JIAJ\
    IGGIcKIeKIgIJ2CJGGJIGJTDJUGJZJJrFK6IKcI00q25k2Vh2Zj2_i3Vh00p3dh08E0NG0WD0tK1GH1OG1QH1bH1cH\
    1dG1fE1kF1oG1pG1qG1rI1vD1zI21I22G23J24J28J2BK2CJ2QH2SH2kI2qJ34E38L3rD4EK4IL4JJ4WE4cJ5IJ96D\
    CdLDnHFwIHOJHQLHvIHzLI5FIAHIGGIOJIcHIdJIeLIgGIrJJ2DJGEJIEJTEJZKJlLJrEK6IKcH08L0NI0VG17D1GH\
    1OL1QJ1XE1bH1cI1fK1oG1pJ1qH1rG1vJ1yG21L22K24L28J2AG2BI2JJ2QK2SL2qJ2rC3AI3OL3tL4DD4xE5IK6LE\
    9IEEyEHQLHRFHSFHWLHbIHtII5IIAJIGJIaHJ3LJIHJJIJZJJlGK6JKHCKcL0AD0GG0OH0RG0TD0rFIZAIn5IqDJRG\
    08i0Nn1Ol1cn1di1fm1in1kh1mp1ol1pm1vk1yl1zl21i23l2Cl2Hl2do35i3eo49i5Ii5Yj5qiC5oHOjHQlHgsHok\
    HppHxiIdlIejI.kJSoJUjJmoK6iKUqKViKck08F09F0NI0jJ15L1GH1MG1QI1YD1bF1cI1fH1pH1qJ1rI1vG1yH1zK\
    22F23H24K28K2BJ2CF2JJ2QH2SI2aD2dL2kK2qG2sC35I5IF5wKDLKEeIHOGHQIHhGHsGI5JIALIGIINEIdHIeHIfG\
    JFIJHDJIKJZIJlKJmCKbJ08H09H0NH0jJ12I1GH1MG1QI1YD1bG1dL1fF1kI1oJ1pH1qK1rH1vG1yI1zL21G23J24I\
    2BH2CK2JH2QL2SK2aC2kF2qG2sD35L5HL5IHEeEHOIHQGHhJHsEI5IIAIINGIdFIeJIfFJFCJHEJIJJZHJeKJlIJmC\
    K6LKcI08J0WK1FI1GH1OI1QH1XL1bI1cL1dK1fK1kI1oL1pH1qH1rG1vL1yK1zJ21K22K24G28H2BG2CI2JG2QH2SH\
    2gL2kH2qI2tE2wG3cA4JI4WK5IF5qD60BCdGE6HHOLHQHHRDI5GIAJIGHIdHIeGI_FJAJJBGJIGJJDJYKJZGJlIK6G\
    KcL08K0NH0ZK0mK0vJ1GK1OK1QH1cI1dI1fI1kG1oH1pH1rL1vL1zH21J22I23G28K2BF2CJ2JK2QK2kK3oJ46J4BF\
    4GI4TG4fE5IG5yCCtDEvJHOIHQIHjDHlGHnJI5GIAJIdJIeJIzEI.JJZGJlJK6IKAGKcI00p1wk2Ih2Ui2Vi0Op0Rr\
    0Tm0qq0rnInm05F0DE0GI0J50OG0RG0UA0eD0rK1sFHPBJ9F08I09L0NH0jK12G1GF1QH1YJ1bH1cH1dL1fE1iJ1oK\
    1pH1qK1rH1vG1yH1zI21I23G24K2BJ2CH2QG2SF2aJ2kH2qH2sG41F5ILDLLEeEHOFHQIHiKHpIHsKHwLHxJIGJINE\
    IdHIeFIfDJIIJeGJlKJmDK6LKbF01q4KhCOjI0hIPh05l0JlIYi0NJ0lL0mK0vG1GL1OJ1QJ1bL1cL1dG1fJ1kH1oJ\
    1pJ1vK1yL1zI21K22J23K24G2CK2JL2QK2SJ2kK2nK2qL3eI3oL4BD4EJ4GG4TH4fJ5IH5yDCtFEvJHOIHQJHjIHlH\
    HnKHyLI5KIeJIzFI.KJZHJlIK6JKAIKcJ0NK0WG0tK1FJ1GK1OJ1QE1bL1dG1fJ1kK1oL1pK1qG1rJ1vK1yL21H22I\
    23J24I28G2BG2JH2QG2SG2gJ2kG2qL2tF2wG3cC3rG4JE5IJ5qJ60B96JCdDE6HHOHHQKHRII5FIAHIGEIdFIeIIrJ\
    I_CJAJJBHJGHJIFJJGJTKJYKJZHJlGJrFK6FKcJ0Di0Li0.l1PnJVh00p01q3Qi05o0JkIYi05o0JlIYi00p2Eh2Vh\
    2_h08K0NJ0lG1GI1OK1QL1bF1cL1dI1kH1oH1pI1qL1rF1zJ21H22I23I24L2CG2QJ2SH2kI2vI34K3eB46E4ED4GD\
    4cH4jJ5IK5yLC5IHQJHjKHlEHuLHyLIGJIOAIeKIzIJ4KJUKJlIK6IKcI0Vi0tm1Fn1Xi2dm2gh2nk2ok2wm35l38o\
    3Ol3cj3ri46i4Di4ji4xh5Ak5Yj8YkDnqE6pFwlHWhHomHuhHvjHxhIcpIrnJ4jJAnJBmJYiJai01s0zj1Rh3Ki3Xi\
    4phI9iJ.h0bh0ej1Bh2mk2xh6Hi6Ii6mhH.lIiiIkmJ5iJuhKZh01p58h5Zj01s0zi1Ri4MhHThI9j00q2_h08K09K\
    0NG12H1GH1ML1QK1YI1bI1cK1fK1iL1oH1pI1rJ1vH1yI1zI21I22L23I28F2BJ2CF2JL2SF2aK2dI2qK2sF41GDLF\
    EeEHOFHQKHiLHpIIGIINDIdHIeJIfCJZJJeGJlJJmBKbD0Ai0OkJRh08I09L0NG0ZK12H1GG1MK1OL1YK1cK1fH1iL\
    1oL1pJ1qI1rH1vH1yJ1zH21L23K24J28H2BJ2CG2HK2JL2QG2aL2dK2kL2qH2sF41H5IIDLIEeFHOHHQLHgLHiJHpK\
    I5JIAHIGLINGIdKIeFIfDJFLJHLJZLJaIJeFJlKJmBK6JKbG0ho1Jq20q2yh31l00p25h3Bi00p1wj25i2Ij2Yi0pm\
    1hp1tj2ikKit04l0wj4Hj53j00q3Lh3si00p1wj1Sk2rhInlJRl01q4Ki5ZiJqj09h0ji0ms0tp0vo1Mj1iq1ms1.r\
    2Km2nl2vl2wl35i3Oo3ci3rl46i4Dl4jj4xk5Aj5Yk5yj8YlC5kDLjEvoHioHomHprHsjHxhI.sJStJajJmmKUrKVs\
    01p2Fh5EhCOhHci01q3ih3qh0Pt0lp0mi0vm1Fj1Xi2nl2oo2sk2vn38r3ej3tm41l49l4In4Ti4Wi5Ml60mC5oDnn\
    E6kEvjFwrHhoHusHvtHwoHyqHznILhIcrIrqJ4tJAhJCp02t0Fl0Ql0Sr0bj0kn1Bh3Uq45m5Wm67h6Ah9Hh9bkA7j\
    BJhCajDChHPkI1oI2oIZmIjsIwhJKjJMrJQkK9i1jHFPL08F0NI0jH12G1GE1ML1OL1QJ1YC1bJ1cK1fH1oK1pH1qK\
    1rJ1vI1yJ1zH21K22F23L24J28J2BJ2CI2JH2SI2aE2dL2qF2sD5ILEeEHOLHQJHhHHsGI5KIGLINJIfEJFFJHDJZH\
    JlJJmDK6LKcK0Oo0Rn0rn2RhJRjJcl02903D07J0H80KB0kL0nB10A1ZI2xDHm9IkBIs9JQB0Pr0ln0tn0vk1Fk2Kk\
    2dl2gl2vn2wn35k38r3On3ci3rj46i4Dj4ji4xj5Al5Yj5ym60h8YlDnoE6iEvkFwrHonHurHvtHxjHyqIcqIrrJ4s\
    JAiJBiJmk0Pi0Vh1Fq1Xn2Ki2go2sl38o3ek3tl41h49l4Im4Tk4Wj4fl5Ah5Mm60m96iC5nDntE6qFwoHWmHhpHtk\
    HvpHwmHyiHzmILkIcsIrsJ3hJ4lJArJBrJCoJYn1Sk3ohInjJRl08H0NE0PH0lJ1GH1OG1QH1bI1cG1dG1fL1kF1oF\
    1pI1qJ1rF1zK21J22G23H24L28J2CI2JK2QH2SI2kG2vK34B3eD46K4EG4GI4WL4jD5IKHOKHQIHlGHuJHyKI5JIAK\
    IGGIOLIdLIeKIgCJ2IJ4GJIIJTLJUDJZJJlIK6GKcI1SkInkJRn0Vh15l1Xp2gj2oj2wh3cj3ej3tj41m49i4Ii4To\
    4Wm4fn5Mj60j96n9IiC5jHarHbpHhkHkiHtnHwhI8sILmIarIbnJ3hJCkKHi1SkInnJRm02m0Qh0Sm3Ui45h5Wi84h\
    HPhI2lIZjIjlJMkK1i0Nm0lr0vp1Ol1cj1do1kn1om1pk21l22l23i2Cj2Qj2Sm2kj3eh4Et4Gj4Im4Wm5Yk8Yl96n\
    C5lCtjEvmHQiHumHxlHynHznIGiIOmIdiIelIzkJanJehK6lKVhKcm09l0Vq0to15o2Km2dm2nm2sh35k3Oo3cl3ri\
    46j4Dl4jj5Am5Yj5yi63j8Yk98iDLhE6iHWnHasHbqHhlHkpHonHtpHxiI8sIasIbpJ3oJ6nJBjJChJYlJahJmiKHh\
    02A03E0H70KC0kK0nD10C1ZI2mBHm8IkFIs9JQAJgK0hm0yj1Ju1gk20o31m33j03m0Bi0in1jj2cj2jh5PjJEi00p\
    1wh2Ii2Yh3Th00p32h4ih00p3Dh3Ek3Gi00q1JE1gH20B2yD0Jq17h1SiInq09l0js0tm1Mq1it1mj1.m2Ki2dj2nj\
    2wj3Ok3cn3ro46k49h4Dk4Tk4fi4jm4xm5Aj5Yi5yn8YlDLrEehHisHonHpsHspHxnJ6iJSjJaoJmlKUn01q3Ni3fh\
    4qhIBkIJiK7j00p3dh0Ni1Om1Qi1Xi1ci1do1kl1ol1pj21l22j28l2Kj2Ql2Sl2gl2ki2nn2oj4Wl4ji5Ai5Ih5yp\
    60q96iFwpHvoHziIAiIGnIclIeiIrnJ2lJ4nJIkJTtJZjK6mKck18L1TK1hJ1tG2WEKiE3ck3rp3tm5qhILo00p2Ui\
    00p3Fi4th00p3Gh4th00p30i42h00q3Ch3wi4th00p30h3Di3wh4ti02i0bh0ei2mk2xh9SiHrhH.iIihIknJ5iKZj\
    1TK1WK1hC1tK2WC2iLKiF00q1xh2.h3gi3sh01p1Ri2LhIUh00p1wi01q2OhIWh01p36i3fi3hi4qhK7j09s0jo0to\
    15r1Mo1ih2Kl2dn2nn2wp35k3cm3rm46j4Dl4jj4xl5Am5Yl5yj8YmDLrHaqHbkHikHkrHopHpiHsqHwjHxiI8pIan\
    IbqJ6qJajJmk1lo4Zj5UhH5iKIhKOl01r0zi2Fj3pj4Uh5KiCrhJ.h01q3iiEci4FW4RW4kW6IWDcWFkWHqWHrWI3W\
    IuWKZW00p2Yi02803K0FF0HC0QK0nA0wF0.H10BHmLI1AIjLIkHIsCJ8AJMEJQ800q00p42i01p2Li07r0Al0Bq0Hi\
    0Kk0Qj0Uq0bj0dl19h1Bj2mr2xr3_n64l6ih6ti8AiDVjF6hFchGgjHmmIXlImhIssIui00p0Em0En0Vl0tk15m1uk\
    2Ki2nk2sm2wl35o3Ah3Ok3rn46o4Dn4jm4xm5Ak5yk63l98j9IiHSjHWhHaqHbrHoiHtoHxmI8sIatIboJ3jJYhJal\
    JmnKHj00q40h4Ch0mq0vi1Xn1im1ms1.r2gk2oj2sl3tp41p49k4Ii4Tm4Wi4fi5Mh60q96jC5lEvmHgiHhjHpoHwl\
    HziILoI.sJCjJSsJmlKUpKVr01q36h3Ni4AiIJi00p3yi40i01q36i3Nh4AhIJhK7i01q0sh3Ri3lh4MhI9iJ.h05L\
    0CL0DA0JH0O90R90r90uA1KK2DK4dLJ9A00r3Li01p4Mh4oiIoiJ.h00q3nh3yh4ih00q3mh4hh4iiJkh1SiInnJRn\
    01q0zi1Rh3Qi4MhIEiJ.h01pHThIEh0Vq0tm2Kh2dm2nl2oh2wm35o3Om3cp46l4Do4jl4xp5Aj5Yh5yo8YiE6nHWq\
    HboHojHtoHxlILhIahJ3qJAiJBqJYqJalJmp00q3Th3vi0Vn0th1Xk2gj2on2sj3ep41o49h4Ih4Tl4fj5Mh60o96h\
    C5jE6kHWqHblHhjHtoHwkHzjILmJ3qJAiJBpJCmJYp0Dp0Jm0Oo0Rl0rn1KqJ9k00p3ni3sj4hh00p3Fh3Gi42i01p\
    0Ei4qi00q3gh3mi3niJkl06I0DK0O90RB0TA0r91n907u0Al0Bn0Hm0Qh0Un0bj0dm0kl1Ah1Bm2mo2xo3Yn64p6ih\
    6tqCpkFmjHGnHJiIXhIjhImiIsnJcmJgnKSi00p3Gh4ti00q3gh09q0jl15p1Mi1Xl2gh2oj2sj3eo3tm49j4Di4Tj\
    4Wj4fk4xh60o96kDLqHanHhkHkpHsqHwkHzhILnIboJ6rJCi00p30i3Wj0CL0D80OF0RA0TL0rD0uAJ9B02o0Sn0kj\
    3UjFOhIjmJMjJQj02l0Qk0Sq0Xj0bi0kj1Bk2hl2ui3Un5Wn5ho9Hh9Rh9bjA7hCamHPjIZkIjuIwiJKlJMoJQkJuj\
    K9i1Fn1Xl2Ki2di2nh2wi35m38j3Oj3cn3rn4Dk4jm4xk5Ai5yk8YiDnpE6lFwhHohHxlIcoIroJAnJBlJGhJYjJan\
    Jmk0Ep1el4Lj52h5dh5.hHVh02l0Lj0bj0en1Bj2mi2xkAQiAfiH7hIZiIiiIklJ5iJuhKGiKZh09m0jo1Mr1Xh1ip\
    1mk1.m2gl2om2si35i3ej3ti41j4Il4Tl4Wh4fl5Mn60j96jC5jDLoHhlHirHomHprHsoHwmHzlJ6iJClJSlKUn01p\
    3fi4qiIBhJ1h05k0JjIYj00p4hh09p0jl0tl15o1Mi2Kl2dj2ni2wj35n3Ol3cl3rp41l46h4ji4xq5Ah5Yh5yo8Yj\
    DLnHanHkpHoiHsoHxjI8kIbnJ6pJalJmp2vh4Wn96nHxpHyjJan3SpI3n1SiIniJRl2Xi53m0Pl0lo0mo0vo1Xj1ml\
    2gm2on2si2vp3ei3th41i49l4Wk4fj5Mj5Yi60i8Yk96kC5kCthEvoHhnHuoHvkHwlHynHzoI.nJ4lJCnJSnKVn0Jp\
    1SjIYkInp01p2bhIPk01q2bjI0h01q0si3ph0Ml3dm4Vi0Go0Uj1Ak1Bl3Uk4Rh5th6Eh6Yh6si8whBFhDyhIskJ8l\
    JMkJcnJghK8h03W0wW0Go0Ui0bi1Ai1Bk3Um4Rh5th8Oi8qhI3hIskJ8mJcnJgi3Sn4Fj4kjI3m0Mm3gi40h4Nh09k\
    0jo1Mo1Xn1iq1.j2gh2oh2so35h3em3tj41m49i4Ii4Wk4fn5Mj60m96lC5iDLoEehHhlHisHprHskHwhHziILlJ6h\
    JCiJShKUm01p0Mn3nj4Nk5zi0Pi0lo0mp0vo1Xo2gi2oj2sn2vp3ej3ti41l4Ii4Tl4fk5Mh96oEvmHhmHunHxhHyn\
    HzjILjI.jJahKVi01rHYh01q2Nh5EiJqh03W0SW0wW7ZWKOW0Zn12h2Gn4GiEylIOjIzhJ2n0Eo05n0Jk0DD0OE0RD\
    0TH0rF0uL3jJJ9701p0sh4XiHYhJqh0jo1Mq1Xn1ir1.i2gi2ol2sn35j3el3ti41m49k4Ih4Tn4Wm5Ml60k96lDLn\
    HhlHirHpqHskHwhHzhILjJChKUm0En4XlHVl00p3ni4Ci4ii00q2.i3mh3vi3yi4hj0ti1Fn1Xk2Kl2nk2wi35o38m\
    3Ol3cn3rm46n4Dl4xi5Aj5Yh5yj8YiDnpE6jFwmHolHvkHxlIcoIrpJAnJBlJGhJYiJaoJmi3Sk4RhHqhI3lIuh0Gl\
    1Di3SiHXiI3l0Mm4Vi5ah5zh0OB0RH0T60rC1PK1nJInKJ9L01q01p01q36i3Nh4AjK7j0En1ej4oj58l02m0Sk0bh\
    3UjDCiIZhIjmJMn00p3Ch3Fi3Gi3.i0Em1ej4ri58h8phGpjJWi0Em0Mn09o0jl0tk15o1Mi2di2nj2wi35m3Oi3cl\
    3rp41l46k4Dp4jj5Ai5ymDLoHamHkpHohHsoHxlIbnJ6oJajJmn0Gn0Uk0bh0xh19h1Ai1Bk3Uk4Rh6Eh6Yh9NhAzh\
    BqhHKhIslIugJ8mJMhJclJgj01r4LiHYhIPiJqi03W0wW4ZW7ZW0Gi0xl3UkBUiCeiCxhIkiJ8oJMlJgh0Mo3yl4Vl\
    5zk0Wl2Hl2pn2rj34m4Bo4Jk5HjHjk0Em47i4LmJ7hJqi04h4Hm0Gm0Uj0bi19h1Aj1Bl3Uj8DiIslJ8kJMkJcl02m\
    0Sn0kj3UjI1jIjmJ5hJMj0Mm3nj4Nh4Sh4Vi50h5zh0Em4zjI7hIFhJ7h01p2Ni5ZiI0i02l0Qh0Sm0ki0nhI1hIjl\
    JMj0Vh0tl1Fo2Km2di2nm2oj2wk35j38o3Ol3cj3rj46i4Di4jk5Yi8YjDnrE6oFwoHWlHbhHokHtjHvnHxiIcqIrp\
    J3jJAoJBnJChJYnJai0Mm3mkJkj0Gh0bi0xn1Ak1Di3Ul5nh7Vj94hJ8oJMk02k0Sm1Bi3Uj5Wj5xh9ilBThFRlHPj\
    I2nIZhIjlJMm01q4Ki4XiCOi2Hj2pj2rl2th34m4Bl5HhHji0Ml68i69hCdhInkJRl0NG0ZL0mL0vH1GJ1cH1dK1fK\
    1kL1oL1pL1qJ1rL1vK1zL21J22L23L24K2BK2CL2SJ2kJ2nL3oL46L4BH4TH4fL5yCCtGEvLHOLHQHHjFHlHHnHHyL\
    I5JIAJIGJIdJIzGI.LJZIJlHK6LKAIKVLKcL0Gn0Ui0bk0xi19h1Aj1Bj3Um5Nh7ji8Hg8qi8wg8yiBFhIslIuiJ8m\
    JMiJclK8h01q0Go0Uh0bj19i3Uk4Rj54h6nh8Hh95hCGhIslJ8lJMjJcmK8i09n0jo1Mq1Xl1iq1mi1.n2gl2om2sl\
    3eh3ti49m4Il4Tj4fj60i96hC5lDLpHhnHirHkiHojHpqHsmHwjHzlIbhI.hJ6iJCmJSlKUmKVh0Gn0bj19j1Bi3Uk\
    4Ri7Bh83j9mi9xhDyhHKhIslJ8mJMkJcmJgj0Gj0xj19i1Ah1Bi3Um4ki5ShBUiBfhDmhIskJ8oJMk7Om02j0Ln0el\
    1Bj2mi2xi4Fk5VjEqhF5hIZjIiiIkkJ5iJuiKZi1Nn3oqHnoJFmJJrJrp0Gl0bh0xo1Ai3Uj5Oi7Xh9eiDBhDWjFVi\
    HqhIskIuhJ8pJMk0Mn69i6Vi03W0wW4ZW7ZWHEW02j0Lk0bi0en1Bi2mh4Fi5Qh8BhIZiIklJ5hJhhJui02p0Lh0So\
    0bk1Bh2hl2uj3Uk45n5Di5hjHPjI2lIZkIjoJKjJMmJQi0Gn0bi0xi3Ul4Rh4yh5Li6Rh6Zj7Dj7Kh7jj8fh8uh9ph\
    A2iAzhBqiIsmIuhJ8lJMkJcnJgkK8i0Pm0lp0mp0ti0vo1mk1.i2Kl2dl2nj2vo2wm35i38i3Ol4Ih5Ah8YlCtiEvo\
    FwhHokHupHvlHxiHyqHzlI.nJ4mJSlJaiKUhKVm01p2Ni4Kh58i0Mm4Vi5zi02k0bl0en1Bi3Sh4FiBwhIZjIijIkl\
    J5h07m0Bl10h2mk2xjFThHmhIsm0Em5Zi5giJWk02l0Ll0bi0em1Bk2mh2xl4Fh5bh5fh7ehEDhIZkIklJ5iJui02j\
    0Lj0bk0em1Bj2mh2xj4Fk5ei8Eh9GhIZjIklJ5jKZj0Ep5mh5.h73h9qhJOh02k0Lm0Sj0bk0nh1Bk3Sh3Ui45o5Wl\
    9Hh9biA7hCanHPkIZlIjmIwhJKlJMiJuj0Gm0Uh0bk19i1Ai1Bi3UnAahBFhIsjJ8kJMkJclJgi0Gj0bh0xm1Ai3Ui\
    IkiJ8oJMj02l0Lj0bh0en1Bi2mj2xl4Fj6Qh7ehBxhDYhFegI3gIZiIklJ5gJjhKZh0bnI3lIlp0En5.hIFiIHhJ7g\
    JOiHXiI3m02l0Li0bi0em1Bh2mj2xj3Sh4Fh75iEIhF.iH.hIZjIkkJuhKZi02l0Fh0Sm1Bg3Uh6Ug6kh9lgBTjDhi\
    HPgI2oIjmJMj3rq3toILnJBiJCg0Gi0bh0xi19i1Bh3Ul6Dh6JhHqhJ8pJMk17o2ao4coHRpIgpJriKbq0bI0En02l\
    0Ll0bh0em1Bh2mj2xkIZhIihIkmJui3ch41q4Dn4xpIbjJHh02l0Sm0bi2hi3Ui45j5Dh5Wh8UjF7lFRhHPjI2mIZj\
    IjmJMlK9l0Ps0ll0tk2Kh2dk2nk2vh2wi35n38p3Oi3cm3rp46l4Do4jk4xo5Ah60iFwqHojHurHvrHxlHyoIcmIro\
    J4sJalJmn0Mo4Sh4Vl4mh50j0Wm2Hh2pn2rh34i4Bo4Jm5HmHjn0En47jIHhJ7h0Ps0lk1Xn2gn2ok2si2vi38r3ep\
    3tp41o49j4Ij4Tk4Wk4fj5Mj5yj96hC5mFwrHhmHuqHvrHwiHymHzlILnIcmIrqJ4rJCl02k0Lh0bj0en2xl4Fh7Mi\
    7Qh8lhBshH.hIZhIiiIkkJjhKZi0Mn3nj56i08h1kh1qh23j2Ji2qh3tl74s98iHQiHtiI5jIAiILlIeiJClJlhK6h\
    07n0Ai0Bm0Um0di1Bi2ml2xm3Yi3_m6ih6tj9TiCphHJiIXmIsoJgi02k0Sm0ki0ni3UhIjmJMkJQk07m0Bm0Hh0Qi\
    19h1Ah1Bh2ml2xj9.iHmhIsn02m0Fh0Sl1Bh2hh3Uj45h5xh9ihBJhHPiI2nIZjIjlJMiJvh0Mm4mh0Ml3nh62h79h\
    02o0Sk0nh3Ul8bhI1iIjpJMoJQk02n0Qh0Sn0nj3UkI1jIjkJMmJQi0xk1Bi3Uj4khAHhEmiIkiJ8oJMmJgh0Gh0xm\
    19j3Uk4ki7Sh9zhC9hIkhIsiJ8oJMj0Gn0Uj0bl1Ai1Bj3Ui6Gj9DkB_jE9iIskJ8lJMjJcnJgk2Hh2rk2tn34i76i\
    HlpIfr0Gm0Ui1Bh3Uk4Rh6Ej9DjAOhB_hCGiI3iIslJ8iJMjJco02n0bi0nl10n2mj2xi6IhBCiBahFuhH.jIkoJ5i\
    JuhKZh3Sl4FhI3i0Gi0bh0xi3Uk5rh7LjBUiIsiIuhJ8oJMm02m0Lk0bj0en1Bj2mh2xi3Sj4Fj7.hACjEqhIZiIkm\
    J5kJuhKZj1Mh1uk2sn3cpHSkKHh0Gn0Uh0bh1Bk3Uk4Oi4Ri6si83h8Xh8fi9miCFhEzhHKjIsmIuhJ8oJMlJclJgh\
    0Gn0Ui0bj19j3Um4Rj4yh8Oh8qh8uh95h9Jh9miFWhIsmJ8mJMiJcnJgi0Go0bl0xh19i1Al1Bj3Uk4Rj5th7Bi8yh\
    8.hATiC1iCXhEJhGviIsmIuhJ8nJMjJcnJgj02l0Sm0ni3UiI1iIjmJMl02j0Lj0bj0el1Bh2mh2xk4Fk7chIZhIkl\
    J5jKZj0Gm0bh19i1Ah3Uj95iCFhHKhIslIuhJ8iJMhJcm02n0Sn0ni3UjI1hIjlJMjJQh0Gj0xj19j1Bj3Uj8vi9fh\
    D0jI3hIslIuiJ8nJMkJgh02m0Sk1Bh3Uk5piBLiHPiI2oIZhIjmJMjK9i0Mn3yi4Nh62h0Eo4KiGph0Gj0xj1Ai3Uk\
    BfiF_jHqhIskIthIuhJ8pJMk0Gn0Uk0bl19h1Aj1Bl3Ul4Qi4Ri6rh95h9Ni9miA2hAahIskIujJ8lJMiJclJgh0Gn\
    0Ul0bi19j1Aj1Bj3Uk5XiAahBqhF1hIslJ8kJMiJclJghKCh0Gj0bh1Ai3UkCfhJ8qJMi02j0bk0em2mh2xj3Sh4Fk\
    5fh8rhH.iIiiIknJuiKZk0Gk0bh0xj19h1Ai3Uj7nhBKiIsjIuiJ8oJMkKdh02l0Qh0Sl0bh0ni3UjI1hIjnJMl02l\
    0Sm0bh3UkE3hHPhI2pIZjIjlJMl02l0Sn0ki3UjI1iIjnJMlJQhJXh02j0Li0en1Bj2mh2xj4Fj5fj5ohIZiIkhJ5i\
    KZh0Gl0Uh0bl1Bi3Ui4Rj8fh8qh95hAOhAThB_hIskJ8jJcoJghKCi07n0Am0Bn0Gh0Uo0bl0di19h1Bl2mn2xm3_h\
    64iGKhIXnIsm02h1Bi2mj2xhH.kIijIknJuhKZj02l0Fh0Sl3UjBThFRhI2mIZiIjkJMkJvi0Gk0xl1Ah3Uj4khC9h\
    IshJ8oJMk0eh3SkHXhI3h0Gl0bi1Ai1Bj3Uk4Rh5Xh6rh8Th8fh95hB_hIsmJ8lJMhJcnJgiKYh07m0Ah0Bi0Hh0Uj\
    0bi2mk2xlHmhIsm0ej1Bj2mi2xjHriH.jIiiIkoKZh02m0Fh0Sm0kh3Uk7viI1jIjnJ5hJMjJQi0Gn0Uj0bi1Aj1Bi\
    3Uj6Yh6nh8OhAThC6hHKhIskJ8lJMkJclJgj0Gn0Ui0bl19i1Aj1Bk3Uj4Oh5Li5ti8Di8Xh8fhCRiDeiIslJ8lJMj\
    Jcl07q0Aj0Bl0Hh0Uj0dl0kh1Bj2mm2xm3Yi3_p64lHGlIXhIsiJcmJgj0Gh0bi0xl19h1Aj1Bi3Uj7VkIkjJ8qJMl\
    0Gi0xk3Uk4khBKjC8hCxhDmjHXhIkhJ8oJMhJgi07n0Bl0Ki1Ai2ml2xnHmkIsk02l0Lk0bh0en1Bk2mi2xj4FiAgh\
    CAjIZjIijIkkJ5iJjh07n0Ah0Bk0Hh0Uj10h1Bh2mk2xjHmiIsj07n0Bl0Hi0Ui0ki1Ah2mj2xiHmjIsl02m0Sm0kh\
    3UkHPiIZhIjnJMk02n0Qi0Sm3UiIZhIjlJMiJQh0xl3UlBmiCShJ8pJMk0Wn2aj2pj4Bo4Jo4cm5HmHRlHjl02o0Qi\
    0Sm0kh0ni3UjI1iIZiIjjJMlJQh0Em52h5gl8phJOh1uk2sn3co63h6Li98hHShI8h02l0Lj0bh0en1Bk2mk2xi4Fh\
    5Qi7MhAQhGqiIknJ5iKGh0Zj1Ym2GmCdlIznJ2i0Gj0bj0xl19h3Uk4lhClhD9iJ8nJMiJgi02n0Sn0nhI1jIjlJMk\
    0Mn56h5Th0Gj0xl19i1Ai3Uk4khDWiE_hFUhIsjIuhJ8nJMlJgh0Go0Uh0bj0xh19j1Aj1Bi3Uk4Ri5th6Oh8whC1i\
    FJhIsjIuhJ8mJMlJcmJgj02n0Sn0ki0nh3UiIihIjmJMkJQh0Gm0Uh0xi19j1Bi3Ul8viCXiGviIskJ8oJMlJclJgh\
    02k0Sl3UkI1iIZiIjlJMlJQi02m0Qh0Sm3UiI1hIjlJMm0Gk0bh0xj19h1Ah3Uk5Si97j9YiBthIsiIuhJ8nJMi0Ep\
    5dh5gh8zjJOh07WI2WIjWJQW0xn1Aj1Di3Ui4lhHXhJ8oJMj0Go0Uj0bj19i1Bj3Ul4Rj4yh5ii6Mi8qh9fi9phA2i\
    CRhCXiCmjDyhEJhI3hIslIujJ8nJMkJcnJgkKEh0Gi0bh0xk3Uk5rh6Jh7Sh9VhA1hAshCxjEphIshIuiJ8qJMl02j\
    0Li0em1Bj2mj2xj4Fj7Rh7eh7_iCchIijIklJ5jJjhJuh02l0Sm0nk3UhI1jIjlJMkJQh5Pm07l0Ai0Bh0Ul0bi0di\
    1Bh2mj2xiB0hCChG6iGBiIXmIsm02k0Lk0bh0em1Bi2xk61hIkkJ5hJuj02l0Ln0bk0en1Bi2xj4Fi5vh61h7Mh8Eh\
    GvhIZlIilIknJ5h0Gh0xm19i3Uk5ShC8iDIiDqkIkiIshJ8oJMl02i0Lm0bh0el1Bi2mi2xkGvhH.jIZiIknJ5jJui\
    02n0Qi0Sm3UjI1iIZhIjlJMj0Gi0xm1Ah3Ui5Cj5nh9WhC4hEshIkhJ8pJMl0Gm0bh0xj19h1Ak1Bi3Ul7phFJiIkh\
    IsiJ8nJMl0Gk0xk19i1Ah1Bh3UkCehD3hDBhEKiE_jFVhHXhIsiJ8oJMkJgiKEh02o0Fh0Sl0ni3Uk8chI1kIZiIjl\
    JMl03W0wW02l0Lj0bh0em1Bh2mh2xi4FjEIhIZjIkmJ5hJui0Gi0xl19j1Dh3Uj9whCwhIkhJ8nJMkJth02k0Lj0em\
    1Bj2mh2xi4Fi61hAhhF.iH.iIZiIiiIkjJ5jJuiKZi02l0Fi0Sj0bh3Uh5WhHPhI2nIZiIjmJMiJvh02k0Lk0bk0en\
    1Bi4Fi7TiIZkIklJ5iJhhJui07k0Bm0Qh0kh10i2mj2xk6whHmjIsn02m0Lj0bk0en1Bk2mh2xj3Si4Fi75iAqhIZj\
    IkmJ5iJjhKZk0yJ1gL2iKKiJ02m0Sl0kh1Bh3UlIZjIjmJMi0Gm0Uj0bi1Aj3Uj4Rh8wh8.iIsmJ8lJMjJcnJgiJni\
    02l0Fi0Qh0Sl1Bi3Ui6ehF7iHPiI2nIZjIjmJMj1BJ02n0Sk0bh1Bh3Uj9bhHPhI2nIZkIjkJMh0Gi0xl19h1Aj3Uj\
    9tiEziIkiIsiJ8pJMi0Gh0xk19h1Ai3Uk5Sh9ehBtiDWiE8hEKiFJhF_hIsjIuiJ8mJMm0Gj19j3Uk4khAyhJ8pJMl\
    Jgh0Go0Ui0bh0xh1Ai1Bj3Uk97iA0hC0hDBhIslIuiJ8nJMlJgi0Gi0xl1Ai1Bh3Uj4ki7AiCuhIkhJ8nJgi02n0Sk\
    0nh3UiI1iIZhIjlJMj02j0Lj0bi0en1Bi2mh2xj9diAChBshFDhIZiIihIklJ5iKZh02j0bh0ei1Bh2mi2xkBNhDci\
    FuhH.kIijIkoJ5iKRiKZh02n0Qh0Sn0kh3Uk6qiI1hIjlJMj02l0Qj0SnI1iIihIjkJMlJQi02l0Lj0en1Bj2mi2xi\
    4Fi7.hEqhH.hIZiIkkJ5iJukKZj02n0Qi0Sn3UhHPhI1iIjlJ5iJMjJQi02i0Qi0Sl0nh3UkIjmJ5jJMlJQh02k0Lk\
    0bj0en1Bj2mj2xl7Mi7aiIZiIijIkmJ5jJukKZi02l0Lk0bk0eo1Bi2xl4Fj6Kh7xi8liBRhBoiIZiIkmJ5iJuiKZi\
    0Gj0xk19h1Ai1Bh3Uk5nhENhIkiIujJ8oJMk02l0Qh0Sn3UjIjlJMm02n0Sl0bh0nh3UjI1iIZhIjjJMm0Gm0bj0xh\
    19h1Ah3Uj5Ni6rhA8hG0hHKhIsmJ8mJMkJcmJnh02m0Sl2uh3UiHPhI2nIZkIjjJMiK1j0Gj0xm19h1Ai3UkDgiEdh\
    IsjJ8oJMlJgh0Gl0bh19h1Bi3Uk4yh6Nh6Yh7Di8gh9mkA2iIslJ8lJMjJcn02m0Sl3UkIjmJMjJQi02k0Lk0en1Bj\
    2mh4Fk9Ai9Li9dhAghFDhIZlIklJ5iJuiKZh0Zl12m2GkEylINiIOnJ2l07n0Ai0Bl0Ki0Um0bh1Bi2ml2xmCpjFch\
    GghIXoIsl02o0Ll0bl0en1Bi2mi2xk4FiAQhBkhBshDAhDziFXhGvhG.hIZkIklJ5iJjhJuh02m0Sn0kh3UiHPhI1i\
    IZhIjiJMj0Gn0Uk0bl1Bj3Uk4Rh54j6Gh6rh8XhAOhHKiI3iIsmJ8mJMiJclJgi02n0Li0bj0el1Bi2xl4Fi5ei5fi\
    9OhBsiCzhFXhGvhH.hIZiIihIkmJuiKZi02m0Qi0Sm0ni3UlG5hIZiIjlJMk07m0Bl0Qh0Ui10h19i2mk2xlHmhIsl\
    0Gm0Uj0bh1Ah1Bj3Uk4Ri4yh5Lj5Nh6Eh8fhAahFWhHKhI3iIslJ8lJMkJcl02m0Lm0bi0em1Bi2mh2xjA_iFDhIZk\
    IihIklJ5i02n0Qi0Sn0ki3UiFgiI1hI2iIZhIjiJMk0Gm0bk19i1Bi3Ui4Rj6hh8Oh95i9xiA2hG0hIslIuhJ8lJMl\
    JcmJgi0yH1JI1gF20H31L0bh0xm1Ah1Bh3Ul5nhA3hAHiCJiDJjEThJ8pJMm02m0Lk0bi0ek1Bi2xlAfiIZiIiiIkl\
    J5jJuj0Go0Ul0bi0xi1Al1Bl3Uk4Qi4Rh6hi83h86iHKhIslJ8lJMkJcn0xk19i3Ui4kh99hIkiJ8oJMk02m0Qh0Sm\
    0kh3UhI1iIjl0yL1JD1gF31LKiJ07m0Bl0Uh19i2mi2xkEbgHmjIsk0Gm0Ui0bj1Bj3Ul4Qh5Lg6Nh9giGviHKiIsn\
    J8lJMlJcoJgl02k0Sk0bi3Uk9RhBJhI2nIZkIjkIwiJMj0Mm02k0Sl1Bh3UjD6hF7gHPhI2nIZjIjjJMl0Gm0Uh1Ai\
    1Bk3Uk4Oi4Ri5Lh9phC0hFWhI3hIslJ8kJMjJcmJgl0Pl0lp0mp0tl0vp1mk2Kl2dn2nl2vp2wl35i3Ol3ci3rj46j\
    4Di4Ij4Wi4jh4xk5Ak5Ym5yh96iCtiEvpHolHunHvkHxjHyoHzlI.pJ4mJSlJajJmiKVn02l0Sm0nk3UjI1jIZhIjl\
    JMj02n0Fh0Qi0Sl0nh3UkIjjJMk02l0Sk0nh3UmI1iIjlJMm02n0Sn0kh3UkI1iIiiIjlJMlJQh02n0Sl3UjBnhFgh\
    I2mIZiIjkJMl0Gk0xl1Ai3Uk4kiBKiBZhC7jDmhFUhIsiIuhJ8pJMlJghKdh0Gn0Uh0bj1Bk3Um6ni8uhDohI3iIsk\
    J8mJMlJcmJgjJnh0Gn0Ui1Aj1Bk3Ul4Ri6Oi86h8qh8.i9DhC6hCFhCXiHKhIslJ8oJMkJcnJgj0Gi0bj0xk19h1Ai\
    3Ul4.hAsiE2hE_hIkhIshIuiJ8oJMjJgh02k0bh0ei2mj2xi6IjH.kIiiIklJ5hJuh02l0Sn0ki0nh1Bh8ChI1iIjm\
    J5hJMj0Gm0xk1Aj3Uk7nhDBhEKiJ8nJMlKEi02l0Lh0bk0en1Bj2ml2xj61h75h7.i9PhIZjIiiIkkJ5i02i0bh0ei\
    1Bh2ml2xh6IhBahH.kIkoJ5iJ8i07m0Bm0kh2mk2xlHmkIsm02n0Qh0Sn0kh0ni3UiIjkJMkJQh0El4uh5dh5gi5mh\
    JWi0Gp0Ui0bi1Aj1Bl3Uk5Jh6Mh6hh6rh7KhIsmIuhJ8mJMkJcmKCi02i0Ll0bh0eo1Bi2mj2xi4FiCohGqjH.hIZi\
    IkkJ5kJuhKZh0hD0yG1JE1gG20G0xm1Bh3Uj7Jh9XiCJhG4hJ8pJMl0Gn0Uh0bk1Aj1Bj3Ui9xhCFhHKhIsmJ8kJMl\
    JcmJgh0Gl0xh1Bh3Ul6TiC2iD3hEzjIsjJ8nJMkJgi0Go0Uj0bi0xh19h1Aj1Bk3Uj4Ri7Bi7ji8yh8.i9NiCXiG0i\
    I3hIsmIuhJ8mJMkJclK8h0Gj0xm1Ai3Ul4khAHiD9hEmiIkhJ8qJMlJgh0Go0bk0xi1Ai1Bk3Uj5Jh6Oi8.hIsmIuh\
    J8mJMlJclJghJnh0Eo7HjGphJWi02l0Sn3UlIZhIjmJMj0Go0Ui19h1Ak1Bl3Ul5Jh6Gh7jh8gh9Jh9mhCXhIslJ8k\
    JMjJcnJgk02m0Ll0bk0em2mh2xj9riFehIZiIihIklJ5j0Gi0xm3UkD9hDTiHXhIshIuhJ8oJMl2Hk2ri2tn34k5Hh\
    HloIfq0bh0eh2mj2xh6Ih8mhAKiBahH.lIijIknJ5iJui0Gh0xl1Ah3Ul5ChAPjBghDwhFvhIkiJ8oJMk0Gn0bi3Uk\
    5Li6Nh6Ri6hhCGhHKiIskJ8lJMlJcnJgk0Ph0lo0mm0vo1Xm2gj2oj2sn2vo3ti41l49j4Ij4Tl4Wo4fk5Mh60jCti\
    EvnHhiHuoHxiHyoHziILlI.iJ4jJChJajKVk0Gn0Uj0bj19h3Ui5Sh7Gi9ciIsjIuiJ8nJMlJcjJgj1Qg1vk1yk2Ji\
    2qk3ti63h74rHQhI5hILjIehJCl0xk1Ah3Ul8PhFvhJ8pJMl02l0Lk0en1Bl2xi4Fi6Qh88i8rhH.hIZjIihIkkJuj\
    0Gi0xk3Uk4kh9VjAyhGhhIkjJ8pJMlJgh0Wj17i2al4Bi4Ji4cp5HiHRnHjjKbk0Gm0bj1Ai1Bk3Uk4Ri6El6Gj8Xh\
    AOiCmhEJhG0hIslJ8lJMlJcm02i0bi0eiCUhH.kIihIkoJ5kJuh02m0Sn0ni3UjI1kIjlJMi02k0Lj0ej1Bh2mj2xl\
    4Fh5fhCzhIihIklJuh02l0Fg0Sk1Bi2hh3Uj45k5Wh5xi6UhI2oIZjIjlJMkK9j1ri24h2Bh3Oh4Dg4xj6LrKHi0Go\
    0bi19g1Bh3Uk4RjI3hIskJ8lJMiJcmJgiKCh0Gi0bh0xm19i3UkBShCnhJ8qJMl02l0Li0bi0en1Bh2xj4Fh5bjCQi\
    CzhIihIklJ5h02n0Sm0ki8FhI1iIjlJMiJQi0Go0bi0xi19i1Aj1Bk3Uk4yh5JhBqhE8hHKhIsjIuhJ8lJMkJci02l\
    0Ll0bh0el1Bi2mj4FiGviIZiIihIklJ5iJuh02k0Lj0bh0em1Bj2mh2xk5eh5fi9dhIZjIiiIklJ5hJui0Qp16h1Bj\
    1Ll02l0Fh0SkBJhHPhI2nIZjIjlJMi02j2mi3HhAphDFhHDjH.kIiiIkoJuhKZl07l0Al0Bj0Kh0Um0bk1Bi2mk2xi\
    64h6ihCChCphD1kIXnIsi02n0Sm0nh3UjI1iIjlJMlJQh0Gi0xk1Aj3Ul9BiEEhG2hJ8qJMlJgi0xl1Ah3UjFxhJ8p\
    JMl0Gh0bh0xl19h3Ul4lhARiDwhGPiJ8pJDhJMl0Gk0xl19h1Ai3Ul7Gi8vi97h9ciBvhD9hIsmIuhJ8oJMjJckJgi\
    0Gl0xm1Aj3Ui5OiHXiIkhJ8oJMl07W0HWIjW02n0Sm0bj2hh3Uj45j5Dh5hk6kh7mh8UhHPjI2oIZlIjmIwhJMm0Gj\
    0xj1Bh3Ul4kiD0jD3iDmiELhIslIuhJ8oJMlJgi02l0Ll0bk0en1Bj2mj2xl4Fi5Qh9PhB.hG.hIZkIihIklJ5h0Gj\
    0bi0xl19i1Ai3Ui5Sj7Gi7oi7qh97hCNhDBjDWhFohIskJ8oJMlJgj0Gn0Ui0bi0xj19j1Aj1Bk3Uk7KiC2iCmiGUi\
    IsjJ8mJMkJcj0Gn0Uk0bh0xh1Bi3Ul4Rj5th6ni8Hi9NhIsjJ8kJMkJckK8hKYi02m0SjIZkIihIjjJ5iJMi02l0Sk\
    1Bi3Ui5DiFRhHPkI2nIZjIjlJMi07o0Bk0Qh10h2mi2xlGQhHmjIsm03W0wW7ZWIyW02k0Fh0Sm3UiBTiDhjI2nIZh\
    IjlJMhJvh0Go0Ui0bl0xh19k1Bk3Ui4Rj4yi5Jh5Nh6Nh6Oh6Zh86k8yhAzhHKiIsmIuhJ8lJMlJcn02m0Qj0Sn0kh\
    0nj3UlI1hIjkJMj02h2mh2xh6IhCUhH.jIiiIkmJ5iKZi0Gm0bl1Ai3Uk5Xi6Mh8Xh9mhA2iAzhBFiEJhI3hIslIuk\
    J8lJMjJclJgi0EpJWh02l0Li0bi0ek1Bi2mh2xl4Fi5bh5fhB.hIZkIiiIkiJ5iJuh02j0Lk0bi0en1Bj2mi2xiFeh\
    IZiIihIkmJ5hJuiKZh0Gi0xj1Ai3Uk7niBvjCMhEzhIsiJ8oJMi0En0Gj0bh0xn1Ai1Bi3UkAPhCjhHXhJ8qJMm0Gj\
    0bh0xm3UhCViDQhDthDwhJ8qJMk0Go0Uh0bi1Bk3Uj4Rh8Kh8ui95h9fhC1hDGiG0hGUhI3hIsjJ8kJMmJcmJgh0Gj\
    0bh0xm3UkArhDgiGGhHXhJ8pJMm0Gi0bh0xk19i1Ai3Ul6Dh7XhADiFVhF_hIsjJ8qJMjJgh02m0Sm0kh3UkI1hIjn\
    JMj07m0Bn0Hi0Qh0kh2mj2xlHmiIsm0Gn0Uh0bi0xh1Aj1Bj3Uk5thC6hG0hI3hIslJ8mJMkJclJgi0Gi0bh0xi1Ai\
    1Bh3Uj4kh85h9VhADhIujJ8oJMk0Go0Uh1Ai1Bk3Uk4Rh5Nh5Xi6Gi7Ki8Ki8.h95h9phCRhI3jIsmIujJ8lJMkJcm\
    KCi0Gi0xl19h1Ah3Uj5nh70iIkiJ8oJMm07n0Bj0Qh19h2mk2xhHmiIsm02i0ei2ml2xkH.jIihIknJujKZj02l0Sl\
    0bh3UjI1iIZiIjlJ5iJMjJQh02l0Sk0bh3Uh5Wk7miHPlI2oIZiIjnJMi0Gm1Ai1Bl3Ui54h83j9JhAzhIslJ8lJMi\
    JclJghJniKCh07m0Bi10h2mk2xiHmiIsnJgh07l0Bl0Hi0Qi19h2mm2xjHmlIsl07o0Bi0Hi0Uj19h1Bh2mm2xlHmh\
    Isl02l0Li0bi0en1Bk2xj61h6Kj7th9shDrhIMhIZiIihIklJ5iKZi0Gh0bh0xl19h1Aj1Bh3Ui6ch9zhAsiIshJ8o\
    JMk02n0Sl3UkI1hIjlJMiJQh0Gj0bi0xm19h1Aj3UkAHiG2hGPhIkhJ8oJMm02m0Sn0kh0nh3UjI1lIihIjmJMhJQh\
    0Gi0xm1Dh3Uk8xhC4hCJiCwiJ8qJMl02m0Qj0Sn0nj3UkI1jIjlJMj02m0Fh0Qh0Sn0bi0kjI1kIjlJMl02j0ek1Dh\
    2mi6Hh93hH4hH.lIikIknKZi02l0Ll0bh0ej1Bk2xlCQhIZmIijIkmJui0Gi0bh0xl1Ai3UjAkjBghD5jHXhJ8qJMn\
    Jgi02n0Sn0kj0nk3UjI1iIihIjmJMi0Go0Uh0bh1Ah1Bk3Uk6Gh6Yh8qh9DiE9iGviI3hIskJ8lJMkJcoJgi0Gh0xl\
    3Ul4kh94j9vh9whBmhEahJ8pJMn02k0Ll0bk0em1Bi2mh2xi75hIZjIkjJ5jJui0Gi0xl1Bh3UkEThGshH2hJ8oJMm\
    02l0Fh0Sl3UhI1hIZhIiiIjmJMl0Go0Ui0bi1Ak1Bk3Uj4Qi4Ri6Oh6rh7Bh7qi8ghC6iIslJ8kJMjJcnJgkJhh02n\
    0Sl0ki0ni3UkI1jIihIjnJMj02n0Sn0kh0ni3UiI1hIjkJMjJQh02m0Fh0Qh0Sn0ki0ni3Ui6.hI1jIjmJ5hJMjJQi\
    07l0Ah0Bi0Um0bi2mk2xj64h6ihEShFmhIXmIsm02i0Fh0Sm3Uj6Ui8dhBLiDUhDhhI2mIZiIjlJMl07n0Bk0Hi2mk\
    2xmHmjIsn0Go0Uh0bj19h1Bj3Ul4Rh6Rh6Yi8qiIskJ8lJMjJcnJgh07o0Bk0Hh2mj2xlHmhImhIsm0Gm1Ai1Bi4Oh\
    9DhHKhIsmJ8mJMiJckJgj0Gh0xl1Aj3UlIsiJ8pJMlJgh07l0Bi0Hi0Uh10h2mm2xlIsn02l0Lj0em1Bj2mj2xi4Fh\
    5fh7xhALhCAhDAhIZhIiiIkmJ5iJuh02l0Ll0bl0en1Bj2mh2xi3Si4Fk5kh7Ti9OiH7iIZjIiiIkmJ5hJuiKZi02k\
    0bi0el2mi2xl4Fh6bjFujH.jIZhIijIknJ5hJuiKZi17m2ak4clHRmIgmKbn02n0Fh0Sn3UhIjnJMi0xl3UlAMiEnh\
    F9hJ8pJMnJgj0Gj0xl1Ai3UiIkiJ8oJMl02m0Qh0Sn0nk3UiI1jIjlJ5h0Wk2ai2pl4Bm4Jj5HlHjj0Gm0Uh0bj19h\
    1Ah1Bi3Uk8uhA8hHKhIskJ8nJMkJckJgjKCh02i0ei1Bi2ml4Fh6IhAhhH.mIihIkoJ5j02l0Lj0bh0em1Bj2xiDAh\
    FDhIZhIiiIklJ5iJuh0bi0xm19h3Uj91jCliDIiGhhIkiJ8pJMkJgi0xj19i1Aj1Bh3Uj4khADiHXhJ8rJMlJgj02k\
    0Lj0bi0ei1Bi2xhIZjIklJuk0eh6mjH.mIiiIknJ5k07k0Bj0Uj19h2mk2xjHmiIsk0xm3Uk5nh9whDthJ8oJMm1Bh\
    2mk2xj6IhBChCIiH.lIknJ8hJujKZi0Gk0xj19i1Bh3Uj4ki6ah7pi9BiD8hIkkJ8pJMkJgh0Gl0bj1Ah3Uj5Lh9mi\
    A8jIsmJ8mJMlJcl02l0Ll0bi0ek1Bh2mj2xi9GhB.hGxhIZiIihIkm07m0Bi0Qh2mj2xlHmjIsn07l0Bi0Ki0Ui1Bi\
    2mk2xj7PhE4hG6hIXnIsl02l0Sl0kh3UkI1jIjnJMl02m0Sl3UhBBiHPhIZhIiiIjmJMiJQi02m0Sl0ni3UjI1jIZi\
    IjmJMk02m0Sk3UiBPhIZhIjnJMk02l0Sl0nh3UlI1kIjlJMlJQh07l0Bj0Ui19h1Ai2mk2xlHmiIsm0Gn0Uh0bi19i\
    1Ai1Bl3Ul4Rj8Xh9fhA0jB_hC6hIslJ8mJMkJcoJgk02n0Qi0Sm0nh3UhI1hIjmJMj0bj0ek2mk2xj7TjH.jIknJ5i\
    JuiKZh02n0Sj1Bh3UjHPiI2iIZlIjlJMk02m0Sn0bh0ki0nh3UlB2hI1kIZiIjmJ5hJMl02j0ei1Bh2mj2xh6Hj6Ih\
    H.lIihIkmJ5iKZj0bF02l0Lj0bi0em1Bi2mh2xh4FkACiBRhDziH.hIZhIkkJ5jKZi0Gn0Ui0bj1Ai1Bi3Uk5ih8fh\
    8uhDyiEJhHKhIskJ8lJMkJck07k0Bj0Uh1Aj2ml2xnHmlIsk07l0Bl19i2mk2xkHmjIsn07l0Bk0Hh10h1Ah2mj2xl\
    HmhIsn02m0Sk1Bh3Uj5Wi67h84iFRiHPiI2nIZjIjlJMl0Gj0xl3Uj6cj6vhADiEpiIuiJ8pJMkKdh02l0Fi0Sk3Uj\
    I2nIZiIjmJMl0bh0ej1Bh2mk6IiBjiH4iH.jIkoJ5hKZi02j0bh0ej1Dh2mj2xjH.lIijIkmJ5hJuh12i2An3ok4Gm\
    EymINmIOlJFjJJlKXh02k0Qh0Sl0kh3UkD6hI2jIZhIihIjmJMkJQh07l0Bk0Uh0kh1Ai2mi2xkHmjIsm02m0Lj0bj\
    0eo1Bl2mk2xk8lhCzhIMhIZhIihIklJ5jKZi0Gi0bh0xk19i1Ah3Ul4kh5riDDiIkiIsjJ8nJMj02l0Sk2hh3Uj5ph\
    6khD6hDOhDhhFRiHPhI2nIZhIjmJMj0Gi0xk19k3Ul5Oh6Ji6liHXhIkiIsiIuiJ8oJMk02n0Qi0Sm3UiIZhIihIjk\
    07l0Bl0Hh0Qh10i2mjHmjIsn0bh0ej1Dh2mj6Hm6IhHXhH.lIkoJ5jKZh02m0Sk0ki3UjHIhI1iIjmJMmJQh0Gk0xl\
    3UkA1iC8iIsiJ8pJMjJgh02j0bh0ej1Bi2mi2xi3Sh6Hh8mjFuhH.kIilIkoJ5hJuhKRj0Gk0bh0xj19i1Ak3Ul4kh\
    5rh7AhJ8nJMl0Gk0xj19h1Ai3UkJ8oJMlJgh0Gh0xi19h3UiEThJ8pJMn02m0bj0ni3UjI1kIiiIjlJ5jJMl0xk1Aj\
    3Ui6XhDWiDmiEdhExiIsiIuiJ8pJMl0xm1Dh3Ul5niJ8pJMl1JG20K2iK31JKiK07n0Bk0Qi2mi2xjHmhIsn02i2mh\
    2xh6Hh6Ij6mhBMhH.lIihIknKZi02i0Li0bl0eo1Bi2mi4Fi5eiDrhIZkIkkJ5j07m0Bl2mj2xjHmiIsm0Gh0xl1Di\
    3Uk9vhGshGwhIkhJ8oJMn02k0Fj0Sl1Bh3Uj6eh8dhHPiI2nIZjIjmJMi02m0Li0bl0en1Bi2mj2xh7Mh7eh8EhCzh\
    EqhFkhIZiIihIklJuh1TG1tGKiD0Gn0Ui0bi0xh1Aj1Bi3UkGUiIsjIujJ8mJMiJclJgh07l0Bj0Hi2mk2xmHmiIsm\
    02k0Lm0eo1Bj2mi2xi4FiIZjIiiIkjJ5hJuhKZi0Gh0xm19h1Aj3Uk7oiDIhIsjJ8oJMl0xj19h3UiCliIkiJ8qJMk\
    0Gh0xj1Ah3Ul7Gh9tjC7hCHjCMiD3hEziIsjJ8oJMj02j0Lh0bi0eo1Bk2mj2xj4Fh5bhDAjIZlIihIkkJ5lKWh02m\
    0Lh0bj0en2mi2xk4Fi5kh7_h9rhIZjIiiIklJ5iKZi07n0Bl0Qh0Ui10i19h1Ah2mk2xlHmkIsn0Gh0xl19i1Ai3Uk\
    CnhDIhDTiHqiIkhJ8oJMl0Gm0Ui1Bj3Ui4Ri6Eh6GhG0hGvhIslJ8lJMkJcl02k0Ll0em1Bh2mi2xi9riA_iGtiIZh\
    IklJ5iJuh0Gn0bh0xi1Bj3Ul4Rh8Xi9DhCMjIskIuhJ8mJMkJcjJgiJhh0Go0Ui0bj0xk19j1Ak1Bl3Uk4Rh4yh7Bi\
    97h9Ni9xhAThCGhDGkI3hIskIuhJ8nJMkJclJgi0Gm0Ui19i1Aj1Bj3Ul9fiDBhGUhIsjIuhJ8nJMlJciJgi0Gh0xj\
    19k3UjC7iCeiCxhIsiJ8pJMjJgh0Gi0bh0xl1Ai3Uj4ki9yhEoiJ8pJMl0ji0lh0mr0vo1Mj1Xh1iq1mr1.q2di2gn\
    2on2si2vm3em3tl41l49l4In4Ti4fh5Ml60m8YhEvpHhoHioHprHwnHzmILjI.sJClJStJmhKUsKVr0Go0Uh0bi0xh\
    1Al1Bk3Ui4Ri4yh8gjATjBqhC0hIsiIulJ8mJMlJcmJgl0Gk0xl1Ah1Bh3Uk4ki7AhBvhC3hDmhELiF_hIkhIshJ8o\
    JMkJgi0Gi0bi0xk1Aj3Ui7ShHqhIshIthJ8nJMkJgi0Gi0xl1Ah3Uk4kh5rh6DhD8hJ8oJMjJgh02l0Lk0bi0em1Bi\
    2mi2xj4Fi7ehFbhH.iIZiIiiIkmJ5hJuh07n0Bm0Hh0kj2mj2xjIsm07n0Aj0Bi0Hh0Uj0bh1Bh2ml2xkIXmIsl0FF\
    0GL0KD0bG0.I1BG1ZL9aK0Gh0xm19h3Uk4kh7bhClhDKjDTiGhhIkhJ8pJMn0Gn0Ui0bk19h1Ak1Bj3Uk9mhAchF1h\
    IslJ8nJMjJcmJgj0Gm0Ui0bj19h1Aj1Bi3Ui4Rh6Gi6Rh6si8Kh8.hCRhHKiIsmJ8mJMhJclJgh0Gj0xk19i3Uk4.h\
    6ThBviIkhIsjJ8pJMj02i0bh0ei2mh2xjAxiDchH.lIiiIknJ5j0xm1Ai3Uj4khDJhJ8pJMlJgh02m0Sm0bhI1kIjl\
    J5hJMj07m0Bl19h2ml2xkHmiIsl0Gl1Ai1Bi3Ul9fj9tjBvhC0jFohIsjIuhJ8oJMkJciJgi0Gi0xl1Ah3Uj4Rh4kh\
    BthCeiE_iI3iIskIuiJ8nJMlJgi01p29k2ej07n0Bj0Uh0bh0ki10h2mj2xjIsn02k0Lm0bj0en2mi2xi3Si4Fh9Li\
    CbhDzhEqiF5hFkiIZhIihIkmJukKZh0Gn0Uh0bj1Ah1Bi3Uk4Ri7Ki83iB_iHKhI3iIslJ8lJMlJcnJglK8i0xm3Ul\
    BdhHChHXiIkkJ8nJMn02l0So0ki3UiI1iIjlJMlJQh02k0bi0ek2mk2xl9Eh9ShH.jIiiIknJ5iJ8i0Gh0xm19h3Uj\
    9wiIkhJ8qJMl07n0Bj0Ki0Uh19h2mk2xkHmhIsl0Gn0bj0xh19j1Aj1Bj3Ul4Ri5Jh6Oh8.i9mhA0hAThI3hIsnIuh\
    J8lJMlJcl02i0bi0eh1Bh2ml6IhFniHrhH.mIkoJ5mJuiKZk12j2Aj3oj4GmINjIOhJFmJJl0Ln0Sn0kh1Bi2hh3Uk\
    45m5hnHPiIZiIjqIwkJKmJMkJQiJuhKDh02j0Lj0bh0em1Bi2xj3Sh4Fj88iIZjIklJ5hJuiKZi02m0Lm0bi0ei1Bh\
    2mi2xi90hDAhE7hIZiIiiIknJuj1SmInlJRn0Gi0bh0xi19i3UlCNiE_hFojIshJ8oJMj0Gj0xk1Ai3Ul4kh6aiFvj\
    GshJ8pJMkJgh02n0Sl0nj3UjI1iIjlJ5hJMjJQi02m0Sn0ki0ni3UhI1kIiiIjmJMjJQh1TF1tG2iHKiE0xm19h1Dh\
    3UkCwhGPhGwhH2jJ8oJMn07l0Bl2ml2xmHmiIsm0Gi0xm19i3Um4liArhIsiJ8pJMiJgi0Gn0bk19h1Ai1Bj3Uk4Ri\
    7KiBFhHKhI3iIsmJ8lJMlJcmJgi0Gi0bk0xk3Uj4kj7phIkhJ8nJMlJgh02l0Li0bi0em1Bh2mi2xh48h4Fi7ci7eh\
    8ri9shAghH.jIZiIijIkl07o0Ak0Bk0Ki0Um0dh1Bj2mk2xj3Yh3_i64i6th8Aj9TiD1iG6hGKhHGiIXnIslKSi07l\
    0Bj0Ul2mk2xk9ThCCiIXlIsl01q07n0Bj0Hi0Uh19i2mk2xnIsl0mi4Wm96mEviHxnJamJmh0Gi0xl1Ai3UjBKhDgh\
    J8pJMjJgh0En0Gi0xk1Di3UkBchEAiEGkHqhIkiJ8oJMm0Gj0xm1Ai3Uj5Sh7Lh9ZhBZiBfhIuiJ8pJMj0xl1Ai3Uk\
    5nhJ8pJMk02l0Li0bk0eo1Bj2mh2xi4Fj8lhBRiBoiIZjIihIklJ5h17o1NlHnlIgmJrlKbl02m0So0ki0nh1Bh3Ui\
    I1kIjnJMlJQh0Gm0xk19h1Ah1Bh3Uk6Ti7Gh7Xh7qh8vh9ckC8iD3hIsjIuiJ8oJMjJchJgi07l0Ah0Bj0Kh0Um2mi\
    2xl9TiCphF6hFCiIXnIsk0xl1Aj1Bh3Ul4kh70hDshIkhJ8pJMlJghKfh0Gi0xk1Ah1Bh3Uk6vhC3hDmjIshJ8pJMj\
    02m0Sj0kh3UlIjnJMlJQi0Gh0xl1Ai1Bi3Ui94hAMiCjiJ8pJMm02l0Sl0bi3Ui7ki8WiF7hHPhI2oIZhIjmJMj02l\
    0Fh0Qh0Sm0ni3UhIjmJMi0Gh0xl19k1Ah3Ui4khC8hC9hFxiG2hJ8oJMi0Gi0xl19h1Ah3UkCyhIkiJ8qJMk02l0Li\
    0em1Bl2mi2xjBwhFbhIZiIihIklJuj0Gm0bh0xj1Bh3Uk5Sh7qh8kh9ekFVhF_hIsiJ8oJMlJgh02m0Sl0kh3Uj4si\
    I1iIihIjnJMkJQi0Gj0xl19h1Ai1Bh3Um7rhBSiDWhIujJ8pJMl0Gh0xm19h3Uk77h8ehDDhE2hENiExhIkhIshJ8q\
    JMmJgi02i0ei1Bh2ml2xj3Sh6Ij7Ti8ihH.jIihIknJ5iJuhKRhKZi0Go0Uj0bi0xi19k1Ai1Bj3Ul4Rh4yhC1kHKj\
    IsmJ8lJMjJckJgl07l0Bj0Hh0kg2ml2xmHmjIsj0Gk0bh0xm19h3Uk6lh7Sh9KhAdhAriBzhEagE_hJ8oJMlJgh0Gj\
    0xk1Ag3UkCJiCVhJ8qJMmJgh0Gj0xl19h3Uj77iJ8nJMkJghJti08l09o0Nl0jq1Gl1Mq1bl1cm1fl1ij1ok1ph1rk\
    1vj1yi1zl2Cl2Jl2dk2ql35n3Om41i49n4Dh4Ti4fl5MiEesHOlHQlHShHghHimHkhHooHsoHwmIemIfoJ6lJHjJlj\
    Jmi07n0Bk0Qh10h19h2mk2xlHmiIsl1TJ2iKKiH02l0Sj3Ul6khBnhHPhI2nIZiIjnJMj0Wk2al2pj4Bm4Jl4ck5Hl\
    HRkHjm0Gh0xm1Aj3Uk4khJ8pJEhJMkJgi07m0Bj0Hi2mk2xjHmhIsn02l0Sm3UiHPiI2oIZhIjmJMh0Gj0xl19h3Ul\
    4kh91iDKhIuhJ8qJMlJgi02j0Fi0Sn5phHPjI2kIZjIjkJMl07W0HWIjW0Gh0xm3Uk4ki5Si7AhDEhE8hEKiENiIsj\
    J8pJMmJgh02m0Sl1Bh3UkIZhIjlJMm02k0Lk0bi0em1Bi2mi2xh7ehDAhGxiIMhIZjIiiIknJ5iJuj07m0Bj0Ul0dh\
    1Bh2mk2xkAXiD1hESlHJkIXkIslKSh0pJ1TG1hK1tIKiF0FA0KF0QK0SF0bL0.G1ZJ7IL9aKIZK02h0eh2ml2xi6Ii\
    7uhDFhH.jIkmJ5iKZj07l0Bk0kh2mk2xkHmhImhIsn0Gm0Uh1Bi3Uk4Rh8Oh9phBqhE9iG0hI3hIslJ8lJMjJcnJgi\
    07o0Bh0Hh0Uh19i2ml2xm9ahHmkIsl0Gi0xk19h3Uk4kiCuhIuiJ8oJMm02k0Fh0Sk0bh3Ui5pi6eh9lhAYiE3hHPj\
    I2nIZjIjlJMj12i2Am3ol4GkEykINiJFjJJl02l0Sn3UkI1hIjmJMlJQi07n0Ah0Bl0Kh0Ul0bi1Bh2mj2xlF6jG8i\
    GKhIXlIsj07n0Bj0Hh0Ui2mk2xkIsl0Gi0xi3Um4.h6vh9thBfiCHhD3jELhEdhHqhIkiIsiJ8pJMjJgh0Nk0Wl0tn\
    1Fo1Oh1Qk1Xp1dl1kk1oi1qm21i22m28n2Bj2Kl2Qm2Sj2gi2km2nk2oo34k3rh46m4Jj4jn5AmE6jI5lIAkIGmIcp\
    IrmI_jJAmJBjJGtJInJroK6lKck0Gn0Uk0bi1Ah1Bi3UkHKhIslJ8nJMjJcm02m0Qj0Sm3UiI1iIjmJMkJQi0Gh0xm\
    19i1Ah1Bh3Ui4ki6lj7SjBuhHqiJ8oJMlJgh02m0Lj0bk0en1Bk2mhFDiFXhIZjIklJ5iKZi0Gk0xi1Ai3Ul4khD2i\
    IkiJ8oJMl0xm5nhARhAwiHqhIklJ8pJMm02i0ej1Bh2ml4FiH.kIknJ5hKRjKZk12i2Al4GlEymINjIOnJFi0xm3Ul\
    9wiJ8pJMl07n0Bk2mk2xlHmiIsm0Go0bj1Aj1Bj3Ui5Ni8uh9xhA0hAOhBFhIsjJ8kJMjJck02n0Lm0bk0en1Bi2mi\
    2xj4FhBEiBshFDhG.iIZhIklJ5iKZh07l0Bk0Hh0Kh0Qh10h2mk2xjHmjIsl0xk1Bi3UmHqhJ8pJMmJgi07n0Bk0Hh\
    0Qh0bh10i19i1Bh2ml2xkIsn0Gi0xl1Ai3UjBfiDgiEEiELiEdhEohHqhIkhIsiJ8pJMlKdj0Gk0bk0xl19i1Al3Uj\
    9YhC7hDDhDEiHXhIsjIuhJ8oJMl02j0Fi0Si0bi3UiI2mIZkIjlJMi07l0Ah0Bi0Kh0Ul1Bh2mk2xkFchG6iIXkIsl\
    0HK0.L7IKJQK0tl1Fl1Om1Qj1Xl1dl1kj1qk21i22k24k28j2Bi2Kk2Qj2Si2gl2on2qh3rh46k4jl5Aj5Im5qk60k\
    CdlDnmHWjI5lIAlIGjILhIriI_sJAoJBpJCmJIhJYnJZiJlhK6jKAhKcj02k0Lj0bj0en1Bi2mi2xm5bh90hCciIZl\
    IiiIkkJui0Gh0xk19h1Ak1Bi3Uj7oiDBhDWiEKiIslIuhJ8mJMj0Gn0Ui0bi1Ai1Bj3Uj8OhB7hGUhHKjIslJ8kJMj\
    JclJgiKCi0Gi0bj0xm19i1Aj1Bh3UiF9jJ8pJMmJgh0yG1JE1gG20L31K0yK1JF1gE31I02l0Lj0eo1Bj4FhFXhIih\
    IkkJuj0Gj0xl19i1Ai1Bi3Uj5jhE1hIkhJ8qJMkJgj0Gj0xl1Ai3UjC4hJ8pJMk0Gk0xl3Ui7bhCwjD2hJ8pJMlJgh\
    Jth07j0Bl0Ui0bh0kh19h1Bh2mk2xlHmiIsm02k0Ll0bi0em1Bj2mh2xj4FjFXgFehIZhIigIkkJ5iKZg0Go0Uj0bj\
    19h1Ag1Bj3Ui4Rj5Lg5ig5tg6Oh9Dh9phATgBFjIsnIuiJ8mJMlJcnJgh0Gj0xk19i1Ah3Uj7XiD3hDWiE8iF_hIsj\
    IuhJ8mJMjJgh0Gj0xm3Uk4kh9fgBfhBvgC7hDmgEdgHXgIkgIuiJ8oJMiKdj1No3ojHnmJFlJJkJrk0Gi0bh0xk3Ul\
    80h9BgDEiIkhIsiJ8pJMlJgj07l0Bj0Uj1Bh2mj2xlIsn02l0Fh0Sl3Uh6khErhHPjI2nIZhIjjJMlKDh17m1Nm2ah\
    4chHnnIgnJrnKbj1TI1tGKiE07m0Ai0Bj0Ki0Ul2mj2xlAXhDZjHJhIXkIskKSh0Gi0xl1Dh3Uk4khJ8oJMlJgh02l\
    0Fh0Sm3UkI1kIjkJMiJuiIkWJ8WJcWJgWJuW02n0Sj0ni3UhIZiIjkJMlJQh07m0Bl0Hh0Uh10h2mj2xiHmhIsm1Np\
    3olHnnJFjJJpJro07m0Bj2ml2xmHmjIsm0Gi0xk19i1Ai3UkAPiIkhIsiJ8pJMj07l0Bj0Ui19h2ml2xkHmjIsm01q\
    0oi3Ri0Gh0xl1Ai3UkDEhDmhELhEohIsjJ8pJMjKdj0jh1Mi35p4To4foJei07l0Bk0Qh0Ui10h2ml2xkHmjIsm17o\
    2ai4ckHRiIgmKbl07n0Bm0Kh0Qk0ki2mk2xiHmjImiIsm07l0Bl0Ui10i1Bh2mk2xkIsl07n0Hi0Kh0Uh0kh10h19h\
    2mj2xkCshIsl02o0Sl3UhI1jIZiIjkJMkJQh0Zk12i2Gh4GiEylINiIOlJ2i0xm19h3Uk6Ch70h8xiHXiJ8pJMn0xl\
    3UlAkhJ8qJMmJgh0Gi0bh0xm19h1Ah3Uj4kh5rhE1hIkhIsiJ8pJMk0Gk0xl19h1Ai3Uj4khBKiCxhD8hIshJ8pJMj\
    Jgh02n0Li0bh0em1Bk2mj2xj4FiBRhCQiDAiIZiIkjJ5jJuiKZj02l0Sk3Ul8UhI2oIZiIjmJMl0xl1Aj1Bi3UkBbi\
    IkhJ8qJMkJgh07n0Bj0Hh2mm2xlHmiIsl0ek2ml2xj8ihH.kIihIkoJuhKZh0Nk0lk0mp0vo1Oj1cm1dl1kn1ol1pl\
    1yh1zj21l22k23k2Ck2Qj2dh2kl2ni2vj4Ih4Wo5Yk5ym6Lh8Yi96pC5iCtrHQhHjhHljHxnHyhHzjIdjIemI.jJSh\
    JapK6mKVlKci07m0Bh0Ui0kh19j2mj2xkHmiIsm0Gi0xj1Ah3UjBfiHXhIkiIsiJ8oJMl05l0JkIYh0Gj0xh19h3Ul\
    8viIsiJ8nJMlJgh0Gj0xl1Ai3Um7Xi9zhBShCNhCehCxhI3hIkhIuhJ8pJMi07m0Bk0Hh10i1Bh2ml2xlHmhImhIsm\
    0Gh0xl1Bh3Uj4kh5nhFvhHXhHqhJ8pJMm0Go0Uj0bk19j1Ai1Bi3Um8uiAzhB_iHKhIsmIuhJ8kJMlJclJgh07n0Bi\
    0Uh19h1Bh2mj2xlHmjIsm0FI0KL0QK0SH0kL0.H1ZK7IH07m0Bk0kh19h2mk2xkHmkIsn02i0Lh0bh0el1Bh2mj2xi\
    3Sh4FiIZiIihIkmJ5jJui07j0Ai0Kh0Um1Bi2ml2xl9TiDkkFMiGghIXlIsj02l0Sj1Bh3Uk5hh7kh8WiHPiI2nIZi\
    IjmJMi07n0Bi0Ui19h2mj2xjIsm0Gi0xl19i3Uk4lhAwiEAhJ8pJMm07m0Bi0Ui1Ah2ml2xlIsk07l0Bj0Ki0Ul1Bi\
    2mj2xkFIlIXkIsl07m0Aj0Bi0Uk2ml2xmD1hIXmIsi02k0Lj0bj0en1Bk2mi2xj4Fi5bh6Kh88hDrhFXhGvhIZiIii\
    IkmJ5jJhh1Nl3olHnmJFnJJlJri07n0Bk0Uh2mk2xmHmiIsl07o0Bj0Uh19h2mk2xjGVhHmkIsl07o0Bi0Hk0Uh10h\
    2mk2xjIsm07l0Bk0Kh0Ul2ml2xh64hE4iFBiIXkIsl0Gk0xk19h1Ah1Bh3Uk7WjIsiJ8nJMm07m0Bj2mk2xmHmhIsn\
    07n0Bj0Uh1Ah2mj2xlEXhHmhIsn07m0Ah0Bj0Kh0Ul0bi0dh2mj2xlF6hIXkIsl02o0Sn0kh0nh3UkI1iIjmJMl02l\
    0Sn0ki3Ui44hI1iIZiIihIjnJ5h03W0wW0_W1jW2jWI6W0Gi0xm1Aj1Bi3Uj7XhDEhENjIsiIuhJ8oJMi02l0Sl0bh\
    1Bh3Uj45i5Dl5xi9iiBJjBTiEriHPiI2oIZkIjmIwhJMl02n0Sl0ki3Ui6Bh82hI1iIZjIjkJMkJQh07n0Bk0Hh10h\
    2mm2xjHmkIsl0Gh0bh0xm3Uk7Ai7Gh80k8khE8hIsiIuhJ8oJMk0Gi0bi0xj1Ai3Uk7Xi7nh9eh9ziBtiDBhFxhHqh\
    IkiIsiIuiJ8pJMlJgk0Gm0Uh0bk1Ah3Um4Rh8Hi8fh9ghCRhE9hIslJ8lJMkJclK8hKYi02n0Lh0bk0eo1Bj2mj2xl\
    8EhDziFDiIZjIikIkjJ5jJuhKZi07n0Ah0Bh0Uj2ml2xlIsl07m0Ah0Bi0Ul0bh2mj2xjIXlIsl02m0Qi0So0ni3Ui\
    I1iIjkJMk02n0Lj0bk0en1Bi2mi2xi5biDAiDrhIMhIZjIikIkmJ5iJuj07m0Bi0Uj1Bh2mk2xi3Yh64h8nhE4hFIh\
    IXiIso02m0Qh0Sm0kh0nh3UjI1jIZhIjmJMjJQi02h0Lj0bi0en1Bk2xi3Sh4Fh90h9shAfhIZiIijIkkJuiKZiGGW\
    IkWJ8WJgWJuW02m0Fh0Sl3Ui8JhHPjI2nIZkIjmJKiJMj07l0Bk0Hh0bh1Ah2mj2xlHmjIsl07o0Bj1Ah2ml2xiHmi\
    Isl02h0bh0ej2mj2xk6IhH.iIZiIknJ5hKZh02k0Ll0bi0em1Bh2mj2xk3Si4Fi7Qh7_iBxhCQiGvhIZjIihIkjJ5h\
    JujKZi07n0Bl0Qh0kh10h2mk2xkHmhIsm07l0Ai0Bj0Uj0bh1Bi2mk2xm3YhAXhIXlIsl0bi0ei2mj6HiH.mIihIkm\
    J5iKZi0Gk0xl1Aj1Bi3Uk5nh9fhCejIsiJ8oJMiJchJghKEh02k0Qh0Sn0kh0nh3UlI1jIjlJMl07n0Bj0Uk0bi1Bh\
    2mk2xjHmhIsm02n0Sn0ni3UjI1jIZhIjlJMj07l0Bk0Hh19i1Ah2mk2xkIsl02l0Sk3UjI1iIZiIjlJMl02k0bh0ek\
    2mj2xk6HiAhlAphH.kIZhIiiIknJ5iKZi0Gk0xl1Ah1Bh3UkBghCfhJ8oJMk0Nj0Pi1Ol1Qk1Xk1ck1dm1kn1oj1pi\
    1qj1zi21k22k28o2Kj2Qm2go2kj2np2om38p46j4Ij4ji5Aj5Ih5ym60nDnhHvlHzjI5iIAlIGkIclIrpJ2kJ4nJIm\
    JTsJZhK6lKcl0Gk0xl1Ak1Dh3Ul9WhCHhF_hIsjIuhJ8oJMkJgh0xm3Ul5niGWiJ8pJMmIkWJ8WJgWJuW0Gk0xl19i\
    1Ai3Uj4ki7oi9zhC7hCMhFxhIsiIuiJ8oJMh02j0Lj0bk0en1Bh2mi2xk4Fh5oi7ci9OhDrhH.iIZkIijIkm0Gn0Uh\
    0bi1Bh3Um4Ri5Ni8Ki9xhBFhDyhEJhI3iIslIuhJ8jJMjJcn07n0Bh0Hi19i2mh2xlHmiIsm0xk1Aj3Ul9BhHXhIkj\
    J8pJMjJgh0LL1BG0xm1Ah3UmGsiIkhJ8nJMm02n0Sk0ni3UkI1hIZhIjlJMkJQh07l0Aj0Bi0Ki0Ul0bh0dh2mj2xj\
    7PiB0hIXmIsj02k0Sk1Bj2hi3UkHPhI2lIjjJMi07m0Ai0Bj0Uj0bh2ml2xlF6hGKiGbhGghIXnIsj07l0Bl0Qh0Ui\
    2ml2xlHmhIsl07l0Bj10i19h2mk2xkHmjIsm07n0Ah0Bi0Ul2mk2xjIXmIslIkWJ8WJgWIkWJ8WJgW02n0Sm0ki1Bh\
    I1kIihIjmJQh07l0Ai0Bk0Uk0dh2mj2xl6ihIXnIsj0Gi0xk19h3UkBziHqhIujJ8oJMk17o1Nk4cjHRiHnmIgpJrl\
    Kbo03W0wW0_W1jW2jWI6W07m0Bj0Ui1Ai2ml2xiIsm07m0Bk0Um0bh1Bi2ml2xl3_i9ThCphG8hIXmIsi0Gi0xl3Uk\
    HXhIkhJ8pJMm07m0Bk0Uh2mm2xlIsl02m0Fh0Sl3UkI1jIiiIjlJMk02k0Qh0Sl0bi0nh3UjBehIjmJ5lJMk0xn3Uj\
    9XhCEhETjJ8oJMmJgh07m0Bk0Ui2mk2xl9jhHmjIsm02m0Qh0Sl3UiI1hIZiIjmJ5hJMlJuh07n0Bk19i2mj2xmHmj\
    Ism1TI1hK1tI2iKKiH0Gm0Ui0bi19i1Aj1Bi3Ui4Ri9fi9xhBqhE9iIsjIuhJ8mJMkJciJgiKEh07n0Bi10h19i2ml\
    2xkIsl0xj3Uk5niEmhFyhJ8oJMm07m0Bk0Uh19i2mj2xkHmlImhIsm07n0Bk0Hh2ml2xlIsm07m0Bm0Qh0kh10j19h\
    2mj2xjIsn02i0Sk0ni3UlI1hIZhIjnJMkJQhJXh07l0Ak0Bj0Um0bi0di1Bi2mj2xi8AhFCiG8iIXmIsiKBh07m0Bi\
    0Uh10j1Ai2mj2xjHmjIsl02l0Sl3UlI1jIjlJMm07m0Ai0Bi0Uk2mj2xlGKhIXlIsj0xl1Dh3UkAkhEnhJ8pJMl07m\
    0Ai0Bi0Kh0Uk0bj2ml2xj3Yk64h8AiAXhCphDkjIXnIskKSh0Gh0xn1Ah1Dh3UiAriCEhIuhJ8nJMlJgh07m0Bj0Uh\
    19i2mj2xjHmjIsn0_W1jW2jWI6W07l0Bi0Hi10h19h2mk2xkHmjImhIsn07j0Bl0Kh2mj2xkHmlIsl07m0Bk0Kh0Uj\
    2mj2xkD1hIXlIsl07n0Bi0Hh0Uh1Ah2mj2xjIsl07o0Bk0kh2ml2xjIsm0Eo4Kk5dh6Wh7Hh02k0Ll0bh0en1Bi2mk\
    2xi4Fj75jE7hIZjIihIklJ5iKGh02l0Qh0Sj3UkI1iIZhIjlJMkJQhJuh0Gi0bh0xl1Aj3UlARjFvhJ8pJMj02i0Lm\
    0eo1Bi2mi2xi4Fi9rhDYhIZlIiiIklJ5hJui0Em0LW8TW8qWAOWIkWJ8WJcWJgWJhWJuW0Gj0xm1Ah3UkH2hJ8oJMk\
    02l0Lk0bl0en1Bi2mj2xiAhhGvhIZiIijIklJ5jJuh07l0Bi0Ui0bh2mk2xlHmjIslKKh07m0Bj10i2ml2xlImhIsl\
    0FD0KH0SH0kK0.H1ZH7IIIZJ02l0Lm0bj0el1Bi2mi2xj4Fh9Ah9dhALhGthIZiIklJ5iKZh02n0Sm0kh3UhIjlJMi\
    02m0Sn1BhI1kIjnJMj0Gi0xl3Um8MhCJhCjjJ8pJMl03W0wWKOW02j0ej1Bj2mi2xj6IhH.iIihIkoJ5jKZh03W0wW\
    7ZW07m0Bj0Ui0kh2mk2xkHmiIsl02k0Lh0bi0em1Bi2mi2xi4FiAfhAgjIZiIklJ5jJuiKGjKZh07m0Bi0Hi0Uj0kh\
    2mi2xlHmkIsm0ej3SkHXiI3h07l0Bi16h19h2ml2xkHmiImiIsn0LW8rW9YWIkWJ8WJcWJgWJuW0xi1Dh3UiBdhCSh\
    IkhJ8pJMm02h0bh0ej1Bh2mk9ShHXhH.jIiiIkoJ5iKZh03W0wW03W0_W1jW2jWI6W07n0Ak0Bh0Uk0bi0dj1Bl2mk\
    2xk3Yi3_o6tmIshJckJgiKSi0FL0KK0.I1ZKJQKJXL02n0Sn0njFphI1iIZiIjlJMhJQh07k0Bi0Uk0bj0dh1Bh2mj\
    2xj64hDZiEShIXmIsl0LW4OW5NW6GW8DW8OW8gW95W9gWAOWB7WBFWCGWDGWE9WIkWJ8WJcWJgWJnWKYW3on4GhHnh\
    JFoJJn03W0wW0_W1jW2jWI6W0BF0FH0KB0QI0bL0kJ0.F16L1ZI9aIDVJJQL08L0NJ0WF0tJ1FK1GK1OI1QH1bK1cL\
    1dJ1fH1iI1kK1qL1rH1vK1zK21G22K23L24K28G2BH2CH2QI2SG2kH2qI2tL3cJ3rD4JC5IH60G96KCdDDnHE6IHQI\
    HpLHwLI5HIAHIGHIcIIdFIeGIrEI_FJAKJGCJIHJJKJZLJeJJlKJrCK6KKcK0.l1Zh1si27iIZjJKk0NJ1FI1GH1OJ\
    1QG1bK1dL1fK1kJ1pH1qG1rF1vJ1yJ1zH21K22H23H24G28I2BI2CI2JH2QG2SH2gL2kI2qI2rK2tE2wI3cA4JK5IH\
    5qC60BCdFE6IHOIHREHSJI5GIAEIGHIeHI_DJAGJBFJIIJJDJYFJZHJlIK6GKcJ0Jq1SkIYhInrJFh1ui2sm3cm6Lk\
    01q3qjHchIQh0Mn0Em1ei4gk5EiIQh08h0Vp0tp1Gh1Ql1Xk1bk1dh1qk1ri1yi22l24l28i2Bm2Ji2Qh2Sj2ki2om\
    2qj2wk3Ar3rn3tl5Ak5Il5qlE6kHbiHhjHtkI5kIAkIGiILmIahJ3rJBpJCpJImJYpJZhJljKci03W0GW0wW0xW1CW\
    1DW3PW4HW53W5PW5SW5nW6DW6mW7OW9wWD5WHdWHeWH.W01q29h4XhJqi0Eo08l09h15o1Gl1Nm1Qm1bn1fm1qj1ro\
    1ur1vk1ym24m2Bm2Ci2Gk2Jm2pi2qn2ss2wn3Ol3cr41h5Hi5Ih63m6Lm74k98mHOjHSnHbhHhoHkkHwlI5mI8rIAl\
    IaoIbmIdjJ6hJFiJHiJZlJlm08l0Vm0to17o1Go1Qo1bm1fk1ql1rn1ul1vm1yj24m28i2Ak2Bn2Jn2Qk2Sl2ql2rl\
    2so2wp3Ah3Oi3cq3tl5Il63h6Lj74l9ImEyjHRiHSsHalHhnHtoI5mI8lIAoIGkILjIarJ3iJCjJInJZnJlmKHm01p\
    2ei3Qh1Ci03i2co4_j9kiIyk0yJ1JH1gK2iKKiF1mh1.i3eqI.iJ4hJmrKUi09l0Vn15p1Xl2gl2op2sl2wj3Oh3ej\
    3tk41j49k4In4Tj4Wj4fh5Mm60j96jC5mDLhHWmHasHbpHkpHtnHwoHzkI8sILhIatIbpJ3lJ6lJBiJCpJYi08l0Nl\
    0Zl0jl12k1Gl1Mp1bh1ck1fn1ir1ol1pj1ri1vn1yl1zj1.h23o2Ck2qj35p49o4Tm4fn5Ik5MoC5kDLjEekHOmHQm\
    HniHopHppHwjINjIdkIelJerKUjKbl05l0Jh08h09p15q1Gl1Qi1bj1fk1vm1yj23h24i2Bj2Ji2qk2wh35i3On41o\
    4Do4xn5Ih5wqDLhHOiHajHolHshHwnI5iIAjIGkIboIdiIekJ6pJHiJUiJZjJlm0Js12h1ShInr03F0BI0KL0LC0QE\
    0bH0kD0wF16L1BF1lG2cFFcJHNHI2LIX8ImEJXCJc9JgBJu71SlInnJRo09p0jt0tm15m1Mt1iu1mm1.q2Kl2do2nk\
    2wo35o3Oo3cj3rk49j4Di4Th4jj4xj5An5Yl8YlDLsHaiHiuHkmHptHsrIbkI.jJ6oJSpJakJmjKUrKVi08m0Nh0Zi\
    12l1Gl1Mm1bi1cm1fm1is1mm1ok1pn1ri1vn1yj1zi1.n23n2Cm2Jk2dl2ql35n49m4Tm4fn5MlC5lHOoHQpHipHoo\
    HwjINnIdlIelJHjJSjJesJlkJmkKUpKbn3Sj08n09q0Ni0jo1Gl1Ml1Qi1Yn1bm1fn1pj1qj1rm1vm1yn23i24m2Bn\
    2Ck2Jk2al2qn2sl35k3Ol41m49k4Dm4Ti4fh4xm5IjDLqHOlHQjHhmHkhHomHwmI5kINhIdjIehJ6mJFjJHtJZhJll\
    08j0Vq0tj1Gj1Qj1Xj1bj1qk1rk1yl24l28i2Bl2Jj2Qj2Si2kh2ok2qj2sk2wl3As3cm3ro3to5Ij74iHWjHbqHhl\
    I5lI8iIAhIGlILoIapJ3nJClJIkJThJYhJZlJli0Nl0Pr0lo1Ol1Qh1co1dm1kl1om1pn1qh1zk21l22m23i28i2Qn\
    2Sk2gn2ki2nl34m4Ii4Wi4ci5Ah5Yj5yo60m8Yh96gHQgHlmHvjHxjHynHzkI5hIgiJ4qJIiJUrK6mKcj0Pq1Om1Qk\
    1cl1dm1km1ph1ql1zh21k22n28k2Qm2Sl2gm2kl2nn2oh38p4Il5Ah5Ih5yo60n96lFwmHukHyhHzjI5jIAlIciIrl\
    J2oJ4pJIjJTtJZiK6mKcn09s0jp15r1Ml1Xj1ij2go2om2sh3Oj3ej3tl41o49l4Im4Tk4fi5Mm60kC5nDLpHapHho\
    HimHkrHohHpjHsrHzlI8pILjIakIbrJ6sJCp0Pl0lo0mo0ti0vo2ni2vl2wi35n3Oj3cn3rm46k4Di4Wj4jl4xm5Ai\
    5Yi5yl8Yj96jEvoHojHunHyoHziI.mJ4hJanJmiKVk0Nk0Pp0lr0vm1Ok1cm1do1km1ok1pk1uj1zj21j22m23m2Ci\
    2Sh2gm2kj2nj2vp3eh4Es4Gk4In4Wm5Ym5yh74h8Yl96kC5kCthHOiHQmHuqHvjHxmHznIOiIzkI_iJ4nJIiJThJah\
    K6mKAlKcm0Pm0lp0mo0vq1Xj1mk1.j2dh2gl2om2sh2vq38h3ej3ti41k49j4In4Tj4Wj4fi5Mj5Yj60k8Yi96kC5n\
    EvoHhlHuqHvjHwjHyqI.oJ4mJCmJSkKUhKVn0yH1JC1gI31J0ch1Ck1lo4Fj6mh7li9QiH9iIklKej01q2Nh03C0FI\
    0L80QE0SI0bK0kD0wK1BL1lI2cIDbII2BImIJQLJXGJcCJg8JuF0GlIntJcr3SW4FW4RW4kW5SW6IW9NWHqWIuWKZW\
    0Zo1Yj2GoCdkIzjJ2n08H0NH0ZD0mK1GI1OH1QE1cI1dG1fF1kH1mK1oH1pG1qK1rK1vJ1yI1zG1.K22K23H24H28K\
    2BH2CK2HG2SL2kJ2nL2qJ3oG4BH4TJ4fD5IJ5MJ5yHCtJHOJHQGHgGHnGHxEIALIdJIeIIzFI.GJSHJZGJaIK6IKAD\
    KVHKcH0_J01q4LhIPi08l15l1Gm1Nn1Qm1bo1fk1qi1rm1us1vo1yn24n2Bl2Ch2Gl2Jn2pm2qo2sq2wo3Ok3cq5Hm\
    5Il6Lk74lHOiHSoHasHbnHhpHkjHtkHwlI5mIAlIGiIaqIbmIdlI_hJChJFjJHiJIiJZmJehJllKHh01q0zj1Ri2Lj\
    2Oh3ii08I0NH0ZF1GI1OI1QJ1bL1cJ1dK1fK1iL1mK1oK1pH1qH1rL1yJ1zI21L22J23I24L28K2BK2CH2HE2QL2SJ\
    2qJ3oH41E4BJ4TL5IH5yFCtJEvKHOIHQHHgGHjJHnKI5JIGIIdJIeJJIIJZKJaDJeIK6LKALKUKKbIKcG01p36j3fh\
    3xh4AjIVl0Ep06h0Or0Ro0To0ro1Sn1nkInxJRw01p2fi3piI9i0El4zh57j5mh08F0NF12C1GI1OI1QK1bJ1cF1dK\
    1fG1iI1kI1mH1oG1pH1qH1rJ1vG1yG1zG1.I21H22J23F28L2BJ2CH2HD2JK2QJ2SL2kI2qH2sJ41E5II5YL8YJHOH\
    HQGHgEHiKHnIHpHHwKHxFIALINFIdIIeHJIFJZLJaCJeCJlKK6JKUGKbBKcH0Em5mhI0iJ7i01q2Li4Xh01p36j3fh\
    3hi4qiIBhIViIkWJ8WJgWJuW0Vq1Xm2ol2sl3en3rj3to41o49h4Ii4Tj4Wk4fi60m96iC5iE6mHWoHbnHhkHtnHwh\
    HzhIahJ3pJAiJBoJCkJYn02k0Li0bh0em1Bk2mi2xiFbiIZiIiiIkjJ5h0Jq0Oh1ShInpIzi1SjInhJRn01s2bi4Kj\
    4zh5ZhI7iJqk01pI9h02m0Sn3UjI1hIZhIjlJMl0Wi2Hn2pj2rl2tl34m4Bi5Hh6FhHljIfm1JI1tH2iK31LKiI01p\
    3Kh01p36h3hi3xhIBlIJiK7h01pCrj0Di0Gh0Js0Lp0eoJch05v0Jv0Ri0YmJ9m0GH0L70OL0bLI2FInEJRCJc508j\
    0Vj0tk17n1Gn1Nh1Qo1bl1fm1qm1rm1up1vl1yj24o28h2Bo2Jm2Ql2Sl2ql2rl2sp2wo3Oi3cq5Il6Ll74kEejEyk\
    HRiHSsHaoHbqHhoHtpHwhI5nI8qIAnIGjJ3jJCiJFhJIlJZlJln08k09h15p1Gm1Qi1bj1fj1oh1ph1rk1vi1yl24l\
    2Bk2Ch2Jk2qk2sh2wm35h3Oo41n4Dm4xn5Ii5wsHaqHhmHkpHokHwoI5lI8pIGhIekJ6iJZkJll0Ni0Wl0tl1Fj1Om\
    1Qm1Xq1dk1kj1ql21k22l28j2Bh2Kn2Ql2Sj2gl2kl2nl2oo38l46p4Jh4jo5An5Ij60iDnqFwkHvhI5jIAjIGmIrq\
    I_jJAiJBhJGrJIoJriKci0WF0tL1OK1QH1bJ1dH1fL1qI1rK1vL22J28I2BJ2CF2QI2SH2gK2kI2qK2tG3cF3rJ4JE\
    5IJ5qJ60G96ICdFE6LHOFHRKI5IIAGIGHIcKIeFIrII_EJ2JJAJJGHJIJJJGJTLJZKJlHJrFK6J08K0NI1FK1GK1OI\
    1QH1XG1bH1dI1qJ1rI1yK21F22H23F24I28H2BJ2CG2JJ2QJ2SK2kL2qK2tE3AJ3cA4JG5IJ5qB60DCdFE6HHOJHQH\
    HREI5HIAHIGHIdFI_FJBFJIHJJCJYFJZLJlKJrIKcJ05n0Jj2GhIYk05p0Jk1Nk2Ai3om4GiHnjJFoJJoJrj0Fj0ch\
    1li3Sh9QlG3hH9hI1nJ5iJXj02C0F70S62hF3UF45L7CL7IJBBLDbFF3KG3LG_IHPKHm7I12I2LJ5LJMEJQG0GI0cK\
    0eE0xF19G1AI2mD2xE3UK4lL6HL9QIH9HH.8Ii9J58JMJKeK0AW0GW0dWI3WJcWJgWJuW0Ln0Qp0khI2jIXpJXnJcp\
    JgiJoiJut0Oi0Ts0fn0riJRx01p3Qj3lh02m0Qi0Sl0nh3UiIjlJMkJXi0AC0GF0OG0RE0TD0rH1_DIZBIn4JRH0Nh\
    0Wm0tk1Fj1Ol1Qn1Xn1ch1di1kn1ql1rj1zi21n22m24i28m2Bk2Kn2Qm2Sn2go2kp2np2on38p46l4Jn4jl5An5yk\
    60jCthDnoE6hEeiFwpHShHvnI5mIAlIGnIcrJ4hJGsJImJZkJrmK6iKck03J0760AH0GC0HL0UB0bL2mA2xA3UEBDJ\
    J8DJMEJbEJcJJgCJwG0Gi0xk3Uk4.hCeiHqjJ8nJMj3So4Ri4kiI3n02m0Fi0Qh0Sn3Uh9RhI2nIZhIjmJMh02p0Fi\
    0Qh0So0bk1Bl2hh3Um45i5DiBAhCakFRhHPmI2pIZkIihIjnJKiJMoK1h0bi0koI3hIlo03W0BW0wW4_W0Jq12h1Ni\
    4JhINhIYiInr1Xo46p4jnIchJGiJeh08j0Nm0Zi0mo0vh1Gi1Oi1cl1dn1fh1km1mm1oi1pi1vh1yh1zl1.j23o2Cl\
    2do2ki3er3oi49i4Ij4Tj4fk5Mk5Yh8YkC5nEvkHOlHQlHgjHxhHzkIdhIejIzkJSoJmqK6jKAsKUiKVqKch03W0SW\
    0_W1jW2jWI6W01p4Ai1Sl3ohInkJRo08h0Vq0tm1Qk1Xl1bj1fi1qi1ri1yh22i24j2Bj2Jl2Qm2Sh2ki2oj2ql2wk\
    3As3rp3tp5Ah5Ij5qkHWpHblHtoI5mIAkIGjILoIajJBlJCnJInJYpJZkJlkKAh0Nl0Pr0li1On1cj1dm1kl1ol1pk\
    1qi1zj21j22l28l2Qj2Sm2go2kk2np34m38o4Ih4ci4jh5Ai5qi5yo60q8YiFwlHQhHlnHuoHvpHyiIAhIGkIglJIi\
    JUtK6nKck0Fl0cm1lm3Sh5li9QlI1mIiiIkhKFi08k09q0jj15m1Gi1bi1fh1qi1vj1yi23i24j2Bi2Ji2ql35o3Ok\
    41o4Dp4xn5Mh5wr6LiDLlHaiHkqHonHsmHwlI5hIAhIGiIbkIdiJHlJIhJZhJlk0En5gh5.h0GB0x61971A82mK2xL\
    3UB4lF5lE5nHH.HIiHIxHJ5IJMB05h0Dp0Ou0Rs0rt0ti1Fn1Oj1Qj1Xo1bh1dj1kl1ql1zi21j22l24k28i2Jh2Kj\
    2Qj2gi2ki2ol3ri46m4jm5Aj5Ij60lCdkDnoE6oI5jIAjIGkI_sJBnJClJGkJIiJYiJZlK6mKci0Vj0tn1Fk1Gk1Qn\
    1Xh1bj1dg1qn1rm21l22j24n28m2Bj2Jk2Kj2Ql2Sl2kl2ol2tn2wm3cj3rm3tm4Jl5Aj5Il5qtDnjE6oHRmHWnI5o\
    IAoIGoILmJ3jJAmJCnJImJJmJYpJZlJli0Vr0tm1Fn1Xj2go2oq2sj3ej3tn41l49j4Io4Th4fh5Mm60l96hC5oDnl\
    E6pHWsHakHboHhpHtqHwoHznI8kILnIaqIciIriJ3qJApJBrJYp04A0BG0SC0w90_D2XA4ZI4kG5UH6IFHqLH.LI6B\
    IjJKIH03907L0BB0SK0i90xI1jA2jC2z94RL4kF5nJ6IK9kIHrKIsJIuKIxLIyL0Jq12i1Sk34hIYiIns1Mh1Xq46o\
    4joIckIri35i3Oh41p4Dn4xo08L0NI0ZE12D1GJ1OK1QH1bH1cJ1fI1iI1kL1mJ1oH1pG1qH1rH1vI1yK1zI1.K23H\
    28J2BJ2CH2HE2SI2kL2qI41B5IK5YLHOHHQJHgHHiGHnDHpKHxDIAGIGHINJIdJIeHJaFJeDKUHKbCKcJ05o0JjIYj\
    0Xs16l1Lh02j0Qh0Sm0ki0nh3UjI1hIZiIjmJMkJQi0270FC0HA0QL0nA0wG0.G10B3UA8tLAkLCJLI19IkKIsBJ8C\
    JQ802n0Sl0ni3UiI1hIZiIjmJMiJQi0En2Ni57h5gi5mi73hIHh0LW54WAcWB7WIkWJ8WJcWJgWJuW0Fq0Hh0Ki0Ls\
    0Qi0kj0.l3UiI2mJMiJcrJglJun08i0Nh0Zn0ml1Ok1ci1dl1fh1kk1mp1ok1pk1yh1zl1.n23m2Cj2do3eo3oi49i\
    5Mj5YhC5oEviHOlHQlHgkHoiHpiIdkIekIzlI.rJmoK6iKAqKUkKVoKci1Xh38i4jj5yp60rHvi5yo60nHuhJ4l0Ch\
    0Dp0Ll0Ol0Rp0rn0Ep4Ll4Xh5Zk5dlJqi0Lo0Qn0kq2ciI2nIXkImkJcqJgrJuo0Vn0tn1Gh1Ql1bl1qm1rl21k22k\
    24l28l2Bm2Ji2Ql2Sl2tl2wk3cn3ro3to5Im5qtE6nHRmHWpHtjI5mIAmIGlILnJ3nJAiJBpJCnJImJJjJZjJll08K\
    0NJ0ZE0mI0vL1GJ1OL1bL1cH1dF1fK1kK1oJ1pJ1qL1rK1vK1yL1zL21K22K23H24F2BG2CI2JJ2QK2SK2kI3oH4BG\
    4GK4TG4fD5II5yCCtHEvKHQJHjFHlFHnHI5GIdIIeKIzFI.JJIKJSKJlLK6KKAGKVKKcL0lo0mm0tk0vn2Ki2di2nk\
    2vo2wj35o3Oi3co3rm46l4Dj4Wh4jo4xj5Aj5Yi5yl8Yl96hCthEvpHolHujHxoHynI.lJmiKVj0bH0Gi0Jp0Lv0Uk\
    0bi0em26i3uiIZhIlk01q4MhIoi1Mk1il35q4Tp4foEeiHpj02l0Qh0Sl3UlI1iIjlJMlJQi19E1AE1L6EVJIlAJPI\
    0LW7BW9NWAKWCXWIkWJ8WJcWJgWJuWKEW0Zn1Yk2GoCdjIzlJ2oJzh02m0Lj0bk0eo1Bi2mi2xk61h6xh7eh8BiFbj\
    IZiIkjJ5i00p3ni3ym0NK0PI0lF1GI1OG1bG1cI1dH1kH1oI1pK1qJ1rG1vK1zH21H22I23J24I2BH2CL2JG2QJ2SK\
    2kK2vL34K3eD46B4EE4GE4cH4jH5AI5IH5yLC5KCtKHOKHQJHjKHlCHuJHyJI5LIALIgGIzFJ4LJUGJZLK6HKcI0mp\
    0tl0vh1im1ms1.r2Ki2dl2nk2wl35l3Ok3cm3eo3rp46k4Do4ji4xp5Yh5yp8YhEvmHgmHojHpqHxkI.sJSsJajKUq\
    KVq0Go0bi1Ak1Bm3Ul4Qh4Rh5th8qh8yhA8hE9hI3hIskIuiJ8kJMhJclJgh04W0iW2XW2zWJpW04W0iW2XW2zWJoW\
    01q2bi4eh4zh5ZhHYhIPj05m0JmIYjJJh17i2am4Jj4coHRnKbl0Gi0xl1Ai1Bh3Uk4kh5niC8hJ8qJMm1L9IiDIkI\
    Il5J5BJPLJ_L02m0Qi0Sm5ph67i7dhI2mIjmJMl0bF07k0Bk10i19i1Bh2mk2xkHmhIsn02j0Sk0ni3UiI1jIZhIij\
    IjlJ5iJMmJQi0Zm1Yo2GlCdpIznJ2k0LW97WIkWJ8WJcWJgWJuW01r2Lh3Qj02m0Sl3UlI1iIjnJMlJQh02j0Sm0bh\
    1Bh2ui3Uh84h9bhHPiI2nIZjIjnIwiJMm02n0Qh0Sl3UhI1iIjlJMj02n0Qi0Sm0nh3UiI1iIjnJMi0Wm2pk4Bm4Jn\
    5HlHji02n0Sm0ki0ni3UiI1iIZiIjmJMkJQi08K0NK15K1GF1NE1OH1QG1YJ1bH1cL1dF1fH1kE1oL1pJ1qK1rH1uF\
    1vK1yJ1zL22L23L24G2BH2CJ2GE2JK2SH2aK2kG2pF2qI35A3tD5HF5II5wE63C74F98CHOKHQKHaJHkIHoJI5GI8L\
    IAIIGIILEIbKIeKJFEJHKJIJJZHJlHKcJ01p36h3Ni4qj0Go0bk19h1Bi3Ul5Jh5Lh6NhCRiIskJ8mJMkJcmJgk02j\
    0Sm2hh3Uj45j5Wi5xj6Uh9Hk9Rh9ihBJhHPjI2oIZjIjmJMl3eq4Wk96lHxhI.jJShJajJmq07n0Bj0Uj1Bi2mk2xl\
    9TiDkiGbhIXmIslKSi0Go0Ui0bj19i1Bh3Ul4Ri5Lh6Zj83i9JhA2iDehE9iI3iIslIuiJ8mJMiJcm02j0Sn3UkCah\
    EPjHPhI2mIjlJMi0Gk0bh0xk19i1Aj3Uk6Xh8kj9ziCehD0hFoiHBhIuhJ8nJMj0LI1BH02j0Ll0bi0en1Bi2mi2xk\
    3Sh48h4Fh9siAQhGqhH7hH.hIZhIihIkkJ5jJuhKZj1ri22i24j28h2Bh2qh3Om4Dh4xk6Ls9IjJli03W0BW0SW0wW\
    7ZW03W0wW07n0Bl19i2ml2xlGyiIsl0Eq07n0Bi0Hi0Uh10i19h2xjIsm0yJ1JF1gG31K03W0wW7ZW02l0Sm0kh3Uj\
    I1jIjnJMkJQi02h0bh2mj6Hi6IjH.lIknJ5jJ8hKZh02j0ek1Bi2mj2xj8rhBahDuhH.kIZiIiiIknJ5hJuj07m0Ai\
    0Bk0Kh0Ul0bi1Bk2ml2xm3_j6ih8Ah9ThB0hGghIXoIsjKBi0BL0FK0KH0.I1ZK0Nl1Mh1Oj1cl1fm1il1mn1ol1pl\
    1yj1zj1.p23l2Hi2dm2qh35h3el49i4fh5Mh5Yi8YjC5mHQiHgrHijHomHpoIdjIeiI.kJSmJmnK6iKci08h0Nh0Zo\
    0mp0vi1Gh1Oj1cn1dl1fi1kk1mj1oi1pl1yi1zl1.h23k2Ck2do3en3oh4Ik5Mi5Yl8YlC5nEvmHOjHQmHgiHnjHxi\
    HzlIdiIeiIzjI.qJSoJmmK6iKArKci1BH0Zi12l2Ap4GnEylINmIOn0Gm0Uh0bk1Ah1Bh3Uk6nh9gjB7hCRhHKiIsm\
    J8jJMjJcmJgi3Sq6IhI3o02n0Sm0ni3UjIjmJMhJXh1SlInmJRo08I0NL17K1GJ1NF1OE1QI1YJ1bI1cL1dI1fK1kF\
    1oJ1qI1rI1uF1vH1yI22J23K24L2BF2CK2GC2JI2aK2pD2qH35B3tG5HF5wF63C74F98GHOIHQLHaHHkLHoKI5LI8J\
    IAJILCIaJIbJIdIIeLJ6HJFFJHJJZIJlIK6K0Gj0bi0xj19h3UiBKhE1iEdiIshJ8pJMjJgh1BH0Gh0xl3Uj9WiE_h\
    FxhIsiJ8oJMkJgh07n0Bi2mm2xkHmkIsm07l0Bj0Hh0Qh19h2mm2xjHmiIsn";
/// The market features' projection onto the Kenyon cells, ten digits a cell, each a normal deviate in [-3.15, 3.15].
const PROJECTION: &str = "\
    RkbiZTIPaUVfgNLZRJkGMhfZEXRLQjhXbZPRZKLgJNeRXZmoXhGzWVXPWVdUWdgZSQXXgYTSXLSUUQSUOfncVemcVQ\
    UWPnhZnKaULcSaRRachXTXWFEXJETPfdWcQEfmNeQaLdbGQVVgRkRfhfaXejkSVTOcKPOROSQgfMJZYAWUFhUZTHYR\
    WAVcZAAbVJOOYoSUONPdLhLLddgYbc3VIFYMiQYQSbjiVciXJjb4WVQYNYDdaUSMEOWPWThTRdcXNSeSPPZIMSiSTP\
    QUcJN8WWlLeYjShWdY7UaSVJYkZNLVLetpQNZjWNSSirjRLXQGjWWMMMXbZKQTWRiRfNbjNhIGQVhrMfUOKiYdYrYa\
    gcKo7YaMEPdSUKKccleWPbSYOZUJRMIoeqVYXVPeabJKNLLSlbdUeTIcLbeXgLUPcZTeJLeQIVPYmu9WejNdeSIkiU\
    HViJXDhCicTfWYaNaNHbeMFeQFhVddQPldMOPcQRMUQhWGPOSQDRelafRabgReTKKbibSLXTchNCVXpCZVIIbgRjeJ\
    LaeSYVjdbUUMVKQOYROnVOIaUXPkUPsKXbKMGNhRVnRsTKf8OZlRRVadblYaPIlkDYSRiJgcLYaOdhZOidQ3XLNkYb\
    XSLXdePPaLLYUcBPRNuTOpBOaRZQcaqXdKXZYabJibUdqTpJUTWP8QcFgMqZeShPmRbLPQdZdNJFfiQVYgV.RYUoYS\
    YcVadphXJMVMWKalYSMmnNmlsZcpaRPTmlthZjgJcMcQedOVdKcTcUJiOecMEnSZLKJJMgcbXQVYUYQHeYhGRhZVOT\
    dgdmWSNPZLmWGXXdbnTkbKfZFjeTHTRigUKQ9ccaSNpZSRRRZVYbU9WcMiZQaQSPSLSiXZYjJbcbTTcQajWUWTgoSJ\
    cfU5LbBYkfbHRjZdjXZKrUDdXVURiRYR8VdUgcZfQaFMXrnTfiYLVMkSKRTebSTfeeIDbTfILOWbHRlMNgQVaUZTYR\
    oJWUMVMCSbKNdWEVgDRFfCZqhOXeWgedikbceUSeaQTJebQeWUPIXIghbQYgXUWWfbYeTVTWedJVImVUPSZWHaGSIY\
    DRDUjn_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_MLMRWgkYEYTOdSMqPWKicQLjWgbQKUlcDPi\
    FRRNabeWYNncWhYoSUTZONNYjjtJVKgJIRRTXhKRSZaboYYXfNIhDSjGdRiZPLVXMrSZVlYcPbJsJOLKUUVaZVZKYd\
    iOeFJeFXbJSSnO_isZYSRgQZOfebYKIorDaLEVTMahaYbaYeYNOYcScmSTcRjfJH7QZOaARbL6cHKYbQ0VIOndYIOd\
    UeQbSTFSZhYOWNbkUMQiIThFTJOWSUHKcPalOThOgKXcJoNRbhSIVXP7aMGERjJqbckj9ZPXYZVYVaReedTIHRKYaO\
    RaLWURhXSGIi9afjaGJbcPOHPsehGXYURbfhNSZPUoqFQPaWfMPMTRatTbWSWKpyKbZaUWHjXQRbPVYbSKeUnGZPNE\
    FfbaQHQPLPVMkODUQagTlLPY6iPXXiTNJTXBhecJPTXRNqeeZKRPUTD9SZVIMjdPAQPUWadWQ8WgOOZhYMiNIZTbha\
    ZFkeYWYIIJrbiVdWQeHl8SJEWYPSbbaZlNbhbjkdgYTWNQYMUWbOYNjUWe6DpMGgvUQlTbiWLiILaUbnbRMTcUGQUg\
    VffRcqJaXSjSOIcYPbReKSZkTXZTibUQDcYNSbaVCZWKQOfPfYeSWFMYMmNQiURJROZbeMXLDoXXTZJeQY7f8UTSik\
    RIpcKjWVQfhYgVaagOJSYQGTVqEQGVEWCBZmYSaNYbUZaLbaUCQTX9YHcVQiMVMmQeeaaYLUgYdZUWN.LgYLYMcONU\
    uQMTTKTeC6MgP4ZTcTsNORBrYjfRRWf8OaYgVIZcTnPVUYgXcKVbScadJHiNXYcUFYbZgkeLbNdSBdRYSKVhQBjSHX\
    sHSVWikoHRhMbbS9UZSeOTVdfRTQiTOfRMQbUgVaXVXccdIcOTacQLWJWbU.aKYRZmhPQN";
