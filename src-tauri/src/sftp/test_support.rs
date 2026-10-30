//! 测试专用 SFTP 服务端（仅 `cfg(test)`）。
//!
//! 直接架在真实文件系统上：`root` 就是「服务器的 `/`」。
//! 客户端传 `/a/b` 时本地解析成 `root/a/b`，所以测试里的路径写法和生产环境完全一致。
//!
//! 只实现客户端真正会用到的操作，其余走 `unimplemented`。
//! 这不是通用实现，目标是**验证我们这侧的编排**（.part、rename、分块、取消），
//! 协议本身的正确性由 `russh-sftp` 自己保证。

use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use russh_sftp::protocol::{
    Attrs, Data, File as SftpFile, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use russh_sftp::server::{Handler, StatusReply};
use tokio::io::{AsyncRead, AsyncWrite};

struct State {
    root: PathBuf,
    next_handle: u64,
    files: HashMap<String, std::fs::File>,
    dirs: HashMap<String, VecDeque<SftpFile>>,
}

impl State {
    /// `/a/b` → `root/a/b`。`/` 映射到 root 本身。
    fn resolve(&self, path: &str) -> PathBuf {
        let relative = path.trim_start_matches('/');
        if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        }
    }

    fn add_file(&mut self, file: std::fs::File) -> String {
        let handle = format!("f{}", self.next_handle);
        self.next_handle += 1;
        self.files.insert(handle.clone(), file);
        handle
    }

    fn add_dir(&mut self, entries: VecDeque<SftpFile>) -> String {
        let handle = format!("d{}", self.next_handle);
        self.next_handle += 1;
        self.dirs.insert(handle.clone(), entries);
        handle
    }
}

pub struct TestSftp {
    state: State,
}

/// 在给定根目录上跑一个 SFTP 服务端，直到对端关闭。
pub async fn serve<S>(stream: S, root: PathBuf)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let handler = TestSftp {
        state: State {
            root,
            next_handle: 0,
            files: HashMap::new(),
            dirs: HashMap::new(),
        },
    };

    russh_sftp::server::run(stream, handler).await;
}

fn io_err(err: std::io::Error) -> StatusReply {
    let code = match err.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        std::io::ErrorKind::AlreadyExists => StatusCode::Failure,
        _ => StatusCode::Failure,
    };
    StatusReply::new(code).with_message(err.to_string())
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

fn attrs_of(meta: &std::fs::Metadata) -> FileAttributes {
    let mut attrs = FileAttributes::empty();
    attrs.size = Some(meta.len());
    attrs.uid = Some(0);
    attrs.gid = Some(0);
    attrs.mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|delta| delta.as_secs() as u32);

    // 先写权限位再打类型位，顺序反了会被 set_type 里的 or 覆盖掉
    if meta.is_dir() {
        attrs.permissions = Some(0o755);
        attrs.set_dir(true);
    } else {
        attrs.permissions = Some(0o644);
        attrs.set_regular(true);
    }

    attrs
}

fn entries_of(local: &Path) -> Result<VecDeque<SftpFile>, StatusReply> {
    let mut entries = VecDeque::new();

    for entry in std::fs::read_dir(local).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let meta = entry.metadata().map_err(io_err)?;
        let name = entry.file_name().to_string_lossy().to_string();

        entries.push_back(SftpFile {
            longname: name.clone(),
            filename: name,
            attrs: attrs_of(&meta),
        });
    }

    Ok(entries)
}

impl Handler for TestSftp {
    type Error = StatusReply;

    fn unimplemented(&self) -> StatusReply {
        StatusReply::new(StatusCode::OpUnsupported)
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, StatusReply> {
        let path = self.state.resolve(&filename);
        let options: OpenOptions = pflags.into();
        let file = options.open(&path).map_err(io_err)?;
        let handle = self.state.add_file(file);
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusReply> {
        self.state.files.remove(&handle);
        self.state.dirs.remove(&handle);
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusReply> {
        let Some(file) = self.state.files.get_mut(&handle) else {
            return Err(StatusReply::new(StatusCode::Failure).with_message("未知句柄"));
        };

        file.seek(SeekFrom::Start(offset)).map_err(io_err)?;

        let mut buffer = vec![0u8; len as usize];
        let read = file.read(&mut buffer).map_err(io_err)?;

        if read == 0 {
            // 规范要求文件读完返回 EOF 而不是空数据
            return Err(StatusReply::new(StatusCode::Eof));
        }

        buffer.truncate(read);
        Ok(Data { id, data: buffer })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusReply> {
        let Some(file) = self.state.files.get_mut(&handle) else {
            return Err(StatusReply::new(StatusCode::Failure).with_message("未知句柄"));
        };

        file.seek(SeekFrom::Start(offset)).map_err(io_err)?;
        file.write_all(&data).map_err(io_err)?;
        Ok(ok(id))
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusReply> {
        let Some(file) = self.state.files.get_mut(&handle) else {
            return Err(StatusReply::new(StatusCode::Failure).with_message("未知句柄"));
        };
        let meta = file.metadata().map_err(io_err)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        let local = self.state.resolve(&path);
        let meta = std::fs::metadata(&local).map_err(io_err)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        let local = self.state.resolve(&path);
        let meta = std::fs::symlink_metadata(&local).map_err(io_err)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusReply> {
        let local = self.state.resolve(&path);
        let entries = entries_of(&local)?;
        let handle = self.state.add_dir(entries);
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusReply> {
        let Some(queue) = self.state.dirs.get_mut(&handle) else {
            return Err(StatusReply::new(StatusCode::Failure).with_message("未知目录句柄"));
        };

        if queue.is_empty() {
            return Err(StatusReply::new(StatusCode::Eof));
        }

        let files: Vec<SftpFile> = queue.drain(..).collect();
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusReply> {
        let local = self.state.resolve(&filename);
        std::fs::remove_file(&local).map_err(io_err)?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let local = self.state.resolve(&path);
        std::fs::create_dir(&local).map_err(io_err)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusReply> {
        let local = self.state.resolve(&path);
        std::fs::remove_dir(&local).map_err(io_err)?;
        Ok(ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, StatusReply> {
        let from = self.state.resolve(&oldpath);
        let to = self.state.resolve(&newpath);
        std::fs::rename(&from, &to).map_err(io_err)?;
        Ok(ok(id))
    }

    /// 客户端拿它确定初始目录。测试里统一把 `/` 当家目录。
    async fn realpath(&mut self, id: u32, _path: String) -> Result<Name, StatusReply> {
        Ok(Name {
            id,
            files: vec![SftpFile::dummy("/")],
        })
    }
}
