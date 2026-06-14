# doris-cli (`dcli`)

面向 Apache Doris 全场景运维的 Rust 命令行工具：**部署 · 扩缩容 · 日常运维 · 故障处理**。

针对单副本场景下宕机导致的副本/版本缺失、BE 扩缩容、集群巡检等运维痛点，提供一套统一、可脚本化的 CLI。

## 设计

```
dcli
├── config    配置文件管理（init / show）
├── profile   多集群 profile 切换（list / current / add / use / remove / show）
├── cluster   集群状态（status / frontends / backends）
├── ops       日常运维（health / tablets / repair / version-gaps / pad-rowset / decommission-status / balance）
├── scale     扩缩容（add-be / decommission-be / drop-be / add-fe / drop-fe）
└── deploy    SSH 自动化部署（init / detect / precheck / install / start / stop / status / bootstrap）
```

底层通过 **MySQL 协议（9030）** 连接 FE 执行 `SHOW`/`ADMIN`/`ALTER SYSTEM` 等语句；
对 BE 版本缺口修复使用 **HTTP（8040）** 调用 `compaction/show` 与 `pad_rowset`。

## 构建

```bash
cargo build --release
# 产物：target/release/dcli
```

## 配置

```bash
dcli config init          # 在 ~/.doris-cli/cluster.yaml 生成示例配置
dcli config show          # 查看解析后的配置
```

也可临时通过参数覆盖（只读命令免配置文件）：

```bash
dcli --fe-host 10.0.0.1 --user root --password '' cluster status
```

环境变量：
- `DORIS_CLI_CONFIG`：指定配置文件路径
- `DORIS_CLI_LOG`：日志级别（如 `info`、`debug`）

## 多集群 profile 切换

管理多套集群时，可为每套集群保存一个 profile（位于 `~/.doris-cli/profiles/<name>.yaml`），
随时切换当前生效的集群，无需反复传 `--config`。

```bash
dcli profile add prod                    # 新建 profile（写入示例配置，再编辑）
dcli profile add staging --from-current  # 以当前配置为模板新建
dcli profile list                        # 列出全部 profile，* 标记当前生效
dcli profile use prod                    # 切换当前生效的 profile
dcli profile current                     # 查看当前生效的 profile
dcli profile show staging                # 查看某个 profile 的解析后配置
dcli profile remove staging              # 删除 profile
```

配置解析优先级：`--config <路径>` > `--profile <名称>` > `DORIS_CLI_CONFIG` >
`profile use` 设置的当前 profile > 默认 `~/.doris-cli/cluster.yaml`。

```bash
# 临时对某个 profile 执行命令，而不切换当前生效的 profile
dcli --profile staging cluster status
```

## 常用场景

### 集群巡检
```bash
dcli cluster status                 # FE/BE 存活与扩缩容概览
dcli cluster backends -f json       # 以 JSON 输出，便于脚本处理
```

### 副本/版本缺失修复（核心痛点）
```bash
dcli ops health                                   # 全集群 tablet 健康度，自动高亮异常库
dcli ops tablets --db mydb --table mytbl          # 查看异常副本（STATUS != OK）
dcli ops repair --db mydb --table mytbl           # 触发高优先级修复，补齐副本/版本
dcli ops repair --db mydb --table mytbl --partitions p1,p2
dcli ops cancel-repair --db mydb --table mytbl

# 单副本宕机后版本链断裂：BE compaction/show 诊断 + pad_rowset 补空 rowset（数据永久丢失）
dcli ops version-gaps --db mydb --table mytbl     # 只读：列出 missing_rowsets
dcli ops pad-rowset --db mydb --table mytbl       # 交互确认后调用 BE /api/pad_rowset
dcli ops pad-rowset --tablet-id 123 --backend-id 10001 --dry-run
```

### 扩容 / 缩容
```bash
dcli scale add-be --hosts 10.0.0.11,10.0.0.12         # 新增 BE
dcli scale decommission-be --hosts 10.0.0.11          # 安全缩容（先迁移数据）
dcli ops decommission-status                          # 跟踪缩容进度（TabletNum 归零即完成）
dcli scale cancel-decommission --hosts 10.0.0.11      # 取消缩容
dcli scale drop-be --hosts 10.0.0.11                  # 强制下线（不迁移数据，危险）

dcli scale add-fe --host 10.0.0.2 --role follower     # 新增 FE
dcli scale drop-fe --host 10.0.0.2 --role observer
```

### 维护期暂停均衡
```bash
dcli ops balance disable      # 扩缩容/维护期间关闭 tablet 均衡
dcli ops balance enable
```

### SSH 自动化部署
全程通过系统 `ssh`/`scp` 远程执行（复用本机 SSH 密钥/agent，无需额外依赖）。

```bash
# 1. 交互式录入拓扑：输入 FE/BE 的 IP、选择哪台 FE 是 leader，自动写入配置
dcli deploy init

# 2. 自动探测每台机器配置（CPU/内存/磁盘/JDK/内核参数）
dcli deploy detect

# 3. 对照 Doris 要求做体检（max_map_count / swappiness / ulimit / JDK 等）
dcli deploy precheck

# 4. 分发安装包 → 解压 → 渲染 fe.conf/be.conf（端口、priority_networks、meta_dir 等）
dcli deploy install

# 5. 按序启动：leader FE → follower/observer FE（--helper）→ BE，并自动 ADD BACKEND
dcli deploy start

# 一键完成 precheck → install → start
dcli deploy bootstrap

# 运行态查看 / 停止
dcli deploy status
dcli deploy stop
```

部署所需的 `topology`（FE/BE 列表与 leader）、`deploy`（install_dir/package/java_home/
priority_networks 等）、`ssh` 配置均由 `deploy init` 生成，也可手动编辑 `cluster.yaml`。

> 自动检测会校验 Doris 关键要求：JDK 是否就绪、`vm.max_map_count ≥ 2000000`（不足判 FAIL）、
> `vm.swappiness`、`ulimit -n`、CPU/内存/磁盘容量等，并给出具体修复命令。

## 安全约定

- `decommission-be`（安全缩容，先迁移数据）优先于 `drop-be`（强制下线，丢副本）。
- 危险操作默认需要确认；可加 `-y/--yes` 跳过（用于自动化）。
- `drop-be` 底层对应 Doris 的 `DROPP BACKEND`（官方故意拼写以防误删）。

## 路线图

- [x] deploy：基于 SSH 的 FE/BE 安装、配置渲染、启停、机器体检
- [x] ops：基于 BE HTTP 的 version-gaps / pad-rowset（单副本版本链修复）
- [ ] ops：更多 BE/FE HTTP 巡检（compaction 状态、tablet 元数据）
- [ ] deploy：滚动升级、配置变更下发、扩容时自动 install+start+register
- [x] 多集群 profile 切换
