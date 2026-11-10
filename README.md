# Download Manager

Download Manager 提供 Rust API 和 JavaScript 页面，显示下载文件、进度、速度、队列和来源。当前支持百度网盘，使用 [BaiduPCS-Go v4.0.2](https://github.com/qjfoidnh/BaiduPCS-Go/releases/tag/v4.0.2) 下载。迅雷将在后续版本接入。

## 构建和发布

在本机仓库目录运行 `./build_docker.sh` 构建镜像。`./build_docker.sh --publish` 向 GHCR 发布 AMD64 和 ARM64 镜像，需要已有的 GHCR 登录。GitHub Actions 在主分支的构建输入变化时调用相同的发布脚本。

镜像地址为 `ghcr.io/yzard/download_manager:4.0.2`。NAS 从 GHCR 拉取镜像。

## NAS Compose 配置

在 `/data/docker/4_torrent.yaml` 中使用下面的服务配置。`lsio.env` 提供 `PUID`、`PGID`、`UMASK` 和 `TZ`。

```yaml
services:
  download_manager:
    container_name: download_manager
    image: ghcr.io/yzard/download_manager:4.0.2
    restart: always
    env_file: ./lsio.env
    environment:
      CHECK_INTERVAL_SECONDS: "300"
    ports:
      - "127.0.0.1:8085:8080"
    volumes:
      - ./data/download_manager:/data
      - ./configs/download_manager:/config
      - /data/downloads/baidu:/downloads
```

`/data` 保存 SQLite 数据库，其中分别存放允许登录的账号和百度专属设置。`/config` 保存 BaiduPCS-Go 的登录状态和续传配置。`/downloads` 是下载输出目录。镜像把 `HOME` 和 `BAIDUPCS_GO_CONFIG_DIR` 固定为 `/config`。

以下命令使用 NAS 上已有的 UID `1002` 和 GID `1003`。

```sh
ssh nas
sudo install -d -o 1002 -g 1003 -m 700 /data/docker/data/download_manager
sudo install -d -o 1002 -g 1003 -m 700 /data/docker/configs/download_manager
sudo install -d -o 1002 -g 1003 -m 755 /data/downloads/baidu
docker compose -f /data/docker/4_torrent.yaml pull download_manager
```

从旧版服务迁移时，先停止 `baidupcs-go`，再改名和启动新服务。沿用旧的 BaiduPCS-Go 配置及下载文件，可以保留登录状态和续传数据。

## 初始化允许登录的账号

在 NAS 终端运行下面的命令，将 `operator` 换成自己的用户名。终端提示输入两次密码，输入时不回显。密码至少需要 12 个字符。

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps -it download_manager init-user operator
docker compose -f /data/docker/4_torrent.yaml up -d --no-deps download_manager
```

初始化命令只允许建立一个账号。用户名和 Argon2id 密码哈希存放在 `/data/download_manager.sqlite3` 中。登录后可通过齿轮设置修改自己的密码。改密会结束已有会话，服务重启后也需要重新登录。

## 打开网页

在自己的电脑上建立 SSH 隧道。

```sh
ssh -N -L 8085:127.0.0.1:8085 nas
```

然后打开 `http://127.0.0.1:8085`。如果 Docker 在当前电脑运行，直接打开该地址即可。用允许登录的账号进入 Download Manager。

## 百度专属设置

登录后，如果缺少 Token，网页会自动打开设置窗口。在“百度网盘”区域填写以下内容。

- `BDUSS` 和 `STOKEN` 是百度网盘的登录凭据。
- 网盘监控目录是百度网盘上的绝对路径，例如 `/NAS下载`。
- 下载保存位置是容器中的 `/downloads` 或其子目录，例如 `/downloads/archive`。

保存后，设置写入 SQLite。后续通过右上角齿轮修改。已有 Token 时，输入框留空会保留原值。设置在当前下载轮次结束后应用。Token、监控目录和保存位置均由网页配置。

获取百度 Token 时，先在电脑浏览器登录 [百度网盘](https://pan.baidu.com/)。在 Chrome 或 Edge 中按 `F12`，打开“应用程序 → Cookie → https://pan.baidu.com”，复制 `BDUSS` 和 `STOKEN` 的完整值。`STOKEN` 的名称为大写。[BaiduPCS-Go 登录说明](https://github.com/qjfoidnh/BaiduPCS-Go#登录百度帐号)

非会员用户将下载并发设为 1。设置由 BaiduPCS-Go 保存在 `/config/pcs_config.json` 中。

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps download_manager config set -max_parallel 1 -max_download_load 1
```

## 自动下载和续传

在百度网盘网页版创建设置中指定的目录，再把要下载到 NAS 的内容放进去。

容器启动后立即检查一次。每轮下载结束后等待 `CHECK_INTERVAL_SECONDS` 秒再检查，默认 300 秒。修改检查间隔后，运行 `docker compose up -d` 应用环境变量。

监控 `/NAS下载` 并保存到 `/downloads` 时，文件落在 NAS 的 `/data/downloads/baidu/NAS下载` 中。保存到 `/downloads/archive` 时，文件落在 `/data/downloads/baidu/archive/NAS下载` 中。

已经下载的同名文件会跳过，中断后会尝试续传未完成的文件。同名文件内容变更时需要手动处理旧文件。下载速度受百度非会员限制。进度页面每秒更新，逐行标注百度来源、进度条和速度。

```sh
docker compose -f /data/docker/4_torrent.yaml logs -f download_manager
docker compose -f /data/docker/4_torrent.yaml stop download_manager
```

如果页面提示百度登录失败，使用齿轮重新填写 Token。
