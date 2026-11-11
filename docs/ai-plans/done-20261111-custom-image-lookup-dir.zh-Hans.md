# 自定义磁盘镜像查找目录（`VIBE_EXTRA_IMAGE_DIR` / `VIBE_PREFERRED_IMAGE_DIR`）

本文是基于 2026-11-11 工作区（`HEAD` 为 `21bc46d`）整理的实现计划，不是已完成功能的说明。本文只新增计划文档，不修改产品代码。

需求点已经和用户确认，结论记录在文末的“已确认的设计决定（Q&A）”一节。下文各任务中的 `Q1`–`Q9` 都指向那一节。

## 目标与范围

新增两个可选环境变量：

```bash
export VIBE_EXTRA_IMAGE_DIR=/Volumes/ExternalDisk/VMImages:/Volumes/ExternalDisk2/VMImages
export VIBE_PREFERRED_IMAGE_DIR=/Volumes/ExternalDisk2/VMImages
```

- `VIBE_EXTRA_IMAGE_DIR`：额外的镜像查找目录，`:` 分隔，可以有多个。
- `VIBE_PREFERRED_IMAGE_DIR`：首选目录，单个路径。未设置或只有空白字符时，首选目录就是默认目录。非空时必须等于默认目录或 `VIBE_EXTRA_IMAGE_DIR` 中的某一项，否则报错（Q1）。

下文中“默认目录”指现有的 `cache_dir`，即 `$XDG_CACHE_HOME/vibe`，未设置 `XDG_CACHE_HOME` 时为 `~/.cache/vibe`。要求中的 `~/.cache/vibe` 按这个含义理解，`XDG_CACHE_HOME` 的现有行为保持不变。

### 查找顺序

首选目录为默认目录时：

1. 默认目录
2. `VIBE_EXTRA_IMAGE_DIR` 中的目录，按书写顺序

首选目录为 `VIBE_EXTRA_IMAGE_DIR` 中的某一项时：

1. `VIBE_PREFERRED_IMAGE_DIR`
2. 默认目录
3. `VIBE_EXTRA_IMAGE_DIR` 中除首选目录外的其他目录，按书写顺序

所有目录都找不到，才算找不到。同名文件出现在多个目录时取第一个命中的（Q8）。

### 查找范围

只有“镜像名”走多目录查找（Q2）：

- `vibe --image NAME`（run）
- `vibe provision --image NAME`（目标镜像及 `--repl`）
- `vibe provision --base NAME`（`NAME` 不含 `/`）
- 隐式的 `default.raw`
- Debian 基础镜像 `.raw` 与 Alpine 维护镜像 `.raw`（只读复用，Q4）

`vibe path/to/disk.raw`（run 的位置参数）、含 `/` 的 `--base`、`.vibe/instance.raw` 的解析不变。

### 可见性要求

为了避免用户忘记自己配置过多个目录：

- run 和 provision 在开始时按顺序打印镜像查找目录，并标注首选目录、默认目录以及不存在而被跳过的目录（Q2、Q6）。`--version` 和 `--help` 不打印这一段（`--help` 自身已经列出查找顺序，Q7）。
- 按名称命中某个镜像（`--image`、`--base`、`default`、Debian/Alpine 基础镜像）时打印命中的完整路径，而不是只打印 `NAME`（Q2）。

打印格式示例（实现时措辞可以微调，但必须包含顺序、完整路径和标注）：

```text
Image directories (lookup order):
  1. /Volumes/ExternalDisk2/VMImages (preferred)
  2. /Users/me/.cache/vibe (default)
  3. /Volumes/ExternalDisk/VMImages (missing, skipped)
```

只有默认目录时同样打印（一行），保持输出稳定。这些行与现有的 `Creating instance disk from ...` 一样输出到 stdout。

### 写入位置

| 内容 | 写入位置 |
| --- | --- |
| Debian 基础镜像下载（`.tar.xz`）和解压（`.raw`） | 首选目录（各目录都没有可用 `.raw` 时） |
| Alpine 维护镜像下载和解压，`alpine.lock` | 首选目录（各目录都没有校验通过的 `.raw` 时） |
| 新建的命名镜像（`provision --image NAME`、隐式创建的 `default.raw`）及其临时文件 `<name>.tmp.<pid>` | 首选目录；已存在时在命中的目录原地替换（Q3） |
| 维护 VM 工作目录 `maint/<pid>-<nonce>/`（Alpine 克隆、seed ISO、EFI 变量、控制台日志临时文件） | 实际使用的 Alpine `.raw` 所在目录；无法创建时回退到首选目录（Q5、Q9） |
| 维护日志 `logs/` | 默认目录（Q5） |
| VirtioFS 映射源目录：`DEFAULT_CACHE_SHARES`（`apt`、`cargo`、`go` 等）、agent 配置（`codex`、`claude` 等）、`guest_node_modules/` | 默认目录（不变） |
| `vibe-usernet` 辅助程序 | 默认目录（不变） |
| 项目实例盘 `.vibe/instance.raw` | 项目目录（不变） |

### 不在本轮范围内

- 不改变 `.vibe/instance.raw`、含 `/` 的 `--base` 和位置参数 `disk.raw` 的解析（Q2）。
- 不迁移、移动或删除已有镜像文件。
- 不新增 CLI 参数，只通过环境变量配置。
- 不改变 run 路径“不探测、不修改磁盘”的约束（`AGENTS.md` 中的 Disk preparation 约定）。

## 当前代码事实与修改入口

行号只反映编写时的工作区，实现时以符号名为准。

| 入口 | 当前行为 | 对计划的影响 |
| --- | --- | --- |
| `src/main.rs` `run_main`（约 190–300 行） | 从 `HOME`/`XDG_CACHE_HOME` 算出唯一的 `cache_dir`，并据此算出 `base_compressed`、`base_raw`，`create_dir_all(&cache_dir)` | 新增镜像目录配置的解析点和启动时的目录打印；`cache_dir` 保留给映射目录、`vibe-usernet` 和日志 |
| `src/main.rs` `--help` 文本末尾的 `Cache directory:` | 只打印 `cache_dir` | 需要列出查找顺序和首选目录，并说明两个环境变量 |
| `scripts/update-usage-in-readme.sh` | 运行 `target/release/vibe --help`，截取到 `^Cache directory:` 之前的部分写入 `readme.md` 的 `## Using Vibe` 代码块 | 必须保证本机目录（默认目录、extra、首选）不会进入 README（Q7） |
| `src/main.rs` `CliCommand::Provision` 分支 | `target_image_raw = image_path(&cache_dir, &image)`；`--repl` 且已存在时直接启动；`--base NAME` 用 `image_path`；无 `--base` 时 `ensure_base_image` | 目标、`--repl`、`--base NAME`、Debian 基础镜像四处都要改 |
| `src/main.rs` `CliCommand::Run` 分支 | `template_raw = image_path(&cache_dir, &image)`；`default` 时 `ensure_default_image`，否则不存在即报错 | 模板查找；`default.raw` 不存在时在首选目录创建 |
| `src/images.rs` `image_path`、`ensure_base_image`、`ensure_instance_disk` | `image_path` 只拼路径；`ensure_base_image` 以 `.raw` 是否存在决定是否下载；`ensure_instance_disk` 已打印 `Creating instance disk from <path>...` | 新增按名称多目录查找的函数；Debian 基础镜像查找 |
| `src/provision.rs` `ensure_default_image`、`provision_image` | `provision_image(cache_dir, …)` 把 `cache_dir` 传给 `ensure_bootable`；临时文件放在 `image_raw` 的父目录；已打印 `Provisioning image '<name>' (path: …; tmp: …)` | 临时文件位置已跟随目标目录，不需要改；`cache_dir` 参数改为 `&ImageDirs` |
| `src/disk_prepare.rs` `ensure_bootable(work_copy, cache_dir)` | 用同一个 `cache_dir` 调用 `ensure_alpine_image` 和 `process_disk` | 改为接收 `&ImageDirs` |
| `src/alpine.rs` `ensure_alpine_image`、`ensure_alpine_image_at`、`ensure_locked` | 在单一目录内加锁、校验、必要时删除和重新下载 | 加入“其他目录只读复用”的前置查找（Q4），已有加锁流程只作用于首选目录 |
| `src/maintenance.rs` `process_disk`、`run_maintenance`、`publish_log` | `maint/` 和 `logs/` 都在 `cache_dir` 下；日志内容只有 hvc0 控制台输出 | 拆成两个目录参数（Q5）；发布日志时加上包含完整路径的头部（Q5） |
| `src/session.rs` `needs_provision_hint`、`DiskSource::CachedImage` | 提示文字提到 “the Vibe cache directory” | 改为“镜像目录（见 `vibe --help`）”的说法 |
| `src/vm_checks/maintenance.rs`、`src/vm_checks/prepare.rs` | 直接调用 `ensure_alpine_image(&cache)`、`process_disk(…, &cache)` | 签名变更后同步修改，行为保持“全部在同一目录” |
| `readme.md`、`docs/how-vm-boot*.md`、`docs/alpine-os-purpose*.md`、`docs/quick-start.md`、`AGENTS*.md` | 都描述“所有缓存文件在 `~/.cache/vibe`” | 文档同步 |

## 设计概要

新增模块 `src/image_dirs.rs`，集中处理两个环境变量。其余模块只拿到解析好的结果，不直接读环境变量。

```rust
pub(crate) struct ImageDirs {
    /// `$XDG_CACHE_HOME/vibe` or `~/.cache/vibe`. VirtioFS shares and logs stay here.
    default: PathBuf,
    /// Where downloads and new images go.
    preferred: PathBuf,
    /// Lookup order, `preferred` first, no duplicates.
    search: Vec<PathBuf>,
    /// Entries of `search` that did not exist when the config was built.
    missing: Vec<PathBuf>,
}

impl ImageDirs {
    /// Pure: no environment access, testable.
    pub(crate) fn from_values(
        default: PathBuf,
        extra: Option<&OsStr>,
        preferred: Option<&OsStr>,
    ) -> Result<Self, ImageDirsError>;
    pub(crate) fn from_env(default: PathBuf) -> Result<Self, ImageDirsError>;
    pub(crate) fn default_dir(&self) -> &Path;
    pub(crate) fn preferred(&self) -> &Path;
    pub(crate) fn search_order(&self) -> &[PathBuf];
    /// First existing `<dir>/<file_name>` in search order.
    pub(crate) fn find_file(&self, file_name: &str) -> Option<PathBuf>;
    /// The numbered "Image directories (lookup order):" block.
    pub(crate) fn describe(&self) -> String;
}
```

`from_values` 只做字符串和路径层面的规则（绝对路径、空段、去重、Q1 的报错）。需要访问文件系统的部分（`canonicalize`、首选目录是否存在、extra 不存在时的标记）放在 `from_env` 或单独的小函数里，这样单元测试可以覆盖纯规则，文件系统部分用临时目录测试。`describe` 同时供启动打印和 `--help` 使用。

`images.rs` 在此基础上加两个函数：

- `find_named_image(dirs, name) -> Option<PathBuf>`：校验名字后调用 `dirs.find_file("<name>.raw")`。
- `new_image_path(dirs, name) -> PathBuf`：`<preferred>/<name>.raw`。

现有的 `image_path(dir, name)` 保留，作为“在指定目录拼出镜像路径”的底层函数。

## 渐进式任务拆分

每个任务只改一小块，完成后都能 `cargo build` 并通过 `cargo test`。在没有设置两个环境变量时，除了 T03 新增的启动目录打印和 `--help` 文本，每一步完成后的行为都与当前版本一致。T01–T02 不改变任何行为；从 T05 开始每个任务只接入一个调用点。

### T01. 新增 `src/image_dirs.rs`：纯解析与查找顺序 ✅ 已完成

- 目标：实现 `ImageDirs::from_values`、`search_order`、`preferred`、`default_dir`，以及 `ImageDirsError`（`Display` 以 `[image-dirs]` 开头，指出变量名和出错的那一项）。
- 规则：按“查找顺序”一节和 Q1、Q6。
- 只在 `main.rs` 中加 `mod image_dirs;`，暂不调用。
- 测试（`#[cfg(test)] mod tests`，只测本任务新增的行为）：
    - 两个变量都未设置：查找顺序只有默认目录。
    - 只有 extra：默认目录在前，extra 按顺序在后。
    - 首选为 extra 中的某一项：首选、默认、其余 extra。
    - 首选不在 extra 中且不是默认目录：报错。
    - 首选为空白字符串：等同未设置。
    - 相对路径报错；空段忽略；重复项和尾部 `/` 去重。
- 依赖：无。

### T02. 文件系统相关校验与 `from_env` ✅ 已完成

- 目标：实现 `from_env`（读取两个环境变量，调用 `from_values`），首选目录存在性检查，`canonicalize` 比较，记录不存在的 extra 目录，`find_file`。
- 在 `run_main` 中 `--version` 处理之后、`--help` 处理之前构造 `image_dirs`（Q7），出错时通过现有的 `Error: …` 路径退出。暂时不使用它。
- 测试：用临时目录覆盖“首选目录不存在时报错”“extra 中有不存在的目录时 `find_file` 跳过它”“同名文件在多个目录时返回查找顺序中的第一个”“通过符号链接写出的同一目录被识别为同一项”。
- 依赖：T01。

### T03. 启动时打印查找目录 ✅ 已完成

- 目标：实现 `ImageDirs::describe`。run 与 provision（包括使用位置参数 `disk.raw` 的 run）在开始时打印一次这段文本；不存在的目录标注 `(missing, skipped)`，这就是 Q6 要求的“打印一次警告”，后续查找不再重复警告。
- 测试：一个用例检查 `describe` 的编号顺序和 `preferred`/`default`/`missing` 标注。
- 依赖：T02。

### T04. `--help` 输出查找顺序，并更新 `scripts/update-usage-in-readme.sh` ✅ 已完成

- 目标：
    - 帮助文本中加入两个环境变量的说明（风格参照现有的 `VIBE_KEEP_FAILED_WORK` 说明），这部分是静态文字，可以进入 README。
    - 帮助文本末尾的 `Cache directory:` 一节保留为本机相关信息的开头，在其后追加 `describe` 的输出（首选目录、默认目录和查找顺序）。所有本机路径都只出现在 `Cache directory:` 之后。
    - `scripts/update-usage-in-readme.sh`：
        - 以 `env -u VIBE_EXTRA_IMAGE_DIR -u VIBE_PREFERRED_IMAGE_DIR "$BIN" --help` 运行，避免维护者本机的配置影响输出或因配置错误导致脚本失败。
        - 继续在 `^Cache directory:` 处截断。在脚本里加一句注释说明这个约定：该标记之后是本机路径，不能写入 README。
        - 截取后检查结果中不包含 `$HOME`、默认目录路径和 `/Volumes/` 等本机路径，发现则 `throw`，防止以后有人把本机路径移到标记之前。
- 验证：运行脚本后 `git diff readme.md` 只包含新增的环境变量说明，没有本机路径。
- 依赖：T03。

### T05. `images.rs`：按名称查找的辅助函数 ✅ 已完成

- 目标：新增 `find_named_image` 和 `new_image_path`，不修改调用方。
- 测试：只需一个用例验证非法名字仍然触发 `assert_valid_image_name`；查找顺序本身已在 T02 测过，不重复。
- 依赖：T02。

### T06. 接入 `provision --base NAME` ✅ 已完成

- 目标：`CliCommand::Provision` 中 `Some(base)`（不含 `/`）分支改用 `find_named_image`。命中时打印 `Using base image <完整路径>`。找不到时报错信息列出已查找的目录，例如 `Base image 'foo' not found in: <dir1>, <dir2>`。
- 含 `/` 的 `--base` 不变（Q2）。
- 依赖：T05。

### T07. 接入 run 的 `--image NAME`（非 `default`） ✅ 已完成

- 目标：`CliCommand::Run` 中 `image != DEFAULT_IMAGE_NAME` 时用 `find_named_image` 找模板；找不到时报错信息列出已查找的目录（替换现有的 `Template image does not exist: …`）。命中路径由现有的 `Creating instance disk from <path>...` 打印，不新增重复输出。
- `.vibe/instance.raw` 已存在时的逻辑不变（仍然忽略 `--image` 并打印提示，提示中已有完整路径）。
- 同名镜像在多个目录时不额外提示（Q8）。
- 依赖：T05。

### T08. 接入 run 的默认镜像 `default.raw` ✅ 已完成

- 目标：`image == DEFAULT_IMAGE_NAME` 时先 `find_named_image("default")`；找到就直接作为模板（`ensure_default_image` 的“已存在”分支）。找不到时把 `new_image_path("default")`（首选目录）作为 `default_raw` 传给 `ensure_default_image`。
- 本任务暂不改 Debian 基础镜像和 Alpine 的位置（仍在默认目录），只改 `default.raw` 本身的位置。临时文件 `default.tmp.<pid>` 由 `provision_image` 自动放在 `default_raw` 的父目录，即首选目录。
- 现有输出 `The default image <path> has been initialized` 和 `Creating instance disk from <path>...` 已包含完整路径。
- 依赖：T05。

### T09. 接入 `provision --image NAME` 的目标与 `--repl` ✅ 已完成

- 目标（Q3）：
    - `--repl` 且没有 `--replace`：用 `find_named_image` 查找已存在的镜像并直接启动，启动前打印命中的完整路径。
    - 目标存在性检查和写入位置：先 `find_named_image`，命中则以命中路径作为 `image_raw`（无 `--replace` 时由 `provision_image` 现有逻辑报错并打印完整路径；有 `--replace` 时临时文件放在同一目录并原地 `rename`）；未命中则用 `new_image_path`。
- `provision_image` 本身不需要改：它已经以 `image_raw` 的父目录放临时文件，并打印 `path` 和 `tmp` 的完整路径。
- 依赖：T05。

### T10. Debian 基础镜像的查找与下载位置 ✅ 已完成

- 目标（Q4）：
    - `run_main` 中 `base_compressed` / `base_raw` 不再固定在 `cache_dir`。在 `images.rs` 新增 `locate_debian_base(dirs) -> (raw, compressed)`：
        1. 在查找顺序中找已有的 Debian `.raw`，命中即返回该路径（只读使用）。
        2. 否则返回首选目录下的 `.raw` 和 `.tar.xz` 路径，交给现有的 `ensure_base_image` 下载和解压。
    - 不复用非首选目录中的 `.tar.xz`，也不在非首选目录解压。
    - 命中已有 `.raw` 时打印 `Using base image <完整路径>`。
- `ensure_base_image` 本身不变。
- 测试：临时目录中只放 `.raw` 文件名（空文件即可），验证返回的是命中目录的路径；都没有时返回首选目录路径。不触发下载。
- 依赖：T02。

### T11. Alpine 维护镜像的查找与下载位置 ✅ 已完成

- 目标（Q4）：
    - 新增 `ensure_alpine_image_in(dirs)`（或给 `ensure_alpine_image` 改签名）：按查找顺序检查非首选目录中是否有 UUID 校验通过的 `.raw`（只读：不加锁、不删除、不重新解压）。命中即返回。校验不通过时打印警告（包含完整路径和原因），跳过该目录，继续查找。都没有命中时对首选目录调用现有的 `ensure_alpine_image_at`（加锁、校验、下载、解压）。
    - 首选目录排在查找顺序第一位时，它仍走现有的加锁流程，不受只读规则影响。首选目录为默认目录且没有 extra 时行为不变。
    - 打印实际使用的 Alpine `.raw` 完整路径。
- 测试：复用 `alpine.rs` 现有测试中构造合成 raw 的辅助函数，新增两个用例：
    - 非首选目录有合法 raw、首选目录为空、URL 指向不可达地址：返回非首选目录中的 raw，首选目录中没有创建文件。
    - 非首选目录中的 raw 校验不通过：文件没有被删除，函数继续走首选目录流程。
- 依赖：T02。

### T12. 维护 VM 工作目录与日志目录分离 ✅ 已完成

- 目标（Q5、Q9）：
    - `process_disk(disk, alpine_raw, cache_dir)` 改为 `process_disk(disk, alpine_raw, log_dir)`，`maint/` 的根目录由 `alpine_raw` 的父目录决定：`<alpine_raw 所在目录>/maint/`。在该目录创建 `maint/` 或私有子目录失败（例如只读卷）时，回退到首选目录下的 `maint/`，并打印一行说明。为便于回退，可以把首选目录作为额外参数传入，或传入一个包含 `fallback_work_root` 与 `log_dir` 的小结构。
    - `cache::sweep_stale` 只清理本次实际使用的 `maint/` 根目录。
    - `publish_log` 写到 `log_dir/logs/`（默认目录）。发布时在日志开头加一段由主机写入的头部，列出完整路径：被处理的磁盘、Alpine 源 `.raw`、Alpine 克隆、seed ISO、`maint` 工作目录（注明成功时会被删除，`VIBE_KEEP_FAILED_WORK=1` 时保留）。头部不包含 shadow 内容或环境变量值。
    - `ensure_bootable(work_copy, cache_dir)` 改为接收 `&ImageDirs`：Alpine 用 T11 的函数，`log_dir` 为默认目录。
    - `provision_image` 和 `ensure_default_image` 的 `cache_dir` 参数改为 `&ImageDirs`，原样传给 `ensure_bootable`。
    - `src/vm_checks/maintenance.rs`、`src/vm_checks/prepare.rs` 同步改签名。它们的 Alpine `.raw` 与日志都在同一个缓存目录，行为不变。
    - `PrepareError` 中 `kept_dir` 打印的是实际工作目录的完整路径（现有行为，路径换了位置后仍成立）。
- 测试：
    - 一个用例覆盖日志头部：用临时文件调用发布函数，检查头部包含传入的各个完整路径，后面紧跟原始日志内容。
    - 现有 `publish_log` / 错误展示测试如需调整，只更新参数，不新增重复用例。
- 依赖：T11。

### T13. 提示与错误信息措辞 ✅ 已完成

- 目标：
    - `session.rs` `needs_provision_hint` 中的 “boot <name>.raw from the Vibe cache directory” 改为指向镜像目录（例如 “from one of the Vibe image directories (see `vibe --help`)”）。相应的现有测试断言同步修改。
    - 检查 T06–T12 中新增的输出与错误信息格式与现有风格一致（无 emoji，路径完整）。
- 依赖：T06–T12。

### T14. 文档同步 ✅ 已完成

- `readme.md`：说明两个环境变量、查找顺序、启动时的目录打印、写入位置，以及映射目录和日志仍在默认目录。修改 “copy them into `~/.cache/vibe/`” 一句，说明也可以放进任何 extra 目录。`## Using Vibe` 中的帮助文本只通过 `scripts/update-usage-in-readme.sh` 更新。
- `docs/how-vm-boot.md` 与 `docs/how-vm-boot.zh-Hans.md`：`Disk files` 一节说明哪些文件在首选目录、哪些在默认目录；`maint/` 跟随 Alpine 镜像所在目录；日志位置与头部内容。
- `docs/alpine-os-purpose.md` 与 `.zh-Hans.md`：Alpine 镜像位置、非首选目录只读复用、校验不通过时的警告。
- `docs/quick-start.md`：如有涉及缓存目录的段落，补充一句外置盘用法示例。
- `AGENTS.md` 与 `AGENTS.zh-Hans.md`：Key Files 中新增 `src/image_dirs.rs` 的说明，更新 `images.rs`、`alpine.rs`、`maintenance.rs`、`scripts/update-usage-in-readme.sh` 相关描述，Test 段落补充新增测试所在模块。
- 依赖：T04–T13 全部完成后进行，避免文档与代码不一致。

### T15. 验证 ✅ 已完成

自动化（每个任务完成后都运行）：

```bash
cargo build
cargo test
```

手工验证（ARM Mac，复用 `~/.cache/vibe` 中已有的 Debian 与 Alpine 镜像，不重新下载）：

1. 两个变量都未设置：`vibe --help` 输出的查找顺序只有默认目录；启动时只打印默认目录一行；在一个已有 `.vibe/instance.raw` 的项目中启动，其余行为与修改前一致。
2. 用两个临时目录（例如 `/tmp/vibe-img-a`、`/tmp/vibe-img-b`）模拟外置盘：
    - `VIBE_EXTRA_IMAGE_DIR=/tmp/vibe-img-a:/tmp/vibe-img-b`，`VIBE_PREFERRED_IMAGE_DIR=/tmp/vibe-img-b`：`vibe --help` 与启动输出的顺序都为 `b`、默认、`a`。
    - `VIBE_PREFERRED_IMAGE_DIR=/tmp/not-listed`：`vibe --help` 和 `vibe` 都报错退出；`vibe --version` 正常。
    - `vibe provision --base default --image t1`：打印 `default.raw` 的完整路径；`t1.raw` 与 `t1.tmp.<pid>` 出现在 `/tmp/vibe-img-b`；Debian/Alpine 镜像从默认目录复用，没有下载；运行中 `maint/` 出现在默认目录（Alpine 所在目录）。
    - 把 `t1.raw` 移到 `/tmp/vibe-img-a`，`vibe --image t1` 能找到并打印 `/tmp/vibe-img-a/t1.raw`。
    - `vibe provision --image t1`（无 `--replace`）报错并打印 `/tmp/vibe-img-a/t1.raw`；带 `--replace` 时临时文件和结果都在 `/tmp/vibe-img-a`。
    - 删除 `/tmp/vibe-img-a` 后再运行，启动输出标注 `(missing, skipped)`，不报错。
3. 映射目录：启动后在 VM 中检查 `apt`、`cargo` 等共享目录的数据仍写入默认目录。
4. 维护流程：在首选目录为 `/tmp/vibe-img-b` 时用新的 Debian 基础镜像跑一次 provision，确认日志写入默认目录的 `logs/`，且日志头部列出磁盘、Alpine 源、Alpine 克隆、seed ISO 和 `maint` 工作目录的完整路径。
5. `scripts/update-usage-in-readme.sh`：在设置了两个变量的 shell 中运行，`git diff readme.md` 中没有本机路径。

## 任务依赖概览

```text
T01 -> T02 -> T03 -> T04
          |-> T05 -> T06
          |       |-> T07
          |       |-> T08
          |       \-> T09
          |-> T10
          \-> T11 -> T12
T06..T12 -> T13
T04..T13 -> T14 -> T15
```

## 已确认的设计决定（Q&A）

### Q1. 首选目录不在列表中怎么办？

原需求中“不是 `VIBE_PREFERRED_IMAGE_DIR` 里面定义的目录则报错”是笔误。规则为：非空的 `VIBE_PREFERRED_IMAGE_DIR` 必须等于默认目录或 `VIBE_EXTRA_IMAGE_DIR` 中的某一项，否则报错。未设置、空字符串或只有空白字符时视为使用默认目录。

### Q2. 哪些输入走多目录查找？启动时要打印什么？

现有的四种指定磁盘的方式：

| 输入 | 当前解析 | 本轮之后 |
| --- | --- | --- |
| `vibe --image NAME`（run） | `image_path(&cache_dir, NAME)` | 多目录查找 |
| `vibe provision --base NAME`（不含 `/`） | `image_path(&cache_dir, NAME)` | 多目录查找 |
| `vibe provision --base path/to/x.raw`（含 `/`） | 原样作为路径（相对路径相对于 cwd） | 不变 |
| `vibe path/to/disk.raw`（run 的位置参数） | 原样作为路径 | 不变 |

`NAME` 只允许字母、数字、`-`、`_`（`assert_valid_image_name`），映射为 `<NAME>.raw`。

另外两个要求：run 与 provision 开始时按顺序打印镜像查找目录；按名称命中镜像时打印完整路径而不是 `NAME`。

### Q3. `provision --image NAME` 的目标已存在于非首选目录时怎么办？

存在性检查覆盖所有查找目录，取第一个命中。没有 `--replace` 时报错并打印命中的完整路径。有 `--replace` 时在命中的目录原地替换，临时文件也放在那个目录，保证 `rename` 在同一文件系统内完成。所有目录都不存在时，新镜像和临时文件写入首选目录。

### Q4. Debian 与 Alpine 镜像是否在所有目录中查找？

是。按查找顺序找已经可用的文件，找到就只读使用；都没有时才在首选目录下载和解压。

- 非首选目录中的镜像文件只读：不加锁、不删除、不重新解压。
- Debian：只认已存在的 `.raw`，不在非首选目录解压 `.tar.xz`。
- Alpine：只认“`.raw` 存在且分区 UUID 校验通过”。校验不通过时跳过该目录、不删除，并打印警告提示用户。最后回到首选目录走现有的加锁下载流程。

### Q5. `maint/` 与 `logs/` 放在哪里？

- `maint/` 是初始化镜像时的临时文件，放在实际使用的 Alpine 镜像文件所在目录（通常是首选目录，复用时可能是默认目录或某个 extra 目录）。这样 Alpine 的 `clonefile` 在同一文件系统内完成。
- `logs/` 留在默认目录，便于用户总在同一个位置找日志。错误信息中仍打印日志完整路径。
- 因为 `maint/` 可能在不同目录，日志里涉及的文件路径都要写完整：被处理的磁盘、Alpine 源 `.raw`、Alpine 克隆、seed ISO 和工作目录。

### Q6. 环境变量中的路径格式与不存在的目录如何处理？

- 每个路径必须是绝对路径，否则报错并指出是哪个变量的哪一项。不做 `~` 展开（交给 shell）。
- 去掉尾部 `/` 后按字面比较；目录存在时再用 `canonicalize` 比较，用于判断“首选目录是否在 extra 中”和去重。
- `VIBE_EXTRA_IMAGE_DIR` 的空段忽略；重复项和等于默认目录的项去重，保留第一次出现的位置。
- extra 中不存在的目录在查找时跳过，并打印一次警告（即启动目录列表中的 `(missing, skipped)` 标注）。
- 非空的首选目录必须已经存在且是目录，否则报错，不自动创建，避免外置盘未挂载时在系统盘的 `/Volumes/...` 下误建目录。默认目录仍按现有逻辑自动创建。

### Q7. 环境变量在哪些命令中校验？`--help` 与 README 如何处理？

`--version` 不读取这两个变量。其余命令（包括 `--help`）在 `run_main` 开头统一解析和校验，配置错误立即报错退出。`--help` 列出最终查找顺序和首选目录。`scripts/update-usage-in-readme.sh` 必须保证这些本机路径不会写入 README（见 T04）。

### Q8. 同名镜像在多个目录中出现时是否提示？

不额外提示，取查找顺序中第一个命中的。启动时的目录列表和命中时打印的完整路径已经足够让用户知道用的是哪个文件。

### Q9. `maint/` 放在 Alpine 所在目录，与 Q4 的“非首选目录只读”是否冲突？

实现时按以下理解处理，二者不冲突：

- Q4 的“只读”针对镜像文件本身：Alpine `.raw` 不加锁、不删除、不重新解压，只作为 `clonefile` 的源。
- 在同一目录下创建 `maint/<pid>-<nonce>/` 工作目录是 Q5 明确要求的，允许。`sweep_stale` 只清理 `maint/` 下属于过期进程的子目录，不触及镜像文件。
- 如果该目录不可写（例如只读卷），回退到首选目录下的 `maint/`（`clone_file` 已有跨文件系统时的复制回退），并打印一行说明实际使用的工作目录。

## 验收记录（Completed）

全部任务（T01–T15）已完成 ✅。

自动化：

- `cargo build --release` 通过；`cargo fmt --check` 无差异。
- `cargo test`：116 通过，3 失败，3 忽略。失败的 `ext4ro::tests::fuzz_rewrites_do_not_panic_or_hang`、`ext4ro::tests::mkfs_fixtures_match_old_and_new_features`、`image_probe::tests::verify_processed_accepts_only_a_clean_result` 在改动前就失败：`tests/fixtures/*.img.gz` 被 gitignore 且本机没有，也没有安装 e2fsprogs，与本次改动无关。
- `cargo clippy --all-targets -- --deny warnings` 只报两处既有代码的新版工具链 lint（`provision.rs` 中的 `useless_borrows_in_formatting` 与 `build_provision_script` 中的 `collapsible_if`），本次没有修改这两处。

手工验证（ARM Mac）：

1. 未设置变量：`--help` 末尾只有 `1. ~/.cache/vibe (preferred, default)`。
2. `VIBE_EXTRA_IMAGE_DIR=/tmp/vibe-img-a:/tmp/vibe-img-b`、`VIBE_PREFERRED_IMAGE_DIR=/tmp/vibe-img-b`：`--help` 与启动输出顺序为 b、默认、a。`VIBE_PREFERRED_IMAGE_DIR=/tmp/not-listed` 时 `--help` 报 `[image-dirs]` 错误，`--version` 正常；相对路径同样报错。
3. 默认目录中没有 `default.raw`，所以用 `vibe provision --image t1`（不带 `--base`）代替 `--base default`：打印并复用默认目录中的 Debian raw 与 Alpine raw，没有下载；`t1.tmp.<pid>` 和最终的 `t1.raw` 出现在 `/tmp/vibe-img-b`；`maint/<pid>-<nonce>/` 出现在默认目录（Alpine 所在目录），结束后被删除。默认的 `nat` 网络下 guest 的 apt 无法解析 DNS（本机网络环境限制），改用 `--network vznat --proxy ...` 后 provision 成功。
4. 把 `t1.raw` 移到 `/tmp/vibe-img-a` 后，`vibe --image t1` 打印 `Creating instance disk from /tmp/vibe-img-a/t1.raw...` 并启动成功；`vibe provision --image t1` 报错 `Image 't1' already exists at /tmp/vibe-img-a/t1.raw`；带 `--replace` 时 `t1.tmp.<pid>` 与锁文件都在 `/tmp/vibe-img-a`。
5. 不存在的 extra 目录在列表中标注 `(missing, skipped)`，不报错。
6. provision 期间 `apt/` 等共享目录的数据写入默认目录。
7. 维护日志写入默认目录的 `logs/`，头部列出磁盘、Alpine 源、Alpine 克隆、seed ISO 与工作目录的完整路径。
8. 在设置了两个变量的 shell 中运行 `scripts/update-usage-in-readme.sh`，`readme.md` 只新增静态的 `Image directories:` 帮助段落，没有本机路径；本机路径混入时脚本报错且不修改 README。

文档：`readme.md`、`docs/how-vm-boot*.md`、`docs/alpine-os-purpose*.md`、`AGENTS*.md` 已更新。`docs/quick-start.md` 当前为空文件，没有涉及缓存目录的段落，无需修改。

镜像下载：没有。全部复用 `~/.cache/vibe` 中已有的 Debian 与 Alpine 镜像。

## 验收后的清理（Cleanup after Acceptance）

无需清理。新增的单元测试（`src/image_dirs.rs`、`src/images.rs`、`src/alpine.rs`、`src/maintenance.rs`、`src/session.rs` 中的用例）覆盖的都是长期行为，不是阶段性验证。手工验证用的临时目录（`/tmp/vibe-img-*`、临时项目目录、日志）已删除。
