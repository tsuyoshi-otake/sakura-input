//! LOUDS trie navigation over the validated NODE, LABL, and LOUD tables.
//!
//! v1 stores labels in LABL; v2 stores the incoming scalar in the final NODE
//! field (`verification/dictionary-format-v2.md`). Lookup and validation reach
//! the trie only through these accessors.

use super::{bit_at, image_format, read_u16, read_u32, to_usize, Dictionary, Error, ImageVersion};

#[derive(Clone, Copy)]
pub(super) struct Node {
    pub(super) first_child: usize,
    pub(super) child_count: usize,
    pub(super) value_start: usize,
    pub(super) value_count: usize,
}

impl Dictionary<'_> {
    pub(super) fn node(&self, index: usize) -> Result<Node, Error> {
        if index >= self.node_count {
            return Err(Error::BadTree);
        }
        let at = index
            .checked_mul(self.version.node_len())
            .ok_or(Error::BadTree)?;
        if self.version == ImageVersion::V1 && read_u32(self.nodes, at + 12) != Some(0) {
            return Err(Error::BadTree);
        }
        Ok(Node {
            first_child: to_usize(read_u32(self.nodes, at).ok_or(Error::BadTree)?)?,
            child_count: usize::from(read_u16(self.nodes, at + 4).ok_or(Error::BadTree)?),
            value_count: usize::from(read_u16(self.nodes, at + 6).ok_or(Error::BadTree)?),
            value_start: to_usize(read_u32(self.nodes, at + 8).ok_or(Error::BadTree)?)?,
        })
    }

    pub(super) fn label(&self, index: usize) -> Result<char, Error> {
        if index >= self.node_count {
            return Err(Error::BadTree);
        }
        let scalar = match self.version {
            ImageVersion::V1 => {
                let at = index.checked_mul(4).ok_or(Error::BadTree)?;
                read_u32(self.labels, at).ok_or(Error::BadTree)?
            }
            ImageVersion::V2 => {
                let at = index
                    .checked_mul(image_format::NODE_LEN_V2)
                    .and_then(|at| at.checked_add(12))
                    .ok_or(Error::BadTree)?;
                read_u32(self.nodes, at).ok_or(Error::BadTree)?
            }
        };
        char::from_u32(scalar).ok_or(Error::BadTree)
    }

    pub(super) fn find_child(&self, node: Node, wanted: char) -> Option<usize> {
        let mut low = node.first_child;
        let mut high = node.first_child.checked_add(node.child_count)?;
        while low < high {
            let mid = low + (high - low) / 2;
            let label = self.label(mid).ok()?;
            match label.cmp(&wanted) {
                core::cmp::Ordering::Less => low = mid + 1,
                core::cmp::Ordering::Greater => high = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    pub(super) fn louds_bit(&self, index: usize) -> Result<bool, Error> {
        if index >= self.louds_bits {
            return Err(Error::BadTree);
        }
        bit_at(self.louds, index).ok_or(Error::BadTree)
    }
}
