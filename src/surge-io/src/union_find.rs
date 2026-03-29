// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Parser-local disjoint-set utilities.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub(crate) struct UnionFindStr {
    parent: HashMap<String, String>,
}

impl UnionFindStr {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn ensure(&mut self, id: &str) {
        self.parent
            .entry(id.to_string())
            .or_insert_with(|| id.to_string());
    }

    pub(crate) fn find(&mut self, id: &str) -> String {
        self.ensure(id);

        let mut root = id.to_string();
        loop {
            let parent = self
                .parent
                .get(&root)
                .expect("union-find parent exists")
                .clone();
            if parent == root {
                break;
            }
            root = parent;
        }

        let mut cur = id.to_string();
        while cur != root {
            let next = self
                .parent
                .get(&cur)
                .expect("union-find parent exists")
                .clone();
            self.parent.insert(cur.clone(), root.clone());
            cur = next;
        }

        root
    }

    pub(crate) fn union(&mut self, a: &str, b: &str) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent.insert(rb, ra);
        }
    }
}
