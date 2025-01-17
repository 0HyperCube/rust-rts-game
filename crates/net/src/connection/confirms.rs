use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use async_std::channel::{SendError, Sender};

use super::book::{Connection, ConnectionBook};
use crate::{
    header::{DatagramHeader, DatagramId},
    messages::MAX_MESSAGE_SIZE,
    tasks::dsender::OutDatagram,
};

/// The buffer is flushed after it grows beyond this number of bytes.
// Each ID is 3 bytes, thus this must be a multiple of 3.
const MAX_BUFF_SIZE: usize = 96;
/// The buffer is flushed after the oldest part is older than this.
const MAX_BUFF_AGE: Duration = Duration::from_millis(100);

pub(crate) struct Confirmations {
    book: ConnectionBook<Buffer>,
}

impl Confirmations {
    pub(crate) fn new() -> Self {
        Self {
            book: ConnectionBook::new(),
        }
    }

    /// This method marks a message with `id` from `addr` as received.
    ///
    /// This method should be called exactly once after each reliable message
    /// is delivered.
    pub(crate) fn received(&mut self, time: Instant, addr: SocketAddr, id: DatagramId) {
        self.book.update(time, addr, Buffer::new).push(time, id);
    }

    /// Send message confirmation packets which are ready to be send.
    ///
    /// # Arguments
    ///
    /// * `time` - current time.
    ///
    /// * `buf` - buffer for message construction. Must be at least
    ///   [`crate::MAX_DATAGRAM_SIZE`] long.
    ///
    /// * `messages` - message connection to be used for delivery of the
    ///   confirmations.
    ///
    /// # Panics
    ///
    /// May panic if `buf` is not large enough.
    pub(crate) async fn send_confirms(
        &mut self,
        time: Instant,
        datagrams: &mut Sender<OutDatagram>,
    ) -> Result<(), SendError<OutDatagram>> {
        while let Some((addr, buffer)) = self.book.next() {
            if buffer.ready(time) {
                while let Some(data) = buffer.flush(MAX_MESSAGE_SIZE) {
                    datagrams
                        .send(OutDatagram::new(
                            DatagramHeader::Confirmation,
                            data.to_vec(),
                            addr,
                        ))
                        .await?;
                }
            }
        }

        Ok(())
    }

    pub(crate) fn clean(&mut self, time: Instant) {
        self.book.clean(time);
    }
}

/// Buffer with datagram confirmations.
struct Buffer {
    oldest: Instant,
    buffer: Vec<u8>,
    flushed: usize,
}

impl Buffer {
    fn new() -> Self {
        Self {
            oldest: Instant::now(),
            buffer: Vec::with_capacity(MAX_BUFF_SIZE),
            flushed: 0,
        }
    }

    /// Pushes another datagram ID to the buffer.
    fn push(&mut self, time: Instant, id: DatagramId) {
        if self.buffer.is_empty() {
            self.oldest = time;
        }
        self.buffer.extend_from_slice(&id.to_bytes());
        self.flushed = self.buffer.len();
    }

    /// Returns true if the buffer is ready to be flushed (too old or too
    /// large).
    fn ready(&self, time: Instant) -> bool {
        if self.buffer.is_empty() {
            return false;
        }

        (self.oldest + MAX_BUFF_AGE) <= time || self.buffer.len() >= MAX_BUFF_SIZE
    }

    /// Return accumulated bytes from the buffer if it is not empty. The number
    /// of returned bytes is always smaller than `max_size`. This method should
    /// be called repeatedly until it returns None.
    fn flush(&mut self, max_size: usize) -> Option<&[u8]> {
        self.buffer.truncate(self.flushed);

        if self.buffer.is_empty() {
            None
        } else {
            // Make sure it is multiple of 4 (i.e. larges multiple of 4 smaller
            // or equal than the original).
            let size = self.buffer.len().min(max_size & (usize::MAX - 3));
            self.flushed = self.buffer.len() - size;
            Some(&self.buffer[self.flushed..])
        }
    }
}

impl Connection for Buffer {
    fn pending(&self) -> bool {
        !self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer() {
        let now = Instant::now();
        let mut buf = Buffer::new();

        assert!(buf.flush(13).is_none());
        assert!(!buf.ready(now));

        buf.push(now, 1042.try_into().unwrap());
        assert!(!buf.ready(now));
        assert_eq!(buf.flush(13).unwrap(), &[0, 4, 18]);
        assert!(!buf.ready(now));
        assert!(buf.flush(13).is_none());
        assert!(!buf.ready(now));

        buf.push(now, 43.try_into().unwrap());
        assert!(!buf.ready(now));
        assert!(buf.ready(now + Duration::from_secs(10)));
        assert_eq!(buf.flush(13).unwrap(), &[0, 0, 43]);
        assert!(buf.flush(13).is_none());

        for i in 0..32 {
            buf.push(now, (100 + i).try_into().unwrap());

            if i < 31 {
                assert!(!buf.ready(now));
            } else {
                assert!(buf.ready(now));
            }
        }

        for i in 0..8 {
            assert_eq!(
                buf.flush(12 + (i as usize) % 3).unwrap(),
                &[
                    0,
                    0,
                    128 - i * 4,
                    0,
                    0,
                    129 - i * 4,
                    0,
                    0,
                    130 - i * 4,
                    0,
                    0,
                    131 - i * 4
                ]
            );
        }

        assert!(buf.flush(8).is_none());
    }
}
