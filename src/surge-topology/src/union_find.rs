// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Disjoint-set (Union-Find) data structures.
//!
//! Index-based union-find used by the topology engine.

// ---------------------------------------------------------------------------
// Index-based Union-Find
// ---------------------------------------------------------------------------

/// Disjoint-set with `usize` keys, path compression, and union by rank.
///
/// This is the canonical implementation used throughout the Surge workspace
/// for bus-section merging (SE topology processor), observable-island detection,
/// graph connectivity, generator-coherency grouping, and synthetic network
/// generation.
#[derive(Debug, Clone)]
pub struct UnionFindIdx {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFindIdx {
    /// Create a new UF with `n` elements, each in its own set.
    pub fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    /// Find the representative (root) of the set containing `x`.
    ///
    /// Uses iterative path compression for O(α(n)) amortised performance
    /// and to avoid stack overflow on long chains.
    pub fn find(&mut self, x: usize) -> usize {
        // Walk up to root.
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while cur != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    /// Merge the sets containing `a` and `b` (union by rank).
    pub fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }

    #[cfg(test)]
    pub fn same(&mut self, a: usize, b: usize) -> bool {
        self.find(a) == self.find(b)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idx_basic() {
        let mut uf = UnionFindIdx::new(5);
        assert!(!uf.same(0, 1));
        uf.union(0, 1);
        assert!(uf.same(0, 1));
        uf.union(2, 3);
        assert!(uf.same(2, 3));
        assert!(!uf.same(1, 2));
        uf.union(1, 3);
        assert!(uf.same(0, 3));
        // Element 4 is still isolated.
        assert!(!uf.same(0, 4));
    }

    #[test]
    fn idx_path_compression() {
        // Build a chain: 0 -> 1 -> 2 -> 3
        let mut uf = UnionFindIdx::new(4);
        uf.parent = vec![1, 2, 3, 3];
        uf.rank = vec![0, 0, 0, 0];
        // find(0) should compress to root 3.
        assert_eq!(uf.find(0), 3);
        assert_eq!(uf.parent[0], 3);
        assert_eq!(uf.parent[1], 3);
    }
}
