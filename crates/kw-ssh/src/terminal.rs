//! 缓冲式 PTY 终端：读半边搬进独立任务，输出先进队列，由调用方按需取走。
//!
//! 手机端（`kw-ffi`）只有请求/响应，没有回调通道，所以不能像桌面的 WebSocket
//! 那样推流。这里把 russh 通道的读半边交给一个任务，它只往队列里塞字节块；
//! Dart 侧轮询 `drain` 取增量。队列有上限，刷屏时丢最旧的整块并置 `dropped`，
//! 让界面知道要先清屏再重绘。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use russh::client::Msg as ClientMsg;

use crate::session::{PtyShell, SshClient, SshError};
use crate::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};

/// 缓冲上限：约 1 MiB，超出就整块丢最旧的。
const QUEUE_LIMIT: usize = 1 << 20;
/// 单条通道消息的上限：超出保留尾部（刷屏时最后一段才是看得见的内容）。
const MAX_CHUNK: usize = 1 << 19;
/// 单次 `drain` 最多返回多少字节，免得一个响应体过大。
const READ_MAX: usize = 1 << 16;

#[derive(Default)]
struct OutQueue {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    dropped: bool,
}

impl OutQueue {
    fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut chunk = data.to_vec();
        if chunk.len() > MAX_CHUNK {
            let skip = chunk.len() - MAX_CHUNK;
            chunk.drain(..skip);
            self.dropped = true;
        }
        self.bytes += chunk.len();
        self.chunks.push_back(chunk);
        while self.bytes > QUEUE_LIMIT {
            let Some(old) = self.chunks.pop_front() else {
                break;
            };
            self.bytes -= old.len();
            self.dropped = true;
        }
    }

    /// 取走至多 `max` 字节；截断剩下的留在队首，下次继续取，不丢数据。
    fn drain(&mut self, max: usize) -> (Vec<u8>, bool) {
        let mut out = Vec::with_capacity(self.bytes.min(max));
        while let Some(chunk) = self.chunks.pop_front() {
            if out.len() + chunk.len() > max {
                let take = max - out.len();
                if take == 0 {
                    // 本次已经取满，整块原样放回队首。
                    self.chunks.push_front(chunk);
                    break;
                }
                out.extend_from_slice(&chunk[..take]);
                self.bytes -= take;
                let mut rest = chunk;
                rest.drain(..take);
                self.chunks.push_front(rest);
                break;
            }
            self.bytes -= chunk.len();
            out.extend_from_slice(&chunk);
        }
        (out, std::mem::take(&mut self.dropped))
    }
}

pub struct Terminal {
    sid: String,
    cols: AtomicU32,
    rows: AtomicU32,
    queue: Mutex<OutQueue>,
    closed: AtomicBool,
    write: Mutex<ChannelWriteHalf<ClientMsg>>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl Terminal {
    pub async fn open(
        client: &SshClient,
        sid: &str,
        cols: u32,
        rows: u32,
    ) -> Result<Arc<Self>, SshError> {
        let shell = PtyShell::open(client, "xterm-256color", cols, rows).await?;
        let (read, write) = shell.into_parts();
        let term = Arc::new(Self {
            sid: sid.to_string(),
            cols: AtomicU32::new(cols),
            rows: AtomicU32::new(rows),
            queue: Mutex::new(OutQueue::default()),
            closed: AtomicBool::new(false),
            write: Mutex::new(write),
            reader: Mutex::new(None),
        });
        let handle = tokio::spawn(read_loop(Arc::clone(&term), read));
        *term.reader.lock().await = Some(handle);
        Ok(term)
    }

    pub fn sid(&self) -> &str {
        &self.sid
    }

    pub fn dims(&self) -> (u32, u32) {
        (
            self.cols.load(Ordering::Relaxed),
            self.rows.load(Ordering::Relaxed),
        )
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    async fn push(&self, data: &[u8]) {
        self.queue.lock().await.push(data);
    }

    /// 取走当前缓冲；`dropped` 为真表示上一段被挤掉过，界面要整屏重绘。
    pub async fn drain(&self) -> (Vec<u8>, bool) {
        self.queue.lock().await.drain(READ_MAX)
    }

    pub async fn input(&self, data: &[u8]) -> Result<(), SshError> {
        if self.is_closed() {
            return Err(SshError::msg("终端已退出，请重新打开"));
        }
        if self
            .write
            .lock()
            .await
            .data_bytes(data.to_vec())
            .await
            .is_err()
        {
            self.closed.store(true, Ordering::Relaxed);
            return Err(SshError::msg("SSH 连接已断开，终端会话结束"));
        }
        Ok(())
    }

    pub async fn resize(&self, cols: u32, rows: u32) -> Result<(), SshError> {
        if self
            .write
            .lock()
            .await
            .window_change(cols, rows, 0, 0)
            .await
            .is_err()
        {
            self.closed.store(true, Ordering::Relaxed);
            return Err(SshError::msg("SSH 连接已断开，终端会话结束"));
        }
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        Ok(())
    }

    pub async fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        if let Some(handle) = self.reader.lock().await.take() {
            handle.abort();
        }
        let write = self.write.lock().await;
        let _ = write.eof().await;
        let _ = write.close().await;
    }
}

async fn read_loop(term: Arc<Terminal>, mut read: ChannelReadHalf) {
    while let Some(msg) = read.wait().await {
        match msg {
            ChannelMsg::Data { data } => term.push(&data).await,
            ChannelMsg::ExtendedData { data, ext } if ext == 1 => term.push(&data).await,
            ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => break,
            _ => {}
        }
    }
    term.closed.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_chunk_keeps_tail_and_flags_drop() {
        let mut queue = OutQueue::default();
        queue.push(&vec![b'a'; MAX_CHUNK * 2]);
        let (bytes, dropped) = queue.drain(READ_MAX);
        assert!(dropped, "被截断的整块必须告诉客户端要重绘");
        assert_eq!(bytes.len(), READ_MAX);
        assert_eq!(queue.bytes, MAX_CHUNK - READ_MAX);
    }

    #[test]
    fn overflow_drops_oldest_whole_chunks() {
        let mut queue = OutQueue::default();
        for _ in 0..40 {
            queue.push(&vec![b'x'; MAX_CHUNK / 2]);
        }
        assert!(queue.bytes <= QUEUE_LIMIT, "缓冲必须封顶");
        let mut dropped = false;
        let mut total = 0;
        loop {
            let (bytes, flag) = queue.drain(READ_MAX);
            dropped |= flag;
            if bytes.is_empty() {
                break;
            }
            total += bytes.len();
        }
        assert!(dropped, "溢出必须告诉客户端要重绘");
        assert!(total > 0 && total <= QUEUE_LIMIT);
        assert_eq!(queue.bytes, 0);
    }

    #[test]
    fn split_chunk_stays_in_queue_until_taken() {
        let mut queue = OutQueue::default();
        queue.push(&vec![b'k'; MAX_CHUNK / 2]);
        let (first, dropped) = queue.drain(READ_MAX);
        assert_eq!(first.len(), READ_MAX, "一次取走量必须封顶");
        assert!(!dropped, "没溢出就不该报丢弃");
        assert_eq!(queue.bytes, MAX_CHUNK / 2 - READ_MAX);
        let (rest, _) = queue.drain(READ_MAX);
        assert_eq!(rest.len(), READ_MAX);
        let mut taken = 2 * READ_MAX;
        while queue.bytes > 0 {
            taken += queue.drain(READ_MAX).0.len();
        }
        assert_eq!(taken, MAX_CHUNK / 2, "一块要能被完整取走，不丢字节");
        assert!(queue.drain(READ_MAX).0.is_empty());
    }

    #[test]
    fn drain_never_loses_bytes_and_empty_push_is_ignored() {
        let mut queue = OutQueue::default();
        queue.push(b"");
        assert_eq!(queue.drain(READ_MAX).0.len(), 0);
        let words = ["alpha ", "bravo ", "charlie"];
        for word in words {
            queue.push(word.as_bytes());
        }
        let mut collected = Vec::new();
        // 逐 4 字节取，正好踩到「块被切开」的分支
        while queue.bytes > 0 {
            let (bytes, dropped) = queue.drain(4);
            assert!(!bytes.is_empty(), "队列里还有字节就必须取到东西");
            assert!(!dropped);
            collected.extend_from_slice(&bytes);
        }
        assert_eq!(collected, b"alpha bravo charlie".to_vec());
    }
}
