//! Deadlines for incomplete protocol data, not for healthy lane inactivity.
//!
//! Once a frame starts, every partial read shares one absolute deadline.
//! Callers must discard the stream after an error or cancellation: restarting
//! a framed read after consuming only part of its header would lose alignment.

use std::{io, time::Duration};

use desklink_transport::MAX_RELIABLE_MESSAGE_BYTES;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    time::Instant,
};

#[derive(Debug, Error)]
pub(crate) enum ReadDeadlineError {
    #[error("incomplete protocol data exceeded its deadline")]
    Timeout,
    #[error("protocol data ended before its declared length")]
    Truncated,
    #[error("reliable message exceeds its size limit")]
    TooLarge,
    #[error(transparent)]
    Read(#[from] io::Error),
}

pub(crate) async fn read_exact_until<R: AsyncRead + Unpin>(
    receive: &mut R,
    bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), ReadDeadlineError> {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            return Err(ReadDeadlineError::Timeout);
        }
        let length = tokio::time::timeout_at(deadline, receive.read(&mut bytes[offset..]))
            .await
            .map_err(|_| ReadDeadlineError::Timeout)??;
        if length == 0 {
            return Err(ReadDeadlineError::Truncated);
        }
        offset += length;
    }
    Ok(())
}

/// Waiting for the first byte is deliberately unlimited. A configured partial
/// frame budget starts only after that byte arrives, and covers both the rest
/// of the length prefix and the full message body without being refreshed.
pub(crate) async fn next_reliable_frame<R: AsyncRead + Unpin>(
    receive: &mut R,
    partial_frame_timeout: Duration,
) -> Result<Option<Vec<u8>>, ReadDeadlineError> {
    let mut length = [0_u8; 4];
    if receive.read(&mut length[..1]).await? == 0 {
        return Ok(None);
    }
    let deadline = Instant::now() + partial_frame_timeout;
    read_exact_until(receive, &mut length[1..], deadline).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_RELIABLE_MESSAGE_BYTES {
        return Err(ReadDeadlineError::TooLarge);
    }
    let mut body = vec![0; length];
    read_exact_until(receive, &mut body, deadline).await?;
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn empty_read_and_zero_length_frame_need_no_additional_bytes() {
        let (mut send, mut receive) = tokio::io::duplex(16);
        read_exact_until(&mut receive, &mut [], Instant::now())
            .await
            .unwrap();
        send.write_all(&[0; 4]).await.unwrap();
        assert_eq!(
            next_reliable_frame(&mut receive, Duration::from_secs(1))
                .await
                .unwrap(),
            Some(Vec::new())
        );
    }

    #[tokio::test]
    async fn clean_eof_between_frames_is_distinct_from_truncated_prefix_or_body() {
        for bytes in [vec![], vec![0], vec![0, 0, 0, 2, 1]] {
            let clean = bytes.is_empty();
            let (mut send, mut receive) = tokio::io::duplex(16);
            send.write_all(&bytes).await.unwrap();
            send.shutdown().await.unwrap();
            let result = next_reliable_frame(&mut receive, Duration::from_secs(1)).await;
            if clean {
                assert!(matches!(result, Ok(None)));
            } else {
                assert!(matches!(result, Err(ReadDeadlineError::Truncated)));
            }
        }
    }

    #[tokio::test]
    async fn oversized_length_is_rejected_without_waiting_for_a_body() {
        let (mut send, mut receive) = tokio::io::duplex(16);
        send.write_all(&((MAX_RELIABLE_MESSAGE_BYTES + 1) as u32).to_be_bytes())
            .await
            .unwrap();
        assert!(matches!(
            next_reliable_frame(&mut receive, Duration::from_secs(1)).await,
            Err(ReadDeadlineError::TooLarge)
        ));
    }

    #[tokio::test]
    async fn waiting_for_a_new_frame_does_not_spend_its_partial_frame_budget() {
        let (mut send, mut receive) = tokio::io::duplex(16);
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            send.write_all(&[0, 0, 0, 2, 4, 2]).await.unwrap();
        });
        assert_eq!(
            next_reliable_frame(&mut receive, Duration::from_millis(30))
                .await
                .unwrap(),
            Some(vec![4, 2])
        );
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn trickling_bytes_does_not_refresh_an_absolute_deadline() {
        let (mut send, mut receive) = tokio::io::duplex(16);
        let writer = tokio::spawn(async move {
            for byte in 0..12 {
                if send.write_all(&[byte]).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let mut target = [0; 12];
        assert!(matches!(
            read_exact_until(
                &mut receive,
                &mut target,
                Instant::now() + Duration::from_millis(45),
            )
            .await,
            Err(ReadDeadlineError::Timeout)
        ));
        writer.abort();
    }

    #[tokio::test]
    async fn length_prefix_and_body_share_the_same_deadline() {
        let (mut send, mut receive) = tokio::io::duplex(16);
        let writer = tokio::spawn(async move {
            send.write_all(&[0]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(35)).await;
            send.write_all(&[0, 0, 1]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(35)).await;
            let _ = send.write_all(&[42]).await;
        });
        assert!(matches!(
            next_reliable_frame(&mut receive, Duration::from_millis(50)).await,
            Err(ReadDeadlineError::Timeout)
        ));
        writer.abort();
    }
}
