# cyeam-api

`api.cyeam.com` 的公开 Rust API。当前提供组字查询，所有字典数据都随镜像发布；运行时不需要数据库、密钥或第三方服务。

## API

`GET /v1/zuzi?parts=豐去皿`

支持直接输入部件，以及 `U+263F6` 形式的 Unicode 码点。响应中的 `exact` 为精确匹配，`partial` 为包含匹配；每个字包含拼音、码点和可用于前端继续查询的部件。输入最多八个已知部件，非法输入返回 `400`。

健康检查：`GET /healthz`

另外提供以下公开只读接口：

- `GET /v1/hanzi?text=你好`：批量返回部首、拆分、余笔、拼音与两个组词。
- `GET /v1/grades/onegrade1st`：返回 12 份教材字表之一及其完整查字结果。
- `GET /v1/pinyin?text=你好`：返回逐字拼音，练习纸由浏览器打印为 PDF。
- `POST /tool/asciiimg/exec`：以 `multipart/form-data` 上传一张 PNG、JPEG 或 GIF（文件字段名不限）并可选传 `columns`（40–170）；返回字符画 `info` 和实际列数 `columns`。上传上限 4MB。

浏览器 CORS 只允许 `https://www.cyeam.com` 和 `https://cyeam.com`，避免该公共 API 被任意网页直接消耗。

## 本地运行

```sh
cargo run
curl 'http://127.0.0.1:8080/v1/zuzi?parts=%E8%B1%90%E5%8E%BB%E7%9A%BF'
```

## 部署到 Fly

`fly.toml` 固定为单核最小规格 `shared-cpu-1x` / 256 MB，并允许闲置时缩至零，适合控制用量；首个请求会有冷启动延迟。

首次创建应用和域名映射时，在本机执行：

```sh
fly apps create cyeam-api
fly certs add api.cyeam.com
fly tokens create deploy -a cyeam-api -x 999999h
```

将最后一条命令的令牌保存到 GitHub 仓库的 Actions secret `FLY_API_TOKEN`。令牌不会写入仓库。DNS 按 `fly certs add` 的输出添加后，推送到 `main` 会自动测试并部署。
# cyeam-api
