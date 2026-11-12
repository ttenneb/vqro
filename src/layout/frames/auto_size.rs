//! Event-only sizing of explicitly created view boxes, using cached scalar facts.
use super::*;
use crate::view_sizing::ViewMeasure;

#[derive(Clone, Copy, Default)]
struct Size {
    minimum: (u16, u16),
    wanted: Option<(u16, u16)>,
    group: Option<PaneId>,
    members: usize,
}

struct Sizer<'a> {
    facts: HashMap<usize, Facts>,
    sizes: HashMap<usize, Size>,
    measures: &'a HashMap<PaneId, ViewMeasure>,
    headers: &'a std::collections::HashSet<PaneId>,
    groups: &'a HashMap<PaneId, PaneId>,
    counts: HashMap<PaneId, usize>,
}

impl Sizer<'_> {
    fn grouped(&self, node: &TypedNode, inside: bool) -> bool {
        !inside
            && matches!(node, TypedNode::Split { .. })
            && self
                .facts
                .get(&key(node))
                .copied()
                .and_then(Facts::group_terminal)
                .is_some()
    }

    fn complete(&self, size: Size) -> bool {
        size.wanted.is_some()
            && size
                .group
                .is_some_and(|group| self.counts.get(&group) == Some(&size.members))
    }

    fn minimum(&mut self, node: &TypedNode, inside: bool) -> Size {
        let grouped = self.grouped(node, inside);
        let inside = inside || grouped;
        let mut size = match node {
            TypedNode::Collapsed { node } => {
                let child = self.minimum(node, inside);
                Size {
                    minimum: (1, 1),
                    wanted: child.wanted.map(|_| (1, 1)),
                    ..child
                }
            }
            // Twenty terminal columns plus the optional scrollbar gutter.
            TypedNode::Leaf(pane) => Size {
                minimum: (21, 4 + u16::from(self.headers.contains(pane))),
                ..Size::default()
            },
            TypedNode::View { pane, .. } => Size {
                minimum: (8, 3 + u16::from(self.headers.contains(pane))),
                wanted: self
                    .groups
                    .get(pane)
                    .and_then(|_| self.measures.get(pane))
                    .map(|measure| {
                        (
                            measure.width,
                            measure
                                .rows
                                .saturating_add(2)
                                .saturating_add(u16::from(self.headers.contains(pane))),
                        )
                    }),
                group: self.groups.get(pane).copied(),
                members: 1,
            },
            TypedNode::Stack {
                selected, children, ..
            } => {
                let child = children
                    .get(*selected)
                    .map(|child| self.minimum(child, inside))
                    .unwrap_or_default();
                // An ordinary stack introduced after creation is not the designated
                // views subtree. Preserve its normal selected-branch geometry.
                Size {
                    minimum: (
                        child.minimum.0,
                        child.minimum.1.saturating_add(u16::from(
                            self.facts
                                .get(&key(node))
                                .is_some_and(|fact| fact.visible_collapse),
                        )),
                    ),
                    ..Size::default()
                }
            }
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let a = self.minimum(first, inside);
                let b = self.minimum(second, inside);
                let group = a.group.filter(|group| Some(*group) == b.group);
                let wanted = group
                    .and_then(|_| a.wanted.zip(b.wanted))
                    .map(|(a, b)| combine(*direction, a, b));
                let automatic = wanted.is_some() || self.complete(a) || self.complete(b);
                let axis = |a: u16, b: u16| {
                    if automatic {
                        a.saturating_add(b)
                    } else {
                        (a as f32 / ratio.max(f32::EPSILON))
                            .ceil()
                            .max((b as f32 / (1.0 - ratio).max(f32::EPSILON)).ceil())
                            .min(u16::MAX as f32) as u16
                    }
                };
                Size {
                    minimum: if *direction == Direction::Horizontal {
                        (axis(a.minimum.0, b.minimum.0), a.minimum.1.max(b.minimum.1))
                    } else {
                        (a.minimum.0.max(b.minimum.0), axis(a.minimum.1, b.minimum.1))
                    },
                    wanted,
                    group,
                    members: a.members.saturating_add(b.members),
                }
            }
        };
        if grouped {
            size.minimum = (
                size.minimum.0.saturating_add(2),
                size.minimum.1.saturating_add(2),
            );
        }
        self.sizes.insert(key(node), size);
        size
    }

    fn apply(
        &self,
        node: &mut TypedNode,
        mut area: Rect,
        inside: bool,
        fitting: Option<PaneId>,
    ) -> bool {
        let grouped = self.grouped(node, inside);
        let own_size = self.sizes[&key(node)];
        let reserve_stack_header = self
            .facts
            .get(&key(node))
            .is_some_and(|fact| fact.visible_collapse);
        let inside = inside || grouped;
        if grouped {
            area = inset(area);
        }
        match node {
            TypedNode::Stack {
                selected, children, ..
            } => children.get_mut(*selected).is_some_and(|child| {
                self.apply(
                    child,
                    if reserve_stack_header {
                        below_stack_header(area)
                    } else {
                        area
                    },
                    inside,
                    None,
                )
            }),
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let a = self.sizes[&key(first)];
                let b = self.sizes[&key(second)];
                let compact = [first.as_ref(), second.as_ref()].iter().any(|child| {
                    self.facts
                        .get(&key(child))
                        .is_some_and(|fact| fact.compact.is_some())
                });
                let extent = axis(*direction, (area.width, area.height));
                let selected = if self.complete(a) {
                    Some((true, a, b))
                } else if self.complete(b) {
                    Some((false, b, a))
                } else {
                    None
                };
                let mut changed = false;
                if inside && !compact {
                    let first_cells = if let Some((before, view, other)) = selected {
                        let (low, high) = if *direction == Direction::Horizontal {
                            (extent.div_ceil(4), (u32::from(extent) * 60 / 100) as u16)
                        } else {
                            (3, ((u32::from(extent) * 35 / 100) as u16).min(12))
                        };
                        let cap = high.min(extent.saturating_sub(axis(*direction, other.minimum)));
                        let wanted = axis(*direction, view.wanted.unwrap_or_default())
                            .clamp(low.min(cap), cap);
                        Some(if before {
                            wanted
                        } else {
                            extent.saturating_sub(wanted)
                        })
                    } else if own_size.wanted.is_some()
                        && fitting.is_some()
                        && fitting == own_size.group
                    {
                        // Only internal edges of the same explicitly created box.
                        // Chrome is already charged once per child above; don't
                        // apply the outer percentage/row cap again here.
                        a.wanted.zip(b.wanted).map(|(a_want, b_want)| {
                            let total = u32::from(axis(*direction, a_want))
                                + u32::from(axis(*direction, b_want));
                            let wanted = (u32::from(extent) * u32::from(axis(*direction, a_want)))
                                .checked_div(total)
                                .unwrap_or(0) as u16;
                            let high = extent.saturating_sub(axis(*direction, b.minimum));
                            wanted.clamp(axis(*direction, a.minimum).min(high), high)
                        })
                    } else {
                        None
                    };
                    if let Some(first_cells) = first_cells {
                        changed |= set_cells(ratio, extent, first_cells);
                    }
                }
                let (a, b) = effective_split(area, *direction, *ratio, first, second, &self.facts);
                let selected = selected.filter(|_| inside && !compact);
                let first_group = selected
                    .filter(|(before, _, _)| *before)
                    .and_then(|(_, size, _)| size.group)
                    .or(fitting);
                let second_group = selected
                    .filter(|(before, _, _)| !*before)
                    .and_then(|(_, size, _)| size.group)
                    .or(fitting);
                changed |= self.apply(first, a, inside, first_group);
                changed |= self.apply(second, b, inside, second_group);
                changed
            }
            // Collapsed descendants keep their expanded shares untouched.
            _ => false,
        }
    }
}

fn axis(direction: Direction, size: (u16, u16)) -> u16 {
    if direction == Direction::Horizontal {
        size.0
    } else {
        size.1
    }
}
fn combine(direction: Direction, a: (u16, u16), b: (u16, u16)) -> (u16, u16) {
    if direction == Direction::Horizontal {
        (a.0.saturating_add(b.0), a.1.max(b.1))
    } else {
        (a.0.max(b.0), a.1.saturating_add(b.1))
    }
}
fn set_cells(ratio: &mut f32, extent: u16, first_cells: u16) -> bool {
    if extent == 0 {
        return false;
    }
    // Automatic bounds may need shares below the explicit manual ten percent.
    let next = (f32::from(first_cells) / f32::from(extent)).clamp(f32::EPSILON, 1.0 - f32::EPSILON);
    if (f32::from(extent) * next).round() as u16 == (f32::from(extent) * *ratio).round() as u16 {
        return false;
    }
    *ratio = next;
    true
}

pub(in crate::layout) fn auto_size(
    root: &mut TypedNode,
    area: Rect,
    measures: &HashMap<PaneId, ViewMeasure>,
    headers: &std::collections::HashSet<PaneId>,
    groups: &HashMap<PaneId, PaneId>,
) -> bool {
    if measures.is_empty() || groups.is_empty() {
        return false;
    }
    #[cfg(test)]
    AUTO_GATHERS.with(|count| count.set(count.get() + 1));
    let mut sizer = Sizer {
        facts: HashMap::new(),
        sizes: HashMap::new(),
        measures,
        headers,
        groups,
        counts: HashMap::new(),
    };
    for group in groups.values() {
        *sizer.counts.entry(*group).or_default() += 1;
    }
    gather(root, &mut sizer.facts);
    sizer.minimum(root, false);
    sizer.apply(root, area, false, None)
}
