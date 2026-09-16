//! **The etalon on a map: every command's tree, clustered.**
//!
//! Plan: `docs/guard-corpus-plan.md` §7. The operator: *"treesitter as
//! normalization for each command and clustering over trees, something like
//! SOM or vectors to see the clusters. draw me them even."*
//!
//! The normalisation is [`letibot_code::shell::shape`]: the tree-sitter parse
//! of the command with its literals replaced by holes, so `grep -n P -A 22
//! a.rs` and `grep -n Q -A 3 b.rs` are one shape. Every bash command in the
//! etalon becomes its shape; the shapes are aggregated (count, outcomes, what
//! layer A reads them as today), vectorised, and laid out on a self-organising
//! map (Kohonen, batch form). The map is what gets drawn.
//!
//! **The vector.** A shape's tokens — program names, flags, the holes, the
//! operators — are feature-hashed into `DIM` signed buckets, plus a few
//! structural counts from the tree itself (stages, pipes, redirects, heredocs,
//! unresolved constructs, subshells), then L2-normalised. Hashing rather than a
//! vocabulary because the vocabulary is open (every program name is a token)
//! and the point is neighbourhoods, not names.
//!
//! **The map.** A `SIDE × SIDE` grid of prototype vectors trained by batch
//! SOM: each epoch, every shape finds its best-matching unit (nearest
//! prototype by dot product; the vectors are unit length), and each prototype
//! becomes the neighbourhood-weighted mean of the shapes that landed near it,
//! with the neighbourhood radius shrinking from `SIDE/2` to 1. Shapes are
//! weighted by `sqrt(count)`: a shape seen 8,000 times should pull harder than
//! one seen once, but not 8,000 times harder, or the map would be `cd <arg> ;
//! grep …` and nothing else. Deterministic: a fixed-seed initialisation, so two
//! runs on the same corpus draw the same map.
//!
//! **What is emitted.** Per cell: how many shapes and commands landed, how many
//! were refused, what layer A reads them as, and the top shapes by count. Plus
//! the U-matrix — each cell's mean distance to its neighbours' prototypes —
//! which is where cluster BOUNDARIES show: a ridge of high U between two
//! plains is two clusters. The page draws both.

use std::collections::HashMap;
use std::path::Path;

use letibot_tools::adjudicate::Tier;
use letibot_tools::intent::{Baseline, BaselineVerdict, Surroundings};

pub const DIM: usize = 256;
pub const SIDE: usize = 32;
pub const EPOCHS: usize = 24;

#[derive(Debug, serde::Deserialize)]
struct Row {
    source: String,
    #[serde(default)]
    cwd: String,
    tool: String,
    arguments: serde_json::Value,
    outcome: String,
    #[serde(default)]
    label: Option<serde_json::Value>,
}

/// One unique shape and everything the corpus says about it.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ShapeAgg {
    pub shape: String,
    pub count: usize,
    pub ran: usize,
    pub refused: usize,
    pub refused_by_human: usize,
    pub error: usize,
    /// What layer A reads it as today, by tier word.
    pub reads: HashMap<String, usize>,
    /// Hosts/harnesses it came from.
    pub sources: HashMap<String, usize>,
    /// One real command, for the hover.
    pub example: String,
    #[serde(skip)]
    vec: Vec<f32>,
    /// Where it landed.
    pub cell: (usize, usize),
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Cell {
    pub x: usize,
    pub y: usize,
    pub shapes: usize,
    pub commands: usize,
    pub refused: usize,
    pub refused_by_human: usize,
    pub error: usize,
    pub always_ask: usize,
    pub may_approve: usize,
    pub not_run: usize,
    /// Mean distance to the neighbouring prototypes (the U-matrix).
    pub u: f32,
    /// Top shapes by count: (shape, count, refused, dominant read, example).
    pub top: Vec<(String, usize, usize, String, String)>,
    /// The two program names that dominate this cell, for the label.
    pub label: String,
}

#[derive(Debug, serde::Serialize)]
pub struct Map {
    pub side: usize,
    pub dim: usize,
    pub epochs: usize,
    pub rows: usize,
    pub commands: usize,
    pub unique_shapes: usize,
    pub cells: Vec<Cell>,
    /// Shapes with their cell and count, for the scatter. Capped to the top N
    /// by count so the page stays under its size budget.
    pub points: Vec<Point>,
}

#[derive(Debug, serde::Serialize)]
pub struct Point {
    pub shape: String,
    pub count: usize,
    pub refused: usize,
    pub read: String,
    pub x: usize,
    pub y: usize,
    pub example: String,
}

fn hash(s: &str) -> u64 {
    // FNV-1a, 64-bit: deterministic across runs and platforms.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Feature-hash a shape and its tree into a unit vector.
fn vectorise(shape: &str, n: &letibot_code::shell::Normalised) -> Vec<f32> {
    let mut v = vec![0f32; DIM];
    let mut put = |tok: &str, w: f32| {
        let h = hash(tok);
        let i = (h % DIM as u64) as usize;
        let sign = if (h >> 63) == 1 { 1.0 } else { -1.0 };
        v[i] += sign * w;
    };
    // The shape's own tokens, and bigrams so `cd <arg> ;` and `; cargo` keep
    // some order.
    let toks: Vec<&str> = shape.split_whitespace().collect();
    for t in &toks {
        put(t, 1.0);
    }
    for w in toks.windows(2) {
        put(&format!("{} {}", w[0], w[1]), 0.5);
    }
    // Program names count double: they are what a person clusters by.
    for st in &n.stages {
        if let letibot_code::shell::Word::Literal(p) = &st.program {
            let base = p.rsplit('/').next().unwrap_or(p);
            put(&format!("prog:{base}"), 2.0);
        }
    }
    // Structure from the tree, as counts.
    let pipes = n.stages.iter().filter(|s| s.pipe_in).count();
    let redirects: usize = n.stages.iter().map(|s| s.redirects.len()).sum();
    let heredocs = n
        .stages
        .iter()
        .flat_map(|s| s.redirects.iter())
        .filter(|r| matches!(r.op, letibot_code::shell::RedirectOp::HereDoc))
        .count();
    let subshells = n
        .stages
        .iter()
        .flat_map(|s| s.context.iter())
        .filter(|c| matches!(c, letibot_code::shell::Context::Subshell | letibot_code::shell::Context::Group))
        .count();
    for (name, k) in [
        ("stages", n.stages.len()),
        ("pipes", pipes),
        ("redirects", redirects),
        ("heredocs", heredocs),
        ("subshells", subshells),
        ("unresolved", n.unresolved.len()),
        ("assignments", n.assignments.len()),
    ] {
        let bucket = match k {
            0 => "0",
            1 => "1",
            2 => "2",
            3..=4 => "3-4",
            _ => "5+",
        };
        put(&format!("{name}:{bucket}"), 1.5);
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn read_word(b: &Baseline) -> &'static str {
    if matches!(b.verdict, BaselineVerdict::NotRun { .. }) {
        return "not_run";
    }
    match b.tier {
        Tier::Auto => "auto",
        Tier::MayApprove => "may_approve",
        Tier::AlwaysAsk { .. } => "always_ask",
        Tier::Inexpressible { .. } => "inexpressible",
    }
}

/// A tiny deterministic PRNG for the initial prototypes.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32) - 0.5
    }
}

pub fn build(path: &Path, env: &Surroundings, limit: usize, points_cap: usize) -> Result<Map, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut aggs: HashMap<String, ShapeAgg> = HashMap::new();
    let mut rows = 0usize;
    let mut commands = 0usize;
    for line in text.lines().take(limit) {
        let row: Row = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        rows += 1;
        if row.tool != "bash" {
            continue;
        }
        let Some(cmd) = row.arguments.get("command").and_then(|v| v.as_str()) else {
            continue;
        };
        commands += 1;
        let own;
        let e = if row.cwd.is_empty() {
            env
        } else {
            own = Surroundings {
                workspace: Some(row.cwd.clone().into()),
                ..env.clone()
            };
            &own
        };
        let b = Baseline::of_command(cmd, e);
        let Some(n) = &b.command else { continue };
        let shape = letibot_code::shell::shape(n);
        let read = read_word(&b);
        let by_human = row
            .label
            .as_ref()
            .and_then(|l| l.get("by"))
            .and_then(|v| v.as_str())
            .map(|s| s.starts_with("human"))
            .unwrap_or(false);
        let a = aggs.entry(shape.clone()).or_insert_with(|| ShapeAgg {
            shape: shape.clone(),
            vec: vectorise(&shape, n),
            example: cmd.chars().take(160).collect::<String>().replace('\n', " "),
            ..Default::default()
        });
        a.count += 1;
        match row.outcome.as_str() {
            "ran" => a.ran += 1,
            "refused" => {
                a.refused += 1;
                if by_human {
                    a.refused_by_human += 1;
                }
            }
            _ => a.error += 1,
        }
        *a.reads.entry(read.to_string()).or_default() += 1;
        *a.sources.entry(row.source.clone()).or_default() += 1;
    }

    // --- the SOM ---
    let mut shapes: Vec<ShapeAgg> = aggs.into_values().collect();
    shapes.sort_by(|a, b| b.count.cmp(&a.count).then(a.shape.cmp(&b.shape)));
    let n_cells = SIDE * SIDE;
    let mut rng = Lcg(0x5eed_1e71_b07);
    let mut proto: Vec<Vec<f32>> = (0..n_cells)
        .map(|_| {
            let mut p: Vec<f32> = (0..DIM).map(|_| rng.next_f32()).collect();
            let nrm = p.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            for x in &mut p {
                *x /= nrm;
            }
            p
        })
        .collect();
    let weights: Vec<f32> = shapes.iter().map(|s| (s.count as f32).sqrt()).collect();
    let cell_xy = |c: usize| ((c % SIDE) as f32, (c / SIDE) as f32);
    let mut bmu = vec![0usize; shapes.len()];
    for epoch in 0..EPOCHS {
        // Radius from SIDE/2 down to 1, geometric.
        let t = epoch as f32 / (EPOCHS.max(2) - 1) as f32;
        let radius = (SIDE as f32 / 2.0) * (1.0f32 / (SIDE as f32 / 2.0)).powf(t);
        let r2 = 2.0 * radius * radius;
        // Best-matching unit per shape.
        for (i, s) in shapes.iter().enumerate() {
            let mut best = 0usize;
            let mut best_d = f32::MIN;
            for (c, p) in proto.iter().enumerate() {
                let d: f32 = s.vec.iter().zip(p).map(|(a, b)| a * b).sum();
                if d > best_d {
                    best_d = d;
                    best = c;
                }
            }
            bmu[i] = best;
        }
        // Batch update: prototype = neighbourhood-weighted mean of shapes.
        let mut num = vec![vec![0f32; DIM]; n_cells];
        let mut den = vec![0f32; n_cells];
        // Precompute the neighbourhood kernel between cells once per epoch.
        let kernel: Vec<Vec<f32>> = (0..n_cells)
            .map(|c| {
                let (cx, cy) = cell_xy(c);
                (0..n_cells)
                    .map(|k| {
                        let (kx, ky) = cell_xy(k);
                        let d2 = (cx - kx).powi(2) + (cy - ky).powi(2);
                        if d2 > 9.0 * radius * radius { 0.0 } else { (-d2 / r2).exp() }
                    })
                    .collect()
            })
            .collect();
        for (i, s) in shapes.iter().enumerate() {
            let b = bmu[i];
            let w = weights[i];
            for (c, h) in kernel[b].iter().enumerate() {
                if *h == 0.0 {
                    continue;
                }
                let hw = h * w;
                let nc = &mut num[c];
                for (k, x) in s.vec.iter().enumerate() {
                    nc[k] += hw * x;
                }
                den[c] += hw;
            }
        }
        for c in 0..n_cells {
            if den[c] > 0.0 {
                let nrm = num[c].iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                for k in 0..DIM {
                    proto[c][k] = num[c][k] / nrm;
                }
            }
        }
    }
    for (i, s) in shapes.iter_mut().enumerate() {
        s.cell = (bmu[i] % SIDE, bmu[i] / SIDE);
    }

    // --- cells ---
    let mut cells: Vec<Cell> = (0..n_cells)
        .map(|c| Cell {
            x: c % SIDE,
            y: c / SIDE,
            ..Default::default()
        })
        .collect();
    let mut per_cell: Vec<Vec<usize>> = vec![Vec::new(); n_cells];
    for (i, s) in shapes.iter().enumerate() {
        let c = s.cell.1 * SIDE + s.cell.0;
        per_cell[c].push(i);
        let cell = &mut cells[c];
        cell.shapes += 1;
        cell.commands += s.count;
        cell.refused += s.refused;
        cell.refused_by_human += s.refused_by_human;
        cell.error += s.error;
        cell.always_ask += s.reads.get("always_ask").copied().unwrap_or(0)
            + s.reads.get("inexpressible").copied().unwrap_or(0);
        cell.may_approve += s.reads.get("may_approve").copied().unwrap_or(0);
        cell.not_run += s.reads.get("not_run").copied().unwrap_or(0);
    }
    for c in 0..n_cells {
        // U-matrix: mean distance (1 - dot) to the 4-neighbours.
        let (x, y) = (c % SIDE, c / SIDE);
        let mut acc = 0f32;
        let mut k = 0usize;
        for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= SIDE as i32 || ny >= SIDE as i32 {
                continue;
            }
            let nc = ny as usize * SIDE + nx as usize;
            let dot: f32 = proto[c].iter().zip(&proto[nc]).map(|(a, b)| a * b).sum();
            acc += 1.0 - dot;
            k += 1;
        }
        cells[c].u = if k > 0 { acc / k as f32 } else { 0.0 };
        // Top shapes and the label.
        let mut idx = per_cell[c].clone();
        idx.sort_by(|a, b| shapes[*b].count.cmp(&shapes[*a].count));
        let mut progs: HashMap<String, usize> = HashMap::new();
        for &i in idx.iter().take(40) {
            for t in shapes[i].shape.split_whitespace() {
                if !t.starts_with('<') && !t.starts_with('-') && t.chars().all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == '/') && t.len() > 1 {
                    *progs.entry(t.rsplit('/').next().unwrap_or(t).to_string()).or_default() += shapes[i].count;
                }
            }
        }
        let mut pv: Vec<_> = progs.into_iter().collect();
        pv.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        cells[c].label = pv.iter().take(2).map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(" · ");
        cells[c].top = idx
            .iter()
            .take(6)
            .map(|&i| {
                let s = &shapes[i];
                let read = s
                    .reads
                    .iter()
                    .max_by_key(|(_, n)| **n)
                    .map(|(r, _)| r.clone())
                    .unwrap_or_default();
                (s.shape.chars().take(140).collect(), s.count, s.refused, read, s.example.clone())
            })
            .collect();
    }
    let points: Vec<Point> = shapes
        .iter()
        .take(points_cap)
        .map(|s| Point {
            shape: s.shape.chars().take(140).collect(),
            count: s.count,
            refused: s.refused,
            read: s
                .reads
                .iter()
                .max_by_key(|(_, n)| **n)
                .map(|(r, _)| r.clone())
                .unwrap_or_default(),
            x: s.cell.0,
            y: s.cell.1,
            example: s.example.clone(),
        })
        .collect();
    Ok(Map {
        side: SIDE,
        dim: DIM,
        epochs: EPOCHS,
        rows,
        commands,
        unique_shapes: shapes.len(),
        cells,
        points,
    })
}
