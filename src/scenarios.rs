//! Deterministic logical scenarios. Creating these never calls Windows APIs.
use crate::decode::Contact;
pub struct Step {
    pub label: &'static str,
    pub contacts: Vec<Contact>,
}
fn c(id: u32, x: i32, y: i32) -> Contact {
    Contact {
        id,
        tip: true,
        confidence: true,
        x,
        y,
        xmin: 0,
        xmax: 1000,
        ymin: 0,
        ymax: 1000,
    }
}
pub fn steps(capacity: usize) -> Vec<Step> {
    let mut out = vec![Step {
        label: "wait-clear",
        contacts: vec![],
    }];
    let mut add = |label, contacts| out.push(Step { label, contacts });
    for i in 0..60 {
        add("single-drag", vec![c(90, 100 + i * 12, 150 + i * 10)]);
    }
    add("single-lift", vec![]);
    for _ in 0..125 {
        add("single-hold", vec![c(90, 500, 500)]);
    }
    add("hold-lift", vec![]);
    for _ in 0..12 {
        add("rapid-tap", vec![c(91, 500, 500)]);
        add("rapid-lift", vec![]);
    }
    if capacity >= 2 {
        for i in 0..100 {
            let offset = if i < 50 { i * 5 } else { (99 - i) * 5 };
            let mut contacts = vec![c(10, 150 + offset, 500), c(70, 850 - offset, 500)];
            if i % 2 == 1 {
                contacts.reverse();
            }
            add("two-finger-pinch-expand", contacts);
        }
        add("partial-lift", vec![c(70, 700, 500)]);
        add("replace-finger", vec![c(70, 650, 500), c(99, 350, 500)]);
        add("two-lift", vec![]);
        for i in 0..60 {
            add(
                "all-fingers-move",
                (0..capacity)
                    .map(|id| {
                        c(
                            200 + id as u32 * 7,
                            100 + id as i32 * 150 + i,
                            200 + id as i32 * 100,
                        )
                    })
                    .collect(),
            );
        }
        add("all-lift", vec![]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::touch::{tests::engine, Action};
    #[test]
    fn acceptance_scenarios_finish_with_balanced_lifecycles() {
        for limit in 1..=5 {
            let mut e = engine(limit);
            for s in steps(limit) {
                e.apply(&s.contacts)
                    .unwrap_or_else(|err| panic!("{}: {err}", s.label));
            }
            assert_eq!(e.active_count(), 0);
            let mut counts = [0i32; 5];
            let mut max_frame = 0;
            for f in e.transport.accepted {
                max_frame = max_frame.max(f.pointers.len());
                for p in f.pointers {
                    match p.action {
                        Action::Down => {
                            assert_eq!(counts[p.slot as usize], 0);
                            counts[p.slot as usize] += 1;
                        }
                        Action::Up | Action::Cancel => {
                            assert_eq!(counts[p.slot as usize], 1);
                            counts[p.slot as usize] -= 1;
                        }
                        Action::Update => assert_eq!(counts[p.slot as usize], 1),
                    }
                }
            }
            assert_eq!(counts, [0; 5]);
            assert_eq!(max_frame, limit);
        }
    }
}
