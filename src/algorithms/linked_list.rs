//! A generic doubly-linked list backed by an arena of indices.
//! This prevents duplicating the list manipulation logic across LRU and ARC.

pub(super) trait HasLinks {
    fn prev(&self) -> Option<usize>;
    fn next(&self) -> Option<usize>;
    fn set_prev(&mut self, v: Option<usize>);
    fn set_next(&mut self, v: Option<usize>);
}

#[derive(Debug, Clone)]
pub(super) struct ArenaList {
    pub head: Option<usize>,
    pub tail: Option<usize>,
    pub len: usize,
}

impl ArenaList {
    pub fn new() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
        }
    }

    /// Detaches a node from this list without freeing it.
    pub fn detach<N: HasLinks>(&mut self, nodes: &mut [N], idx: usize) {
        let prev = nodes[idx].prev();
        let next = nodes[idx].next();

        if let Some(p) = prev {
            nodes[p].set_next(next);
        } else {
            self.head = next;
        }

        if let Some(n) = next {
            nodes[n].set_prev(prev);
        } else {
            self.tail = prev;
        }

        self.len -= 1;
        nodes[idx].set_prev(None);
        nodes[idx].set_next(None);
    }

    /// Attaches a node to the head (MRU) of this list.
    pub fn attach_head<N: HasLinks>(&mut self, nodes: &mut [N], idx: usize) {
        nodes[idx].set_prev(None);
        nodes[idx].set_next(self.head);

        if let Some(h) = self.head {
            nodes[h].set_prev(Some(idx));
        } else {
            self.tail = Some(idx);
        }

        self.head = Some(idx);
        self.len += 1;
    }

    /// Attaches a node to the tail (LRU) of this list.
    pub fn attach_tail<N: HasLinks>(&mut self, nodes: &mut [N], idx: usize) {
        nodes[idx].set_prev(self.tail);
        nodes[idx].set_next(None);

        if let Some(t) = self.tail {
            nodes[t].set_next(Some(idx));
        } else {
            self.head = Some(idx);
        }

        self.tail = Some(idx);
        self.len += 1;
    }

    /// Detaches and returns the tail node (LRU), if any.
    pub fn pop_tail<N: HasLinks>(&mut self, nodes: &mut [N]) -> Option<usize> {
        let tail_idx = self.tail?;
        self.detach(nodes, tail_idx);
        Some(tail_idx)
    }
}
