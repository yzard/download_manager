# BaiduPCS-Go for NAS

这个仓库把 [BaiduPCS-Go v4.0.2](https://github.com/qjfoidnh/BaiduPCS-Go/releases/tag/v4.0.2) 放进一个约 15 MB 的镜像。当前只有百度网盘命令行工具，没有网页服务或监听端口。

在 NAS 的 `/data/docker/4_torrent.yaml` 中填写百度网盘登录凭据和监控目录。镜像将 `HOME` 和 `BAIDUPCS_GO_CONFIG_DIR` 都设为 `/config`。NAS 上的 `/data/docker/configs/baidupcs-go` 保存程序生成的登录状态。独立运行示例 `docker-compose.yaml` 使用 `.env`，可从 `.env.example` 复制。

## 在 NAS 上部署

GitHub Actions 将 AMD64 和 ARM64 镜像发布到 `ghcr.io/yzard/baidupcs-go-srv:4.0.2`。在 NAS 的 `/data/docker/4_torrent.yaml` 中将服务的 `image` 设为该地址。登录 NAS，创建可写目录并拉取镜像。

```sh
ssh nas
sudo install -d -o 1002 -g 1003 -m 700 /data/docker/configs/baidupcs-go
sudo install -d -o 1002 -g 1003 -m 755 /data/downloads/baidu
docker compose -f /data/docker/4_torrent.yaml pull baidupcs-go
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps baidupcs-go --version
```

将 `/data/docker/4_torrent.yaml` 的权限设为 `600`，将 `/data/docker/configs/baidupcs-go` 的权限设为 `700`，因为它们保存登录凭据。容器启动时会切换到 `lsio.env` 提供的 `PUID` 和 `PGID`。这组 UID 和 GID 必须对配置目录及 `/downloads` 对应的宿主机目录有写入权限。

服务通过 `docker compose up -d` 常驻，Docker 根据 `restart: always` 重启服务。单独使用本仓库时，可以改用根目录的 `docker-compose.yaml` 示例。

## 登录和设置非会员下载

先在电脑浏览器登录 [百度网盘](https://pan.baidu.com/)。在 Chrome 或 Edge 中按 `F12`，打开“应用程序”或“Application”标签。在左侧展开“Cookie”，选择 `https://pan.baidu.com`。分别找到 `BDUSS` 和 `STOKEN`，复制它们的“值”或“Value”。`STOKEN` 不是 `bdstoken`，两个值都不能公开分享。[BaiduPCS-Go 登录说明](https://github.com/qjfoidnh/BaiduPCS-Go#登录百度帐号)

在 NAS 上编辑 `/data/docker/4_torrent.yaml`。在 `baidupcs-go` 的 `environment` 下填写以下值。服务通过 `env_file: ./lsio.env` 加载 NAS 的通用变量。`MONITOR_DIRECTORY` 是百度网盘上的绝对路径，`CHECK_INTERVAL_SECONDS` 是两轮检查之间的等待秒数。

```yaml
BAIDU_BDUSS: "这里填BDUSS的值"
BAIDU_STOKEN: "这里填STOKEN的值"
MONITOR_DIRECTORY: "/NAS下载"
CHECK_INTERVAL_SECONDS: "300"
```

保存 Compose 文件后，运行下面的命令。第一条从环境变量登录，第二条列出网盘根目录。

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps baidupcs-go login-env
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps baidupcs-go ls /
```

第一条应显示你的百度账号，第二条应列出网盘文件。如果登录失败，请检查 `STOKEN` 是否来自百度网盘页面。程序会在 `/config/pcs_config.json` 保存登录状态，并在 Cookie 更新后自动重新登录。Docker 管理员可以通过容器环境查看这两个值。

你没有会员，因此只需再运行一次下面的命令，将两个下载并发设置为 1。程序会把设置写进 `/config/pcs_config.json`，以后不必重复设置。[非会员参数说明](https://github.com/qjfoidnh/BaiduPCS-Go#显示和修改程序配置项)

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps baidupcs-go config set -max_parallel 1 -max_download_load 1
```

## 自动下载和断点续传

在百度网盘网页版创建 `MONITOR_DIRECTORY` 指向的文件夹，再把要下载到 NAS 的内容放进去。更改环境变量后，重新运行 `docker compose up -d` 以应用设置。

先手动检查远端文件夹，再做一次下载：

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps baidupcs-go ls /NAS下载
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps baidupcs-go auto-download
```

用以下命令启动服务并查看日志。

```sh
docker compose -f /data/docker/4_torrent.yaml up -d --no-deps baidupcs-go
docker compose -f /data/docker/4_torrent.yaml logs -f baidupcs-go
```

容器启动后立即检查一次。每轮下载结束后，容器等待 `CHECK_INTERVAL_SECONDS` 秒再检查，默认 300 秒。下载轮次不会重叠。容器的 `/downloads` 挂载到 NAS 的 `/data/downloads/baidu`。监控 `/NAS下载` 时，文件保存在 `/data/downloads/baidu/NAS下载`。已经下载的同名文件会跳过，下载中断后下次运行会尝试续传未完成的文件。相同文件名但内容已变更时，请手动处理旧文件。下载速度仍受百度网盘非会员限制。[下载命令与续传说明](https://github.com/qjfoidnh/BaiduPCS-Go#下载文件目录)

如果任务没有下载新文件，先查看容器日志。Cookie 失效时，在 NAS 的 Compose 文件中更新两个值，再运行 `docker compose up -d`。用 `docker compose stop baidupcs-go` 停止检查。
