# circus-sandbox

Rust 库，包名 `circus-sandbox`。当前只提供 workspace 文件能力：在 tempfile 新建的临时目录里，通过 cap-std 目录句柄做相对路径的同步读写。

这不是进程沙箱，也不是容器。它不隔离 CPU、网络、系统调用或进程，不接受调用方传入的宿主目录，公开 API 也不返回工作区根路径。Git、命令执行、生命周期和容器后端都还没有实现。也没有工作区总字节配额、文件数配额或宿主磁盘配额。

工具链基线是 Rust **1.99.1**、edition **2024**。直接依赖是 cap-std **4.0.3**、cap-fs-ext **4.0.3**（仅 Unix `OpenOptionsSyncExt::nonblock`，不开 default features）、tempfile **3.27.0**、thiserror **2.0.21**（非预发布）。`Cargo.lock` 固定解析版本；检查和运行都加 `--locked`。

## 同步文件 API

```rust
use circus_sandbox::Workspace;

fn prepare() -> Result<(), circus_sandbox::WorkspaceError> {
    let mut workspace = Workspace::new(1024)?;
    workspace.create_dir_all("notes")?;
    workspace.write_file("notes/hello.txt", b"hello")?;
    let bytes = workspace.read_file("notes/hello.txt")?;
    assert_eq!(bytes, b"hello");
    workspace.remove_file("notes/hello.txt")?;
    workspace.cleanup()?;
    Ok(())
}
```

| 方法                             | 行为                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Workspace::new(max_file_bytes)` | 新建独立临时目录。`max_file_bytes` 必须是 `1..=usize::MAX-1`，否则返回 `InvalidLimit`。这是单个文件的读写上限，不是预分配大小，不是工作区总字节或文件数配额，也不是宿主磁盘配额或进程隔离。                                                                                                                                                                                                                                                                                                                                                 |
| `write_file(path, contents)`     | 写入或覆盖已有普通文件。不隐式创建父目录，也没有原子替换。`Closed`、非法路径、超限字节、缺失父目录和非普通文件都在截断之前返回，这些失败不修改已有文件，也不留下新文件。Unix 同样以 `nonblock(true)` 打开，Windows 不设置该标志，CreateFile 是 write+create 且不预先 truncate。确认是普通文件后先 `set_len(0)` 再 `write_all`。此后的 I/O 失败可能截断已有内容，或留下不完整或空文件。                                                                                                                                                      |
| `read_file(path)`                | 读取普通文件的全部字节。目录、FIFO、设备及其他非普通节点，包括工作区内指向它们的符号链接，返回 `NotRegularFile`，并保留调用方路径。指向普通文件的工作区符号链接仍跟随。Unix 用 cap-std `OpenOptions` 的 `nonblock(true)` 打开，所以 `open` 本身不等待 FIFO 对端；这不是路径 metadata 预检。Windows 不设置该标志，读取是只读打开。不会按不可信 metadata 无限分配。超过上限时最多探测 `limit + 1` 字节后返回 `LimitExceeded`，`actual` 是这次探测到的长度，不是未知的文件总长。逃出工作区的符号链接仍是 cap-std 的 I/O 错误，不是新错误变体。 |
| `create_dir_all(path)`           | 创建相对目录及其缺失的父目录。已存在时成功。                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `remove_file(path)`              | 通过 cap-std `Dir::remove_file` 对最终组件 unlink。删除非目录项，包括普通文件、符号链接、FIFO 和 socket。不跟随符号链接目标，不删除目录。目录得到 `Io`，不是 `NotRegularFile`。删除前不做类型预检。                                                                                                                                                                                                                                                                                                                                         |
| `cleanup(&mut self)`             | 删除临时目录。可重复调用：已经删除视为成功。成功之后，上述文件操作都返回 `Closed`。失败时保留工作区归属，可以再调一次；失败不会把工作区标成已关闭。                                                                                                                                                                                                                                                                                                                                                                                         |

这些方法会阻塞当前线程。库本身没有异步运行时。将来在 Tokio 异步上下文里调用时，必须放到 `spawn_blocking` 上，不能直接占住异步 worker，也不要引入第二套异步运行时。

`Drop` 只尽力删除临时目录，不返回错误。需要确认清理结果时调用 `cleanup`。

## 单文件上限、删除与并发

`max_file_bytes` 只约束一次 `write_file` 的字节数，以及一次 `read_file` 最多探测的长度。读和写共用这一个单文件上限，不是把多个文件的用量加总，也不是文件个数上限。`new` 不按该值预分配，也不向操作系统申请磁盘配额。多次各自未超限的写入仍可能写满宿主临时目录；那是磁盘耗尽，本库不会把它报成 `LimitExceeded`。当前没有总配额 API，也不提供进程隔离。以后若实现真实的进程或容器后端，必须单独、显式地限制 CPU、内存、进程数和磁盘，不能把这个参数当成沙箱配额。本库不为此增加总配额 API。

`remove_file` 就是 cap-std 的 unlink：去掉路径最终组件上的非目录项。普通文件、符号链接、FIFO、socket，以及该平台 unlink 会删除的其他非目录节点，都会被删掉。最终组件是符号链接时，删的是链接本身，不打开、不跟随目标，因此也不会删掉链接指向的目录或文件。目录删不掉，返回 `Io`，目录及其内容保留。这不是“只删除普通文件”。读写才用 `NotRegularFile` 拒绝非普通节点；删除前再做类型检查会和 unlink 竞态，本 API 不做。

同一 `Workspace` 的并发读写由内部 `RwLock` 保护，避免本进程里对这一个值的交错写入。这不是外部进程隔离：其他进程仍可改临时目录。这也不是崩溃安全的原子写。`write_file` 没有原子替换；只有已关闭、非法路径、超限、缺失父目录和非普通文件这些失败发生在截断之前，不修改已有文件。打开普通文件并 `set_len(0)` 之后的 I/O 失败，仍可能截断已有内容或留下不完整文件。这把锁不补上那些保证，也不构成磁盘配额。

可运行示例在 `examples/workspace.rs`。它实际执行写、读、删除和 `cleanup`。成功时标准输出只有读到的字节再加一个换行；失败写到标准错误并以状态码 1 退出，不打印与 `Result` 无关的成功字样：

```bash
cargo run --locked --example workspace
```

在 `circus-sandbox/` 目录执行。rustup 会按该目录的 `rust-toolchain.toml` 选择 1.99.1。

## 命令

同样在 `circus-sandbox/` 下执行：

| 目的                | 命令                                                                                            |
| ------------------- | ----------------------------------------------------------------------------------------------- |
| 格式检查 / 格式化   | `cargo fmt --check` / `cargo fmt`                                                               |
| Clippy              | `cargo clippy --locked --all-targets -- -D warnings`                                            |
| 构建                | `cargo build --locked`                                                                          |
| 测试                | `cargo test --locked`                                                                           |
| 只跑本 API 集成测试 | `cargo test --locked --test workspace`                                                          |
| 文档                | `RUSTDOCFLAGS=-D warnings cargo doc --locked --no-deps`                                         |
| 运行示例            | `cargo run --locked --example workspace`                                                        |
| 依赖策略            | 先 `cargo install --locked --version 0.20.2 cargo-deny`，再在本目录 `cargo deny --locked check` |

`cargo test --locked` 覆盖库测试和 `tests/`。要连示例一起编译，使用 `cargo test --locked --all-targets`。不要手改 `Cargo.lock` 来绕过解析失败。

Windows 本机目标是 `x86_64-pc-windows-msvc`。这里的编译成功不表示进程隔离或 Linux 容器后端可用；那些后端还没有实现。符号链接测试需要创建符号链接的权限（开发者模式，或进程持有 `SeCreateSymbolicLinkPrivilege`）。没有该权限时，相关测试应失败，不能静默跳过；不要修改宿主安全设置来让它们通过。

## CI

`.github/workflows/sandbox-rust.yml`（`Sandbox Rust`）在每个 pull request、`main` push、merge queue 和手动运行时启动，没有路径过滤，也不调用 Paseo CI。`rust` job 在 `ubuntu-latest` 与 `windows-latest` 上跑上表的 fmt、clippy、build、test、doc 和 example。`dependencies` job 只在 Ubuntu 安装 cargo-deny 0.20.2 并执行 `cargo deny --locked check`。`codeql` job 只在 Ubuntu，以 Rust `build-mode: none` 分析 `circus-sandbox` 源码，不编译，配置是 `.github/codeql/sandbox-rust.yml`。

许可证 allow 只有 Apache-2.0、MIT、`Apache-2.0 WITH LLVM-exception`（winx 0.36.4）和 Unicode-3.0（unicode-ident 1.0.27）。没有例外列表。`multiple-versions` 只警告。当前 lock 里去不掉的传递重复是 `io-lifetimes` 2.0.4 与 3.0.1，以及 Windows SDK 绑定 `windows-sys` 0.59.0、0.60.2 与 0.61.2，都来自 cap-std / cap-fs-ext 4.0.3 的依赖闭包。细节见 [AGENTS.md](AGENTS.md)。工作流已配置不等于这些检查已经通过；结果看对应 PR 的 Actions。

## 路径

拒绝发生在打开句柄之前，并且 Windows 与 Linux 使用同一规则。非法路径是：空路径（`Empty`）、含 `\`（`Backslash`）、以盘符 `A:`–`Z:` 开头或以 `//` 开头（`WindowsPrefix`）、以 `/` 开头或 `Path::is_absolute()`（`Absolute`）、任一组件是 `..`（`ParentComponent`）。`cleanup` 成功后的文件操作是 `Closed`，不是这些路径类别。

`foo/bar`、`foo/./bar` 可以通过。`create_dir_all` 之外不会自动创建父目录。

通过检查之后，读写都从临时目录的 cap-std `Dir` 句柄出发。cap-std 会拒绝逃出该目录的 `..` 和符号链接，避免“先检查字符串再打开路径”的 TOCTOU。符号链接用例在 crate 私有测试里，因为公开 API 不暴露工作区根，也不能创建符号链接。这只约束文件访问路径，不能阻止进程打开其他宿主路径。

## 错误

`WorkspaceError` 实现了 `std::error::Error`，可以用 `match` 区分：

| 变体                              | 何时出现                                                                                                                                                                                                                                                                                                   |
| --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Closed`                          | `cleanup` 已经成功。优先于路径和长度检查。                                                                                                                                                                                                                                                                 |
| `InvalidLimit { max_file_bytes }` | `new(0)` 或 `new(usize::MAX)`。                                                                                                                                                                                                                                                                            |
| `PathRejected { path, reason }`   | 上一节的非法路径。`path` 是调用方原始路径。                                                                                                                                                                                                                                                                |
| `LimitExceeded { limit, actual }` | 写入或读取超过 `max_file_bytes`。写入时 `actual` 是 `contents.len()`。读取时最多探测 `limit + 1` 字节，`actual` 是探测长度，不是未知的文件总长。                                                                                                                                                           |
| `NotRegularFile { path }`         | 把目录、FIFO 或其他非普通文件当作文件读写，包括工作区内指向它们的符号链接。不用于 `remove_file`。Unix 上不会为了等 FIFO 对端而卡住。拒绝发生在截断之前。                                                                                                                                                   |
| `Io(std::io::Error)`              | 缺失文件、缺失父目录、`remove_file` 遇到目录、临时目录创建/删除失败，以及写入过程中的文件系统错误。缺失路径是 `ErrorKind::NotFound`，发生在截断之前。unlink 掉符号链接、FIFO 或 socket 是成功，不是这个变体，也不是 `NotRegularFile`。写入时若错误发生在打开普通文件之后，文件可能已被截断或只写入一部分。 |

`cleanup` 的 I/O 失败也是 `Io`。此时工作区仍然打开，应保留同一个 `Workspace` 再调用 `cleanup`，不要丢弃错误后假设目录已消失。

## 变更

### 0.1.0

这是当前 crate 版本，也是 workspace 文件能力第一次对调用方可用的范围。它不是进程沙箱或容器。

可用：

- `Workspace::new`、`write_file`、`read_file`、`create_dir_all`、`remove_file`、`cleanup`。方法阻塞调用线程，库不提供异步 API。
- 临时目录由库自己创建。公开 API 不接受宿主目录，也不返回工作区根路径。
- 空路径、绝对路径、`..`、反斜杠和 Windows 盘符/UNC 前缀在打开前拒绝。通过后的访问从 cap-std 目录句柄出发。
- 读和写使用同一个 `max_file_bytes`，含义是单个文件的上限，不是工作区总字节、文件数或宿主磁盘配额。超限写入在创建或截断之前失败。读取不按不可信 metadata 分配；超限时 `actual` 是探测到的长度，不是未知总长。当前没有总配额 API。
- `cleanup` 成功后文件操作返回 `Closed`，并可重复调用。清理失败保留工作区，可以再试。`Drop` 只尽力删除，不返回错误。

限制：

- 没有命令执行、Git、生命周期状态机、容器后端或沙箱池。
- `write_file` 不是原子替换，也没有单独的原子写入 API。只有关闭、非法路径、超限、缺失父目录和非普通文件这类前置失败保证不改文件。打开普通文件之后的 I/O 失败可能截断已有文件，或留下部分内容。
- `remove_file` 按 cap-std unlink 删除非目录项，包括符号链接、FIFO 和 socket。不跟随符号链接目标，不删除目录，删除前也不做类型预检。
- 同一 `Workspace` 的并发读写由内部 `RwLock` 保护。这不是外部进程隔离，也不是崩溃安全的原子写。
- 没有工作区总配额或宿主磁盘配额。后续真实后端必须显式配置资源限制。

## 许可证

本 crate 以 [Apache License 2.0](LICENSE) 授权。依赖保持各自的许可证。本目录没有、也不编造 `NOTICE` 文件。
