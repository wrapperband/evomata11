use super::cell::*;
use super::fluid::*;
use itertools::Itertools;
use std::mem;
use rand::{Isaac64Rng, Rng};
use noise::{Brownian2, perlin2};
use num_cpus;
use crossbeam;

const KILL_FLUID_COLOR_NORMAL: f64 = 0.01;
const SIGNAL_FLUID_SQRT_NORMAL: f64 = 5.0;
const SIGNAL_FLUID_COLOR_NORMAL: f32 = 0.4;
const FOOD_FLUID_COLOR_NORMAL: f64 = 600.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Mate {
    mate: (usize, usize),
    source: (usize, usize),
}

#[derive(Debug, Serialize, Deserialize)]
struct Delta {
    movement_attempts: Vec<(usize, usize)>,
    mate_attempts: Vec<Mate>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Hex {
    pub solution: Solution,
    pub cell: Option<Cell>,
    pub decision: Option<Decision>,
    delta: Delta,
}

struct GridCont(*mut Grid);

unsafe impl Sync for GridCont {}
unsafe impl Send for GridCont {}

impl Hex {
    pub fn color(&self) -> [f32; 4] {
        let killf = ((self.solution.fluids[3] - KILL_FLUID_NORMAL) /
                     KILL_FLUID_COLOR_NORMAL) as f32;
        let mut ocolors = [killf.abs(),
                           (self.solution.fluids[0] / FOOD_FLUID_COLOR_NORMAL) as f32,
                           0.25 * self.solution.fluids[2] as f32,
                           1.0];
        let signal_colors = [[0.0, 0.5, 0.5], [0.5, 0.5, 0.5], [0.5, 0.0, 0.5], [0.5, 0.5, 0.0]];
        for i in 0..4 {
            let signalf = ((self.solution.fluids[4 + i] / SIGNAL_FLUID_SQRT_NORMAL) as f32)
                .abs()
                .sqrt() / SIGNAL_FLUID_COLOR_NORMAL;
            for j in 0..3 {
                ocolors[j] += signal_colors[i][j] * signalf;
            }
        }
        ocolors
    }
}

#[derive(Serialize, Deserialize)]
pub struct Grid {
    pub spawning: bool,
    pub width: usize,
    pub height: usize,
    pub consumption: f64,
    pub spawn_rate: f64,
    pub inhale_minimum: usize,
    pub inhale_cap: usize,
    pub movement_cost: usize,
    pub divide_cost: usize,
    pub explode_requirement: usize,
    pub death_release_coefficient: f64,
    pub explode_amount: f64,
    tiles: Vec<Hex>,
}

impl Grid {
    pub fn new(width: usize,
               height: usize,
               consumption: f64,
               spawn_rate: f64,
               inhale_minimum: usize,
               inhale_cap: usize,
               movement_cost: usize,
               divide_cost: usize,
               explode_requirement: usize,
               death_release_coefficient: f64,
               explode_amount: f64,
               rng: &mut Isaac64Rng)
               -> Self {
        Grid {
            spawning: true,
            width: width,
            height: height,
            consumption: consumption,
            spawn_rate: spawn_rate,
            inhale_minimum: inhale_minimum,
            inhale_cap: inhale_cap,
            movement_cost: movement_cost,
            divide_cost: divide_cost,
            explode_requirement: explode_requirement,
            death_release_coefficient: death_release_coefficient,
            explode_amount: explode_amount,
            tiles: randomizing_vec(width, height, rng),
        }
    }

    pub fn randomize(&mut self, rng: &mut Isaac64Rng) {
        self.tiles = randomizing_vec(self.width, self.height, rng);
    }

    pub fn hex(&self, x: usize, y: usize) -> &Hex {
        &self.tiles[x + y * self.width]
    }

    pub fn hex_mut(&mut self, x: usize, y: usize) -> &mut Hex {
        &mut self.tiles[x + y * self.width]
    }

    fn hex_and_neighbors(&mut self, x: usize, y: usize) -> (&mut Hex, [&Hex; 6]) {
        (unsafe { mem::transmute(self.hex_mut(x, y)) },
         if y % 2 == 0 {
            [// UpRight
             self.hex((x + self.width + 1) % self.width,
                      (y + self.height - 1) % self.height),
             // UpLeft
             self.hex(x, (y + self.height - 1) % self.height),
             // Left
             self.hex((x + self.width - 1) % self.width, y),
             // DownLeft
             self.hex(x, (y + self.height + 1) % self.height),
             // DownRight
             self.hex((x + self.width + 1) % self.width,
                      (y + self.height + 1) % self.height),
             // Right
             self.hex((x + self.width + 1) % self.width, y)]
        } else {
            [// UpRight
             self.hex(x, (y + self.height - 1) % self.height),
             // UpLeft
             self.hex((x + self.width - 1) % self.width,
                      (y + self.height - 1) % self.height),
             // Left
             self.hex((x + self.width - 1) % self.width, y),
             // DownLeft
             self.hex((x + self.width - 1) % self.width,
                      (y + self.height + 1) % self.height),
             // DownRight
             self.hex(x, (y + self.height + 1) % self.height),
             // Right
             self.hex((x + self.width + 1) % self.width, y)]
        })
    }

    pub fn cycle(&mut self, rng: &mut Isaac64Rng) {
        if self.spawning {
            self.cycle_spawn(rng);
        }

        self.cycle_cells();

        self.cycle_decisions(rng);

        self.cycle_fluids();

        self.cycle_death();
    }

    fn cycle_spawn(&mut self, rng: &mut Isaac64Rng) {
        if self.spawn_rate >= 1.0 {
            for _ in 0..self.spawn_rate as usize {
                let tile = rng.gen_range(0, self.width * self.height);
                if self.tiles[tile].cell.is_none() {
                    self.tiles[tile].cell = Some(Cell::new(rng));
                }
            }
        } else {
            if rng.next_f64() < self.spawn_rate {
                let tile = rng.gen_range(0, self.width * self.height);
                if self.tiles[tile].cell.is_none() {
                    self.tiles[tile].cell = Some(Cell::new(rng));
                }
            }
        }
    }

    fn cycle_cells(&mut self) {
        let g = GridCont(self as *mut Grid);
        let g = &g;
        let numcpus = num_cpus::get();
        crossbeam::scope(|scope| {
            for i in 0..numcpus {
                scope.spawn(move || {
                    let g: &mut Grid = unsafe { mem::transmute(g.0) };
                    for x in 0..g.width {
                        for y in (g.height * i / numcpus)..(g.height * (i + 1) / numcpus) {
                            let (this, neighbors) = g.hex_and_neighbors(x, y);
                            this.decision = if let Some(ref mut this_cell) = this.cell {
                                let neighbor_presents = [neighbors[0].cell.is_some(),
                                                         neighbors[1].cell.is_some(),
                                                         neighbors[2].cell.is_some(),
                                                         neighbors[3].cell.is_some(),
                                                         neighbors[4].cell.is_some(),
                                                         neighbors[5].cell.is_some()];

                                Some(this_cell.decide([&this.solution.fluids,
                                                       &neighbors[0].solution.fluids,
                                                       &neighbors[1].solution.fluids,
                                                       &neighbors[2].solution.fluids,
                                                       &neighbors[3].solution.fluids,
                                                       &neighbors[4].solution.fluids,
                                                       &neighbors[5].solution.fluids],
                                                      &neighbor_presents))
                            } else {
                                None
                            }
                        }
                    }
                });
            }
        });
    }

    fn cycle_decisions(&mut self, rng: &mut Isaac64Rng) {
        let g = GridCont(self as *mut Grid);
        let g = &g;
        let explode_amount = self.explode_amount;
        let explode_requirement = self.explode_requirement;
        let numcpus = num_cpus::get();
        // Compute the deltas resulting from the decision.
        crossbeam::scope(|scope| {
            for i in 0..numcpus {
                scope.spawn(move || {
                    let g: &mut Grid = unsafe { mem::transmute(g.0) };
                    for x in 0..g.width {
                        for y in (g.height * i / numcpus)..(g.height * (i + 1) / numcpus) {
                            let (width, height) = (g.width, g.height);
                            let (this, neighbors) = g.hex_and_neighbors(x, y);
                            // Clear the movements from the previous cycle.
                            this.delta.movement_attempts.clear();
                            this.delta.mate_attempts.clear();
                            this.solution.coefficients = if let Some(ref decision) = this.decision {
                                decision.coefficients
                            } else {
                                // Set the diffusion coefficients to the normal values.
                                [NORMAL_DIFFUSION; 6]
                            };

                            // Only add movements here if no cell is present.
                            if this.cell.is_none() {
                                // Add any neighbor movements to the movement_attempts vector.
                                for (n, &facing) in neighbors.iter().zip(&[Direction::DownLeft,
                                                                           Direction::DownRight,
                                                                           Direction::Right,
                                                                           Direction::UpRight,
                                                                           Direction::UpLeft,
                                                                           Direction::Left]) {
                                    match n.decision {
                                        Some(Decision { choice: Choice::Move(direction), .. }) => {
                                            // It attempted to move into this hex cell.
                                            if facing == direction {
                                                this.delta
                                                    .movement_attempts
                                                    .push(in_direction(x, y, width, height, facing.flip()));

                                                // No need to continue if we reach 2 attempts.
                                                if this.delta.movement_attempts.len() == 2 {
                                                    break;
                                                }
                                            }
                                        }
                                        Some(Decision { choice: Choice::Divide { mate, spawn }, .. }) => {
                                            // It attempted to spawn into this hex cell.
                                            if facing == spawn {
                                                let source = in_direction(x, y, width, height, facing.flip());;
                                                this.delta
                                                    .mate_attempts
                                                    .push(Mate {
                                                        mate: in_direction(source.0,
                                                                           source.1,
                                                                           width,
                                                                           height,
                                                                           mate),
                                                        source: source,
                                                    });

                                                // No need to continue if we reach 2 attempts.
                                                if this.delta.mate_attempts.len() == 2 {
                                                    break;
                                                }
                                            }
                                        }
                                        Some(Decision { choice: Choice::Explode(way), .. }) => {
                                            if let Some(ref mut c) = this.cell {
                                                if c.inhale >= explode_requirement {
                                                    this.solution.diffuse[2] += if way {
                                                        explode_amount
                                                    } else {
                                                        -explode_amount
                                                    };
                                                }
                                            }
                                        }
                                        Some(Decision { choice: Choice::Suicide, .. }) => {
                                            if let Some(ref mut c) = this.cell {
                                                c.suicide = true;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });

        // Perform the deltas.
        for x in 0..self.width {
            for y in 0..self.height {
                // Handle movement.
                if self.hex(x, y).delta.movement_attempts.len() == 1 {
                    let from_coord = self.hex(x, y).delta.movement_attempts[0];
                    self.hex_mut(x, y).cell = self.hex_mut(from_coord.0, from_coord.1).cell.take();
                    // Apply movement cost.
                    let inhale = self.hex(x, y).cell.as_ref().unwrap().inhale;
                    if inhale >= self.movement_cost {
                        self.hex_mut(x, y).cell.as_mut().unwrap().inhale -= self.movement_cost;
                    } else {
                        self.hex_mut(x, y).cell.as_mut().unwrap().inhale = 0;
                    }
                    // Handle mating.
                } else if self.hex(x, y).delta.mate_attempts.len() == 1 {
                    let mate = self.hex(x, y).delta.mate_attempts[0].clone();
                    self.hex_mut(x, y).cell = if mate.mate == (x, y) {
                        // Apply movement and divide cost to source.
                        let inhale =
                            self.hex(mate.source.0, mate.source.1).cell.as_ref().unwrap().inhale;
                        if inhale >= self.movement_cost + self.divide_cost {
                            self.hex_mut(mate.source.0, mate.source.1)
                                .cell
                                .as_mut()
                                .unwrap()
                                .inhale -= self.movement_cost + self.divide_cost;
                        } else {
                            self.hex_mut(mate.source.0, mate.source.1)
                                .cell
                                .as_mut()
                                .unwrap()
                                .inhale = 0;
                        }
                        Some(self.hex_mut(mate.source.0, mate.source.1)
                            .cell
                            .as_mut()
                            .unwrap()
                            .divide(rng))
                    } else {
                        if self.hex(mate.mate.0, mate.mate.1).cell.is_some() {
                            // Apply movement and divide cost to source.
                            let inhale = self.hex(mate.source.0, mate.source.1)
                                .cell
                                .as_ref()
                                .unwrap()
                                .inhale;
                            if inhale >= self.movement_cost + self.divide_cost {
                                self.hex_mut(mate.source.0, mate.source.1)
                                    .cell
                                    .as_mut()
                                    .unwrap()
                                    .inhale -= self.movement_cost + self.divide_cost;
                            } else {
                                self.hex_mut(mate.source.0, mate.source.1)
                                    .cell
                                    .as_mut()
                                    .unwrap()
                                    .inhale = 0;
                            }
                            // This is safe so long as the cells arent the same.
                            Some(unsafe {
                                    mem::transmute::<_,
                                                     &mut Hex>(self.hex_mut(mate.source.0, mate.source.1))
                                }
                                .cell
                                .as_mut()
                                .unwrap()
                                .mate(&self.hex(mate.mate.0, mate.mate.1)
                                          .cell
                                          .as_ref()
                                          .unwrap(),
                                      rng))
                        } else {
                            None
                        }
                    };
                }

                // Clear the decisions.
                self.hex_mut(x, y).decision = None;
            }
        }
    }

    fn cycle_fluids(&mut self) {
        let g = GridCont(self as *mut Grid);
        let g = &g;
        let numcpus = num_cpus::get();
        // Then update diffusion.
        crossbeam::scope(|scope| {
            for i in 0..numcpus {
                scope.spawn(move || {
                    let g: &mut Grid = unsafe { mem::transmute(g.0) };
                    for x in 0..g.width {
                        for y in (g.height * i / numcpus)..(g.height * (i + 1) / numcpus) {
                            let (this, neighbors) = g.hex_and_neighbors(x, y);

                            for (i, n) in neighbors.iter().enumerate() {
                                this.solution.diffuse_from(&n.solution,
                                                           match n.cell {
                                                               Some(_) => {
                                                                   DiffusionType::FlatSignals
                                                               }
                                                               None => DiffusionType::DynSignals,
                                                           },
                                                           (i + 3) % 6);
                            }
                        }
                    }
                });
            }
        });

        // Finish the cycle.
        crossbeam::scope(|scope| {
            for i in 0..numcpus {
                scope.spawn(move || {
                    let g: &mut Grid = unsafe { mem::transmute(g.0) };
                    for x in 0..g.width {
                        for y in (g.height * i / numcpus)..(g.height * (i + 1) / numcpus) {
                            g.hex_mut(x, y).solution.end_cycle();
                        }
                    }
                });
            }
        });
    }

    fn cycle_death(&mut self) {
        let g = GridCont(self as *mut Grid);
        let g = &g;
        let numcpus = num_cpus::get();
        let consumption = self.consumption;
        // Finish the cycle.
        crossbeam::scope(|scope| {
            for i in 0..numcpus {
                scope.spawn(move || {
                    let g: &mut Grid = unsafe { mem::transmute(g.0) };
                    let inhale_minimum = g.inhale_minimum;
                    let inhale_cap = g.inhale_cap;
                    let death_release_coefficient = g.death_release_coefficient;
                    for x in 0..g.width {
                        for y in (g.height * i / numcpus)..(g.height * (i + 1) / numcpus) {
                            let hex = g.hex_mut(x, y);
                            if hex.cell.is_some() {
                                if hex.cell.as_ref().unwrap().suicide ||
                                   hex.solution.fluids[3] > KILL_FLUID_UPPER_THRESHOLD ||
                                   hex.solution.fluids[3] < KILL_FLUID_LOWER_THRESHOLD ||
                                   hex.cell.as_ref().unwrap().inhale < inhale_minimum {
                                    hex.solution.fluids[0] +=
                                        death_release_coefficient * consumption *
                                        hex.cell.as_ref().unwrap().inhale as f64;
                                    hex.cell = None;
                                } else if hex.solution.fluids[0] <= consumption {
                                    if hex.cell.as_ref().unwrap().inhale != 0 {
                                        hex.cell.as_mut().unwrap().inhale -= 1;
                                    } else {
                                        hex.cell = None;
                                    }
                                } else {
                                    hex.solution.fluids[0] -= consumption;
                                    // NOTE: This used to be survival threshold.
                                    if hex.solution.fluids[0] < 0.0 {
                                        if hex.cell.as_ref().unwrap().inhale != 0 {
                                            hex.cell.as_mut().unwrap().inhale -= 1;
                                        } else {
                                            hex.solution.fluids[0] +=
                                                death_release_coefficient * consumption *
                                                hex.cell.as_ref().unwrap().inhale as f64;
                                            hex.cell = None;
                                        }
                                    } else {
                                        if hex.cell.as_ref().unwrap().inhale < inhale_cap {
                                            hex.cell.as_mut().unwrap().inhale += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });
    }
}

fn randomizing_vec(width: usize, height: usize, rng: &mut Isaac64Rng) -> Vec<Hex> {
    let seeds = [rng.gen(), rng.gen()];
    let noise = Brownian2::new(perlin2, 4).wavelength(24.0);
    (0..height)
        .cartesian_product((0..width))
        .map(|(x, y)| {
            Hex {
                solution: Solution::new([0.0,
                                         1.0,
                                         noise.apply(&seeds[0], &[x as f64, y as f64]),
                                         KILL_FLUID_NORMAL,
                                         0.0,
                                         0.0,
                                         0.0,
                                         0.0],
                                        [NORMAL_DIFFUSION; 6]),
                cell: None,
                decision: None,
                delta: Delta {
                    movement_attempts: Vec::with_capacity(6),
                    mate_attempts: Vec::with_capacity(6),
                },
            }
        })
        .collect_vec()
}

fn in_direction(x: usize,
                y: usize,
                width: usize,
                height: usize,
                direction: Direction)
                -> (usize, usize) {
    let diff = direction.delta(y % 2 == 0);
    (((width + x) as isize + diff.0) as usize % width,
     ((height + y) as isize + diff.1) as usize % height)
}
