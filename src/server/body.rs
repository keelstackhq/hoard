//! Request-body stream that enforces the declared `Content-Length`.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use bytes::Bytes;
use futures::{Stream, StreamExt};

/// Passes chunks through and yields an error if the total differs from `expected` (more or
/// fewer bytes), so the store aborts the write and keeps no object.
pub struct ExactLength<S> {
    inner: S,
    expected: u64,
    seen: u64,
    done: bool,
}

impl<S> ExactLength<S> {
    pub fn new(inner: S, expected: u64) -> Self {
        Self {
            inner,
            expected,
            seen: 0,
            done: false,
        }
    }
}

impl<S> Stream for ExactLength<S>
where
    S: Stream<Item = io::Result<Bytes>> + Unpin,
{
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        let item = ready!(self.inner.poll_next_unpin(cx));
        let this = &mut *self;
        Poll::Ready(match item {
            Some(Ok(chunk)) => {
                this.seen += chunk.len() as u64;
                if this.seen > this.expected {
                    this.done = true;
                    Some(Err(mismatch(this.expected, this.seen, "more")))
                } else {
                    Some(Ok(chunk))
                }
            }
            Some(Err(e)) => {
                this.done = true;
                Some(Err(e))
            }
            None => {
                this.done = true;
                (this.seen != this.expected)
                    .then(|| Err(mismatch(this.expected, this.seen, "fewer")))
            }
        })
    }
}

fn mismatch(expected: u64, seen: u64, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("body has {what} bytes than Content-Length {expected} (got at least {seen})"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    async fn collect(chunks: &[&'static [u8]], expected: u64) -> Vec<io::Result<Bytes>> {
        let s = stream::iter(chunks.iter().map(|c| Ok(Bytes::from_static(c))));
        ExactLength::new(s, expected).collect().await
    }

    #[tokio::test]
    async fn exact_length_passes_through() {
        let out = collect(&[b"ab", b"cde"], 5).await;
        let bytes: Vec<_> = out.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            bytes,
            vec![Bytes::from_static(b"ab"), Bytes::from_static(b"cde")]
        );
        assert!(collect(&[], 0).await.is_empty());
    }

    #[tokio::test]
    async fn short_body_errors_at_end() {
        let out = collect(&[b"ab", b"c"], 5).await;
        assert_eq!(out.len(), 3);
        assert!(out[0].is_ok() && out[1].is_ok());
        assert_eq!(
            out[2].as_ref().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn long_body_errors_and_stops() {
        let out = collect(&[b"ab", b"cdef", b"gh"], 5).await;
        assert_eq!(out.len(), 2);
        assert!(out[0].is_ok());
        assert!(out[1].is_err());
    }

    #[tokio::test]
    async fn inner_error_is_forwarded_once() {
        let s = stream::iter(vec![
            Ok(Bytes::from_static(b"a")),
            Err(io::Error::other("boom")),
            Ok(Bytes::from_static(b"b")),
        ]);
        let out: Vec<_> = ExactLength::new(s, 2).collect().await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].as_ref().unwrap_err().to_string(), "boom");
    }
}
