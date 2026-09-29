use crate::model::{Request, Response};
use anyhow::{Context, Result, bail};
use std::path::Path;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const MAX_FRAME: usize = 1024 * 1024;

/// A bounded JSON-lines frame; never buffers arbitrary amounts of client output.
pub async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            if frame.is_empty() {
                return Ok(None);
            }
            bail!("connection closed before newline");
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(bytes.len(), |pos| pos + 1);
        if frame.len() + count > MAX_FRAME {
            bail!("IPC frame exceeds 1 MiB");
        }
        frame.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(Some(frame));
        }
    }
}

pub async fn request(state_dir: &Path, request: Request) -> Result<Response> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut stream = UnixStream::connect(state_dir.join("control.sock"))
            .await
            .context("cannot reach Prodex; start `prodex daemon` with the same state directory")?;
        let mut data = serde_json::to_vec(&request)?;
        data.push(b'\n');
        if data.len() > MAX_FRAME {
            bail!("request exceeds 1 MiB");
        }
        stream.write_all(&data).await?;
        let bytes = read_frame(&mut BufReader::new(stream))
            .await?
            .context("service closed connection")?;
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await
    .context("service request timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_oversized_and_truncated_frames() {
        let data = vec![b'x'; MAX_FRAME + 1];
        assert!(
            read_frame(&mut BufReader::new(data.as_slice()))
                .await
                .is_err()
        );
        assert!(
            read_frame(&mut BufReader::new(b"{}".as_slice()))
                .await
                .is_err()
        );
        assert_eq!(
            read_frame(&mut BufReader::new(b"{}\n".as_slice()))
                .await
                .unwrap()
                .unwrap(),
            b"{}\n"
        );
    }
}
