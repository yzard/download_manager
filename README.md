# Download Manager

Download Manager 用 Rust 和 JavaScript 显示文件的下载进度、速度、队列和来源。当前支持百度网盘，使用 [BaiduPCS-Go v4.0.2](https://github.com/qjfoidnh/BaiduPCS-Go/releases/tag/v4.0.2) 下载。迅雷将在后续版本接入。

镜像将 `HOME` 和 `BAIDUPCS_GO_CONFIG_DIR` 都设为 `/config`。NAS 上的 `/data/docker/configs/baidupcs-go` 保存网页设置和百度登录状态。独立运行示例 `docker-compose.yaml` 使用 `.env`，可从 `.env.example` 复制。

## 在 NAS 上部署

发布工作流将 AMD64 和 ARM64 镜像推送到 `ghcr.io/yzard/download-manager:4.0.2`。从旧服务迁移时，先用 `docker compose -f /data/docker/4_torrent.yaml stop baidupcs-go` 停止旧容器。然后在同一个 Compose 文件中将服务改名为 `download-manager`，并更新镜像地址。沿用原来的配置和下载挂载，可以保留登录状态及续传文件。

```sh
ssh nas
sudo install -d -o 1002 -g 1003 -m 700 /data/docker/configs/baidupcs-go
sudo install -d -o 1002 -g 1003 -m 755 /data/downloads/baidu
docker compose -f /data/docker/4_torrent.yaml pull download-manager
```

将 `/data/docker/4_torrent.yaml` 的权限设为 `600`，将 `/data/docker/configs/baidupcs-go` 的权限设为 `700`，因为它们保存登录凭据。容器读取 `lsio.env` 中的 `PUID`、`PGID`、`UMASK` 和 `TZ`。这组 UID 和 GID 必须对配置目录及 `/downloads` 对应的宿主机目录有写入权限。

服务通过 `docker compose up -d` 常驻，Docker 根据 `restart: always` 重启服务。单独使用本仓库时，可以改用根目录的 `docker-compose.yaml` 示例。

在 NAS 的 `download-manager` 服务里添加本机端口映射。

```yaml
ports:
  - "127.0.0.1:8085:8080"
```

在自己的电脑上运行 `ssh -N -L 8085:127.0.0.1:8085 nas`，然后用浏览器打开 `http://127.0.0.1:8085`。如果 Docker 在当前电脑运行，直接打开该地址即可。设置页面通过本机访问或 SSH 隧道使用。

## 创建允许登录的账号

在 NAS 上运行下面的命令。将 `operator` 换成你要使用的用户名。终端会提示输入密码两次，输入时不回显。密码至少需要 12 个字符。

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps -it download-manager init-user operator
docker compose -f /data/docker/4_torrent.yaml up -d --no-deps download-manager
```

这条命令只允许初始化一个账号。打开网页后用该账号登录。登录后可在齿轮设置里修改自己的密码。改密会结束现有会话，容器重启后也需要重新登录。

## 设置百度网盘

先在电脑浏览器登录 [百度网盘](https://pan.baidu.com/)。在 Chrome 或 Edge 中按 `F12`，打开“应用程序”或“Application”标签。在左侧展开“Cookie”，选择 `https://pan.baidu.com`。分别找到 `BDUSS` 和 `STOKEN`，复制它们的“值”或“Value”。`STOKEN` 不是 `bdstoken`，两个值都不能公开分享。[BaiduPCS-Go 登录说明](https://github.com/qjfoidnh/BaiduPCS-Go#登录百度帐号)

登录后，如果缺少 Token，页面会打开百度网盘设置。输入 `BDUSS`、`STOKEN` 和网盘目录，例如 `/NAS下载`，然后点击“保存”。之后可通过右上角齿轮更新设置。已有 Token 时，留空会保留原值。

网页保存到 `/config/config.toml`，权限为 `600`。已有 NAS 部署在首次保存前仍可读取 Compose 中的 `BAIDU_BDUSS`、`BAIDU_STOKEN` 和 `MONITOR_DIRECTORY`。保存后以配置文件为准。如果有一轮下载正在运行，新设置会在该轮结束后应用。

你没有会员，因此只需再运行一次下面的命令，将两个下载并发设置为 1。程序会把设置写进 `/config/pcs_config.json`，以后不必重复设置。[非会员参数说明](https://github.com/qjfoidnh/BaiduPCS-Go#显示和修改程序配置项)

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps download-manager config set -max_parallel 1 -max_download_load 1
```

## 自动下载和断点续传

在百度网盘网页版创建设置中指定的目录，再把要下载到 NAS 的内容放进去。

用以下命令启动服务并查看日志。

```sh
docker compose -f /data/docker/4_torrent.yaml up -d --no-deps download-manager
docker compose -f /data/docker/4_torrent.yaml logs -f download-manager
```

容器启动后立即检查一次。每轮下载结束后，容器等待 `CHECK_INTERVAL_SECONDS` 秒再检查，默认 300 秒。下载轮次不会重叠。容器的 `/downloads` 挂载到 NAS 的 `/data/downloads/baidu`。监控 `/NAS下载` 时，文件保存在 `/data/downloads/baidu/NAS下载`。已经下载的同名文件会跳过，下载中断后下次运行会尝试续传未完成的文件。相同文件名但内容已变更时，请手动处理旧文件。下载速度仍受百度网盘非会员限制。[下载命令与续传说明](https://github.com/qjfoidnh/BaiduPCS-Go#下载文件目录)

如果百度网盘显示登录失败，用齿轮重新填写 Token。修改 Compose 中的 `CHECK_INTERVAL_SECONDS` 后，用 `docker compose up -d` 应用检查间隔。用 `docker compose stop download-manager` 停止检查。
