//! SFTP 测试共用的假 SSH 服务端与内存文件系统。

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::PrivateKey;
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Server as _, Session as SshSession};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use kw_ssh::known_hosts::KnownHosts;

pub struct TestServer {
    pub port: u16,
    pub host_key_blob: Vec<u8>,
    shutdown: server::RunningServerHandle,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let generated = kw_keys::generate_ed25519(Some("sftp-test"), None).unwrap();
        let host_key = PrivateKey::from_openssh(&generated.private_key_pem).unwrap();
        let host_key_blob = host_key.public_key().to_bytes().unwrap();
        let config = Arc::new(server::Config {
            keys: vec![host_key],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });

        let mut server = FakeServer;
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let running = server.run_on_socket(config, &listener);
            let _ = tx.send(running.handle());
            let _ = running.await;
        });
        let shutdown = rx.await.unwrap();

        Self {
            port: addr.port(),
            host_key_blob,
            shutdown,
            task,
        }
    }

    /// 把本次测试的临时主机指纹写进 known_hosts，免去弹出确认。
    pub fn trust(&self, known_hosts: &KnownHosts) {
        known_hosts.save("127.0.0.1", self.port, &self.host_key_blob);
    }

    pub async fn stop(self) {
        self.shutdown.shutdown(String::new());
        let _ = self.task.await;
    }
}

struct FakeServer;

impl server::Server for FakeServer {
    type Handler = FakeSshHandler;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
        FakeSshHandler::default()
    }
}

#[derive(Default)]
struct FakeSshHandler {
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl server::Handler for FakeSshHandler {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut SshSession,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(id)?;
            return Ok(());
        }
        let channel = self
            .channels
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("channel {id:?} not found"))?;
        session.channel_success(id)?;
        russh_sftp::server::run(channel.into_stream(), FakeFs::default()).await;
        Ok(())
    }
}

pub struct ClientVerifier;

impl russh::client::Handler for ClientVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

enum Open {
    File { path: String, writable: bool },
    Dir { path: String, listed: bool },
}

pub struct FakeFs {
    files: HashMap<String, Vec<u8>>,
    dirs: HashSet<String>,
    /// 路径 → chmod 后的完整权限位（含类型位）
    perms: HashMap<String, u32>,
    handles: HashMap<String, Open>,
}

impl Default for FakeFs {
    fn default() -> Self {
        Self {
            files: HashMap::new(),
            dirs: HashSet::from(["/".to_string()]),
            perms: HashMap::new(),
            handles: HashMap::new(),
        }
    }
}

impl FakeFs {
    fn attrs(&self, path: &str) -> Option<FileAttributes> {
        if self.dirs.contains(path) {
            return Some(make_attrs(0, true, self.perms.get(path).copied()));
        }
        self.files
            .get(path)
            .map(|data| make_attrs(data.len() as u64, false, self.perms.get(path).copied()))
    }

    fn apply_perms(&mut self, path: &str, mode: Option<u32>) {
        let Some(mode) = mode else { return };
        let current = self
            .attrs(path)
            .and_then(|a| a.permissions)
            .unwrap_or(0o100644);
        self.perms
            .insert(path.to_string(), (current & 0o170000) | (mode & 0o7777));
    }

    fn children(&self, dir: &str) -> Vec<File> {
        let prefix = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let mut out = Vec::new();
        for d in &self.dirs {
            if d != dir {
                if let Some(rest) = d.strip_prefix(&prefix) {
                    if !rest.contains('/') && !rest.is_empty() {
                        out.push(File::new(
                            rest,
                            make_attrs(0, true, self.perms.get(d).copied()),
                        ));
                    }
                }
            }
        }
        for (f, data) in &self.files {
            if let Some(rest) = f.strip_prefix(&prefix) {
                if !rest.contains('/') && !rest.is_empty() {
                    out.push(File::new(
                        rest,
                        make_attrs(data.len() as u64, false, self.perms.get(f).copied()),
                    ));
                }
            }
        }
        out
    }

    fn has_children(&self, dir: &str) -> bool {
        !self.children(dir).is_empty()
    }
}

fn norm(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

fn make_attrs(size: u64, dir: bool, perms: Option<u32>) -> FileAttributes {
    FileAttributes {
        size: Some(size),
        permissions: Some(perms.unwrap_or(if dir { 0o040755 } else { 0o100644 })),
        uid: Some(1000),
        gid: Some(1000),
        user: Some("tester".to_string()),
        group: Some("tester".to_string()),
        atime: Some(1_700_000_000),
        mtime: Some(1_700_000_000),
        ..Default::default()
    }
}

fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

impl russh_sftp::server::Handler for FakeFs {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy(norm(&path))],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let path = norm(&path);
        if !self.dirs.contains(&path) {
            return Err(StatusCode::NoSuchFile);
        }
        let handle = format!("d{id}");
        self.handles.insert(
            handle.clone(),
            Open::Dir {
                path,
                listed: false,
            },
        );
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let (path, already_listed) = match self.handles.get(&handle) {
            Some(Open::Dir { path, listed }) => (path.clone(), *listed),
            _ => return Err(StatusCode::Failure),
        };
        if already_listed {
            return Err(StatusCode::Eof);
        }
        if let Some(Open::Dir { listed, .. }) = self.handles.get_mut(&handle) {
            *listed = true;
        }
        let files = self.children(&path);
        Ok(Name { id, files })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = norm(&filename);
        let exists = self.files.contains_key(&path);
        if pflags.contains(OpenFlags::EXCLUDE) && exists {
            return Err(StatusCode::Failure);
        }
        if !exists && !pflags.contains(OpenFlags::CREATE) {
            return Err(StatusCode::NoSuchFile);
        }
        if pflags.contains(OpenFlags::CREATE) {
            self.files.entry(path.clone()).or_default();
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            self.files.insert(path.clone(), Vec::new());
        }
        let handle = format!("f{id}");
        self.handles.insert(
            handle.clone(),
            Open::File {
                path,
                writable: pflags.contains(OpenFlags::WRITE),
            },
        );
        Ok(Handle { id, handle })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let Some(Open::File { path, .. }) = self.handles.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let Some(data) = self.files.get(path) else {
            return Err(StatusCode::NoSuchFile);
        };
        let start = offset as usize;
        if start >= data.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(data.len());
        Ok(Data {
            id,
            data: data[start..end].to_vec(),
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let Some(Open::File { path, writable }) = self.handles.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        if !writable {
            return Err(StatusCode::PermissionDenied);
        }
        let path = path.clone();
        let buf = self.files.entry(path).or_default();
        let start = offset as usize;
        if buf.len() < start + data.len() {
            buf.resize(start + data.len(), 0);
        }
        buf[start..start + data.len()].copy_from_slice(&data);
        Ok(ok_status(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok_status(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let path = norm(&path);
        let attrs = self.attrs(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs { id, attrs })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = norm(&path);
        if self.attrs(&path).is_none() {
            return Err(StatusCode::NoSuchFile);
        }
        self.apply_perms(&path, attrs.permissions);
        Ok(ok_status(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = match self.handles.get(&handle) {
            Some(Open::File { path, .. }) | Some(Open::Dir { path, .. }) => path.clone(),
            None => return Err(StatusCode::Failure),
        };
        self.apply_perms(&path, attrs.permissions);
        Ok(ok_status(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = norm(&path);
        if self.files.contains_key(&path) || self.dirs.contains(&path) {
            return Err(StatusCode::Failure);
        }
        self.dirs.insert(path);
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let path = norm(&path);
        if path == "/" || !self.dirs.contains(&path) {
            return Err(StatusCode::Failure);
        }
        if self.has_children(&path) {
            return Err(StatusCode::Failure);
        }
        self.dirs.remove(&path);
        self.perms.remove(&path);
        Ok(ok_status(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let path = norm(&filename);
        if self.files.remove(&path).is_some() {
            self.perms.remove(&path);
            Ok(ok_status(id))
        } else {
            Err(StatusCode::NoSuchFile)
        }
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let old = norm(&oldpath);
        let new = norm(&newpath);
        if let Some(data) = self.files.remove(&old) {
            self.files.insert(new, data);
            return Ok(ok_status(id));
        }
        if self.dirs.remove(&old) {
            self.dirs.insert(new);
            return Ok(ok_status(id));
        }
        Err(StatusCode::NoSuchFile)
    }
}
