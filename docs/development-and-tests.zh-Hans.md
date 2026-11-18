# 开发与测试

构建分支代码、运行测试并替换固定的磁盘镜像。二进制文件的日常使用请参阅 [readme.md](../readme.md)，命令行参数请参阅 [USAGE.md](../USAGE.md)。

## 克隆与初始化

克隆此分支及其网络子模块。在 Apple Silicon Mac 上运行 `cargo build` 会编译该子模块。

```bash
git clone --recurse-submodules git@github.com:hangxingliu/vibe.git
cd vibe

# （可选）如果你已经安装了最新版本的 Rust、Go、shfmt 和 shellcheck
mise install --locked
```

如果克隆时未包含子模块，请进行初始化：

```bash
git submodule update --init helpers/gvisor-tap-vsock
```

## 构建

```bash
cargo build --release --locked
```

构建出的二进制文件位于 `target/release/vibe`。`./scripts/vibe_dev` 会执行 `cargo run --release --`。调试构建命令为 `cargo build --locked`。

`build.rs` 仅针对宿主机 `apple-darwin` 和目标架构 `aarch64-apple-darwin` 构建 `helpers/vibe-usernet`。该构建过程会设置 `CGO_ENABLED=1`、`GOOS=darwin` 和 `GOARCH=arm64`，然后嵌入该二进制文件。其他宿主机平台只会嵌入一个空桩（stub）。在这些平台上 `cargo check` 仍可正常运行。Release 构建需要在 `PATH` 中找到 `go`。Crate 和 Go 模块的下载会使用 shell 中的代理环境变量。

## 测试与代码检查（Lint）

在仓库根目录下运行代码检查。必须激活 mise。

```bash
./scripts/test.sh          # 依次检查 rust、go，最后是 shell
./scripts/test.sh rust
./scripts/test.sh go
./scripts/test.sh shell
```

Rust 检查命令包括：

```bash
cargo fmt --check
cargo clippy --all-targets -- --deny warnings
cargo test
```

`cargo test` 会运行进程内测试并编译集成测试。被忽略的测试默认保持关闭，除非传入 `--ignored`。

格式化这三种语言的代码：

```bash
./scripts/format.sh
cargo fmt
```

`./scripts/test.sh` 的 Go 部分会在 `helpers/vibe-usernet` 目录下运行 `gofmt -l` 和 `go vet ./...`。如需运行 `go test`，请参考下文的 Go 章节。Shell 部分仅针对 `scripts/` 目录运行 `shfmt --indent 2 --diff` 和 `shellcheck`，并允许 ShellCheck `SC2164` 规则。

## `cargo test` 跳过的 Rust 测试

Virtualization.framework 运行在进程的主线程上。而 `cargo test` 的 worker 是后台线程。每个被忽略的 VM 测试都会通过 `VIBE_VM_CHECK` 启动 `vibe` 二进制文件。`main` 函数会在解析 CLI 参数之前运行该检查。检查列表详见[虚拟机启动原理](how-vm-boot.md#tests-that-boot-or-classify-disks)。

`cargo test -- --ignored` 仅运行被忽略的测试。`cargo test -- --include-ignored` 会同时运行两类测试。（不过请不要直接这么运行 ignored 的测试，请阅读下文了解详情）

### 真实磁盘镜像

有三个单元测试被默认忽略。未设置所需环境变量时，每个测试都会直接返回且不报错。设置环境变量后，再传入 `--ignored`。若输出包含跳过行（skip line），则表示测试并未读取磁盘。

| 环境变量 | 磁盘 |
| --- | --- |
| `VIBE_TEST_DEBIAN_NEW_RAW` | 当前的 nocloud raw。类型为 `NeedsPreparation`。Debian 13.7。`machine-id` 为 `uninitialized`。root 密码字段非空。 |
| `VIBE_TEST_DEBIAN_OLD_RAW` | 较旧的 nocloud raw。类型为 `LegacyCompatible`。Debian 13.3。空的 `machine-id`。空的 root 密码字段。 |
| `VIBE_TEST_ALPINE_RAW` | 固定的 Alpine tiny raw。 |
| `VIBE_TEST_DEBIAN_LOCKED_ROOT_RAW` | 可选。空的 `machine-id` 且 root 已锁定。仍为 `NeedsPreparation`。 |

默认镜像目录为 `$XDG_CACHE_HOME/vibe` 或 `~/.cache/vibe`。当前固定的版本为：

- `VIBE_TEST_DEBIAN_NEW_RAW` 对应 `debian-13-nocloud-arm64-20261001-2618.raw`
- `VIBE_TEST_DEBIAN_OLD_RAW` 对应 `debian-13-nocloud-arm64-20260112-2355.raw`
- `VIBE_TEST_ALPINE_RAW` 对应 `alpine-3.24.2-aarch64-tiny-r1.raw`

测试需要 `.raw` 文件。压缩包内的成员文件为 `disk.raw`。如果缺少 raw 文件，请解压本地压缩包。当 `shasum -a 512` 已经匹配时，无需重复下载压缩包。

```bash
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/vibe"
tar -xOf "$CACHE/debian-13-nocloud-arm64-20260112-2355.tar.xz" disk.raw > "$CACHE/debian-13-nocloud-arm64-20260112-2355.raw"

export VIBE_TEST_DEBIAN_NEW_RAW="$CACHE/debian-13-nocloud-arm64-20261001-2618.raw"
export VIBE_TEST_DEBIAN_OLD_RAW="$CACHE/debian-13-nocloud-arm64-20260112-2355.raw"
export VIBE_TEST_ALPINE_RAW="$CACHE/alpine-3.24.2-aarch64-tiny-r1.raw"

cargo test --lib real_disk_layouts -- --ignored --nocapture
cargo test --lib real_debian_root_matches_recorded_files -- --ignored --nocapture
cargo test --lib real_images_match_recorded_classes -- --ignored --nocapture
```

这些测试位于 `src/gpt.rs`、`src/ext4ro/tests.rs` 和 `src/image_probe.rs`。

`tests/fixtures/make-ext4.sh` 用于重新构建小型 gzip ext4 镜像。当缺少测试夹具（fixture）时，进程内测试会运行该脚本。该脚本会按顺序在 `MKFS_EXT4`、`/tmp/e2fs-install`、Homebrew 的 `e2fsprogs` 以及 `PATH` 中查找 `mkfs.ext4`。

### 被忽略的 VM 测试

如果未设置所需的环境变量，这些测试会直接 panic。请在 Mac 上运行它们。

| Cargo 测试 | 依赖项 |
| --- | --- |
| `--test vm_devices` `second_disk_is_read_only_in_the_guest` | `VIBE_TEST_DEBIAN_OLD_RAW` |
| `--test vm_devices` `headless_sleep_times_out_within_the_budget` | `VIBE_TEST_ALPINE_RAW` |
| `--test disk_prepare` `old_base_skips_alpine_and_new_base_is_processed` | 全部三个 `VIBE_TEST_*_RAW` 路径。 |
| `--test alpine_maintenance` `alpine_maintenance_prepares_and_rejects_disks` | 全部三个 raw 镜像。来自 `VIBE_E2FS_SBIN` 或 `PATH` 的 `e2fsprogs`。 |
| `--test vm_user` `vm_user_session_and_shares` 和 `root_password_login` | `VIBE_VM_CHECK_DISK`。可选的 `VIBE_VM_CHECK_ARGS`。 |

若存在同名的 `<alpine-raw>.tar.gz` 兄弟文件，`disk_prepare` 会将其复制。将 `VIBE_TEST_ALPINE_RAW` 指向 `alpine-3.24.2-aarch64-tiny-r1.raw`，以便兄弟文件为真实的压缩包。旧镜像相关测试会保留一个空缓存。

`alpine_maintenance` 总是设置 `VIBE_VM_CHECK=maintenance-all`。其 `download` 步骤会将固定的 Alpine 压缩包下载到临时缓存中。即使 `~/.cache/vibe` 中已存在该压缩包也是如此。如果不希望触发该下载，可以直接在二进制文件上运行单项检查：

```bash
VIBE_VM_CHECK=maintenance-prepare-new \
VIBE_TEST_ALPINE_RAW="$CACHE/alpine-3.24.2-aarch64-tiny-r1.raw" \
VIBE_TEST_DEBIAN_NEW_RAW="$CACHE/debian-13-nocloud-arm64-20261001-2618.raw" \
  ./target/release/vibe
```

`maintenance-<name>` 支持的值包括：`download`、`shadow`、`prepare-new`、`prepare-old`、`prepare-again`、`negative`、`needs-recovery`、`ssh`、`logs` 和 `all`。`VIBE_MAINT_TEST_PARTUUID` 用于强制产生目标不匹配。`VIBE_KEEP_FAILED_WORK=1` 用于保留失败的工作副本。

`VIBE_VM_CHECK_DISK` 应为一个已准备好的镜像，或没有 root 密码的 `default.raw`。检查程序会对其进行克隆。`VIBE_VM_CHECK_ARGS` 会作为额外的 CLI 参数附加到每个内部 `vibe` 调用中。由于配置过程会安装 `bindfs`，因此虚拟机内部需要配置代理：

```bash
export VIBE_VM_CHECK_DISK="$CACHE/default.raw"
export VIBE_VM_CHECK_ARGS="--proxy socks5://<COMPANY_PROXY_SERVER_IP>:3003 --proxy-udp --dns 1.1.1.1 --dns 8.8.8.8"
cargo test --test vm_user -- --ignored
```

将虚拟机流量路由至 `socks5://<COMPANY_PROXY_SERVER_IP>:3003`。需传入 `--proxy` 和 `--proxy-udp`。同时传入 `--dns 1.1.1.1`。宿主机的链路本地解析器在绕过列表中，因此虚拟机无法使用它。宿主机上的 `cargo`、`go` 和镜像下载所用的 `curl` 会读取 `https_proxy`、`HTTPS_PROXY`、`http_proxy` 和 `HTTP_PROXY`。具体规则见[网络与代理](network-proxy.md)。

```bash
cargo test --test vm_devices -- --ignored
cargo test --test disk_prepare -- --ignored
```

## 构建和测试 Go 辅助程序

NAT 功能使用了两个 Go 模块。两者均声明为 `go 1.26.0`。在构建任一模块前，请先初始化子模块。子模块的 rebase 是单独的操作，详见：[如何 rebase 到 gvisor-tap-vsock 最新代码](ai-tasks/how-to-rebase-onto-gvisor-tap-vsock-latest-code.md)。

### helpers/gvisor-tap-vsock

该目录是 fork 的 [gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock) 子模块，即用户态网络栈。其依赖已保存在 vendor 目录中。进入该目录操作：

```bash
cd helpers/gvisor-tap-vsock

# 运行此 fork 所修改包的单元测试。
go test -count=1 -timeout 180s -skip TestSuite \
  ./pkg/services/noproxy/ \
  ./pkg/services/netproxy/ \
  ./pkg/services/dns/ \
  ./pkg/services/forwarder/

# 上游 gvproxy CLI。vibe 实际嵌入的是 helpers/vibe-usernet。
go build -o bin/gvproxy ./cmd/gvproxy
```

`-skip TestSuite` 会跳过 `pkg/services/forwarder` 中的端口转发测试。这些测试会在宿主机地址上监听并建立连接。如需明确运行它们：

```bash
go test -count=1 -timeout 180s -run TestSuite ./pkg/services/forwarder/
```

`go test ./...` 还会编译 `test-qemu/` 和 `test-vfkit/`。这些测试套件需要虚拟机监控器（Hypervisor）和虚拟机镜像。`make test` 会构建 `gvproxy` 并运行所有包的测试。`./scripts/test-gvisor.sh` 会测试上述四个包（包括 `TestSuite`）并构建 `bin/gvproxy`。

DNS 测试套件会通过宿主机解析器查询 `redhat.com`。运行该测试时需要具备外网 DNS 访问能力。

### helpers/vibe-usernet

该模块是 `vibe` 嵌入的辅助程序。它没有 `vendor/` 目录。`go test` 和 `go build` 使用模块缓存。其 `replace` 指令直接引用子模块中的 `gvisor-tap-vsock`。

```bash
cd helpers/vibe-usernet
go test -count=1 ./...
go build -o bin/vibe-usernet .
```

上面生成的二进制文件是独立的辅助程序。在 Apple Silicon Mac 上，`cargo build` 会重新编译该包并将结果嵌入。

## 更新 Debian 和 Alpine 镜像

固定版本（Pin）是源代码中的常量。使用更新的缓存文件不会自动修改该常量。修改常量后需要进行验证。下载行为详见[虚拟机启动原理](how-vm-boot.md#debian-base)和 [Alpine 维护镜像](alpine-os-purpose.md)。

镜像目录为 `$XDG_CACHE_HOME/vibe` 或 `~/.cache/vibe`。可以通过 `VIBE_EXTRA_IMAGE_DIR` 和 `VIBE_PREFERRED_IMAGE_DIR` 添加目录。`vibe --help` 会打印目录查找顺序。

宿主机下载会使用 shell 代理变量。请勿使用第三方镜像源。虚拟机网络检查使用 `--proxy socks5://<COMPANY_PROXY_SERVER_IP>:3003 --proxy-udp`。

如果 `shasum -a 512` 与固定值一致，则保留缓存的压缩包。仅在摘要不匹配时才重新下载。

### Debian

固定常量位于 `src/images.rs` 中的 `DEBIAN_COMPRESSED_DISK_URL`、`DEBIAN_COMPRESSED_SHA` 和 `DEBIAN_COMPRESSED_SIZE_BYTES`。当前使用的文件为来自 <https://cdimage.debian.org/cdimage/cloud/trixie/> 的 `debian-13-nocloud-arm64-20261001-2618.tar.xz`。

1. 打开 <https://cdimage.debian.org/cdimage/cloud/trixie/>。选择最新的 `debian-13-nocloud-arm64-*.tar.xz`。忽略 genericcloud 和 qcow2 文件。
2. 查看该目录下的 `SHA512SUMS`。将对应的校验和复制到 `DEBIAN_COMPRESSED_SHA` 中。
3. 将 `DEBIAN_COMPRESSED_SIZE_BYTES` 设置为略小于压缩包大小的值。若文件大小达到该值但摘要错误，该文件将被保留在原处。
4. 若 `.raw` 文件已存在，`ensure_base_image` 会直接返回。全新解压前请删除该 `.raw` 文件。全新下载前请删除不完整的 `.tar.xz` 文件。
5. 保留 `debian-13-nocloud-arm64-20260112-2355.raw` 作为 `VIBE_TEST_DEBIAN_OLD_RAW`。将 `VIBE_TEST_DEBIAN_NEW_RAW` 指向新的 raw 文件。
6. 如果 `/etc/debian_version`、`machine-id` 或 root 密码字段发生变化，请更新 `real_debian_root_matches_recorded_files` 和 `real_images_match_recorded_classes`。更新 [how-vm-boot.md](how-vm-boot.md) 和 [how-vm-boot.zh-Hans.md](how-vm-boot.zh-Hans.md) 中的版本标记（stamp）。

使用成员名称 `disk.raw` 进行解压：

```bash
tar -xOf "$CACHE/debian-13-nocloud-arm64-STAMP.tar.xz" disk.raw \
  > "$CACHE/debian-13-nocloud-arm64-STAMP.raw"
shasum -a 512 "$CACHE/debian-13-nocloud-arm64-STAMP.tar.xz"
```

### Alpine

请遵循[替换固定版本](alpine-os-purpose.md#replacing-the-pin)的步骤。简要流程如下：

1. 从 <https://alpinelinux.org/cloud/> 下载最新的 aarch64 tiny `.raw.tar.gz`。忽略 `.qcow2`、`.vhd`、`cloudinit` 和 `metal`。
2. 对照发布的 `.sha512` 文件及镜像 yaml 验证 SHA-512。
3. 更新 `src/alpine.rs` 中的 `ALPINE_IMAGE_NAME`、`ALPINE_ARCHIVE_URL`、`ALPINE_ARCHIVE_SHA512`、`ALPINE_ROOT_FS_UUID` 和 `ALPINE_ROOT_PART`。
4. 更新该文档章节中提及的其他副本。如果某个 raw 镜像仍包含旧 UUID，它仍会被使用。
5. 从所有镜像目录中删除旧的 `<name>.raw` 和 `<name>.raw.tar.gz`。

只要 raw 文件和压缩包均有效，即可保持离线运行。

### 验证

1. 确保每个压缩包的 `shasum -a 512` 均与代码中的常量匹配。
2. 运行三个真实磁盘单元测试。它们必须断言成功。出现 skip line 意味着测试忽略了该磁盘。
3. 运行 `cargo test --test disk_prepare -- --ignored`。旧 Debian 会输出 `kind=AlreadyCompatible` 并保持缓存为空。新 Debian 会输出 `kind=Processed`。
4. 如上文所示运行 `VIBE_VM_CHECK=maintenance-prepare-new`。维护日志应显示 `PREP-RESULT` 成功。工作副本应包含空的 `machine-id` 和空的 root 密码字段。缓存的 Alpine raw 文件应保持其 SHA-256。完整规范请参阅 [alpine-os-purpose.md](alpine-os-purpose.md)。
5. 若第 4 步已足够，可跳过 `maintenance-all`。`maintenance-all` 会包含将 Alpine 下载到临时缓存的步骤。
6. 使用新名称配置虚拟机。在该启动成功之前，不要替换 `default.raw`。

```bash
./target/release/vibe provision --image dev-check \
  --proxy socks5://<COMPANY_PROXY_SERVER_IP>:3003 --proxy-udp \
  --dns 1.1.1.1 --dns 8.8.8.8 \
  --repl
```

在虚拟机内，读取 `/etc/debian_version` 和 `/etc/os-release`。确认 `/etc/machine-id` 为空。输入 `exit` 返回宿主机。检查完成后，从镜像目录中删除 `dev-check.raw`。

直接启动新的基础 raw 镜像会卡在首次启动向导中。`vibe provision` 会先运行 Alpine 辅助程序。详见[虚拟机启动原理](how-vm-boot.md)。
