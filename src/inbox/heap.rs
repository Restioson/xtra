use std::mem;

/// A binary max-heap optimised for the case where most priorities are the same
#[derive(Debug)]
pub struct BinaryHeap<T>(Vec<T>);

impl<T: Ord> BinaryHeap<T> {
    pub fn new() -> BinaryHeap<T> {
        BinaryHeap(Vec::new())
    }

    pub fn push(&mut self, item: T) {
        let mut node = self.0.len();
        self.0.push(item);

        while node > 0 && self.0[node] > self.0[node >> 1] {
            self.0.swap(node, node >> 1);
            node = node >> 1;
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        let end = self.0.len() - 1;
        self.0.swap(0, end);
        let item = self.0.pop()?;

        // NB: 1-indexed! This makes the math easier
        let mut node = 1;
        let mut left_child = node << 1;
        let len = self.0.len();

        while left_child < len {
            if self.0[left_child - 1] > self.0[node - 1] {
                self.0.swap(left_child - 1, node - 1);
                node = left_child - 1;
                left_child = node << 1;
            } else if self.0[left_child] > self.0[node - 1] {
                // Actually checking right child here - +1 for right child, -1 for 1-index to 0-index
                self.0.swap(left_child, node - 1);
                node = left_child;
                left_child = node << 1;
            } else {
                break;
            }
        }

        Some(item)
    }

    pub fn peek(&self) -> Option<&T> {
        self.0.get(0)
    }
}

#[cfg(test)]
mod test {
    use crate::inbox::heap::BinaryHeap;

    #[test]
    fn test_heap() {
        let mut heap = BinaryHeap::new();
        heap.push(3);
        heap.push(4);
        assert_eq!(heap.pop().unwrap(), 4);
        assert_eq!(heap.pop().unwrap(), 3);

        heap.push(0);
        heap.push(1);
        heap.push(1);
        heap.push(1);
        heap.push(2);

        dbg!(&heap);

        assert_eq!(heap.pop().unwrap(), 2);
        dbg!(&heap);
        assert_eq!(heap.pop().unwrap(), 1);
        assert_eq!(heap.pop().unwrap(), 1);
        assert_eq!(heap.pop().unwrap(), 1);
        assert_eq!(heap.pop().unwrap(), 0);

        heap.push(2);
        heap.push(4);
        heap.push(1);
        heap.push(1);
        heap.push(1);
        assert_eq!(heap.pop().unwrap(), 4);
        assert_eq!(heap.pop().unwrap(), 2);
        assert_eq!(heap.pop().unwrap(), 1);
        assert_eq!(heap.pop().unwrap(), 1);
        assert_eq!(heap.pop().unwrap(), 1);

    }
}