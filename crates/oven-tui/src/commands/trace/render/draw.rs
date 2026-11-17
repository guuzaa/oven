//! A tree as text, the way the `tree` command draws it.

use super::model::{Kind, Node};

/// The nodes as lines: each root flush left, its descendants hanging off
/// branches.
pub fn draw(roots: &[Node], label: &dyn Fn(&Kind) -> String) -> Vec<String> {
    let mut lines = Vec::new();
    for root in roots {
        lines.push(label(&root.kind));
        children(root, "", label, &mut lines);
    }
    lines
}

/// The children of a node, under the `prefix` that carries the branches of
/// its ancestors.
fn children(node: &Node, prefix: &str, label: &dyn Fn(&Kind) -> String, lines: &mut Vec<String>) {
    let count = node.children.len();
    for (at, child) in node.children.iter().enumerate() {
        let last = at + 1 == count;
        let (branch, carry) = if last {
            ("└── ", "    ")
        } else {
            ("├── ", "│   ")
        };
        lines.push(format!("{prefix}{branch}{}", label(&child.kind)));
        children(child, &format!("{prefix}{carry}"), label, lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: u64, children: Vec<Node>) -> Node {
        Node {
            kind: Kind::Agent { id },
            children,
        }
    }

    #[test]
    fn a_branch_continues_while_a_sibling_follows() {
        let tree = agent(
            1,
            vec![agent(2, vec![agent(3, Vec::new())]), agent(4, Vec::new())],
        );
        let label = |kind: &Kind| match kind {
            Kind::Agent { id } => format!("n{id}"),
            _ => String::new(),
        };

        assert_eq!(
            draw(&[tree], &label),
            ["n1", "├── n2", "│   └── n3", "└── n4"]
        );
    }
}
