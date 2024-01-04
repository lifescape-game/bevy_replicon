use std::{io::Cursor, ops::Range};

/// A reusable buffer with replicated data.
#[derive(Default)]
pub(crate) struct ReplicationBuffer {
    /// Serialized data.
    cursor: Cursor<Vec<u8>>,

    /// Range of last written data from [`Self::get_or_write`].
    last_write: Option<Range<usize>>,
}

impl ReplicationBuffer {
    /// Clears the buffer.
    ///
    /// Keeps allocated capacity for reuse.
    pub(super) fn clear(&mut self) {
        self.cursor.set_position(0);
        self.last_write = None;
    }

    /// Returns an iterator over slices data from the buffer.
    pub(super) fn iter_ranges<'a>(
        &'a self,
        ranges: &'a [Range<usize>],
    ) -> impl Iterator<Item = u8> + 'a {
        ranges
            .iter()
            .flat_map(|range| &self.cursor.get_ref()[range.clone()])
            .copied()
    }

    /// Finishes the current write by clearing last written range.
    ///
    /// Next call [`Self::get_or_write`] will write data into the buffer.
    pub(super) fn end_write(&mut self) {
        self.last_write = None;
    }

    /// Writes data into the buffer and returns its range.
    ///
    /// See also [`Self::end_write`].
    pub(super) fn write(
        &mut self,
        write_fn: impl FnOnce(&mut Cursor<Vec<u8>>) -> bincode::Result<()>,
    ) -> bincode::Result<Range<usize>> {
        let begin = self.cursor.position() as usize;
        (write_fn)(&mut self.cursor)?;
        let end = self.cursor.position() as usize;

        Ok(begin..end)
    }

    /// Returns previously written range or a new range for the written data.
    ///
    /// See also [`Self::end_write`].
    pub(super) fn get_or_write(
        &mut self,
        write_fn: impl FnOnce(&mut Cursor<Vec<u8>>) -> bincode::Result<()>,
    ) -> bincode::Result<Range<usize>> {
        if let Some(last_write) = &self.last_write {
            return Ok(last_write.clone());
        }

        let begin = self.cursor.position() as usize;
        (write_fn)(&mut self.cursor)?;
        let end = self.cursor.position() as usize;
        self.last_write = Some(begin..end);

        Ok(begin..end)
    }
}
