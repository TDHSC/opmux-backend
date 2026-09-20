//! Bounded accumulation of upstream HTTP bodies before JSON parsing.

use super::error::ExecutorError;

/// Fails when a `Content-Length` header already exceeds the configured cap.
///
/// A missing length is allowed; chunked and other unknown-length bodies are
/// still capped while reading.
pub(crate) fn check_advertised_content_length(
    content_length: Option<u64>,
    max_bytes: u64,
) -> Result<(), ExecutorError> {
    if content_length.is_some_and(|len| len > max_bytes) {
        return Err(ExecutorError::InvalidUpstreamResult);
    }
    Ok(())
}

/// Appends one chunk if the result would still fit in `max_bytes`.
///
/// The chunk is not copied when it would exceed the cap, so callers can stop
/// pulling from the source after this error.
pub(crate) fn append_bounded(
    buf: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> Result<(), ExecutorError> {
    let new_len = buf.len().saturating_add(chunk.len());
    if new_len > max_bytes {
        return Err(ExecutorError::InvalidUpstreamResult);
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// Accumulates iterator chunks until EOF or the exclusive byte cap.
///
/// Stops without pulling further items once a chunk would exceed the cap.
/// Does not JSON-decode the body.
#[cfg(test)]
pub(crate) fn accumulate_chunks<I, B>(
    chunks: I,
    max_bytes: usize,
) -> Result<Vec<u8>, ExecutorError>
where
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    let mut buf = Vec::new();
    for chunk in chunks {
        append_bounded(&mut buf, chunk.as_ref(), max_bytes)?;
    }
    Ok(buf)
}

/// Reads a Reqwest response body, honoring `max_bytes` while streaming.
///
/// Advertised `Content-Length` values above the cap fail before chunks are
/// pulled. Chunked and other unknown-length bodies are still bounded. The
/// body is not deserialized here.
pub(crate) async fn read_bounded_response_body(
    mut response: reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, ExecutorError> {
    check_advertised_content_length(response.content_length(), max_bytes)?;
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => append_bounded(&mut body, &chunk, max_bytes)?,
            Ok(None) => return Ok(body),
            Err(err) => return Err(transport_error(err)),
        }
    }
}

fn transport_error(err: reqwest::Error) -> ExecutorError {
    if err.is_timeout() {
        ExecutorError::TimeoutError(0)
    } else {
        ExecutorError::NetworkError("upstream transport error".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct CountingChunks {
        inner: std::vec::IntoIter<Vec<u8>>,
        pulled: Rc<Cell<usize>>,
    }

    impl Iterator for CountingChunks {
        type Item = Vec<u8>;

        fn next(&mut self) -> Option<Self::Item> {
            let item = self.inner.next()?;
            self.pulled
                .set(self.pulled.get().saturating_add(item.len()));
            Some(item)
        }
    }

    fn counting(chunks: Vec<Vec<u8>>) -> (CountingChunks, Rc<Cell<usize>>) {
        let pulled = Rc::new(Cell::new(0));
        (
            CountingChunks {
                inner: chunks.into_iter(),
                pulled: pulled.clone(),
            },
            pulled,
        )
    }

    #[test]
    fn exact_cap_is_accepted_without_parsing() {
        let body = accumulate_chunks([b"{\"ok\":true}".as_slice()], 11)
            .expect("exact-size body must be accepted");
        assert_eq!(body, b"{\"ok\":true}");
    }

    #[test]
    fn one_byte_over_cap_fails_and_does_not_keep_the_overflow_bytes() {
        let mut buf = b"{\"ok\":true}".to_vec();
        match append_bounded(&mut buf, b"!", 11) {
            Err(ExecutorError::InvalidUpstreamResult) => {}
            other => panic!("expected InvalidUpstreamResult, got {other:?}"),
        }
        assert_eq!(buf, b"{\"ok\":true}");
    }

    #[test]
    fn accumulation_stops_before_pulling_remaining_chunks() {
        let (chunks, pulled) = counting(vec![
            vec![b'a'; 10],
            vec![b'b'; 10],
            vec![b'c'; 10],
            vec![b'x'; 10_000],
        ]);
        match accumulate_chunks(chunks, 25) {
            Err(ExecutorError::InvalidUpstreamResult) => {}
            other => panic!("expected InvalidUpstreamResult, got {other:?}"),
        }
        assert_eq!(
            pulled.get(),
            30,
            "reader must stop after the overflowing chunk rather than draining the remainder"
        );
    }

    #[test]
    fn advertised_content_length_above_cap_fails_without_reading() {
        match check_advertised_content_length(Some(101), 100) {
            Err(ExecutorError::InvalidUpstreamResult) => {}
            other => panic!("expected InvalidUpstreamResult, got {other:?}"),
        }
        check_advertised_content_length(Some(100), 100)
            .expect("exact advertised length is allowed");
        check_advertised_content_length(None, 100)
            .expect("missing content-length is capped while reading");
    }
}
