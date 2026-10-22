//! Layered (Sugiyama-style) auto-layout, left to right: cycle removal,
//! longest-path layering with dummy nodes for long edges, barycentric
//! crossing reduction, and iterative coordinate assignment. Disconnected
//! components are laid out separately and stacked; lone nodes are packed in
//! rows underneath.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{CLUSTER_PAD, CLUSTER_TITLE_H, Graph};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
}

pub const LAYER_GAP: f32 = 90.0;
pub const NODE_GAP: f32 = 28.0;
pub const COMPONENT_GAP: f32 = 70.0;
const DUMMY_H: f32 = 8.0;

/// Top-left positions for every node of `g`. Each section cluster is laid
/// out on its own and then placed as one block (with room for its box and
/// title), so section boxes never overlap each other or outside nodes.
pub fn layout(g: &Graph) -> Vec<Point> {
    let n = g.nodes.len();
    let sizes: Vec<(f32, f32)> = g.nodes.iter().map(|n| n.size()).collect();
    let mut edges: Vec<(usize, usize)> =
        g.edges.iter().map(|e| (e.from.node, e.to.node)).filter(|(a, b)| a != b && *a < n && *b < n).collect();
    edges.sort();
    edges.dedup();
    if g.nodes.iter().all(|n| n.cluster.is_none()) {
        return layout_sized(&sizes, &edges);
    }
    // blocks: one per cluster with members, one per node outside any cluster
    let mut block_of = vec![0usize; n];
    let mut blocks: Vec<(bool, Vec<usize>)> = Vec::new();
    let mut by_cluster: BTreeMap<usize, usize> = BTreeMap::new();
    for v in 0..n {
        let b = match g.nodes[v].cluster {
            Some(c) => *by_cluster.entry(c).or_insert_with(|| {
                blocks.push((true, vec![]));
                blocks.len() - 1
            }),
            None => {
                blocks.push((false, vec![]));
                blocks.len() - 1
            }
        };
        blocks[b].1.push(v);
        block_of[v] = b;
    }
    let mut inner = vec![Point::default(); n];
    let mut block_sizes = Vec::with_capacity(blocks.len());
    for (boxed, members) in &blocks {
        if !boxed {
            block_sizes.push(sizes[members[0]]);
            continue;
        }
        let local: BTreeMap<usize, usize> = members.iter().enumerate().map(|(i, &v)| (v, i)).collect();
        let sub_edges: Vec<(usize, usize)> =
            edges.iter().filter(|(a, b)| local.contains_key(a) && local.contains_key(b)).map(|(a, b)| (local[a], local[b])).collect();
        let sub_sizes: Vec<(f32, f32)> = members.iter().map(|&v| sizes[v]).collect();
        let pts = layout_sized(&sub_sizes, &sub_edges);
        let (mut w, mut h) = (0.0f32, 0.0f32);
        for (i, &v) in members.iter().enumerate() {
            inner[v] = Point::new(pts[i].x + CLUSTER_PAD, pts[i].y + CLUSTER_TITLE_H);
            w = w.max(pts[i].x + sub_sizes[i].0);
            h = h.max(pts[i].y + sub_sizes[i].1);
        }
        block_sizes.push((w + 2.0 * CLUSTER_PAD, h + CLUSTER_TITLE_H + CLUSTER_PAD));
    }
    let mut block_edges: Vec<(usize, usize)> =
        edges.iter().map(|&(a, b)| (block_of[a], block_of[b])).filter(|(a, b)| a != b).collect();
    block_edges.sort();
    block_edges.dedup();
    let bpos = layout_sized(&block_sizes, &block_edges);
    (0..n).map(|v| Point::new(bpos[block_of[v]].x + inner[v].x, bpos[block_of[v]].y + inner[v].y)).collect()
}

/// Layered layout of boxes of the given sizes joined by `edges`.
fn layout_sized(sizes: &[(f32, f32)], edges: &[(usize, usize)]) -> Vec<Point> {
    let n = sizes.len();
    // connected components (union-find)
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut x = x;
        while p[x] != r {
            let nx = p[x];
            p[x] = r;
            x = nx;
        }
        r
    }
    for &(a, b) in edges {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }
    let mut comps: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for v in 0..n {
        let r = find(&mut parent, v);
        comps.entry(r).or_default().push(v);
    }

    let mut pos = vec![Point::default(); n];
    let mut y0 = 0.0f32;
    let mut max_w = 0.0f32;
    let mut singles = Vec::new();
    for (_, members) in comps {
        if members.len() == 1 {
            singles.push(members[0]);
            continue;
        }
        let local: BTreeMap<usize, usize> = members.iter().enumerate().map(|(i, &v)| (v, i)).collect();
        let sub_edges: Vec<(usize, usize)> =
            edges.iter().filter(|(a, _)| local.contains_key(a)).map(|(a, b)| (local[a], local[b])).collect();
        let sub_sizes: Vec<(f32, f32)> = members.iter().map(|&v| sizes[v]).collect();
        let (pts, w, h) = layout_component(&sub_sizes, &sub_edges);
        for (i, &v) in members.iter().enumerate() {
            pos[v] = Point::new(pts[i].x, pts[i].y + y0);
        }
        y0 += h + COMPONENT_GAP;
        max_w = max_w.max(w);
    }
    // pack lone nodes in rows
    let row_w = max_w.max(900.0);
    let (mut x, mut row_h) = (0.0f32, 0.0f32);
    for v in singles {
        let (w, h) = sizes[v];
        if x > 0.0 && x + w > row_w {
            x = 0.0;
            y0 += row_h + NODE_GAP;
            row_h = 0.0;
        }
        pos[v] = Point::new(x, y0);
        x += w + NODE_GAP;
        row_h = row_h.max(h);
    }
    pos
}

/// Lay out one connected component; returns positions and the extent.
fn layout_component(sizes: &[(f32, f32)], edges: &[(usize, usize)]) -> (Vec<Point>, f32, f32) {
    let n = sizes.len();
    let dag = remove_cycles(n, edges);

    // longest-path layering
    let mut indeg = vec![0usize; n];
    let mut out: Vec<Vec<usize>> = vec![vec![]; n];
    for &(a, b) in &dag {
        out[a].push(b);
        indeg[b] += 1;
    }
    let mut layer = vec![0usize; n];
    let mut queue: Vec<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    let mut qi = 0;
    while qi < queue.len() {
        let v = queue[qi];
        qi += 1;
        for &w in &out[v] {
            layer[w] = layer[w].max(layer[v] + 1);
            indeg[w] -= 1;
            if indeg[w] == 0 {
                queue.push(w);
            }
        }
    }
    // pull sources right, next to their first consumer (keeps params and
    // adapters close to what they feed)
    for &v in queue.iter().rev() {
        if let Some(min_succ) = out[v].iter().map(|&w| layer[w]).min() {
            if min_succ > layer[v] + 1 && dag.iter().all(|&(_, b)| b != v) {
                layer[v] = min_succ - 1;
            }
        }
    }

    // dummy nodes for edges spanning several layers
    let mut node_layer = layer.clone();
    let mut heights: Vec<f32> = sizes.iter().map(|s| s.1).collect();
    let mut segs: Vec<(usize, usize)> = Vec::new();
    for &(a, b) in &dag {
        let mut prev = a;
        for l in layer[a] + 1..layer[b] {
            let d = node_layer.len();
            node_layer.push(l);
            heights.push(DUMMY_H);
            segs.push((prev, d));
            prev = d;
        }
        segs.push((prev, b));
    }
    let total = node_layer.len();
    let n_layers = node_layer.iter().max().map_or(1, |m| m + 1);
    let mut layers: Vec<Vec<usize>> = vec![vec![]; n_layers];
    for v in 0..total {
        layers[node_layer[v]].push(v);
    }
    let mut ups: Vec<Vec<usize>> = vec![vec![]; total];
    let mut downs: Vec<Vec<usize>> = vec![vec![]; total];
    for &(a, b) in &segs {
        downs[a].push(b);
        ups[b].push(a);
    }

    // crossing reduction: barycenter sweeps, keep the best ordering
    let mut best = layers.clone();
    let mut best_c = crossings(&layers, &downs, total);
    for it in 0..16 {
        let down = it % 2 == 0;
        let order: Vec<usize> = if down { (1..n_layers).collect() } else { (0..n_layers.saturating_sub(1)).rev().collect() };
        for l in order {
            let fixed = if down { l - 1 } else { l + 1 };
            let mut idx = vec![0f32; total];
            for (i, &v) in layers[fixed].iter().enumerate() {
                idx[v] = i as f32;
            }
            let nbrs = if down { &ups } else { &downs };
            let cur: BTreeMap<usize, usize> = layers[l].iter().enumerate().map(|(i, &v)| (v, i)).collect();
            let mut keyed: Vec<(f32, usize)> = layers[l]
                .iter()
                .map(|&v| {
                    let ns = &nbrs[v];
                    let k = if ns.is_empty() { cur[&v] as f32 } else { ns.iter().map(|&u| idx[u]).sum::<f32>() / ns.len() as f32 };
                    (k, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(cur[&a.1].cmp(&cur[&b.1])));
            layers[l] = keyed.into_iter().map(|(_, v)| v).collect();
        }
        let c = crossings(&layers, &downs, total);
        if c < best_c {
            best_c = c;
            best = layers.clone();
        }
    }
    let layers = best;

    // x: one column per layer
    let mut xs = vec![0f32; n_layers];
    let mut x = 0.0;
    for (l, vs) in layers.iter().enumerate() {
        xs[l] = x;
        let w = vs.iter().filter(|&&v| v < n).map(|&v| sizes[v].0).fold(0.0, f32::max);
        x += w.max(20.0) + LAYER_GAP;
    }

    // y: stack each layer, then relax toward neighbour centres
    let mut y = vec![0f32; total];
    for vs in &layers {
        let mut c = 0.0;
        for &v in vs {
            y[v] = c;
            c += heights[v] + NODE_GAP;
        }
    }
    for it in 0..12 {
        let down = it % 2 == 0;
        let order: Vec<usize> = if down { (0..n_layers).collect() } else { (0..n_layers).rev().collect() };
        for l in order {
            let nbrs = if down { &ups } else { &downs };
            let desired: Vec<f32> = layers[l]
                .iter()
                .map(|&v| {
                    let ns: Vec<usize> = nbrs[v].iter().copied().chain(if it > 5 { if down { downs[v].clone() } else { ups[v].clone() } } else { vec![] }).collect();
                    if ns.is_empty() {
                        y[v]
                    } else {
                        ns.iter().map(|&u| y[u] + heights[u] / 2.0).sum::<f32>() / ns.len() as f32 - heights[v] / 2.0
                    }
                })
                .collect();
            place_column(&layers[l], &desired, &heights, &mut y);
        }
    }
    let min_y = (0..total).map(|v| y[v]).fold(f32::INFINITY, f32::min);
    let min_y = if min_y.is_finite() { min_y } else { 0.0 };
    let mut pts = Vec::with_capacity(n);
    let mut max_y = 0.0f32;
    for v in 0..n {
        let p = Point::new(xs[node_layer[v]], y[v] - min_y);
        max_y = max_y.max(p.y + sizes[v].1);
        pts.push(p);
    }
    (pts, x - LAYER_GAP, max_y)
}

/// Place a column's nodes (in order) as close to `desired` as possible
/// without overlaps: forward pass pushes down, backward pass pulls up, and
/// the two are averaged.
fn place_column(vs: &[usize], desired: &[f32], heights: &[f32], y: &mut [f32]) {
    if vs.is_empty() {
        return;
    }
    let k = vs.len();
    let mut down = vec![0f32; k];
    for i in 0..k {
        down[i] = if i == 0 { desired[0] } else { desired[i].max(down[i - 1] + heights[vs[i - 1]] + NODE_GAP) };
    }
    let mut up = vec![0f32; k];
    for i in (0..k).rev() {
        up[i] = if i == k - 1 { desired[i] } else { desired[i].min(up[i + 1] - heights[vs[i]] - NODE_GAP) };
    }
    let mut prev_end = f32::NEG_INFINITY;
    for i in 0..k {
        let mut v = (down[i] + up[i]) / 2.0;
        if v < prev_end {
            v = prev_end;
        }
        y[vs[i]] = v;
        prev_end = v + heights[vs[i]] + NODE_GAP;
    }
}

fn crossings(layers: &[Vec<usize>], downs: &[Vec<usize>], total: usize) -> usize {
    let mut pos = vec![0usize; total];
    for vs in layers {
        for (i, &v) in vs.iter().enumerate() {
            pos[v] = i;
        }
    }
    let mut c = 0;
    for vs in layers {
        let es: Vec<(usize, usize)> = vs.iter().flat_map(|&a| downs[a].iter().map(move |&b| (a, b))).map(|(a, b)| (pos[a], pos[b])).collect();
        for i in 0..es.len() {
            for j in i + 1..es.len() {
                let (a, b) = (es[i], es[j]);
                if (a.0 < b.0 && a.1 > b.1) || (a.0 > b.0 && a.1 < b.1) {
                    c += 1;
                }
            }
        }
    }
    c
}

/// Reverse DFS back edges so the graph is acyclic.
fn remove_cycles(n: usize, edges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out: Vec<Vec<usize>> = vec![vec![]; n];
    for &(a, b) in edges {
        out[a].push(b);
    }
    // 0 = unvisited, 1 = on stack, 2 = done
    let mut state = vec![0u8; n];
    let mut back = std::collections::BTreeSet::new();
    for s in 0..n {
        if state[s] != 0 {
            continue;
        }
        let mut stack = vec![(s, 0usize)];
        state[s] = 1;
        while let Some((v, i)) = stack.pop() {
            if i < out[v].len() {
                stack.push((v, i + 1));
                let w = out[v][i];
                match state[w] {
                    0 => {
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    1 => {
                        back.insert((v, w));
                    }
                    _ => {}
                }
            } else {
                state[v] = 2;
            }
        }
    }
    let mut dag: Vec<(usize, usize)> = edges.iter().map(|&(a, b)| if back.contains(&(a, b)) { (b, a) } else { (a, b) }).collect();
    dag.sort();
    dag.dedup();
    dag
}

/// Final positions: stored positions win; other nodes get their auto-layout
/// position, shifted with their placed neighbours and nudged off overlaps.
pub fn place(g: &Graph, stored: &BTreeMap<String, Point>) -> Vec<Point> {
    let auto = layout(g);
    let n = g.nodes.len();
    let mut pos = auto.clone();
    let mut placed = vec![false; n];
    for (i, node) in g.nodes.iter().enumerate() {
        if let Some(p) = stored.get(&node.id) {
            pos[i] = *p;
            placed[i] = true;
        }
    }
    if placed.iter().all(|p| !p) {
        return pos;
    }
    let offsets: Vec<Point> = (0..n).map(|i| Point::new(pos[i].x - auto[i].x, pos[i].y - auto[i].y)).collect();
    let offset = |i: usize| offsets[i];
    let global = {
        let ps: Vec<usize> = (0..n).filter(|&i| placed[i]).collect();
        let k = ps.len() as f32;
        Point::new(ps.iter().map(|&i| offset(i).x).sum::<f32>() / k, ps.iter().map(|&i| offset(i).y).sum::<f32>() / k)
    };
    let sizes: Vec<(f32, f32)> = g.nodes.iter().map(|n| n.size()).collect();
    for i in 0..n {
        if placed[i] {
            continue;
        }
        let nbrs: Vec<usize> = g
            .edges
            .iter()
            .filter_map(|e| if e.from.node == i { Some(e.to.node) } else if e.to.node == i { Some(e.from.node) } else { None })
            .filter(|&j| placed[j])
            .collect();
        let off = if nbrs.is_empty() {
            global
        } else {
            let k = nbrs.len() as f32;
            Point::new(nbrs.iter().map(|&j| offset(j).x).sum::<f32>() / k, nbrs.iter().map(|&j| offset(j).y).sum::<f32>() / k)
        };
        let mut p = Point::new(auto[i].x + off.x, auto[i].y + off.y);
        // nudge down until it overlaps nothing already placed
        for _ in 0..200 {
            let hit = (0..n).filter(|&j| placed[j]).find(|&j| {
                p.x < pos[j].x + sizes[j].0 + 12.0
                    && pos[j].x < p.x + sizes[i].0 + 12.0
                    && p.y < pos[j].y + sizes[j].1 + 12.0
                    && pos[j].y < p.y + sizes[i].1 + 12.0
            });
            match hit {
                Some(j) => p.y = pos[j].y + sizes[j].1 + NODE_GAP,
                None => break,
            }
        }
        pos[i] = p;
        placed[i] = true;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::signup;
    use crate::{DataOptions, FlowOptions, data_model_graph, flow_graph};

    fn overlaps(g: &Graph, pos: &[Point]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for i in 0..g.nodes.len() {
            for j in i + 1..g.nodes.len() {
                let (a, b) = (g.nodes[i].size(), g.nodes[j].size());
                if pos[i].x < pos[j].x + b.0 && pos[j].x < pos[i].x + a.0 && pos[i].y < pos[j].y + b.1 && pos[j].y < pos[i].y + a.1 {
                    out.push((g.nodes[i].id.clone(), g.nodes[j].id.clone()));
                }
            }
        }
        out
    }

    #[test]
    fn flow_layout_is_layered_and_overlap_free() {
        let m = signup();
        let g = flow_graph(&m, &FlowOptions::default());
        let pos = layout(&g);
        assert_eq!(pos.len(), g.nodes.len());
        assert!(overlaps(&g, &pos).is_empty(), "{:?}", overlaps(&g, &pos));
        // data flows left to right
        for e in g.edges_of_kind(crate::EdgeKind::Data) {
            assert!(pos[e.from.node].x < pos[e.to.node].x, "{} -> {}", g.nodes[e.from.node].id, g.nodes[e.to.node].id);
        }
    }

    #[test]
    fn data_layout_is_overlap_free() {
        let m = signup();
        let g = data_model_graph(&m, &DataOptions { module: None, overlay: true });
        let pos = layout(&g);
        assert!(overlaps(&g, &pos).is_empty(), "{:?}", overlaps(&g, &pos));
    }

    /// Section boxes as the canvas draws them: the members' bounds plus padding.
    fn cluster_boxes(g: &Graph, pos: &[Point]) -> Vec<(String, [f32; 4])> {
        let mut out = Vec::new();
        for (ci, c) in g.clusters.iter().enumerate() {
            let ms: Vec<usize> = (0..g.nodes.len()).filter(|&i| g.nodes[i].cluster == Some(ci)).collect();
            if ms.is_empty() {
                continue;
            }
            let x0 = ms.iter().map(|&i| pos[i].x).fold(f32::MAX, f32::min) - CLUSTER_PAD;
            let y0 = ms.iter().map(|&i| pos[i].y).fold(f32::MAX, f32::min) - CLUSTER_TITLE_H;
            let x1 = ms.iter().map(|&i| pos[i].x + g.nodes[i].size().0).fold(f32::MIN, f32::max) + CLUSTER_PAD;
            let y1 = ms.iter().map(|&i| pos[i].y + g.nodes[i].size().1).fold(f32::MIN, f32::max) + CLUSTER_PAD;
            out.push((c.title.clone(), [x0, y0, x1, y1]));
        }
        out
    }

    #[test]
    fn section_boxes_do_not_overlap() {
        let m = signup();
        for g in [flow_graph(&m, &FlowOptions::default()), data_model_graph(&m, &DataOptions { module: None, overlay: false })] {
            let pos = layout(&g);
            let boxes = cluster_boxes(&g, &pos);
            assert!(boxes.len() >= 2, "{boxes:?}");
            for i in 0..boxes.len() {
                for j in i + 1..boxes.len() {
                    let (a, b) = (boxes[i].1, boxes[j].1);
                    assert!(a[2] <= b[0] || b[2] <= a[0] || a[3] <= b[1] || b[3] <= a[1], "{} overlaps {}", boxes[i].0, boxes[j].0);
                }
            }
            // and no node outside a section sits inside its box
            for (i, n) in g.nodes.iter().enumerate() {
                for (title, r) in &boxes {
                    if n.cluster.is_some_and(|c| g.clusters[c].title == *title) {
                        continue;
                    }
                    let (w, h) = n.size();
                    let inside = pos[i].x < r[2] && r[0] < pos[i].x + w && pos[i].y < r[3] && r[1] < pos[i].y + h;
                    assert!(!inside, "{} is inside {title}", n.id);
                }
            }
        }
    }

    #[test]
    fn cycles_do_not_hang() {
        let e = remove_cycles(3, &[(0, 1), (1, 2), (2, 0)]);
        assert_eq!(e.len(), 3);
        let (pts, _, _) = layout_component(&[(100.0, 40.0); 3], &[(0, 1), (1, 2), (2, 0)]);
        assert_eq!(layout_sized(&[(100.0, 40.0); 3], &[(0, 1)]).len(), 3);
        assert_eq!(pts.len(), 3);
    }

    #[test]
    fn stored_positions_win_and_new_nodes_avoid_them() {
        let m = signup();
        let g = flow_graph(&m, &FlowOptions::default());
        let mut stored = BTreeMap::new();
        stored.insert("signup.validate".to_string(), Point::new(5000.0, 5000.0));
        let pos = place(&g, &stored);
        let v = g.index_of("signup.validate").unwrap();
        assert_eq!(pos[v], Point::new(5000.0, 5000.0));
        assert!(overlaps(&g, &pos).iter().all(|(a, b)| a != "signup.validate" && b != "signup.validate"));
    }
}
