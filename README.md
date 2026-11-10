# Download Manager

Download Manager 提供 Rust API 和 JavaScript 页面，显示下载文件、进度、速度、队列和来源。当前支持百度网盘，使用 [BaiduPCS-Go v4.0.2](https://github.com/qjfoidnh/BaiduPCS-Go/releases/tag/v4.0.2) 下载。迅雷将在后续版本接入。

## 构建和发布

在本机仓库目录运行 `./build_docker.sh` 构建镜像。`./build_docker.sh --publish` 向 GHCR 发布 AMD64 和 ARM64 镜像，需要已有的 GHCR 登录。GitHub Actions 在主分支的构建输入变化时调用相同的发布脚本。

镜像地址为 `ghcr.io/yzard/download_manager:4.0.2`。NAS 从 GHCR 拉取镜像。

## 本地开发

Rust 后端在 `src/backend/`，网页在 `src/frontend/`。测试在 `tests/backend/`，容器配置在 `docker/`。

在仓库根目录运行 Rust 测试。

```sh
cargo test --manifest-path src/backend/Cargo.toml --target-dir build/backend --locked
```

用 Docker Compose 和 Python 3 运行 `./run_playground.sh`，构建镜像并启动本地服务。运行数据保存在 `playground/`。打开 `http://127.0.0.1:8085` 创建登录账号。如需通过局域网访问，在 `playground/.env` 的 `WEB_ALLOWED_HOSTS` 中填入本机地址和端口。

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
      # RESET_USER_PASSWORD: "true"
      WEB_ALLOWED_HOSTS: "192.168.1.10:8085"
    ports:
      - "0.0.0.0:8085:8080"
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

## 创建或重设登录账号

首次打开网页时，创建唯一的登录账号。密码至少需要 12 个字符。保存后，网页显示登录表单。用户名和 Argon2id 密码哈希保存在 `/data/download_manager.sqlite3`。

忘记密码时，在 NAS 的服务配置中加入 `RESET_USER_PASSWORD: "true"`，然后在 NAS 运行下面的命令。

```sh
docker compose -f /data/docker/4_torrent.yaml up -d --no-deps download_manager
```

打开网页，填写用户名和新密码。保存后重设入口关闭。同一次启用期间，重启容器也会保持关闭。

重设完成后，删除该变量并再次应用 Compose 配置。以后需要重设时，再设为 `"true"` 并应用配置。

登录后可通过齿轮设置修改自己的密码。改密会结束已有会话。

## 打开网页

在局域网浏览器打开 `http://192.168.1.10:8085`。将示例地址替换为你的 NAS 地址，并把相同的主机和端口填入 `WEB_ALLOWED_HOSTS`。多个地址用逗号分隔。按网页提示创建账号或登录。

网页使用 `/api/v1/auth/` 和 `/api/v1/baidu/` 下的 POST 接口。浏览器会话需要同源请求。

## 百度专属设置

登录后，如果缺少 Token，网页会自动打开设置窗口。在“百度网盘”区域填写以下内容。

- `BDUSS` 和 `STOKEN` 是百度网盘的登录凭据。
- 网盘监控目录是百度网盘上的绝对路径，例如 `/NAS下载`。
- 下载保存位置是容器中的 `/downloads` 或其子目录，例如 `/downloads/archive`。
- 检查间隔以秒为单位，默认 300 秒。

保存后，设置写入 SQLite。后续通过右上角齿轮修改。已有 Token 时，输入框留空会保留原值。设置在当前下载轮次结束后应用。

获取百度 Token 时，先在电脑浏览器登录 [百度网盘](https://pan.baidu.com/)。在 Chrome 或 Edge 中按 `F12`，打开“应用程序 → Cookie → https://pan.baidu.com”，复制 `BDUSS` 和 `STOKEN` 的完整值。`STOKEN` 的名称为大写。[BaiduPCS-Go 登录说明](https://github.com/qjfoidnh/BaiduPCS-Go#登录百度帐号)

非会员用户将下载并发设为 1。设置由 BaiduPCS-Go 保存在 `/config/pcs_config.json` 中。

```sh
docker compose -f /data/docker/4_torrent.yaml run --rm --no-deps download_manager config set -max_parallel 1 -max_download_load 1
```

## 自动下载和续传

在百度网盘网页版创建设置中指定的目录，再把要下载到 NAS 的内容放进去。

容器启动后立即检查一次。每轮下载结束后，按百度网盘设置中的间隔等待下一轮检查。修改间隔后，在网页保存即可。

监控 `/NAS下载` 并保存到 `/downloads` 时，文件落在 NAS 的 `/data/downloads/baidu/NAS下载` 中。保存到 `/downloads/archive` 时，文件落在 `/data/downloads/baidu/archive/NAS下载` 中。

已经下载的同名文件会跳过，中断后会尝试续传未完成的文件。同名文件内容变更时需要手动处理旧文件。下载速度受百度非会员限制。进度页面每秒更新，逐行标注百度来源、进度条和速度。

```sh
docker compose -f /data/docker/4_torrent.yaml logs -f download_manager
docker compose -f /data/docker/4_torrent.yaml stop download_manager
```

如果页面提示百度登录失败，使用齿轮重新填写 Token。
